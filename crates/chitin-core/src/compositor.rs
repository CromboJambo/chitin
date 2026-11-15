//! Wayland compositor core: socket binding, client acceptance, protocol dispatch.
//!
//! This module implements the bare-minimum Wayland server for subspec 01:
//! - Binds a Unix socket in XDG_RUNTIME_DIR (via `ListeningSocket`)
//! - Accepts client connections and registers them with the display
//! - Exposes `wl_registry`, `wl_compositor` (v4), and `wl_shm` (v3) globals
//! - Handles surface lifecycle: create, attach buffer, damage, commit, destroy
//! - Runs a poll-based event loop on both listener and display fds

use std::io;

use crate::gpu::{GpuContext, Renderer, SurfaceFrame};
use crate::window::{Size, WindowId, WindowManager};
use std::collections::HashMap;
use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::sync::Arc;

use wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;

use wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use wayland_server::GlobalDispatch;
use wayland_server::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::{self, WlCompositor},
    wl_region::{self, WlRegion},
    wl_output::{self, WlOutput},
    wl_shm::{self, WlShm},
    wl_shm_pool::{self, WlShmPool},
    wl_surface::{self, WlSurface},
};
use wayland_server::{Client, DataInit, Display, DisplayHandle, ListeningSocket, New, Resource, WEnum};


// ── Buffer tracking ──────────────────────────────────────────────────────────

/// A mmap'd region of shared memory.
///
/// Wraps a raw pointer to mmap'd data with proper cleanup on drop.
/// Marked Send + Sync because we only access it on the main compositor thread.
#[derive(Debug)]
pub struct MmapRegion {
    ptr: *const u8,
    size: usize,
}

unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

impl MmapRegion {
    /// Mmap a file descriptor for read access.
    pub fn new(fd: std::os::unix::io::RawFd, size: usize) -> Option<Self> {
        if size == 0 {
            return None;
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            tracing::warn!("failed to mmap shm pool fd={}: {}", fd, std::io::Error::last_os_error());
            None
        } else {
            Some(Self { ptr: ptr as *const u8, size })
        }
    }

    /// Get a byte slice of the entire region.
    pub fn as_slice(&self) -> &[u8] {
        if self.ptr.is_null() || self.size == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }

    /// Get a byte slice at an offset (bounds-checked).
    pub fn get(&self, offset: usize, len: usize) -> Option<&[u8]> {
        if offset + len > self.size {
            return None;
        }
        Some(&self.as_slice()[offset..offset + len])
    }

    /// Get the raw pointer (null if not mapped).
    pub fn ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Get the size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for MmapRegion {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.size > 0 {
            unsafe {
                libc::munmap(self.ptr as *mut _, self.size);
            }
        }
    }
}

/// Tracked shm pool: an mmap'd file descriptor.
#[derive(Debug)]
pub struct ShmPoolData {
    pub fd: std::os::unix::io::RawFd,
    pub size: usize,
    /// Shared with buffers created from this pool (Arc prevents double-munmap).
    pub region: Option<std::sync::Arc<MmapRegion>>,
}

impl std::fmt::Display for ShmPoolData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ShmPool(fd={}, size={})", self.fd, self.size)
    }
}

/// Tracked shm buffer: a region within a pool.
#[derive(Debug, Clone)]
pub struct ShmBufferData {
    pub region: Option<std::sync::Arc<MmapRegion>>,
    pub offset: i32,
    pub width: u32,
    pub height: u32,
    pub stride: i32,
    /// Raw format value from the protocol (e.g. wl_shm::Format::Xrgb8888 = 1).
    pub format: u32,
}

impl std::fmt::Display for ShmBufferData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ShmBuffer({}x{} stride={} offset={})", self.width, self.height, self.stride, self.offset)
    }
}

impl Default for ShmBufferData {
    fn default() -> Self {
        // XRGB8888 = 1 in the wl_shm protocol
        Self { region: None, offset: 0, width: 0, height: 0, stride: 0, format: 1 }
    }
}

/// Per-surface state: attached buffer and position.
#[derive(Debug, Default, Clone)]
pub struct SurfaceState {
    pub buffer: Option<ShmBufferData>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A mapped surface ready for compositing.
#[derive(Debug)]
pub struct MappedSurface {
    /// Clone of the WlSurface resource handle (keeps it alive).
    pub surface: WlSurface,
    /// Current state of this surface.
    pub state: SurfaceState,
}


// ── Client data ──────────────────────────────────────────────────────────────

/// Per-client user data (empty for now — subspec 01 doesn't track client state).
#[derive(Debug)]
struct ChitinClientData;

impl ClientData for ChitinClientData {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!("client {:?} initialized", client_id);
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::info!("client {:?} disconnected: {:?}", client_id, reason);
    }
}

// ── Compositor state ─────────────────────────────────────────────────────────

/// Main compositor state, passed to all protocol callbacks.
pub struct ChitinState {
    /// Display handle for creating resources and sending events.
    pub dh: DisplayHandle,
    /// Whether a redraw is needed (set when surfaces change).
    pub needs_redraw: bool,
    /// Mapped surfaces in z-order (bottom to top) for compositing.
    pub mapped_surfaces: Vec<MappedSurface>,
    /// GPU context (None if headless/fallback).
    pub gpu: Option<GpuContext>,
    /// Renderer (created when GPU is available).
    pub renderer: Option<Renderer>,
    /// Window manager: tracks top-level windows in a slab.
    pub windows: WindowManager,
    /// Maps wl_surface resource ID → xdg surface info (for surfaces with an xdg role).
    pub xdg_surfaces: HashMap<ObjectId, XdgSurfaceInfo>,
    /// Maps xdg_toplevel resource ID → window ID.
    pub toplevel_window_map: HashMap<ObjectId, WindowId>,
}

/// Mutable state for a surface that has an xdg role.
#[derive(Debug)]
pub struct XdgSurfaceInfo {
    /// The underlying wl_surface resource handle.
    pub surface: WlSurface,
    /// The configured size from the last configure event.
    pub configured_size: Size,
    /// Serial of the last sent configure event.
    pub configure_serial: u32,
    /// Whether a configure has been sent but not yet acknowledged.
    pub pending_configure: bool,
    /// The window ID if this surface has a toplevel role (None = no role yet).
    pub window_id: Option<WindowId>,
}

impl ChitinState {
    pub fn new(dh: DisplayHandle) -> Self {
        Self {
            dh,
            needs_redraw: false,
            mapped_surfaces: Vec::new(),
            gpu: None,
            renderer: None,
            windows: WindowManager::new(Size { width: 1920, height: 1080 }),
            xdg_surfaces: HashMap::new(),
            toplevel_window_map: HashMap::new(),
        }
    }

    /// Attach GPU context and renderer (called after setup).
    pub fn with_gpu(mut self, gpu: Option<GpuContext>, renderer: Option<Renderer>) -> Self {
        self.gpu = gpu;
        self.renderer = renderer;
        self
    }

    /// Find a mapped surface by its resource handle.
    fn find_surface(&mut self, surface: &WlSurface) -> Option<&mut MappedSurface> {
        self.mapped_surfaces.iter_mut().find(|ms| ms.surface.id() == surface.id())
    }

    /// Extract pixel data from a shm buffer's mmap'd region.
    fn extract_pixels(buf: &ShmBufferData) -> Option<Vec<u8>> {
        let region = buf.region.as_ref()?;
        let row_bytes = buf.width * 4; // RGBA8
        let start = buf.offset as usize;
        if start + (row_bytes as usize * buf.height as usize) > region.size {
            tracing::warn!("buffer data exceeds mmap region");
            return None;
        }

        let slice = region.as_slice();
        // Compact rows if stride != width*4.
        if buf.stride == row_bytes as i32 {
            Some(slice[start..start + (row_bytes as usize * buf.height as usize)].to_vec())
        } else {
            let mut out = Vec::with_capacity(row_bytes as usize * buf.height as usize);
            for y in 0..buf.height {
                let row_start = start + (y as usize * buf.stride as usize);
                out.extend_from_slice(&slice[row_start..row_start + row_bytes as usize]);
            }
            Some(out)
        }
    }

    /// Render all mapped surfaces to the GPU output.
    fn render_frame(&mut self) {
        let (Some(gpu), Some(renderer)) = (&self.gpu, self.renderer.as_mut()) else {
            return; // Headless mode — nothing to render.
        };

        let mut frames = Vec::new();
        for ms in &self.mapped_surfaces {
            if let Some(buf) = &ms.state.buffer {
                match Self::extract_pixels(buf) {
                    Some(pixels) => match gpu.upload_texture(&pixels, buf.width, buf.height, buf.stride as u32) {
                        Ok(view) => frames.push(SurfaceFrame {
                            texture_view: view,
                            x: ms.state.x,
                            y: ms.state.y,
                            width: ms.state.width,
                            height: ms.state.height,
                        }),
                        Err(e) => tracing::error!("texture upload failed: {}", e),
                    },
                    None => tracing::warn!("failed to extract pixels from buffer"),
                }
            }
        }

        if let Err(e) = renderer.render(&frames) {
            tracing::error!("render pass failed: {}", e);
        } else {
            tracing::debug!("rendered {} surfaces", frames.len());
        }
    }
}

// ── Global dispatch implementations ──────────────────────────────────────────

/// Global for `wl_compositor`: clients bind this to create surfaces and regions.
impl GlobalDispatch<WlCompositor, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlCompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the compositor object with unit user data.
        data_init.init(resource, ());
        tracing::debug!("wl_compositor bound");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

/// Global for `wl_shm`: clients bind this to create shared memory pools.
impl GlobalDispatch<WlShm, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlShm>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the shm object.
        let wl_shm = data_init.init(resource, ());

        // Advertise supported pixel formats (argb8888=0, xrgb8888=1).
        wl_shm.send_event(wl_shm::Event::Format { format: WEnum::Unknown(0) }).ok();
        wl_shm.send_event(wl_shm::Event::Format { format: WEnum::Unknown(1) }).ok();
        tracing::debug!("wl_shm bound, advertised ARGB8888 + XRGB8888");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

// ── Request dispatch implementations ─────────────────────────────────────────


/// Global for `wl_output`: clients bind this to query screen geometry.
impl GlobalDispatch<WlOutput, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlOutput>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the output object.
        let wl_output = data_init.init(resource, ());

        // Send geometry event: virtual 1920x1080 output at origin.
        // subpixel=0 (unknown), make="chitin", model="virtual", transform=0 (normal).
        wl_output.send_event(wl_output::Event::Geometry {
            x: 0,
            y: 0,
            physical_width: 527,
            physical_height: 296,
            subpixel: WEnum::Unknown(0),
            make: "chitin".into(),
            model: "virtual".into(),
            transform: WEnum::Unknown(0),
        }).ok();

        // Send mode event: current mode is 1920x1080 @ 60Hz.
        // flags=0x1 (CURRENT).
        wl_output.send_event(wl_output::Event::Mode {
            flags: WEnum::Unknown(0x1),
            width: 1920,
            height: 1080,
            refresh: 60_000, // mHz (millihertz)
        }).ok();

        // Send done event (version 2+).
        wl_output.send_event(wl_output::Event::Done).ok();

        tracing::debug!("wl_output bound, advertised 1920x1080 virtual output");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

/// Handle `wl_output` requests (only destroy).
impl wayland_server::Dispatch<WlOutput, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlOutput,
        request: wl_output::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_output::Request::Release => {
                tracing::debug!("wl_output release");
            }
            req => tracing::warn!("unhandled wl_output request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlOutput, _data: &()) {
    }
}

/// Handle `wl_compositor` requests: create_surface, create_region.
impl wayland_server::Dispatch<WlCompositor, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                tracing::debug!("create_surface");
                data_init.init(id, std::sync::Mutex::new(SurfaceState::default()));
            }
            wl_compositor::Request::CreateRegion { id } => {
                tracing::debug!("create_region");
                data_init.init(id, ());
            }
            req => tracing::warn!("unhandled wl_compositor request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlCompositor, _data: &()) {
    }
}

/// Handle `wl_shm` requests: create_pool, release.
impl wayland_server::Dispatch<WlShm, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                tracing::debug!("create_pool fd={:?} size={}", fd, size);
                // Mmap the pool fd and store in the pool's user data.
                let raw_fd = fd.as_raw_fd();
                let region = MmapRegion::new(raw_fd, size as usize).map(std::sync::Arc::new);
                let pool_data = ShmPoolData { fd: raw_fd, size: size as usize, region };
                data_init.init(id, Some(pool_data));
            }
            wl_shm::Request::Release => {
                tracing::debug!("wl_shm release");
            }
            req => tracing::warn!("unhandled wl_shm request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlShm, _data: &()) {
    }
}

/// Handle `wl_shm_pool` requests: create_buffer, destroy.
impl wayland_server::Dispatch<WlShmPool, Option<ShmPoolData>, ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlShmPool,
        request: wl_shm_pool::Request,
        data: &Option<ShmPoolData>,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, format } => {
                tracing::debug!(
                    "create_buffer: {}x{} stride={} offset={} format={:?} from pool {:?}",
                    width, height, stride, offset, format, data
                );
                // Store buffer metadata referencing the pool's mmap'd data.
                let buf_data = data.as_ref().and_then(|pool| {
                    pool.region.as_ref().map(|region| ShmBufferData {
                        region: Some(std::sync::Arc::clone(region)),
                        offset, width: width as u32, height: height as u32, stride, format: match format { wayland_server::WEnum::Value(v) => v as u32, wayland_server::WEnum::Unknown(v) => v },
                    })
                });
                data_init.init(id, buf_data);
            }
            wl_shm_pool::Request::Destroy => {
                tracing::debug!("destroy pool");
            }
            req => tracing::warn!("unhandled wl_shm_pool request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlShmPool, _data: &Option<ShmPoolData>) {
    }
}

/// Handle `wl_buffer` requests: destroy.
impl wayland_server::Dispatch<WlBuffer, Option<ShmBufferData>, ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlBuffer,
        request: wl_buffer::Request,
        data: &Option<ShmBufferData>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_buffer::Request::Destroy => {
                tracing::debug!("destroy buffer {:?}", data);
            }
            req => tracing::warn!("unhandled wl_buffer request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlBuffer, _data: &Option<ShmBufferData>) {
    }
}

/// Handle `wl_surface` requests: attach, damage, frame, commit, destroy.
impl wayland_server::Dispatch<WlSurface, std::sync::Mutex<SurfaceState>, ChitinState> for ChitinState {
    fn request(
        state: &mut ChitinState,
        _client: &Client,
        resource: &WlSurface,
        request: wl_surface::Request,
        data: &std::sync::Mutex<SurfaceState>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        let mut sd = data.lock().unwrap();
        match request {
            wl_surface::Request::Attach { buffer, x, y } => {
                tracing::debug!(
                    "attach buffer={:?} at ({}, {})",
                    buffer.as_ref().map(|b| b.id()),
                    x,
                    y
                );
                sd.x = x;
                sd.y = y;
                if let Some(buf) = buffer {
                    // Try to get the buffer's metadata from its user data.
                    if let Some(Some(bd)) = buf.data::<Option<ShmBufferData>>() {
                        tracing::debug!("resolved buffer: {}", bd);
                        sd.buffer = Some(bd.clone());
                        sd.width = bd.width;
                        sd.height = bd.height;
                    }
                } else {
                    sd.buffer = None;
                    sd.width = 0;
                    sd.height = 0;
                }
            }
            wl_surface::Request::Damage { x, y, width, height } => {
                tracing::debug!("damage ({}, {}) {}x{}", x, y, width, height);
            }
            wl_surface::Request::Frame { callback } => {
                let _ = callback;
            }
            wl_surface::Request::Commit => {
                tracing::debug!("commit — surface now {}x{} at ({}, {})", sd.width, sd.height, sd.x, sd.y);
                // Update the central registry with this surface's state.
                let ms = MappedSurface {
                    surface: resource.clone(),
                    state: (*sd).clone(),
                };
                if let Some(existing) = state.find_surface(resource) {
                    existing.state = (*sd).clone();
                } else {
                    state.mapped_surfaces.push(ms);
                    tracing::info!("surface mapped: {}x{} at ({}, {})", sd.width, sd.height, sd.x, sd.y);
                }
                state.needs_redraw = true;
            }
            wl_surface::Request::Destroy => {
                tracing::debug!("destroy surface");
            }
            req => tracing::warn!("unhandled wl_surface request: {:?}", req),
        }
    }

    fn destroyed(state: &mut ChitinState, _client: ClientId, resource: &WlSurface, _data: &std::sync::Mutex<SurfaceState>) {
        // Remove from the central registry.
        state.mapped_surfaces.retain(|ms| ms.surface.id() != resource.id());
        tracing::debug!("surface destroyed, {} surfaces remain", state.mapped_surfaces.len());
    }
}

/// Handle `wl_region` requests: add, subtract, destroy.
impl wayland_server::Dispatch<WlRegion, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlRegion,
        request: wl_region::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_region::Request::Add { x, y, width, height } => {
                tracing::debug!("region add ({}, {}) {}x{}", x, y, width, height);
            }
            wl_region::Request::Subtract { x, y, width, height } => {
                tracing::debug!("region subtract ({}, {}) {}x{}", x, y, width, height);
            }
            wl_region::Request::Destroy => {
                tracing::debug!("destroy region");
            }
            req => tracing::warn!("unhandled wl_region request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlRegion, _data: &()) {
    }
}

// ── Event loop ───────────────────────────────────────────────────────────────

/// Compositor setup result: display, listener, and state.
pub struct CompositorSetup {
    pub display: Display<ChitinState>,
    pub listener: ListeningSocket,
    pub socket_name: String,
    /// GPU context for rendering (None if unavailable).
    pub gpu: Option<GpuContext>,
    /// Renderer for compositing surfaces (None if GPU unavailable).
    pub renderer: Option<Renderer>,
}

/// Set up the compositor: bind socket, create display, register globals.
///
/// This is the non-blocking setup phase. Call [`run_event_loop`] to start processing.
pub fn setup() -> io::Result<CompositorSetup> {
    // Step 1: Bind the listening socket.
    let listener = ListeningSocket::bind_auto("wayland", 0..32)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let socket_name = listener
        .socket_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no socket name"))?
        .to_string_lossy()
        .into_owned();

    tracing::info!("chitin compositor listening on {}", socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    // Step 2: Create the display and register globals.
    let display: Display<ChitinState> = Display::new()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let dh = display.handle();
    dh.create_global::<ChitinState, WlCompositor, ()>(4, ());
    dh.create_global::<ChitinState, WlShm, ()>(3, ());
    dh.create_global::<ChitinState, WlOutput, ()>(4, ());
    dh.create_global::<ChitinState, XdgWmBase, ()>(4, ());
    tracing::info!("globals registered: wl_compositor(v4), wl_shm(v3), wl_output(v4), xdg_wm_base(v4)");

    tracing::info!("initializing GPU context...");
    // Try to initialize GPU context (non-fatal if it fails).
    let gpu = match GpuContext::new() {
        Ok(g) => {
            tracing::info!("GPU rendering enabled");
            Some(g)
        }
        Err(e) => {
            tracing::warn!("GPU unavailable, running headless: {}", e);
            None
        }
    };

    let renderer = gpu.as_ref().and_then(|g| Renderer::new(g, 1920, 1080).ok());

    Ok(CompositorSetup {
        display,
        listener,
        socket_name,
        gpu,
        renderer,
    })
}

/// Run the Wayland compositor event loop (blocking).
///
/// This is the main entry point for the compositor binary. It:
/// 1. Calls [`setup`] to bind socket and register globals
/// 2. Runs a poll-based event loop on both listener and display fds
pub fn run() -> io::Result<()> {
    let mut setup = setup()?;
    let mut state = ChitinState::new(setup.display.handle())
        .with_gpu(setup.gpu.take(), setup.renderer.take());

    tracing::info!("entering event loop");
    run_event_loop(&mut setup.display, &setup.listener, &mut state)
}

/// Run the poll-based event loop on an already-set-up compositor.
fn run_event_loop(
    display: &mut Display<ChitinState>,
    listener: &ListeningSocket,
    state: &mut ChitinState,
) -> io::Result<()> {
    // Get the display handle once — it's valid for the lifetime of the display.
    let mut dh = display.handle();

    loop {
        // Build poll set: [listener_fd, display_fd].
        let listener_borrowed = unsafe { BorrowedFd::borrow_raw(listener.as_raw_fd()) };
        let display_fd = display.backend().poll_fd();

        let mut fds = [
            rustix::event::PollFd::new(&listener_borrowed, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(&display_fd, rustix::event::PollFlags::IN),
        ];

        // Poll with a 100ms timeout (allows periodic housekeeping later).
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 100_000_000 };
        let ready = match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(n) => n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => {
                tracing::error!("poll error: {}", e);
                break;
            }
        };

        if ready == 0 {
            continue; // Timeout — nothing to do.
        }

        // Check listener for new clients.
        if fds[0].revents().contains(rustix::event::PollFlags::IN) {
            match listener.accept() {
                Ok(Some(stream)) => {
                    tracing::info!("new client connected");
                    match dh.insert_client(stream, Arc::new(ChitinClientData)) {
                        Ok(_client) => tracing::debug!("client inserted"),
                        Err(e) => tracing::error!("failed to insert client: {}", e),
                    }
                }
                Ok(None) => {
                    // No connection ready (spurious wakeup or race).
                }
                Err(e) => {
                    tracing::warn!("accept error: {}", e);
                }
            }
        }

        // Check display for pending client requests.
        if fds[1].revents().contains(rustix::event::PollFlags::IN) {
            match display.dispatch_clients(state) {
                Ok(n) => {
                    if n > 0 {
                        tracing::trace!("dispatched {} requests", n);
                    }
                }
                Err(e) => {
                    tracing::error!("dispatch error: {}", e);
                }
            }
            // Flush outgoing events to clients.
            if let Err(e) = display.flush_clients() {
                tracing::warn!("flush error: {}", e);
            }
        }

        // Render if a surface changed since the last frame.
        if state.needs_redraw {
            state.render_frame();
            state.needs_redraw = false;
        }
    }

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the compositor can bind a socket, create a display, and register globals.
    #[test]
    fn test_compositor_setup() {
        // Bind a listening socket.
        let listener = ListeningSocket::bind_auto("chitin-test", 0..32)
            .expect("failed to bind listening socket");

        // Verify we got a socket name.
        let socket_name = listener.socket_name().expect("no socket name");
        assert!(!socket_name.is_empty());

        // Create the display.
        let display: Display<ChitinState> = Display::new().expect("failed to create display");

        // Register globals.
        let dh = display.handle();
        dh.create_global::<ChitinState, WlCompositor, ()>(4, ());
        dh.create_global::<ChitinState, WlShm, ()>(3, ());
    dh.create_global::<ChitinState, WlOutput, ()>(4, ());

        // Verify the listener is still valid (we can check its fd).
        let _fd = listener.as_raw_fd();
        assert!(_fd >= 0);

        // Drop everything to clean up.
        drop(display);
        drop(listener);
    }

    /// Test that the state struct can be created.
    #[test]
    fn test_state_creation() {
        let display: Display<ChitinState> = Display::new().expect("failed to create display");
        let dh = display.handle();
        let state = ChitinState::new(dh);
        // Just verify it exists and has the expected fields.
        let _dh = &state.dh;
    }
}
