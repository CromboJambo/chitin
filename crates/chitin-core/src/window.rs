//! Window management: slab-based window state tracking.
//!
//! Implements the window model from subspec 04: each top-level surface gets
//! a `WindowId` (slab index) and associated state. The compositor core tracks
//! surfaces; this module tracks windows (the logical view of mapped surfaces).

use std::collections::VecDeque;

use slab::Slab;
use wayland_server::backend::ObjectId;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::Resource;

/// Opaque window identifier — this IS the slab index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(usize);

impl WindowId {
    /// Get the raw usize value (for debugging/logging).
    #[allow(dead_code)]
    pub fn as_usize(self) -> usize {
        self.0
    }
}

/// Point in output coordinates (top-left origin, y-down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// Actions the shell/WM can perform on a window.
#[allow(dead_code)] // Phase 4b will use these
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
#[allow(dead_code)] // Used in later phases for event-driven updates
pub enum WindowEvent {
    Mapped { id: WindowId, title: String },
    Unmapped { id: WindowId },
    Moved { id: WindowId, position: Point },
    Resized { id: WindowId, size: Size },
    Maximized { id: WindowId, maximized: bool },
    Fullscreen { id: WindowId, fullscreen: bool },
    Closed { id: WindowId },
}

/// State for a single top-level window.
#[derive(Debug)]
pub struct WindowState {
    /// The underlying Wayland surface resource.
    pub surface: WlSurface,
    /// Window title (from xdg_toplevel.set_title).
    pub title: String,
    /// Application ID (from xdg_toplevel.set_app_id, if set).
    pub app_id: String,
    /// Position in output coordinates.
    pub position: Point,
    /// Current size.
    pub size: Size,
    /// Whether the window is maximized.
    pub maximized: bool,
    /// Whether the window is fullscreen.
    pub fullscreen: bool,
    /// Whether this window has focus (Phase 4b).
    #[allow(dead_code)]
    pub focused: bool,
}

/// Window manager state — windows stored in a slab for O(1) lookup by index.
pub struct WindowManager {
    /// Pre-allocated storage for window state.
    windows: Slab<WindowState>,
    /// Z-order: indices into the slab, back = topmost.
    z_order: VecDeque<usize>,
    /// Currently focused window (Phase 4b).
    #[allow(dead_code)]
    focused_window: Option<WindowId>,
    /// Output size for clamping positions.
    output_size: Size,
    /// Offset for cascading new windows.
    cascade_offset: i32,
}

impl WindowManager {
    /// Create a new window manager with the given output size.
    pub fn new(output_size: Size) -> Self {
        Self {
            windows: Slab::with_capacity(16),
            z_order: VecDeque::new(),
            focused_window: None,
            output_size,
            cascade_offset: 0,
        }
    }

    /// Get the number of mapped windows.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Check if there are no mapped windows.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Compute a cascade position for a new window.
    fn next_position(&mut self, size: Size) -> Point {
        let base_x = (self.output_size.width as i32 - size.width as i32) / 2;
        let base_y = (self.output_size.height as i32 - size.height as i32) / 2;
        let offset = self.cascade_offset % 6; // Reset after 6 windows
        self.cascade_offset += 1;
        Point {
            x: base_x + offset * 32,
            y: base_y + offset * 32,
        }
    }

    /// Map a new window. Returns the WindowId and initial position/size to configure.
    pub fn map_window(&mut self, surface: WlSurface, title: String) -> (WindowId, Point, Size) {
        // Default size if client hasn't specified one yet.
        let size = Size { width: 800, height: 600 };
        let position = self.next_position(size);

        let index = self.windows.insert(WindowState {
            surface,
            title: title.clone(),
            app_id: String::new(),
            position,
            size,
            maximized: false,
            fullscreen: false,
            focused: true, // First window gets focus
        });

        self.z_order.push_back(index);
        let id = WindowId(index);
        tracing::info!(
            "window {} mapped: '{}' at ({}, {}) {}x{}",
            index, title, position.x, position.y, size.width, size.height
        );

        (id, position, size)
    }

    /// Unmap a window. Returns the removed state if it existed.
    pub fn unmap_window(&mut self, id: WindowId) -> Option<WindowState> {
        let index = id.0;
        if !self.windows.contains(index) {
            return None;
        }
        let state = self.windows.remove(index);
        self.z_order.retain(|&i| i != index);
        tracing::info!("window {} unmapped: '{}'", index, state.title);
        Some(state)
    }

    /// Get a mutable reference to a window's state.
    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.get_mut(id.0)
    }

    /// Get an immutable reference to a window's state.
    pub fn get(&self, id: WindowId) -> Option<&WindowState> {
        self.windows.get(id.0)
    }

    /// Find the window ID for a given surface resource ObjectId.
    pub fn find_by_surface_id(&self, surface_id: ObjectId) -> Option<WindowId> {
        self.windows.iter().find_map(|(idx, ws)| {
            if ws.surface.id() == surface_id {
                Some(WindowId(idx))
            } else {
                None
            }
        })
    }

    /// Raise a window to the top of the z-order.
    pub fn raise(&mut self, id: WindowId) {
        let index = id.0;
        if !self.z_order.contains(&index) {
            return;
        }
        self.z_order.retain(|&i| i != index);
        self.z_order.push_back(index);
    }

    /// Get all window IDs in z-order (bottom to top).
    pub fn z_ordered_ids(&self) -> Vec<WindowId> {
        self.z_order.iter().copied().map(WindowId).collect()
    }

    /// Set a window's title.
    pub fn set_title(&mut self, id: WindowId, title: String) {
        if let Some(ws) = self.windows.get_mut(id.0) {
            ws.title = title;
        }
    }

    /// Set a window's app_id.
    pub fn set_app_id(&mut self, id: WindowId, app_id: String) {
        if let Some(ws) = self.windows.get_mut(id.0) {
            ws.app_id = app_id;
        }
    }

    /// Set a window's position (clamped to output).
    pub fn set_position(&mut self, id: WindowId, pos: Point) {
        if let Some(ws) = self.windows.get_mut(id.0) {
            ws.position.x = pos.x.max(0).min(self.output_size.width as i32);
            ws.position.y = pos.y.max(0).min(self.output_size.height as i32);
        }
    }

    /// Set a window's size.
    pub fn set_size(&mut self, id: WindowId, size: Size) {
        if let Some(ws) = self.windows.get_mut(id.0) {
            ws.size.width = size.width.min(self.output_size.width);
            ws.size.height = size.height.min(self.output_size.height);
        }
    }

    /// Close a window: send the close event and unmap it.
    /// Returns true if the window existed and was closed.
    pub fn close_window(&mut self, id: WindowId) -> Option<WindowState> {
        let index = id.0;
        if !self.windows.contains(index) {
            return None;
        }
        // Send close event to the client (the xdg_shell module handles this).
        // Here we just unmap; the actual protocol close is sent by the caller.
        self.unmap_window(id)
    }

    /// Get the output size.
    pub fn output_size(&self) -> Size {
        self.output_size
    }
}
