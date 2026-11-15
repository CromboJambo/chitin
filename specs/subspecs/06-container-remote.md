# Subspec 06: Container & Remote Access

**Status**: Draft
**Parent**: [specs/SPEC.md](../SPEC.md) §6
**Date**: 2026-11-14

---

## Goal

Run chitin inside a Podman container (or QEMU VM) with GPU access, and make it remotely accessible via RustDesk without a physical display. The entire desktop environment should be deployable as a single container image.

## Non-Goals (this iteration)

- No multi-user session management
- No Wayland security policy enforcement (all clients are trusted within the container)
- No HDR or variable refresh rate
- No audio passthrough (Phase 2)

## Dependencies

- Subspec 01–05: All core subsystems working ✓
- Podman with GPU device support (`--device /dev/dri`)
- RustDesk server (self-hosted `rustdesk-server` or hbbs/hbbr relay)
- Optional: QEMU with virtio-gpu for testing without physical hardware

## Design Decisions

### Container runtime: Podman, not Docker

**Decision**: Target Podman specifically.

**Rationale**:
- Rootless containers match our security model (no privileged daemon)
- `--device /dev/dri` GPU passthrough works cleanly in rootless mode
- Podman is already the standard on Artix/Arch systems in the fleet
- Docker's cgroup requirements are heavier than needed for this use case

### GPU access: direct device passthrough

**Decision**: Pass `/dev/dri/card*` and `/dev/dri/renderD*` directly into the container.

```bash
podman run --device /dev/dri --security-opt label=disable chitin:latest
```

- wgpu detects the Vulkan device through the standard `/dev/dri` nodes
- No nvidia-container-toolkit needed (we're on AMD/Intel in the fleet)
- For NVIDIA hosts: add `--gpus all` (requires nvidia-container-toolkit)

### Headless operation: virtual output

**Decision**: When no DRM/KMS connector is detected, chitin creates a virtual output.

```rust
pub struct Output {
    pub name: String,          // "headless-0" or "DP-1" etc.
    pub width: u32,            // default 1920
    pub height: u32,           // default 1080
    pub refresh_rate: u32,     // default 60
    pub is_virtual: bool,      // true when no physical connector
}
```

- Virtual output renders to an offscreen wgpu texture (no scanout)
- The texture is the "framebuffer" that RustDesk captures and streams
- Configurable via `CHITIN_OUTPUT_W`, `CHITIN_OUTPUT_H`, `CHITIN_OUTPUT_HZ` env vars

### Remote access: RustDesk as a Wayland client

**Decision**: Run `rustdesk` inside the container as a regular Wayland client that captures the virtual output.

Architecture:
```
┌─────────────────────────────────────────────┐
│  Podman Container                            │
│  ┌───────────┐    ┌──────────────────────┐  │
│  │ chitin    │◄──►│ rustdesk (client)     │  │
│  │ compositor│    │ captures virtual out  │  │
│  └───────────┘    └──────────┬───────────┘  │
│                              │ Wayland       │
│                              ▼               │
│                      ┌──────────────┐        │
│                      │ rustdesk     │        │
│                      │ server/hbbs  │        │
│                      └──────┬───────┘        │
└─────────────────────────────┼────────────────┘
                              │ WebSocket/TCP
                              ▼
                    ┌──────────────────┐
                    │ Remote Viewer    │
                    │ (any platform)   │
                    └──────────────────┘
```

- RustDesk runs headless mode (`rustdesk --headless`) inside the container
- It connects to the Wayland socket, captures the virtual output texture
- Streams via its own protocol to the rustdesk-server relay
- Remote user connects with any RustDesk client (Windows/Mac/Linux/mobile)
- Input from remote is injected as synthetic Wayland events (keyboard + pointer)

**Why RustDesk over alternatives**:
- Self-hostable (hbbs/hbbr relay servers, no third-party dependency)
- Works over NAT without port forwarding (uses relay)
- Low latency for desktop use cases
- Actively maintained, Rust-based server components
- Already in the fleet's tooling (user has it set up)

**Alternative considered**: `wayvnc` (VNC over Wayland). Simpler protocol but higher latency and no built-in relay. RustDesk is better for unattended remote access across networks.

### Input injection for remote users

RustDesk needs to send keyboard/pointer events into the compositor. Two approaches:

1. **Synthetic evdev device** (preferred): Create a virtual input device via `uinput` that RustDesk writes to. The compositor sees it as a regular input device.
   - Pros: Clean separation, works with existing input pipeline
   - Cons: Requires `uinput` kernel module + permissions in container

2. **Protocol-level injection**: RustDesk talks directly to the compositor via a custom protocol extension (`chitin_remote_input`).
   - Pros: No kernel dependencies
   - Cons: Custom protocol, less standard

**Decision**: Use approach 1 (uinput) for Phase 1. Fall back to approach 2 if uinput isn't available in the container environment.

### Container image

**Decision**: Multi-stage build producing a minimal image.

```dockerfile
# Stage 1: Build
FROM rust:slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p chitin-compositor -p chitin-shell

# Stage 2: Runtime
FROM debian:bookworm-slim
RUN apt-get install -y --no-install-recommends \
    libvulkan1 mesa-vulkan-drivers \
    rustdesk \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/chitin-compositor /usr/local/bin/
COPY --from=builder /build/target/release/chitin-shell /usr/local/bin/
COPY assets/cursors/ /usr/share/icons/chitin/

ENTRYPOINT ["/entrypoint.sh"]
```

`entrypoint.sh`:
```bash
#!/bin/bash
# Start compositor in background
chitin-compositor &
COMPOSITOR_PID=$!

# Wait for Wayland socket
for i in {1..30}; do
    [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && break
    sleep 0.5
done

# Start shell
chitin-shell &

# Start RustDesk headless
rustdesk --headless --key "$RUSTDESK_KEY" &

# Wait for compositor (keeps container alive)
wait $COMPOSITOR_PID
```

### QEMU testing (no GPU required)

For CI or environments without a physical GPU:

```bash
qemu-system-x86_64 \
    -m 2G \
    -display none \
    -device virtio-gpu-pci \
    -drive file=chitin.qcow2,format=qcow2 \
    -nographic
```

- `virtio-gpu` provides a basic GPU that wgpu can use (Vulkan via lavapipe software renderer)
- No physical display needed; output goes to the virtual framebuffer
- RustDesk captures from there as normal

## Implementation Plan

### Phase 6a: Headless mode (this iteration)

- Detect absence of DRM/KMS connectors at startup
- Create a virtual 1920x1080 output when no physical display found
- Render to offscreen texture (no scanout)
- Expose the texture for capture (memory-mapped or via protocol)

### Phase 6b: Containerization (next iteration)

- Write Dockerfile + entrypoint script
- Test GPU passthrough with `podman run --device /dev/dri`
- Verify compositor starts, shell appears, weston-terminal works inside container

### Phase 6c: Remote access (final iteration)

- Install and configure RustDesk in the container
- Set up uinput device for remote input injection
- Configure rustdesk-server relay (self-hosted on a fleet node)
- End-to-end test: connect from a remote machine, interact with the desktop

## API Surface (environment variables & config)

```bash
# Output configuration
CHITIN_OUTPUT_W=1920          # virtual output width
CHITIN_OUTPUT_H=1080          # virtual output height
CHITIN_OUTPUT_HZ=60           # virtual refresh rate
CHITIN_OUTPUT_NAME=headless-0 # wl_output name

# Input devices (container)
CHITIN_INPUT_DEVICES=/dev/input/event0,/dev/input/event1  # explicit device list

# Remote access
RUSTDESK_KEY=<base64-id>      # RustDesk machine ID
RUSTDESK_RELAY=hbbs.example.com:21117  # relay server address

# Logging
CHITIN_LOG_LEVEL=info         # tracing level
```

## Testing Strategy

- Integration test: start compositor with no GPU → verify virtual output created
- Container test: `podman build && podman run --device /dev/dri` → verify desktop starts
- Remote test: connect via RustDesk client from another machine → interact with desktop
- Performance test: measure frame latency (input → rendered frame) over remote connection

## Acceptance Criteria

- [ ] Compositor starts with no physical display and creates a virtual output
- [ ] `podman build` produces a working container image (<500MB target)
- [ ] Container runs with `--device /dev/dri` and GPU rendering works
- [ ] `weston-terminal` is interactive inside the container
- [ ] RustDesk captures and streams the virtual output
- [ ] Remote keyboard/pointer input reaches applications in the container
- [ ] End-to-end: connect from a remote machine, open a terminal, type text
- [ ] Container can be stopped/started without data loss (state persisted to volume)

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| No Vulkan driver in container | Install `mesa-vulkan-drivers` + use lavapipe software fallback; GPU passthrough for performance |
| uinput not available in rootless container | Fall back to protocol-level input injection (custom extension) |
| RustDesk licensing/availability | Self-hosted hbbs/hbbr is MIT licensed; rustdesk client is also open source |
| Frame latency over remote connection | Use hardware encoding (VA-API via /dev/dri) for capture; RustDesk handles compression |
| Container GPU permissions vary by host | Document required permissions; provide a `chitin-container-run.sh` wrapper script |

## Related Specs

- [Subspec 01: Compositor Core](./01-compositor-core.md) — startup, output detection
- [Subspec 02: GPU Rendering](./02-gpu-rendering.md) — offscreen rendering, headless fallback
- [Subspec 03: Input Handling](./03-input.md) — synthetic input via uinput
- [Fleet deployment docs](../../crabjar/) — Podman container management patterns
