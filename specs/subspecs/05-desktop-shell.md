# Subspec 05: Desktop Shell

**Status**: Draft
**Parent**: [specs/SPEC.md](../SPEC.md) §5
**Date**: 2026-11-14

---

## Goal

Provide a minimal desktop shell (`chitin-shell` binary) that runs as a regular Wayland client on top of chitin-core: a bottom panel with taskbar, clock, and application launcher. The shell is NOT part of the compositor — it's just another client.

## Non-Goals (this iteration)

- No system tray / status icons
- No notification center (basic D-Bus notifications only)
- No settings dialog
- No wallpaper management (solid color background from compositor)
- No multi-monitor panel configuration

## Dependencies

- Subspec 01: Compositor core running ✓
- Subspec 02: GPU rendering ✓
- Subspec 04: Window management (taskbar needs window list) — concurrent development
- `smithay-wayland-protocols` for `wlr-layer-shell` bindings
- `zbus` for D-Bus notifications
- **Optional**: `iced_wgpu` for panel UI widgets (decision point below)

## Design Decisions

### Shell as a separate client

**Decision**: `chitin-shell` is a standalone binary, NOT linked into the compositor.

**Rationale**:
- Matches how real desktops work (KDE's plasmashell, GNOME's shell are separate processes)
- If the shell crashes, the compositor keeps running — windows stay visible
- The shell can be replaced with any other layer-shell client (swaybar, polybar, custom)
- Clean separation: compositor handles protocol + rendering; shell handles UI logic

### Panel via wlr-layer-shell

**Decision**: Use `wlr-layer-shell-unstable-v1` for the panel surface.

- Layer: `bottom` (below normal windows) or `top` (above, for overlays)
- Region: `bottom`, exclusive zone = panel height (48px)
- The compositor reserves this space; regular windows are not placed in it

### UI architecture: Elm-inspired (inspired by iced-rs Program trait)

**Decision**: Structure the shell using the Elm Architecture pattern, matching what iced-rs uses for its `Program` trait:

```rust
/// Shell state — everything the UI needs to render.
struct Shell {
    windows: BTreeMap<WindowId, WindowInfo>,  // from compositor via protocol
    active_window: Option<WindowId>,
    launcher_visible: bool,
    clock: DateTime,
}

/// Messages — user interactions or system events.
enum Message {
    WindowMapped(WindowId, String),
    WindowUnmapped(WindowId),
    WindowActivated(WindowId),
    LauncherToggled,
    AppLaunched(String),       // .desktop file path
    ClockTick,                 // fires every second
    KeyPressed(Key),           // Mod+D → toggle launcher
}

/// Update — pure state transition. Returns actions to perform.
fn update(state: &mut Shell, msg: Message) -> Vec<ShellAction> {
    match msg {
        Message::WindowMapped(id, title) => {
            state.windows.insert(id, WindowInfo { id, title });
            vec![]
        }
        Message::LauncherToggled => {
            state.launcher_visible = !state.launcher_visible;
            vec![ShellAction::RefreshPanel]
        }
        Message::AppLaunched(path) => {
            vec![ShellAction::SpawnProcess(path)]
        }
        // ...
    }
}

/// View — build the widget tree from state (declarative).
fn view(state: &Shell) -> PanelWidget {
    panel_column(
        taskbar_row(&state.windows, state.active_window),
        clock_label(&state.clock),
    )
}
```

**Why this pattern**:
- `update()` is pure — trivially testable without a display or GPU
- `view()` is declarative — the UI is always a function of state, no imperative "set label text" calls
- Messages are the only way state changes — easy to trace what caused what
- If we later swap our hand-rolled widgets for iced widgets, this structure maps directly

### Widget implementation: hand-rolled vs iced

**Decision**: Hand-roll minimal widgets for Phase 1. Keep `iced_wgpu` as an option for Phase 2.

**Rationale**:
- Phase 1 needs: labels (text), buttons (clickable areas), a row/column layout, and a text input for the launcher search
- That's ~300 lines of rendering code using our existing wgpu quad pipeline
- Pulling in `iced_wgpu` adds a large dependency tree and we'd need to solve the "render into our own Wayland surface" problem
- Once Phase 1 works, we can evaluate whether iced widgets are worth the complexity for richer UI (scrollable app list, tooltips, etc.)

**Minimal widget set**:
```rust
struct Label { text: String, position: Point, font_size: u16 }
struct Button { label: String, rect: Rectangle, on_click: Message }
struct TextInput { value: String, position: Point, width: u32, placeholder: String }
struct Row { children: Vec<Widget>, spacing: u16 }
struct Column { children: Vec<Widget>, spacing: u16 }
```

Each widget knows how to:
1. Render itself (draw quads + text via wgpu)
2. Hit-test (does this point fall within my bounds?)
3. Handle input (if hit, produce a Message)

### Application launcher

**Decision**: Read `.desktop` files from standard XDG directories, display in a simple overlay.

- Sources: `~/.local/share/applications/`, `/usr/share/applications/`
- Parse with `xdg-desktop-portal` conventions (Name, Exec, Icon, Categories)
- Launcher overlay: full-width panel above the taskbar, scrollable list of apps
- Filter as user types in the search box
- Click → spawn via `std::process::Command` (parse Exec line, handle %u/%f placeholders minimally)

**Phase 1 simplification**: No icons, no categories. Just a text list filtered by name.

### Clock

- Updates every second via a timerfd in the event loop
- Format: `%H:%M` (24-hour), expands to full date on hover (Phase 2)

### Notifications

**Decision**: Implement `org.freedesktop.Notifications` D-Bus interface via `zbus`.

- Shell registers as the notification daemon on the session bus
- Incoming notifications appear as transient toasts in the top-right corner
- Phase 1: text-only, auto-dismiss after 5s, no action buttons
- No persistent history or notification center

## Implementation Plan

### Phase 5a: Panel skeleton (this iteration)

- Create `chitin-shell` binary crate
- Connect to Wayland compositor as a client
- Create a layer-shell surface at the bottom (48px height)
- Render a solid background + "chitin" label on the left + clock on the right
- Event loop: process input, update clock, render

### Phase 5b: Taskbar + launcher (next iteration)

- Subscribe to window events (via a custom protocol extension or by tracking mapped surfaces)
- Render taskbar buttons for each open window
- Click taskbar button → focus that window
- Mod+D toggles the launcher overlay
- Launcher lists apps from .desktop files, click to spawn

### Phase 5c: Notifications + polish (later)

- D-Bus notification daemon
- Toast notifications
- Window close buttons on taskbar (X icon)
- Active window highlight in taskbar

## API Surface (public — for other clients to interact with the shell)

The shell exposes a minimal protocol extension `chitin_shell_v1`:

```
interface chitin_shell_manager;
request get_panel -> panel;
request focus_window(id: window_id);
request close_window(id: window_id);
```

This allows external tools (or agents) to control the shell via standard Wayland protocol — no special backdoors, just another protocol interface.

## Testing Strategy

- Unit tests: `update()` function (pure state transitions)
- Integration test: shell connects to compositor, panel appears at bottom
- Manual test: open terminal from launcher, verify it appears in taskbar, close via Mod+Q

## Acceptance Criteria

- [ ] `chitin-shell` binary builds and runs as a Wayland client
- [ ] Panel appears at the bottom of the screen (48px, layer-shell)
- [ ] Clock displays current time, updates every second
- [ ] Taskbar shows buttons for each open window
- [ ] Clicking a taskbar button focuses that window
- [ ] Mod+D opens/closes the application launcher
- [ ] Launcher lists apps from .desktop files
- [ ] Clicking an app in the launcher spawns it
- [ ] Shell crash does not kill the compositor or existing windows

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| wlr-layer-shell protocol complexity | Use smithay's reference implementation as guide; start with a static panel before adding interactivity |
| Getting window list without a shared API | Define `chitin_shell_v1` extension early (Phase 5a) so the shell can query windows |
| .desktop file parsing edge cases (quoting, env vars in Exec) | Use a minimal parser; fall back to ignoring malformed entries with a warning |
| Shell and compositor version mismatch | Protocol extensions are versioned; shell requests v1, compositor supports v1 — no breaking changes without bumping |

## Related Specs

- [Subspec 01: Compositor Core](./01-compositor-core.md) — Wayland client connection
- [Subspec 04: Window Management](./04-window-management.md) — window list, focus events
- [iced-rs Program trait](https://github.com/iced-rs/iced/blob/master/program/src/lib.rs) — reference for Elm architecture
- [iced-rs multi_window example](https://github.com/iced-rs/iced/blob/master/examples/multi_window/src/main.rs) — reference for window state management
