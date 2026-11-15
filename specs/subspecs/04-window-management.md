# Subspec 04: Window Management

**Status**: Draft
**Parent**: [specs/SPEC.md](../SPEC.md) §4
**Date**: 2026-11-14

---

## Goal

Manage `xdg_wm_base` top-level surfaces: map/unmap, position, resize, maximize, fullscreen, close, and z-order. Provide a clean API between the compositor core (surface tracking) and window manager logic (layout decisions).

## Non-Goals (this iteration)

- No animations or transitions
- No workspace/virtual desktop management
- No snap-to-edge auto-tiling (manual tiling via shortcuts only)
- No per-window rules or policies

## Dependencies

- Subspec 01: Compositor core, surface lifecycle ✓
- Subspec 02: GPU rendering (positioning surfaces in the render pass) ✓
- Subspec 03: Input handling (focus model, keyboard shortcuts) — concurrent development
- `smithay-wayland-protocols` for `xdg_wm_base` bindings
- `slab` crate — pre-allocated storage for window state (WindowId = slab index)

## Design Decisions

### Window model (inspired by iced-rs, storage via slab)

**Decision**: Define our window API mirroring the structure used by `iced::window`, with window state stored in a `slab::Slab` for efficient allocation and stable integer IDs:

```rust
use slab::Slab;

/// Opaque window identifier — this IS the slab index.
/// Small, fast to compare/hash/serialize, reused after removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(usize);

/// Actions the shell/WM can perform on a window.
pub enum WindowAction {
    Close(WindowId),
    Move(WindowId, Point),
    Resize(WindowId, Size),
    SetMaximized(WindowId, bool),
    SetFullscreen(WindowId, bool),
    SetTitle(WindowId, String),
    Focus(WindowId),
}

/// Events emitted when window state changes.
pub enum WindowEvent {
    Mapped { id: WindowId, title: String },
    Unmapped { id: WindowId },
    Moved { id: WindowId, position: Point },
    Resized { id: WindowId, size: Size },
    Maximized { id: WindowId, maximized: bool },
    Fullscreen { id: WindowId, fullscreen: bool },
    Closed { id: WindowId },
}

/// Window manager state — windows stored in a slab.
pub struct WindowManager {
    /// Pre-allocated storage for window state. Insert returns the index,
    /// which becomes the WindowId. Lookup is O(1) by index.
    windows: Slab<WindowState>,
    focused_window: Option<WindowId>,
    output_size: Size,
}
```

**Why `Slab` over `BTreeMap`/`HashMap`:**
- Keys are small integers (the slab index) — trivially Hash/Eq/Copy, no wrapping needed
- Pre-allocated storage avoids fragmentation from frequent map/unmap cycles
- Key reuse is automatic: when a window is removed, its index can be reassigned
- Matches how Wayland itself works — every protocol object has a numeric ID assigned by the server
- The `WlSurface` resource handle is stored INSIDE `WindowState`, not used as a key

This gives us a clean boundary: the compositor tracks surfaces and sends `WindowEvent`s; the WM logic responds with `WindowAction`s. Neither needs to know about the other's internals.

### xdg_wm_base implementation

The compositor implements the manager side of `xdg_wm_base`:

1. Client creates `xdg_surface` from `wl_surface`
2. Client calls `get_toplevel(xdg_surface)` → we create an `XdgToplevel` resource
3. We send `configure` with initial size (client's requested or default 800x600)
4. Client commits buffer → surface is mapped, appears on screen
5. Client can request: move, resize, maximize, fullscreen, close
6. When closed: we send `close`, client destroys surfaces, we remove from registry

**Key protocol details**:
- Every state change requires a round-trip: client requests → compositor acknowledges via `configure` with new state → client commits
- `configure_serial` must be tracked and echoed back in `ack_configure`
- Initial configure MUST happen before the first commit (client waits for it)

### Layout model

**Phase 1: Stacking layout**
- New windows appear centered, offset by (32, 32) from the previous window
- Z-order: most recently focused is on top
- No automatic tiling; user uses Mod+Arrows to tile manually

**Tiling shortcuts** (handled by WM logic):
- Mod+Left: tile left half (position 0,0, size W/2 x H)
- Mod+Right: tile right half (position W/2, 0, size W/2 x H)
- Mod+Up: maximize
- Mod+Down: restore to previous size/position

### Decorations

**Decision**: Client-side decorations (CSD) only. The compositor does NOT draw title bars or borders.

- Clients using `xdg_toplevel` provide their own decorations
- Compositor adds a subtle drop shadow (rendered as a semi-transparent quad behind the surface)
- No resize handles — resizing is done via client-side edge detection or shortcuts

### State persistence

**Decision**: Save window positions to `$XDG_STATE_HOME/chitin/layout.json` on unmap, restore on next session.

```json
{
  "windows": [
    {
      "app_id": "org.freedesktop.Terminal",
      "position": [100, 50],
      "size": [800, 600],
      "maximized": false
    }
  ]
}
```

- Keyed by `xdg_toplevel.app_id` (not window title — titles change)
- On map: if app_id exists in layout.json, restore position/size
- On unmap: update entry in layout.json
- File written atomically (write to temp + rename)

## Implementation Plan

### Phase 4a: Basic xdg_wm_base (this iteration)

- Generate `xdg_wm_base` bindings from smithay-wayland-protocols
- Implement manager side: handle `get_toplevel`, send `configure`
- Track window state: position, size, maximized, fullscreen, title
- Map/unmap lifecycle: surface appears/disappears on screen
- Close handling: `xdg_toplevel.close` → client destroys → we clean up

### Phase 4b: Window operations (next iteration)

- Move/resize via pointer drag (client requests `move_grab`/`resize_grab`)
- Maximize/fullscreen state transitions
- Z-order management (focus raises window)
- Keyboard shortcuts for tiling

### Phase 4c: Polish (later)

- State persistence to layout.json
- Drop shadow rendering
- Focus ring for active window (subtle border highlight)

## API Surface (public)

```rust
use slab::Slab;

pub struct WindowManager {
    windows: Slab<WindowState>,
    focused_window: Option<WindowId>,
    output_size: Size,
}

#[derive(Debug, Clone)]
pub struct WindowState {
    pub surface: WlSurface,       // Wayland resource handle (stored, not used as key)
    pub toplevel: XdgToplevel,    // xdg_toplevel protocol object
    pub title: String,
    pub app_id: String,
    pub position: Point,
    pub size: Size,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl WindowManager {
    pub fn new(output_size: Size) -> Self;
    
    /// Handle a new xdg_toplevel being created. Inserts into the slab,
    /// returns (WindowId, initial Configure).
    pub fn map_window(&mut self, toplevel: XdgToplevel, surface: WlSurface) -> (WindowId, Configure);
    
    /// Remove a window from the slab. The WindowId may be reused later.
    pub fn unmap_window(&mut self, id: WindowId) -> Option<WindowState>;
    
    /// Process a window action (from shell or shortcut handler).
    pub fn handle_action(&mut self, action: WindowAction) -> Vec<WindowEvent>;
    
    /// Get all visible windows in z-order (bottom to top) for rendering.
    pub fn visible_windows(&self) -> impl Iterator<Item = &WindowState>;
    
    /// Set focus to a window (called by input handler on click).
    pub fn focus_window(&mut self, id: WindowId) -> Vec<WindowEvent>;
}
```

## Testing Strategy

- Unit tests: window state transitions (map → maximize → restore → close)
- Protocol test: verify configure/ack_configure round-trips are correct
- Integration test: run `weston-terminal`, verify it appears, can be moved/closed
- Manual test: open multiple windows, click to focus, use Mod+Q to close

## Acceptance Criteria

- [ ] `cargo build` succeeds with `smithay-wayland-protocols` dependency
- [ ] Client can create an `xdg_toplevel` and receive initial `configure`
- [ ] Mapped surfaces appear at their configured position on screen
- [ ] Window can be closed via `xdg_toplevel.close` (Mod+Q or client close button)
- [ ] Focus changes raise the window to top of z-order
- [ ] Maximize/fullscreen state transitions work correctly
- [ ] Multiple windows can coexist with correct stacking order
- [ ] `weston-terminal` opens, displays content, and closes cleanly

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| xdg_wm_base protocol is finicky (configure serials) | Follow smithay's reference implementation closely; log every protocol message during development |
| Client doesn't respond to configure | Timeout after 5s, unmap the surface and log a warning |
| Window position off-screen on restore | Clamp position to visible output area on map |
| Resize grab edge cases (client requests resize from wrong edge) | Validate edge value; ignore invalid resize requests with a protocol error |

## Related Specs

- [Subspec 01: Compositor Core](./01-compositor-core.md) — surface registry, event loop
- [Subspec 03: Input Handling](./03-input.md) — focus model, keyboard shortcuts
- [Subspec 05: Desktop Shell](./05-desktop-shell.md) — shell sends WindowActions (close, minimize)
- [iced-rs window module](https://github.com/iced-rs/iced/blob/master/core/src/window.rs) — reference for WindowId/Action/Event model
