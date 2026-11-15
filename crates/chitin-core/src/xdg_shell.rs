//! XDG shell protocol implementation: xdg_wm_base, xdg_surface, xdg_toplevel.
//!
//! Implements the server side of the XDG shell stable protocol (v6), which is the
//! standard way for Wayland clients to create top-level windows. This module provides
//! the `GlobalDispatch` and `Dispatch` implementations needed to register `xdg_wm_base`
//! as a global and handle all related requests.
//!
//! Design notes:
//! - Per-resource data is minimal (just enough to identify what the resource represents).
//! - All mutable window/surface state lives in `ChitinState` (the compositor's central state),
//!   which is passed as `&mut State` to every dispatch handler.
//! - Window tracking uses the slab-based `WindowManager` from `window.rs` per subspec 04.

use wayland_server::backend::{ClientId, ObjectId};
use wayland_server::{Client, DataInit, DisplayHandle, GlobalDispatch, New, Resource};
use wayland_protocols::xdg::shell::server::{
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::compositor::ChitinState;
use crate::window::{Size, WindowId};

// ── Per-resource user data (minimal, immutable after creation) ───────────────

/// User data for an `xdg_wm_base` resource. Stores the client ID for cleanup.
#[derive(Debug)]
pub struct XdgWmBaseData {
    pub client_id: ClientId,
}

/// User data for an `xdg_surface` resource. Identifies which wl_surface it wraps.
#[derive(Debug)]
pub struct XdgSurfaceData {
    /// The ObjectId of the underlying wl_surface.
    pub surface_id: ObjectId,
}

/// User data for an `xdg_toplevel` resource. Links to the window in WindowManager.
#[derive(Debug)]
pub struct XdgToplevelData {
    /// The window ID assigned by the WindowManager.
    pub window_id: WindowId,
}

// ── Global dispatch: xdg_wm_base bind ────────────────────────────────────────

impl GlobalDispatch<XdgWmBase, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        client: &Client,
        resource: New<XdgWmBase>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        tracing::info!("client bound to xdg_wm_base");

        let client_id = client.id();
        data_init.init(resource, XdgWmBaseData { client_id });

        // Send the initial ping so the client knows we're alive.
        // (Real implementations would track pings for liveness detection.)
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

// ── Dispatch: xdg_wm_base requests ───────────────────────────────────────────

impl wayland_server::Dispatch<XdgWmBase, XdgWmBaseData, ChitinState> for ChitinState {
    fn request(
        state: &mut ChitinState,
        client: &Client,
        resource: &XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &XdgWmBaseData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                tracing::debug!("create_positioner");
                // Positioners are lightweight; store minimal data.
                data_init.init(id, ());
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let surface_id = surface.id();
                tracing::info!(
                    "get_xdg_surface: wl_surface {:?} → new xdg_surface",
                    surface_id
                );

                // Check if this surface already has an xdg role.
                if let Some(existing) = state.xdg_surfaces.get(&surface_id) {
                    if existing.window_id.is_some() {
                        kill_client_protocol_error(
                            client, dhandle, resource,
                            "surface already has an xdg role",
                        );
                        return;
                    }
                }

                // Create the xdg_surface resource with minimal per-resource data.
                data_init.init(id, XdgSurfaceData { surface_id: surface_id.clone() });

                // Track it in central state (no window role yet).
                state.xdg_surfaces.insert(
                    surface_id,
                    crate::compositor::XdgSurfaceInfo {
                        surface: surface.clone(),
                        configured_size: Size::default(),
                        configure_serial: 0,
                        pending_configure: false,
                        window_id: None,
                    },
                );
            }
            xdg_wm_base::Request::Destroy => {
                tracing::debug!("xdg_wm_base destroy");
                // Nothing to clean up — the framework handles resource destruction.
            }
            req => tracing::warn!("unhandled xdg_wm_base request: {:?}", req),
        }
    }

    fn destroyed(
        _state: &mut ChitinState,
        _client: ClientId,
        _resource: &XdgWmBase,
        data: &XdgWmBaseData,
    ) {
        tracing::debug!("xdg_wm_base destroyed for client {:?}", data.client_id);
        // Clean up any xdg surfaces owned by this client.
        // (In a full implementation we'd track which surfaces belong to which client.)
    }
}

// ── Dispatch: xdg_surface requests ───────────────────────────────────────────

impl wayland_server::Dispatch<XdgSurface, XdgSurfaceData, ChitinState> for ChitinState {
    fn request(
        state: &mut ChitinState,
        client: &Client,
        resource: &XdgSurface,
        request: xdg_surface::Request,
        data: &XdgSurfaceData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        let surface_id = data.surface_id.clone();

        match request {
            xdg_surface::Request::Destroy => {
                tracing::debug!("xdg_surface destroy for {:?}", surface_id);
                // Remove from tracking. The framework will drop the resource.
                state.xdg_surfaces.remove(&surface_id);
            }
            xdg_surface::Request::GetToplevel { id } => {
                handle_get_toplevel(
                    state, client, resource, dhandle, data_init, surface_id, id,
                );
            }
            xdg_surface::Request::GetPopup { .. } => {
                // Popups are not implemented yet (subspec 04 covers only toplevels for now).
                kill_client_protocol_error(
                    client, dhandle, resource,
                    "popups not yet implemented",
                );
            }
            xdg_surface::Request::SetWindowGeometry { x, y, width, height } => {
                tracing::debug!(
                    "set_window_geometry: ({}, {}) {}x{}",
                    x, y, width, height
                );
                // Store geometry for later use by the window manager.
                if let Some(info) = state.xdg_surfaces.get_mut(&surface_id) {
                    info.configured_size = Size {
                        width: width as u32,
                        height: height as u32,
                    };
                }
            }
            xdg_surface::Request::AckConfigure { serial } => {
                tracing::debug!("ack_configure serial={}", serial);
                if let Some(info) = state.xdg_surfaces.get_mut(&surface_id) {
                    if info.configure_serial == serial as u32 {
                        info.pending_configure = false;
                        // The surface is now ready to be mapped once a buffer is committed.
                        tracing::info!(
                            "window {} configure acknowledged, ready to map",
                            info.window_id.map(|w| w.as_usize()).unwrap_or(0)
                        );
                    }
                }
            }
            req => tracing::warn!("unhandled xdg_surface request: {:?}", req),
        }
    }

    fn destroyed(
        state: &mut ChitinState,
        _client: ClientId,
        _resource: &XdgSurface,
        data: &XdgSurfaceData,
    ) {
        tracing::debug!("xdg_surface destroyed for {:?}", data.surface_id);
        let surface_id = data.surface_id.clone();

        // If this surface had a toplevel role, unmap the window.
        if let Some(info) = state.xdg_surfaces.get(&surface_id) {
            if let Some(window_id) = info.window_id {
                tracing::info!("unmapping window {} due to xdg_surface destruction", window_id.as_usize());
                state.windows.unmap_window(window_id);

                // Remove the toplevel → window mapping.
                state.toplevel_window_map.retain(|_, w| *w != window_id);
            }
        }

        state.xdg_surfaces.remove(&surface_id);
    }
}

// ── Dispatch: xdg_toplevel requests ──────────────────────────────────────────

impl wayland_server::Dispatch<XdgToplevel, XdgToplevelData, ChitinState> for ChitinState {
    fn request(
        state: &mut ChitinState,
        client: &Client,
        resource: &XdgToplevel,
        request: xdg_toplevel::Request,
        data: &XdgToplevelData,
        dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        let window_id = data.window_id;

        match request {
            xdg_toplevel::Request::Destroy => {
                tracing::debug!("xdg_toplevel destroy for window {}", window_id.as_usize());
                // Unmap the window.
                state.windows.unmap_window(window_id);
                state.toplevel_window_map.remove(&resource.id());

                // Mark the surface as no longer having a role.
                if let Some(surface_id) = get_surface_for_toplevel(state, resource) {
                    if let Some(info) = state.xdg_surfaces.get_mut(&surface_id) {
                        info.window_id = None;
                    }
                }
            }
            xdg_toplevel::Request::SetParent { parent: _ } => {
                // Parenting not implemented yet.
                tracing::debug!("set_parent (ignored — parenting not implemented)");
            }
            xdg_toplevel::Request::SetTitle { title } => {
                tracing::debug!("set_title: {}", title);
                state.windows.set_title(window_id, title);
            }
            xdg_toplevel::Request::SetAppId { app_id } => {
                tracing::debug!("set_app_id: {}", app_id);
                // Store app_id if WindowManager supports it.
            }
            xdg_toplevel::Request::ShowWindowMenu { .. } => {
                // Context menu not implemented yet.
                tracing::debug!("show_window_menu (ignored — not implemented)");
            }
            xdg_toplevel::Request::Move { .. } => {
                // Interactive move requires input support (subspec 03).
                kill_client_protocol_error(
                    client, dhandle, resource,
                    "interactive move not yet supported (no input)",
                );
            }
            xdg_toplevel::Request::Resize { .. } => {
                // Interactive resize requires input support (subspec 03).
                kill_client_protocol_error(
                    client, dhandle, resource,
                    "interactive resize not yet supported (no input)",
                );
            }
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                tracing::debug!("set_max_size: {}x{}", width, height);
                // Max size constraints not enforced yet (Phase 4b).
            }
            xdg_toplevel::Request::SetMinSize { width, height } => {
                tracing::debug!("set_min_size: {}x{}", width, height);
                // Min size constraints not enforced yet (Phase 4b).
            }
            xdg_toplevel::Request::SetMaximized => {
                tracing::debug!("set_maximized for window {}", window_id.as_usize());
                // Maximize to output size (or configured workspace area).
                let max_size = state.windows.output_size();
                state.windows.set_size(window_id, max_size);
            }
            xdg_toplevel::Request::UnsetMaximized => {
                tracing::debug!("unset_maximized for window {}", window_id.as_usize());
                // Restore to last known size (simplified: just clear maximized state).
            }
            xdg_toplevel::Request::SetFullscreen { output: _ } => {
                // Fullscreen requires output management.
                kill_client_protocol_error(
                    client, dhandle, resource,
                    "fullscreen not yet supported",
                );
            }
            xdg_toplevel::Request::UnsetFullscreen => {
                tracing::debug!("unset_fullscreen (no-op — fullscreen not implemented)");
            }
            xdg_toplevel::Request::SetMinimized => {
                tracing::debug!("set_minimized for window {}", window_id.as_usize());
                // Minimize: hide the surface from compositing (Phase 4b).
            }
            req => tracing::warn!("unhandled xdg_toplevel request: {:?}", req),
        }
    }

    fn destroyed(
        state: &mut ChitinState,
        _client: ClientId,
        resource: &XdgToplevel,
        data: &XdgToplevelData,
    ) {
        tracing::debug!("xdg_toplevel destroyed for window {}", data.window_id.as_usize());
        // Unmap the window if it's still mapped.
        state.windows.unmap_window(data.window_id);
        state.toplevel_window_map.remove(&resource.id());
    }
}

// ── Dispatch: xdg_positioner (minimal — just needs to exist) ─────────────────

impl wayland_server::Dispatch<XdgPositioner, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &XdgPositioner,
        request: xdg_positioner::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Positioner requests are just state setters; they don't need to do anything
        // until we implement popups. For now, silently accept them.
        match request {
            xdg_positioner::Request::Destroy => {
                tracing::debug!("xdg_positioner destroy");
            }
            _ => {
                // Size, anchor rect, anchor, gravity, constraints, offset — all no-ops for now.
            }
        }
    }

    fn destroyed(
        _state: &mut ChitinState,
        _client: ClientId,
        _resource: &XdgPositioner,
        _data: &(),
    ) {
    }
}

// ── Helper functions ─────────────────────────────────────────────────────────

/// Handle the `xdg_surface.get_toplevel` request.
fn handle_get_toplevel(
    state: &mut ChitinState,
    client: &Client,
    xdg_surface_resource: &XdgSurface,
    dhandle: &DisplayHandle,
    data_init: &mut DataInit<'_, ChitinState>,
    surface_id: ObjectId,
    id: New<XdgToplevel>,
) {
    tracing::info!(
        "get_toplevel for surface {:?} → new window",
        surface_id
    );

    // Check if this surface already has a role.
    if let Some(info) = state.xdg_surfaces.get(&surface_id) {
        if info.window_id.is_some() {
            kill_client_protocol_error(
                client, dhandle, xdg_surface_resource,
                "surface already has a toplevel role",
            );
            return;
        }
    }

    // Get the wl_surface resource from our tracking (stored in get_xdg_surface).
    let wl_surface = match state.xdg_surfaces.get(&surface_id) {
        Some(info) => info.surface.clone(),
        None => {
            kill_client_protocol_error(
                client, dhandle, xdg_surface_resource,
                "internal error: xdg_surface not found in tracking",
            );
            return;
        }
    };

    // Create the window in the WindowManager.
    let (window_id, _position, size) = state.windows.map_window(
        wl_surface.clone(),
        "untitled".to_string(),
    );

    // Create the xdg_toplevel resource.
    let toplevel = data_init.init(id, XdgToplevelData { window_id });

    // Update central state.
    if let Some(info) = state.xdg_surfaces.get_mut(&surface_id) {
        info.window_id = Some(window_id);
        info.configured_size = size;
        info.configure_serial += 1;
        info.pending_configure = true;
    }

    // Track the toplevel → window mapping.
    state.toplevel_window_map.insert(toplevel.id(), window_id);

    // Send the initial configure event.
    // The client MUST acknowledge this before committing its first buffer.
    let serial = if let Some(info) = state.xdg_surfaces.get(&surface_id) {
        info.configure_serial
    } else {
        0
    };

    toplevel.send_event(xdg_toplevel::Event::Configure {
        width: size.width as i32,
        height: size.height as i32,
        states: vec![],
    })
    .ok();

    tracing::info!(
        "window {} created: initial configure {}x{} serial={}",
        window_id.as_usize(),
        size.width,
        size.height,
        serial
    );
}

/// Find the surface ObjectId associated with a toplevel resource.
fn get_surface_for_toplevel(state: &ChitinState, toplevel: &XdgToplevel) -> Option<ObjectId> {
    let window_id = state.toplevel_window_map.get(&toplevel.id())?;
    // Find the xdg_surface that has this window_id.
    for (surface_id, info) in &state.xdg_surfaces {
        if info.window_id == Some(*window_id) {
            return Some(surface_id.clone());
        }
    }
    None
}

/// Kill a client with a protocol error.
fn kill_client_protocol_error<R: Resource>(
    client: &Client,
    dhandle: &DisplayHandle,
    resource: &R,
    message: &str,
) {
    use wayland_server::backend::protocol::ProtocolError;

    tracing::error!("protocol error on {:?}: {}", R::interface().name, message);
    client.kill(
        dhandle,
        ProtocolError {
            code: 0, // Generic protocol error
            object_id: resource.id().protocol_id(),
            object_interface: R::interface().name.to_string(),
            message: message.to_string(),
        },
    );
}
