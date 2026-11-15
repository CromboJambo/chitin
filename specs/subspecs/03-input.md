# Subspec 03: Input Handling

**Status**: Draft
**Parent**: [specs/SPEC.md](../SPEC.md) §3
**Date**: 2026-11-14

---

## Goal

Route input device events (keyboard, pointer, touch) from the kernel to the correct Wayland surface via `wl_seat`, maintaining consistent focus state.

## Non-Goals (this iteration)

- No multi-seat support
- No input method framework (IME) — raw keycodes only
- No tablet/stylus handling
- No input shaping or remapping (libinput handles that)

## Dependencies

- Subspec 01: Compositor core, `wl_seat` global registration ✓
- Subspec 02: GPU rendering (for cursor surface compositing) ✓
- `evdev` crate (raw event device access) — preferred over `libinput-rs` for minimalism

## Design Decisions

### Raw evdev, not libinput

**Decision**: Use the `evdev` crate directly; do NOT use `libinput-rs`.

**Rationale**:
- We need keyboard + pointer routing, not gesture recognition or device tuning
- `evdev` gives us raw events from `/dev/input/event*` with zero abstraction overhead
- libinput adds value for touchpads (scroll acceleration, gestures) — we don't need that in Phase 1
- Escape hatch: if we need better pointer behavior later, wrap evdev events through a libinput callout

### Event model (inspired by iced-rs)

**Decision**: Define our event types mirroring the structure used by `iced_core::event`:

```rust
pub enum InputEvent {
    Keyboard(KeyboardEvent),
    Pointer(PointerEvent),
    Touch(TouchEvent),
}

#[derive(Debug, Clone, Copy)]
pub struct KeyboardEvent {
    pub key_code: u32,      // Linux input event code (KEY_A, KEY_1, etc.)
    pub state: KeyState,    // Pressed | Released
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub position: Point,    // absolute in output coordinates
    pub event_type: PointerEventType,
}

pub enum PointerEventType {
    Motion { delta: (f64, f64) },
    Button { button: u32, state: ButtonState },
    Scroll { delta_x: f64, delta_y: f64 },
}

#[derive(Debug, Clone, Copy)]
pub struct TouchEvent {
    pub finger_id: u32,
    pub position: Point,
    pub state: TouchState,  // Down | Motion | Up
}
```

This structure is minimal, complete for Phase 1, and maps cleanly to Wayland's `wl_keyboard`/`wl_pointer`/`wl_touch` event formats.

### Focus model

**Decision**: Click-to-focus with focus-follows-pointer as a config option.

- Pointer button press on a surface → that surface gains keyboard focus
- Focus never leaves the top-level window (only moves between surfaces within it)
- `wl_keyboard.enter` / `wl_keyboard.leave` events sent to clients accordingly
- Modifiers tracked globally, reported with every key event

### Keyboard shortcuts

Handled at the compositor level (before routing to client):

| Shortcut | Action |
|----------|--------|
| Mod+Q | Close focused window (`xdg_toplevel.close`) |
| Mod+D | Show/hide launcher (subspec 05) |
| Mod+←/→/↑/↓ | Tile focused window (subspec 04) |
| Mod+C | Copy focused window position to clipboard (debug) |

Mod = Super/LWin key. Shortcuts are checked in a global hook; if matched, the event is consumed and NOT forwarded to the client.

### Event loop integration

Input devices are watched via `mio` alongside the Wayland socket:

```rust
loop {
    poll(&mut [wayland_fd, input_fds...], timeout);
    
    // Process Wayland protocol messages
    dispatch_wayland_requests();
    
    // Process input events
    for event in drain_input_events() {
        match event {
            InputEvent::Keyboard(ke) => handle_keyboard(ke),
            InputEvent::Pointer(pe) => handle_pointer(pe),
            InputEvent::Touch(te) => handle_touch(te),
        }
    }
    
    // Render if needed
    if state.needs_redraw() { render_frame(); }
}
```

## Protocol Objects We Implement

| Object | Interface | Purpose |
|--------|-----------|---------|
| Seat | `wl_seat` (v8+) | Client requests keyboard/pointer/touch capabilities |
| Keyboard | `wl_keyboard` | Key events, modifiers, focus enter/leave |
| Pointer | `wl_pointer` | Motion, button, scroll events; cursor shape feedback |
| Touch | `wl_touch` | Multi-touch events (basic) |

**Cursor rendering**: The compositor draws a default cursor arrow at the pointer position. Clients can override by setting a custom cursor surface via `wl_data_device`'s `set_cursor_image`. For Phase 1: static arrow, no animation.

## API Surface (public)

```rust
pub struct InputManager {
    devices: Vec<InputDevice>,
    focused_surface: Option<WlSurface>,
    pointer_position: Point,
    modifiers: Modifiers,
}

impl InputManager {
    pub fn new(device_paths: &[PathBuf]) -> io::Result<Self>;
    
    /// Poll for pending events (non-blocking). Returns None if no events.
    pub fn poll_events(&mut self) -> Vec<InputEvent>;
    
    /// Route an event to the focused surface via wl_seat resources.
    pub fn dispatch(&mut self, event: InputEvent);
    
    /// Set the focused surface (called by window manager on focus change).
    pub fn set_focus(&mut self, surface: Option<WlSurface>);
}
```

## Testing Strategy

- Unit tests: event parsing from raw evdev frames
- Integration test: simulate keyboard/pointer events, verify correct `wl_keyboard`/`wl_pointer` events sent to client
- Manual test: run `weston-terminal`, type text, move mouse, click buttons

## Acceptance Criteria

- [ ] `cargo build` succeeds with `evdev` dependency
- [ ] InputManager opens and reads from `/dev/input/event*` devices
- [ ] Keyboard events reach the focused surface as `wl_keyboard.key` events
- [ ] Pointer motion updates cursor position and sends `wl_pointer.motion`
- [ ] Click-to-focus works: clicking a surface gives it keyboard focus
- [ ] Mod+Q closes the focused window
- [ ] `weston-terminal` is fully interactive (type, scroll, click)

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| No input devices in container | Accept `--input-devices` flag; run headless with synthetic events for testing |
| Evdev requires root or input group | Container runs as user with `/dev/input` mounted + appropriate cgroup permissions |
| Key repeat handling | Compositor sends key events on each press (no auto-repeat); clients handle their own repeat if needed |
| Pointer position outside any surface | Cursor still moves; no `wl_pointer.enter` sent until over a surface |

## Related Specs

- [Subspec 01: Compositor Core](./01-compositor-core.md) — event loop, wl_seat global
- [Subspec 04: Window Management](./04-window-management.md) — focus model, keyboard shortcuts
- [iced-rs event model](https://github.com/iced-rs/iced/blob/master/core/src/event.rs) — reference for event structure
