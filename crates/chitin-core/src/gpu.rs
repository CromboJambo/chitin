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
    #[error("buffer upload failed: {0}")]
    BufferUpload(String),
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

        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(ctx)) => Ok(ctx),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(GpuError::DeviceCreation("GPU init timed out (30s)".into())),
        }
    }

    fn init_inner() -> Result<Self, GpuError> {
        tracing::debug!("gpu: creating wgpu instance (sync)...");
        let start = std::time::Instant::now();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default(),
            backend_options: Default::default(),
            memory_budget_thresholds: Default::default(),
        });
        tracing::debug!("gpu: instance created in {:.2}s", start.elapsed().as_secs_f64());

        // Use enumerate_adapters directly to avoid request_adapter() hanging
        tracing::debug!("gpu: enumerating adapters...");
        let start = std::time::Instant::now();
        let adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN);
        tracing::debug!("gpu: found {} adapters in {:.2}s", adapters.len(), start.elapsed().as_secs_f64());

        // Pick the best adapter: prefer discrete GPU, then integrated, then CPU
        let adapter = adapters.iter()
            .find(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
            .or_else(|| adapters.iter().find(|a| a.get_info().device_type == wgpu::DeviceType::IntegratedGpu))
            .or_else(|| adapters.first())
            .cloned()
            .ok_or_else(|| GpuError::DeviceCreation("no Vulkan adapter found".into()))?;

        let adapter_name = adapter.get_info().name.clone();
        tracing::debug!("gpu: selected adapter: {}", adapter_name);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|e| GpuError::DeviceCreation(e.to_string()))?;

        let (device, queue) = rt.block_on(async {
            tracing::debug!("gpu: requesting device...");
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
            tracing::debug!("gpu: device created");

            Ok::<_, GpuError>((device, queue))
        })?;

        tracing::info!("GPU context created ({})", adapter_name);
        Ok(Self {
            _instance: instance,
            device,
            queue,
        })
    }

    /// Upload a raw pixel buffer (RGBA8) to a GPU texture and return its view.
    ///
    /// `data` is the raw RGBA bytes, `width`/`height` are dimensions,
    /// `stride` is bytes per row (may be > width*4 due to alignment).
    pub fn upload_texture(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<wgpu::TextureView, GpuError> {
        // If stride matches width*4, we can upload directly.
        // Otherwise, we need to compact the rows.
        let row_bytes = width * 4;
        let compacted: Vec<u8>;
        let upload_data = if stride == row_bytes {
            data.to_vec()
        } else {
            compacted = (0..height)
                .flat_map(|y| {
                    let offset = y as usize * stride as usize;
                    data[offset..offset + row_bytes as usize].to_vec()
                })
                .collect();
            compacted
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chitin-surface"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &upload_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(view)
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
    device: wgpu::Device,
    queue: wgpu::Queue,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Output texture (virtual display).
    _output_texture: wgpu::Texture,
    output_view: wgpu::TextureView,
    width: u32,
    height: u32,
    /// Render pipeline for drawing surfaces.
    pipeline: wgpu::RenderPipeline,
    /// Vertex buffer for full-screen quads (one per surface).
    vertex_buffer: wgpu::Buffer,
}

/// Vertex format for surface quads: position (x,y in NDC) + uv (texture coords).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

impl SurfaceVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 2]>() as u64,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }
    }
}

impl Renderer {
    /// Create a new renderer for the given output size.
    pub fn new(ctx: &GpuContext, width: u32, height: u32) -> Result<Self, GpuError> {
        // Output texture (virtual display).
        let output_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chitin-output"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Shader module for surface rendering.
        let shader = ctx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chitin-surface-shader"),
            source: wgpu::ShaderSource::Wgsl(quote_surface_shader().into()),
        });

        // Bind group layout.
        let bind_group_layout = ctx.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("surface-bind-group"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
        });

        // Render pipeline.
        let pipeline_layout = ctx.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chitin-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = ctx.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chitin-surface-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[SurfaceVertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        // Vertex buffer for a single quad (6 vertices = 2 triangles).
        // We'll update this per surface draw.
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chitin-quad"),
            size: std::mem::size_of::<SurfaceVertex>() as u64 * 6,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        tracing::info!("renderer created: {}x{} virtual display", width, height);
        Ok(Self {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            bind_group_layout,
            _output_texture: output_texture,
            output_view,
            width,
            height,
            pipeline,
            vertex_buffer,
        })
    }

    /// Render a frame from the given surfaces.
    pub fn render(&mut self, surfaces: &[SurfaceFrame]) -> Result<(), GpuError> {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("chitin-frame"),
        });

        // Sampler for texture sampling.
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("chitin-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("chitin-render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1, g: 0.1, b: 0.15, a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Draw each surface.
            for surface in surfaces {
                // Compute NDC position for the quad.
                // Surface at (x, y) with (w, h) on a (W, H) display:
                let ndc_x0 = 2.0 * surface.x as f32 / self.width as f32 - 1.0;
                let ndc_x1 = 2.0 * (surface.x + surface.width as i32) as f32 / self.width as f32 - 1.0;
                let ndc_y0 = 1.0 - 2.0 * surface.y as f32 / self.height as f32;
                let ndc_y1 = 1.0 - 2.0 * (surface.y + surface.height as i32) as f32 / self.height as f32;

                // Build quad vertices: top-left, top-right, bottom-left,
                //                  top-right, bottom-right, bottom-left.
                let vertices = [
                    SurfaceVertex { position: [ndc_x0, ndc_y0], uv: [0.0, 0.0] },
                    SurfaceVertex { position: [ndc_x1, ndc_y0], uv: [1.0, 0.0] },
                    SurfaceVertex { position: [ndc_x0, ndc_y1], uv: [0.0, 1.0] },
                    SurfaceVertex { position: [ndc_x1, ndc_y0], uv: [1.0, 0.0] },
                    SurfaceVertex { position: [ndc_x1, ndc_y1], uv: [1.0, 1.0] },
                    SurfaceVertex { position: [ndc_x0, ndc_y1], uv: [0.0, 1.0] },
                ];

                // Upload vertices.
                self.queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));

                // Create bind group for this surface's texture.
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("surface-bind-group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&surface.texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                });

                // Draw.
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }

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

/// WGSL shader for surface rendering.
fn quote_surface_shader() -> String {
    r#"
struct VsOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_main(@location(0) position: vec2f, @location(1) uv: vec2f) -> VsOut {
    var out: VsOut;
    out.position = vec4f(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var surface_tex: texture_2d<f32>;
@group(0) @binding(1) var surface_sampler: sampler;

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4f {
    return textureSample(surface_tex, surface_sampler, input.uv);
}
"#
    .to_string()
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

    #[test]
    fn test_texture_upload() {
        let ctx = match GpuContext::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("GPU unavailable, skipping texture upload test: {}", e);
                return;
            }
        };

        // Create a 4x4 red square.
        let data: Vec<u8> = (0..16).flat_map(|_| [255u8, 0, 0, 255]).collect();
        let view = ctx.upload_texture(&data, 4, 4, 16);
        assert!(view.is_ok());
    }
}
