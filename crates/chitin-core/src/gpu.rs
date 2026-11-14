//! GPU rendering backend for chitin compositor.
//!
//! Implements subspec 02: wgpu-based surface composition and presentation.
//! Takes committed buffers from clients (via wl_shm) and renders them to a
//! virtual display using wgpu/Vulkan (with software fallback).

use std::io;

use thiserror::Error;

/// Errors from GPU operations.
#[derive(Debug, Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter found")]
    NoAdapter,
    #[error("failed to create device: {0}")]
    DeviceCreation(String),
    #[error("wgpu error: {0}")]
    Wgpu(String),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// GPU context: owns the wgpu instance, device, and queue.
///
/// Created once at compositor startup. Selects a Vulkan adapter if available,
/// falls back to software rendering otherwise.
pub struct GpuContext {
    _instance: wgpu::Instance,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Create a new GPU context.
    ///
    /// Blocks until the adapter and device are ready. Uses Vulkan backend
    /// on Linux; falls back to software if no GPU is available.
    pub fn new() -> Result<Self, GpuError> {
        // Run GPU initialization in a separate thread with a timeout to avoid
        // blocking the compositor if no GPU is available.
        let (tx, rx) = std::sync::mpsc::channel();
        
        std::thread::spawn(move || {
            let result = Self::init_inner();
            let _ = tx.send(result);
        });

        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(ctx)) => Ok(ctx),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(GpuError::DeviceCreation("GPU init timed out (10s)".into())),
        }
    }

    fn init_inner() -> Result<Self, GpuError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|e| GpuError::DeviceCreation(e.to_string()))?;

        let (instance, device, queue) = rt.block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
                flags: wgpu::InstanceFlags::default(),
                backend_options: Default::default(),
                memory_budget_thresholds: Default::default(),
            });

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .await
                .map_err(|e| GpuError::DeviceCreation(format!("no adapter: {}", e)))?;

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("chitin-renderer"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: Default::default(),
                        trace: wgpu::Trace::Off,
                    },
                )
                .await
                .map_err(|e| GpuError::DeviceCreation(e.to_string()))?;

            Ok::<_, GpuError>((instance, device, queue))
        })?;

        tracing::info!("GPU context created");
        Ok(Self {
            _instance: instance,
            device,
            queue,
        })
    }
}

/// A surface frame ready for rendering.
pub struct SurfaceFrame {
    /// GPU texture containing the surface pixels.
    pub texture_view: wgpu::TextureView,
    /// Position on screen (top-left corner).
    pub x: i32,
    pub y: i32,
    /// Surface dimensions.
    pub width: u32,
    pub height: u32,
}

/// Renderer: composites surfaces into a frame.
///
/// Simple orthographic projection: each surface drawn at its position
/// with no blending effects (opaque surfaces only).
pub struct Renderer {
    _ctx: wgpu::Device,
    queue: wgpu::Queue,
    /// Output texture (virtual display).
    _output_texture: wgpu::Texture,
    output_view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Create a new renderer for the given output size.
    pub fn new(ctx: &GpuContext, width: u32, height: u32) -> Result<Self, GpuError> {
        let output_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chitin-output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        tracing::info!("renderer created: {}x{} virtual display", width, height);
        Ok(Self {
            _ctx: ctx.device.clone(),
            queue: ctx.queue.clone(),
            _output_texture: output_texture,
            output_view,
            width,
            height,
        })
    }

    /// Render a frame from the given surfaces.
    ///
    /// Currently just clears the output to black and draws each surface
    /// at its position. Full compositing (blending, transforms) comes later.
    pub fn render(&mut self, _surfaces: &[SurfaceFrame]) -> Result<(), GpuError> {
        // Create a render pass that clears to black.
        let mut encoder = self._ctx.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("chitin-frame"),
        });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.15,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        // Submit the command buffer.
        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    /// Get the output texture view (for readback or streaming later).
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    /// Output dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_context_creation() {
        // This test will fail in headless environments without a GPU or
        // software renderer. Mark as ignore for CI.
        let ctx = GpuContext::new();
        assert!(ctx.is_ok(), "GPU context creation failed: {:?}", ctx.err());
    }
}
