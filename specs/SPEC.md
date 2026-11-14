# chitin — High-Level Specification

**Status**: Draft — ready for fold-down
**Date**: 2026-11-14
**Scope**: Minimal Wayland compositor + desktop shell from Rust crates

---

## Vision

A self-contained, container-runnable Wayland compositor and desktop environment built entirely from Rust crates. GPU-accelerated via wgpu/Vulkan. Designed to be:

- **Minimal**: ~2-5k lines for a usable desktop (compositor + panel + launcher)
- **Composable**: Each subsystem is an independent crate with a clear API boundary
- **Agent-controllable**: The Wayland protocol IS the control surface — agents interact via standard protocol messages, no special backdoors
- **Container-friendly**: Runs in Podman/QEMU with GPU passthrough; no host desktop required

## Core Principle

**The kernel is just sockets.** Wayland makes this explicit: the compositor is a Unix socket server, everything else is a client. Chitin embraces this — no magic, no hidden state, all interaction through the protocol.

## Architecture Layers

```
┌─────────────────────────────────────────────────┐
│  chitin-shell (panel, launcher, notifications)  │  ← layer-shell clients
├─────────────────────────────────────────────────┤
│  chitin-core (compositor: WM, focus, layout)    │  ← Wayland protocol server
├─────────────────────────────────────────────────┤
│  chitin-render (wgpu/Vulkan frame pipeline)     │  ← GPU compositing
├─────────────────────────────────────────────────┤
│  chitin-input (libinput → seat routing)         │  ← input devices
├─────────────────────────────────────────────────┤
│  DRM/KMS + GBM (kernel scanout, buffer mgmt)    │  ← hardware
└─────────────────────────────────────────────────┘
```

## Subsystems (Fold-Down Targets)

Each section below becomes a subspec as implementation begins. The high-level contract is defined here; details live in the linked subspec.

### 1. Protocol & Compositor Core
**Subspec**: [specs/subspecs/01-compositor-core.md](subspecs/01-compositor-core.md) *(to be created)*

- Wayland protocol implementation via `wayland-server` + `smithay`
- Core objects: `wl_compositor`, `wl_shm`, `wl_seat`, `wl_registry`, `wl_output`
- XDG extensions: `xdg_wm_base` (window management), `wlr-layer-shell` (panels)
- Surface lifecycle: create → map → commit → unmap → destroy
- Event loop: single-threaded, driven by socket readiness + timerfd

**Contract**: Given a connected Wayland client, chitin-core accepts surfaces, manages their z-order and visibility, and routes input events to the focused surface.

### 2. GPU Rendering
**Subspec**: [specs/subspecs/02-gpu-rendering.md](subspecs/02-gpu-rendering.md) *(to be created)*

- Backend: wgpu (Vulkan on Linux) — single rendering path, no EGL complexity
- Frame pipeline: acquire client buffers → composite to target texture → present via DRM/KMS
- Buffer formats: ARGB8888 (client), XRGB8888 (scanout)
- Multi-monitor: one wgpu surface per physical output
- Fallback: software rasterizer for headless/testing (no GPU required)

**Contract**: Given a set of mapped surfaces with buffers, chitin-render produces a composited frame on the target display at ≥60fps for typical desktop workloads.

### 3. Input Handling
**Subspec**: [specs/subspecs/03-input.md](subspecs/03-input.md) *(to be created)*

- Source: libinput via `libinput-rs` (or raw evdev for minimalism)
- Routing: pointer/keyboard/touch events → focused surface via `wl_seat`
- Focus model: click-to-focus (initial), keyboard focus-follows-mouse (configurable)
- Keyboard shortcuts: Mod+Q (quit focused), Mod+D (show launcher), Mod+Arrows (tile)

**Contract**: Given active input devices, chitin-input routes events to the correct surface and maintains consistent focus state.

### 4. Window Management
**Subspec**: [specs/subspecs/04-window-management.md](subspecs/04-window-management.md) *(to be created)*

- `xdg_wm_base` top-level surfaces: title, min/max size, maximize, fullscreen, close
- Layout: stacking (initial), simple tiling (later)
- Decorations: client-side (SSO), compositor provides shadow only
- State persistence: window positions saved to `$XDG_STATE_HOME/chitin/layout.json`

**Contract**: Given mapped top-level surfaces, chitin-core arranges them on screen, handles user interactions (move, resize, close), and persists layout across sessions.

### 5. Desktop Shell
**Subspec**: [specs/subspecs/05-desktop-shell.md](subspecs/05-desktop-shell.md) *(to be created)*

- Panel: bottom bar via `wlr-layer-shell` (taskbar, clock, system tray placeholder)
- Launcher: application menu from `.desktop` files, spawned via `std::process`
- Notifications: `org.freedesktop.Notifications` D-Bus interface via `zbus`
- Shell is a SEPARATE binary (`chitin-shell`) — not part of the compositor

**Contract**: Given a running chitin compositor, chitin-shell provides a taskbar, application launcher, and notification area. It is just another Wayland client.

### 6. Container & Remote Access
**Subspec**: [specs/subspecs/06-container-remote.md](subspecs/06-container-remote.md) *(to be created)*

- Podman: GPU passthrough (`--device /dev/dri`), XDG_RUNTIME_DIR socket mount
- QEMU: virtio-gpu for testing without physical hardware
- Remote: RustDesk server integration for unattended Wayland access
- Headless mode: render to offscreen buffer, stream via WebRTC/RustDesk

**Contract**: Given a container or VM with GPU access, chitin starts, accepts clients, and is remotely accessible without a physical display.

## Non-Goals (Explicit)

- X11 compatibility (XWayland may be added later as an external client)
- Multi-seat support
- Advanced effects (blur, animations) — polish, not correctness
- Proprietary driver support beyond what Vulkan/wgpu handles
- Being a full OS (chitin runs ON Linux, not instead of it)

## Dependencies (External Crates)

| Crate | Purpose | Version Policy |
|-------|---------|----------------|
| `wayland-server` | Core protocol implementation | Latest stable |
| `smithay` / `smithay-wayland-protocols` | XDG extensions, utilities | Latest stable |
| `wgpu` | GPU rendering (Vulkan backend) | Latest stable |
| `libinput-rs` or `evdev` | Input device access | Latest stable |
| `zbus` | D-Bus system integration | Latest stable |
| `thiserror` | Error types | Latest stable |
| `tracing` | Structured logging | Latest stable |

## Success Criteria (Phase 1)

A "done" Phase 1 means:

1. `chitin` binary starts on a Linux machine with GPU, binds Wayland socket
2. A Wayland client (e.g., `weston-terminal`) can connect and display content
3. Keyboard/pointer input reaches the focused client
4. Mod+Q closes the focused window
5. `chitin-shell` shows a panel at the bottom with a working app launcher
6. Entire stack runs inside a Podman container with GPU passthrough
7. Remote access via RustDesk works unattended

## Open Questions

- wgpu vs raw Vulkan (ash): wgpu is safer/simpler, ash gives more control. Start with wgpu, escape hatch to ash if needed.
- libinput-rs vs raw evdev: libinput is more complete but heavier. For minimal, evdev might suffice.
- Single binary vs workspace from day one: Workspace is cleaner but more ceremony. Decision: workspace from day one (matches subspec boundaries).
- Session management: XDG autostart files? Custom config? Defer until Phase 2.

---

*This spec is the fold-down root. Each numbered subsystem section will become a standalone subspec document as implementation begins. Update this document when contracts change; put implementation details in the subspecs.*
