# Subspec 01: Compositor Core

**Status**: Implementing
**Parent**: [specs/SPEC.md](../SPEC.md) §1
**Date**: 2026-11-14

---

## Goal

A minimal Wayland compositor core that:
1. Binds a Wayland socket (`$XDG_RUNTIME_DIR/wayland-$n`)
2. Accepts client connections
3. Exposes `wl_registry`, `wl_compositor`, `wl_shm` globals
4. Handles surface creation and basic lifecycle
5. Runs an event loop processing client requests

## Design Decisions

### Raw `wayland-server`, not smithay

**Decision**: Use `wayland-server` directly; do NOT use the `smithay` framework crate for Phase 1.

**Rationale**:
- Smithay is a full compositor framework — it abstracts away exactly what we want to understand
- The learning goal is "how does a Wayland compositor actually work" — that means handling protocol messages ourselves
- Raw `wayland-server` gives us the socket, resource management, and fd passing; we implement the protocol logic
- Escape hatch: if we get stuck on low-level plumbing, smithay examples show the pattern

### Protocol interfaces

Wayland protocol interfaces (wl_compositor, wl_shm, etc.) are defined in XML and need Rust bindings. Options:
1. `wayland-scanner` at build time — generate from `wayland-protocols` XML
2. Pre-generated bindings crate — if one exists for 0.31.x

**Decision**: Use build-time code generation via `wayland-scanner` (already a transitive dep of `wayland-server`). The `wayland-protocols` crate provides the XML files.

### Event loop

**Decision**: Single-threaded, `mio`-based event loop:
- Watch Wayland server fd for client connections
- Process protocol messages as they arrive
- Timerfd for frame scheduling (later)
- No async runtime — Wayland is inherently synchronous per-client

## Implementation Plan

### Phase 1a: Socket + Registry (this iteration)

```rust
// Minimal structure:
pub struct ChitinCompositor {
    server: WaylandServer,
    globals: Vec<GlobalId>,
}

impl ChitinCompositor {
    pub fn new() -> Self;           // bind socket, create globals
    pub fn run(&mut self);          // event loop
}
```

- Create `WaylandServer` (binds socket automatically)
- Register `wl_registry` global
- Register `wl_compositor` global (v4+)
- Register `wl_shm` global
- Event loop: accept connections, dispatch messages
- Print log lines for each protocol interaction

### Phase 1b: Surface Lifecycle (next iteration)

- Handle `wl_compositor.create_surface` → track surface state
- Handle `wl_shm.create_pool` + `create_buffer` → map shared memory
- Handle `wl_surface.attach` + `commit` → buffer is ready to render
- Track z-order, focus (single-surface for now)

### Phase 1c: Rendering (subspec 02)

- Wire up wgpu
- Composite attached buffers to screen

## Protocol Objects We Implement

| Object | Interface | Purpose |
|--------|-----------|---------|
| Registry | `wl_registry` | Client discovers available globals |
| Compositor | `wl_compositor` | Creates surfaces, regions, sub-surfaces |
| SHM | `wl_shm` | Shared memory buffer allocation |
| Surface | `wl_surface` | Per-client drawable surface (created by compositor) |

**Not implementing yet**: `wl_seat`, `wl_output`, `xdg_wm_base`, `wlr-layer-shell` — these come in later phases.

## Wire Protocol Notes

Wayland messages are length-prefixed over Unix sockets:
```
[4 bytes: opcode + args count] [8 bytes each: args (int/fd/object)]
```
- Args can be: int32, fixed-point, object id, new_id, fd (SCM_RIGHTS)
- `wayland-server` handles the wire format; we implement the semantics

## Testing Strategy

- Unit tests: surface state machine transitions
- Integration test: spawn a minimal Wayland client (hand-written), verify it can connect and create a surface
- Later: run `weston-terminal` or `cage` as a real client

## Acceptance Criteria (Phase 1a)

- [x] `cargo build` succeeds
- [ ] `chitin-core` binary starts, prints socket path
- [ ] Client can connect and receive `global` events from registry
- [ ] Client can bind `wl_compositor` and create a surface
- [ ] Log output shows each protocol interaction
