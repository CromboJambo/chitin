# Subspec 02: GPU Rendering

## Scope

GPU-accelerated surface composition and presentation. Takes committed buffers
from clients (via wl_shm) and renders them to a target using wgpu/Vulkan.

## Non-Goals (this iteration)

- No DRM/KMS output — virtual framebuffer only
- No damage tracking optimization yet
- No multi-output support
- No hardware video decode

## Dependencies

- Subspec 01: Compositor core, wl_shm buffer protocol ✓
- wgpu crate (Vulkan backend on Linux, software fallback for headless)

## Design

### Architecture

```
Client commits surface with wl_buffer (shm)
        │
        ▼
SurfaceState receives attach + commit
        │
        ▼
BufferPool: shm file → mmap → GPU texture upload (via wgpu Buffer COPY_DST)
        │
        ▼
Renderer: render pass draws all visible surfaces in stacking order
        │
        ▼
Present: swapchain frame or offscreen texture (virtual display)
```

### Key Components

1. **`GpuContext`** — owns wgpu Instance, Device, Queue
   - Created once at compositor startup
   - Selects Vulkan adapter; falls back to software if no GPU
   
2. **`BufferPool`** — manages shm → texture uploads
   - Tracks uploaded buffers by (fd, offset, stride)
   - Reuses GPU textures when the same shm buffer is re-attached
   - Uploads pixel data via `queue.write_buffer()` or `queue.submit()` with a staging buffer

3. **`Renderer`** — composites surfaces into a frame
   - Simple orthographic projection: each surface drawn at its position
   - Full-screen clear + draw call per visible surface
   - No blending/compositing effects yet (opaque surfaces only)

4. **`VirtualDisplay`** — holds the final composed frame
   - Offscreen texture at output resolution (1920x1080)
   - Can be read back for debugging or streamed to RustDesk later

### wgpu Setup

```rust
let instance = Instance::new(&InstanceDescriptor {
    backends: Backend::VULKAN | Backend::GL,
    ..Default::default()
});
// Prefer discrete GPU, fall back to integrated, then software.
let adapter = instance.request_adapter(&RequestAdapterOptions {
    power_preference: PowerPreference::HighPerformance,
    ..Default::default()
}).await?;
let (device, queue) = adapter.request_device(&DeviceDescriptor {
    label: Some("chitin-renderer"),
    required_features: wgpu::Features::empty(),
    required_limits: wgpu::Limits::default(),
}, None)?;
```

### Shm Buffer Upload Path

1. Client creates `wl_shm_pool` from a memfd with pixel data
2. Client creates `wl_buffer` (offset, width, height, stride, format)
3. Client attaches buffer to surface and commits
4. Compositor:
   - mmaps the shm file at the given offset
   - Creates/reuses a wgpu Buffer (COPY_DST) with the pixel data
   - Creates a wgpu TextureView from it (or copies into a texture)
   - Stores in SurfaceState for the next render pass

### Render Loop Integration

The existing event loop gains a render step:
```rust
loop {
    poll(&mut fds, timeout);
    accept_clients();
    dispatch_requests();
    
    // NEW: if any surface changed since last frame, re-render.
    if state.needs_redraw() {
        renderer.render(&state.visible_surfaces(), &gpu_ctx)?;
        state.mark_drawn();
    }
}
```

## API Surface (public)

```rust
pub struct GpuContext { /* wgpu instance/device/queue */ }
impl GpuContext {
    pub async fn new() -> io::Result<Self>;
    pub fn device(&self) -> &wgpu::Device;
    pub fn queue(&self) -> &wgpu::Queue;
}

pub struct Renderer { /* render pipeline, shaders */ }
impl Renderer {
    pub fn new(ctx: &GpuContext, width: u32, height: u32) -> io::Result<Self>;
    pub fn render(&mut self, surfaces: &[SurfaceFrame]) -> io::Result<()>;
}

pub struct SurfaceFrame {
    pub texture_view: wgpu::TextureView,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
```

## Acceptance Criteria

- [x] `cargo build` succeeds with wgpu dependency
- [x] `GpuContext::new()` creates a device (Vulkan or software)
- [x] Shm buffer can be uploaded to a GPU texture
- [x] `Renderer::render()` produces a frame without crashing
- [x] Compositor renders committed surfaces to the virtual display
- [x] Headless operation works (no physical GPU required — software fallback)

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| No Vulkan driver in container | wgpu software adapter (wgpu-hal's `dx12`/`metal` not applicable; use `gl` with llvmpipe or pure CPU) |
| Shm buffer format mismatch | Validate format at attach time, reject unsupported formats with error event |
| Upload performance for large buffers | Use staging buffers + DMA-BUF later (subspec 06); shm upload is fine for now |
| wgpu async setup vs sync event loop | Block on `tokio::runtime::Runtime::new().block_on()` at startup only |

## Related Specs

- [Subspec 01: Compositor Core](./01-compositor-core.md) — buffer protocol, surface lifecycle
- [Subspec 06: Container & Remote Access](./06-container-remote.md) — will consume rendered frames for streaming
