//! DRM/KMS display backend for chitin compositor.
//!
//! Implements real monitor output by opening a DRM device, setting up KMS
//! modesetting (connector → encoder → CRTC), and page-flipping rendered frames.
//!
//! Uses "dumb" GEM buffers for simplicity — linear, CPU-writable buffers that
//! work with any DRM driver. Sufficient for the initial implementation; can be
//! upgraded to GBM or prime-imported wgpu textures later for zero-copy paths.

use std::io;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, OwnedFd};
use std::ptr;

use drm_ffi as drm;
use drm_ffi::drm_sys::*;
use nix::fcntl::{open, OFlag};
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use thiserror::Error;

// DRM constants not exposed by drm-sys.
const DRM_MODE_CONNECTED: u32 = 1;
const DRM_FORMAT_XRGB8888: u32 = 0x34325258; // fourcc 'X','R','2','4'
const DRM_MODE_PAGE_FLIP_WAIT: u32 = 0x0100;

/// Errors from display operations.
#[derive(Debug, Error)]
pub enum DisplayError {
    #[error("no DRM device with connected display found")]
    NoConnectedDisplay,
    #[error("DRM ioctl failed: {0}")]
    Ioctl(#[from] io::Error),
    #[error("failed to open DRM device: {0}")]
    OpenFailed(String),
    #[error("failed to map buffer: {0}")]
    MapFailed(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
}

/// DRM/KMS display backend.
///
/// Owns the DRM file descriptor, CRTC state, and double-buffered scanout buffers.
/// Created once at compositor startup when a real display is available.
pub struct DisplayBackend {
    fd: OwnedFd,
    crtc_id: u32,
    #[allow(dead_code)]
    connector_id: u32,
    width: u32,
    height: u32,
    stride: u32,
    buffer_size: usize,

    // Double buffering for tear-free page flips.
    back_handle: u32,
    front_handle: u32,
    back_fb: u32,
    front_fb: u32,
    back_map: *mut u8,
    front_map: *mut u8,
}

impl DisplayBackend {
    /// Create a new display backend by auto-detecting a connected display.
    ///
    /// Opens DRM devices in order (card0, card1, ...), finds the first one with
    /// a connected connector, and sets up KMS modesetting at the preferred mode.
    pub fn new() -> Result<Self, DisplayError> {
        for card in 0..8 {
            let path = format!("/dev/dri/card{card}");
            match Self::open_card(&path) {
                Ok(display) => return Ok(display),
                Err(DisplayError::NoConnectedDisplay) => continue,
                Err(e) => {
                    tracing::debug!("display: card{card} unavailable: {e}");
                    continue;
                }
            }
        }
        Err(DisplayError::NoConnectedDisplay)
    }

    fn open_card(path: &str) -> Result<Self, DisplayError> {
        let fd = open(
            path,
            OFlag::O_RDWR | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| DisplayError::OpenFailed(e.to_string()))?;

        // Get DRM resources.
        let mut fbs = Vec::new();
        let mut crtcs = Vec::new();
        let mut connectors = Vec::new();
        let mut encoders = Vec::new();
        let _resources = drm::mode::get_resources(
            fd.as_fd(),
            Some(&mut fbs),
            Some(&mut crtcs),
            Some(&mut connectors),
            Some(&mut encoders),
        )?;

        // Find a connected connector and its preferred mode.
        let mut best: Option<(u32, drm_mode_modeinfo)> = None;

        for &connector_id in connectors.iter() {
            let mut props = Vec::new();
            let mut prop_values = Vec::new();
            let mut modes = Vec::new();
            let mut conn_encoders = Vec::new();

            let connector = drm::mode::get_connector(
                fd.as_fd(),
                connector_id,
                Some(&mut props),
                Some(&mut prop_values),
                Some(&mut modes),
                Some(&mut conn_encoders),
                false,
            )?;

            if connector.connection != DRM_MODE_CONNECTED {
                continue;
            }

            // Find the preferred mode.
            let preferred_mode = modes
                .iter()
                .find(|m| m.type_ & DRM_MODE_TYPE_PREFERRED != 0)
                .or_else(|| modes.first())
                .ok_or(DisplayError::NoConnectedDisplay)?;

            tracing::info!(
                "display: found connected connector {} at {}x{}",
                connector_id,
                preferred_mode.hdisplay,
                preferred_mode.vdisplay
            );

            best = Some((connector_id, *preferred_mode));
            break;
        }

        let (connector_id, mode) = best.ok_or(DisplayError::NoConnectedDisplay)?;

        let width = mode.hdisplay as u32;
        let height = mode.vdisplay as u32;

        // Find a CRTC that can drive this connector.
        let crtc_id = Self::find_crtc_for_connector(
            &fd.as_fd(),
            connector_id,
            &crtcs,
        )?;

        // Allocate two dumb buffers for double buffering.
        let stride = width * 4; // RGBA8888
        let buffer_size = (stride as usize) * height as usize;

        let back_db = drm::mode::dumbbuffer::create(fd.as_fd(), width, height, 32, 0)?;
        let front_db = drm::mode::dumbbuffer::create(fd.as_fd(), width, height, 32, 0)?;

        // Map both buffers into CPU address space.
        let back_map_info = drm::mode::dumbbuffer::map(
            fd.as_fd(),
            back_db.handle,
            0,
            0,
        )?;
        let front_map_info = drm::mode::dumbbuffer::map(
            fd.as_fd(),
            front_db.handle,
            0,
            0,
        )?;

        let back_map = Self::mmap_buffer(&fd.as_fd(), back_map_info.offset, buffer_size)?;
        let front_map = Self::mmap_buffer(&fd.as_fd(), front_map_info.offset, buffer_size)?;

        // Create framebuffers for both buffers.
        let handles = [back_db.handle, 0, 0, 0];
        let pitches = [stride, 0, 0, 0];
        let offsets = [0u32; 4];
        let modifiers = [0u64; 4];

        let back_fb_cmd = drm::mode::add_fb2(
            fd.as_fd(),
            width,
            height,
            DRM_FORMAT_XRGB8888,
            &handles,
            &pitches,
            &offsets,
            &modifiers,
            0,
        )?;

        let front_handles = [front_db.handle, 0, 0, 0];
        let front_fb_cmd = drm::mode::add_fb2(
            fd.as_fd(),
            width,
            height,
            DRM_FORMAT_XRGB8888,
            &front_handles,
            &pitches,
            &offsets,
            &modifiers,
            0,
        )?;

        let back_fb = back_fb_cmd.fb_id;
        let front_fb = front_fb_cmd.fb_id;

        // Initialize the back buffer with a dark background.
        unsafe {
            ptr::write_bytes(back_map, 0x10, buffer_size);
        }

        // Set up the CRTC: attach framebuffer, set mode, enable.
        drm::mode::set_crtc(
            fd.as_fd(),
            crtc_id,
            back_fb,
            0,
            0,
            &[connector_id],
            Some(mode),
        )?;

        tracing::info!(
            "display: KMS active — connector {}, CRTC {} at {}x{}",
            connector_id,
            crtc_id,
            width,
            height
        );

        Ok(Self {
            fd,
            crtc_id,
            connector_id,
            width,
            height,
            stride,
            buffer_size,
            back_handle: back_db.handle,
            front_handle: front_db.handle,
            back_fb,
            front_fb,
            back_map,
            front_map,
        })
    }

    fn find_crtc_for_connector(
        fd: &std::os::fd::BorrowedFd<'_>,
        connector_id: u32,
        crtcs: &[u32],
    ) -> Result<u32, DisplayError> {
        let mut props = Vec::new();
        let mut prop_values = Vec::new();
        let mut modes = Vec::new();
        let mut conn_encoders = Vec::new();

        let _connector = drm::mode::get_connector(
            *fd,
            connector_id,
            Some(&mut props),
            Some(&mut prop_values),
            Some(&mut modes),
            Some(&mut conn_encoders),
            false,
        )?;

        // Try the currently assigned encoder's CRTC first.
        if let Some(&encoder_id) = conn_encoders.first() {
            let encoder = drm::mode::get_encoder(*fd, encoder_id)?;
            if encoder.crtc_id != 0 {
                return Ok(encoder.crtc_id);
            }
        }

        // Otherwise find any CRTC that's not currently in use.
        for &crtc_id in crtcs.iter() {
            let crtc = drm::mode::get_crtc(*fd, crtc_id)?;
            if crtc.fb_id == 0 {
                return Ok(crtc_id);
            }
        }

        // Fall back to first CRTC.
        crtcs.first().copied().ok_or(DisplayError::NoConnectedDisplay)
    }

    fn mmap_buffer(
        fd: &std::os::fd::BorrowedFd<'_>,
        offset: u64,
        size: usize,
    ) -> Result<*mut u8, DisplayError> {
        let ptr = unsafe {
            mmap(
                None,
                NonZeroUsize::new(size).ok_or(DisplayError::InvalidState("zero-size buffer".into()))?,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                offset as i64,
            )
        };

        match ptr {
            Err(e) => Err(DisplayError::MapFailed(e.to_string())),
            Ok(ptr) => Ok(ptr.as_ptr() as *mut u8),
        }
    }

    /// Present a frame to the display.
    ///
    /// Copies `pixels` (RGBA8888, row-major, width*height*4 bytes) into the
    /// back buffer and issues a page flip with wait-for-vblank to avoid tearing.
    pub fn present(&mut self, pixels: &[u8]) -> Result<(), DisplayError> {
        if pixels.len() < self.buffer_size {
            return Err(DisplayError::InvalidState(format!(
                "pixel buffer too small: {} < {}",
                pixels.len(),
                self.buffer_size
            )));
        }

        // Copy pixels to back buffer, respecting stride.
        unsafe {
            for y in 0..self.height {
                let src = &pixels[(y as usize * self.width as usize * 4)..];
                let dst = self.back_map.add(y as usize * self.stride as usize);
                ptr::copy_nonoverlapping(src.as_ptr(), dst, (self.width as usize) * 4);
            }
        }

        // Page flip with wait-for-vblank.
        drm::mode::page_flip(
            self.fd.as_fd(),
            self.crtc_id,
            self.back_fb,
            DRM_MODE_PAGE_FLIP_EVENT | DRM_MODE_PAGE_FLIP_WAIT,
            0,
        )?;

        // Swap buffer roles for next frame.
        std::mem::swap(&mut self.back_handle, &mut self.front_handle);
        std::mem::swap(&mut self.back_fb, &mut self.front_fb);
        std::mem::swap(&mut self.back_map, &mut self.front_map);

        Ok(())
    }

    /// Output dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Output width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Output height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for DisplayBackend {
    fn drop(&mut self) {
        // Unmap buffers.
        unsafe {
            if !self.back_map.is_null() {
                munmap(
                    std::ptr::NonNull::new(self.back_map as *mut std::ffi::c_void).unwrap(),
                    self.buffer_size,
                )
                .ok();
            }
            if !self.front_map.is_null() {
                munmap(
                    std::ptr::NonNull::new(self.front_map as *mut std::ffi::c_void).unwrap(),
                    self.buffer_size,
                )
                .ok();
            }
        }

        // Destroy framebuffers.
        drm::mode::rm_fb(self.fd.as_fd(), self.back_fb).ok();
        drm::mode::rm_fb(self.fd.as_fd(), self.front_fb).ok();

        // Destroy dumb buffers.
        drm::mode::dumbbuffer::destroy(self.fd.as_fd(), self.back_handle).ok();
        drm::mode::dumbbuffer::destroy(self.fd.as_fd(), self.front_handle).ok();
    }
}
