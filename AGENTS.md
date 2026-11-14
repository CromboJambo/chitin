# chitin — Project Guidelines

## Scope

**In scope:** Minimal Wayland compositor + desktop shell from Rust crates. GPU rendering (wgpu/Vulkan), containerized execution, agent-controllable display surface.

**Not in scope:**
- Full KDE/Plasma parity (use KDE for that)
- X11 compatibility (XWayland is a later concern)
- New kernel or OS (runs on Linux, uses DRM/KMS via existing stack)
- Accessibility stack (deferred)

## Build & Test

```bash
cargo check --workspace
cargo test --workspace
cargo clippy -- -D warnings   # CI gate only; dev: cargo clippy
```

## Architecture

Workspace layout (evolves as subspecs are folded down):

```
crates/
  chitin-core/       # Compositor core: protocol, surfaces, event loop
  chitin-render/     # GPU rendering via wgpu
  chitin-input/      # Input handling and routing
  chitin-shell/      # Desktop shell (panel, launcher) — layer-shell client
```

## Spec-Driven Development

This project is spec-first. The high-level spec at [specs/SPEC.md](specs/SPEC.md) links to subspecs under `specs/subspecs/`. Implementation follows the fold-down pattern:

1. High-level spec defines boundaries and contracts
2. Subspecs define implementation details for each subsystem
3. Code implements subspecs; specs are updated as decisions crystallize
4. ADRs in `specs/ADRs/` capture significant architectural decisions

## Coding Style

- `cargo fmt` before commit
- `thiserror` for library errors, `?` propagation, no `unwrap()` outside tests
- 500 LoC module limit (split by concern when exceeded)
- `snake_case` modules/functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants

## Git

Remote: SSH only (`git@github.com:CromboJambo/chitin.git`). Never switch to HTTPS.
