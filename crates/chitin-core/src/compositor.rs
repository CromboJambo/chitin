//! Wayland compositor core: socket binding, client acceptance, protocol dispatch.
//!
//! This module implements the bare-minimum Wayland server for subspec 01:
//! - Binds a Unix socket in XDG_RUNTIME_DIR (via `ListeningSocket`)
//! - Accepts client connections and registers them with the display
//! - Exposes `wl_registry`, `wl_compositor` (v4), and `wl_shm` (v3) globals
//! - Handles surface lifecycle: create, attach buffer, damage, commit, destroy
//! - Runs a poll-based event loop on both listener and display fds

use std::io;
use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::sync::Arc;

use wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use wayland_server::GlobalDispatch;
use wayland_server::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::{self, WlCompositor},
    wl_region::{self, WlRegion},
    wl_output::{self, WlOutput},
    wl_shm::{self, WlShm},
    wl_shm_pool::{self, WlShmPool},
    wl_surface::{self, WlSurface},
};
use wayland_server::{Client, DataInit, Display, DisplayHandle, ListeningSocket, New, Resource, WEnum};

// ── Client data ──────────────────────────────────────────────────────────────

/// Per-client user data (empty for now — subspec 01 doesn't track client state).
#[derive(Debug)]
struct ChitinClientData;

impl ClientData for ChitinClientData {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!("client {:?} initialized", client_id);
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::info!("client {:?} disconnected: {:?}", client_id, reason);
    }
}

// ── Compositor state ─────────────────────────────────────────────────────────

/// Main compositor state, passed to all protocol callbacks.
#[derive(Debug)]
pub struct ChitinState {
    /// Display handle for creating resources and sending events.
    pub dh: DisplayHandle,
}

impl ChitinState {
    pub fn new(dh: DisplayHandle) -> Self {
        Self { dh }
    }
}

// ── Global dispatch implementations ──────────────────────────────────────────

/// Global for `wl_compositor`: clients bind this to create surfaces and regions.
impl GlobalDispatch<WlCompositor, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlCompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the compositor object with unit user data.
        data_init.init(resource, ());
        tracing::debug!("wl_compositor bound");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

/// Global for `wl_shm`: clients bind this to create shared memory pools.
impl GlobalDispatch<WlShm, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlShm>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the shm object.
        let wl_shm = data_init.init(resource, ());

        // Advertise supported pixel formats (argb8888=0, xrgb8888=1).
        wl_shm.send_event(wl_shm::Event::Format { format: WEnum::Unknown(0) }).ok();
        wl_shm.send_event(wl_shm::Event::Format { format: WEnum::Unknown(1) }).ok();
        tracing::debug!("wl_shm bound, advertised ARGB8888 + XRGB8888");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

// ── Request dispatch implementations ─────────────────────────────────────────


/// Global for `wl_output`: clients bind this to query screen geometry.
impl GlobalDispatch<WlOutput, (), ChitinState> for ChitinState {
    fn bind(
        _state: &mut ChitinState,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlOutput>,
        _global_data: &(),
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        // Initialize the output object.
        let wl_output = data_init.init(resource, ());

        // Send geometry event: virtual 1920x1080 output at origin.
        // subpixel=0 (unknown), make="chitin", model="virtual", transform=0 (normal).
        wl_output.send_event(wl_output::Event::Geometry {
            x: 0,
            y: 0,
            physical_width: 527,
            physical_height: 296,
            subpixel: WEnum::Unknown(0),
            make: "chitin".into(),
            model: "virtual".into(),
            transform: WEnum::Unknown(0),
        }).ok();

        // Send mode event: current mode is 1920x1080 @ 60Hz.
        // flags=0x1 (CURRENT).
        wl_output.send_event(wl_output::Event::Mode {
            flags: WEnum::Unknown(0x1),
            width: 1920,
            height: 1080,
            refresh: 60_000, // mHz (millihertz)
        }).ok();

        // Send done event (version 2+).
        wl_output.send_event(wl_output::Event::Done).ok();

        tracing::debug!("wl_output bound, advertised 1920x1080 virtual output");
    }

    fn can_view(_client: Client, _global_data: &()) -> bool {
        true
    }
}

/// Handle `wl_output` requests (only destroy).
impl wayland_server::Dispatch<WlOutput, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlOutput,
        request: wl_output::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_output::Request::Release => {
                tracing::debug!("wl_output release");
            }
            req => tracing::warn!("unhandled wl_output request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlOutput, _data: &()) {
    }
}

/// Handle `wl_compositor` requests: create_surface, create_region.
impl wayland_server::Dispatch<WlCompositor, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                tracing::debug!("create_surface");
                data_init.init(id, ());
            }
            wl_compositor::Request::CreateRegion { id } => {
                tracing::debug!("create_region");
                data_init.init(id, ());
            }
            req => tracing::warn!("unhandled wl_compositor request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlCompositor, _data: &()) {
    }
}

/// Handle `wl_shm` requests: create_pool, release.
impl wayland_server::Dispatch<WlShm, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                tracing::debug!("create_pool fd={:?} size={}", fd, size);
                // Store the fd and size in the pool's user data.
                data_init.init(id, ());
            }
            wl_shm::Request::Release => {
                tracing::debug!("wl_shm release");
            }
            req => tracing::warn!("unhandled wl_shm request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlShm, _data: &()) {
    }
}

/// Handle `wl_shm_pool` requests: create_buffer, destroy.
impl wayland_server::Dispatch<WlShmPool, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlShmPool,
        request: wl_shm_pool::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, format } => {
                tracing::debug!(
                    "create_buffer: {}x{} stride={} offset={} format={:?}",
                    width,
                    height,
                    stride,
                    offset,
                    format
                );
                data_init.init(id, ());
            }
            wl_shm_pool::Request::Destroy => {
                tracing::debug!("destroy pool");
            }
            req => tracing::warn!("unhandled wl_shm_pool request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlShmPool, _data: &()) {
    }
}

/// Handle `wl_buffer` requests: destroy.
impl wayland_server::Dispatch<WlBuffer, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlBuffer,
        request: wl_buffer::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_buffer::Request::Destroy => {
                tracing::debug!("destroy buffer");
            }
            req => tracing::warn!("unhandled wl_buffer request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlBuffer, _data: &()) {
    }
}

/// Handle `wl_surface` requests: attach, damage, frame, commit, destroy.
impl wayland_server::Dispatch<WlSurface, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_surface::Request::Attach { buffer, x, y } => {
                tracing::debug!(
                    "attach buffer={:?} at ({}, {})",
                    buffer.as_ref().map(|b| b.id()),
                    x,
                    y
                );
            }
            wl_surface::Request::Damage { x, y, width, height } => {
                tracing::debug!("damage ({}, {}) {}x{}", x, y, width, height);
            }
            wl_surface::Request::Frame { callback } => {
                let _ = callback;
            }
            wl_surface::Request::Commit => {
                tracing::debug!("commit");
            }
            wl_surface::Request::Destroy => {
                tracing::debug!("destroy surface");
            }
            req => tracing::warn!("unhandled wl_surface request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlSurface, _data: &()) {
    }
}

/// Handle `wl_region` requests: add, subtract, destroy.
impl wayland_server::Dispatch<WlRegion, (), ChitinState> for ChitinState {
    fn request(
        _state: &mut ChitinState,
        _client: &Client,
        _resource: &WlRegion,
        request: wl_region::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, ChitinState>,
    ) {
        match request {
            wl_region::Request::Add { x, y, width, height } => {
                tracing::debug!("region add ({}, {}) {}x{}", x, y, width, height);
            }
            wl_region::Request::Subtract { x, y, width, height } => {
                tracing::debug!("region subtract ({}, {}) {}x{}", x, y, width, height);
            }
            wl_region::Request::Destroy => {
                tracing::debug!("destroy region");
            }
            req => tracing::warn!("unhandled wl_region request: {:?}", req),
        }
    }

    fn destroyed(_state: &mut ChitinState, _client: ClientId, _resource: &WlRegion, _data: &()) {
    }
}

// ── Event loop ───────────────────────────────────────────────────────────────

/// Compositor setup result: display, listener, and state.
pub struct CompositorSetup {
    pub display: Display<ChitinState>,
    pub listener: ListeningSocket,
    pub socket_name: String,
}

/// Set up the compositor: bind socket, create display, register globals.
///
/// This is the non-blocking setup phase. Call [`run_event_loop`] to start processing.
pub fn setup() -> io::Result<CompositorSetup> {
    // Step 1: Bind the listening socket.
    let listener = ListeningSocket::bind_auto("wayland", 0..32)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let socket_name = listener
        .socket_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no socket name"))?
        .to_string_lossy()
        .into_owned();

    tracing::info!("chitin compositor listening on {}", socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    // Step 2: Create the display and register globals.
    let display: Display<ChitinState> = Display::new()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let dh = display.handle();
    dh.create_global::<ChitinState, WlCompositor, ()>(4, ());
    dh.create_global::<ChitinState, WlShm, ()>(3, ());
    dh.create_global::<ChitinState, WlOutput, ()>(4, ());
    tracing::info!("globals registered: wl_compositor(v4), wl_shm(v3), wl_output(v4)");

    Ok(CompositorSetup {
        display,
        listener,
        socket_name,
    })
}

/// Run the Wayland compositor event loop (blocking).
///
/// This is the main entry point for the compositor binary. It:
/// 1. Calls [`setup`] to bind socket and register globals
/// 2. Runs a poll-based event loop on both listener and display fds
pub fn run() -> io::Result<()> {
    let mut setup = setup()?;
    let mut state = ChitinState::new(setup.display.handle());

    tracing::info!("entering event loop");
    run_event_loop(&mut setup.display, &setup.listener, &mut state)
}

/// Run the poll-based event loop on an already-set-up compositor.
fn run_event_loop(
    display: &mut Display<ChitinState>,
    listener: &ListeningSocket,
    state: &mut ChitinState,
) -> io::Result<()> {
    // Get the display handle once — it's valid for the lifetime of the display.
    let mut dh = display.handle();

    loop {
        // Build poll set: [listener_fd, display_fd].
        let listener_borrowed = unsafe { BorrowedFd::borrow_raw(listener.as_raw_fd()) };
        let display_fd = display.backend().poll_fd();

        let mut fds = [
            rustix::event::PollFd::new(&listener_borrowed, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(&display_fd, rustix::event::PollFlags::IN),
        ];

        // Poll with a 100ms timeout (allows periodic housekeeping later).
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 100_000_000 };
        let ready = match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(n) => n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => {
                tracing::error!("poll error: {}", e);
                break;
            }
        };

        if ready == 0 {
            continue; // Timeout — nothing to do.
        }

        // Check listener for new clients.
        if fds[0].revents().contains(rustix::event::PollFlags::IN) {
            match listener.accept() {
                Ok(Some(stream)) => {
                    tracing::info!("new client connected");
                    match dh.insert_client(stream, Arc::new(ChitinClientData)) {
                        Ok(_client) => tracing::debug!("client inserted"),
                        Err(e) => tracing::error!("failed to insert client: {}", e),
                    }
                }
                Ok(None) => {
                    // No connection ready (spurious wakeup or race).
                }
                Err(e) => {
                    tracing::warn!("accept error: {}", e);
                }
            }
        }

        // Check display for pending client requests.
        if fds[1].revents().contains(rustix::event::PollFlags::IN) {
            match display.dispatch_clients(state) {
                Ok(n) => {
                    if n > 0 {
                        tracing::trace!("dispatched {} requests", n);
                    }
                }
                Err(e) => {
                    tracing::error!("dispatch error: {}", e);
                }
            }
            // Flush outgoing events to clients.
            if let Err(e) = display.flush_clients() {
                tracing::warn!("flush error: {}", e);
            }
        }
    }

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the compositor can bind a socket, create a display, and register globals.
    #[test]
    fn test_compositor_setup() {
        // Bind a listening socket.
        let listener = ListeningSocket::bind_auto("chitin-test", 0..32)
            .expect("failed to bind listening socket");

        // Verify we got a socket name.
        let socket_name = listener.socket_name().expect("no socket name");
        assert!(!socket_name.is_empty());

        // Create the display.
        let display: Display<ChitinState> = Display::new().expect("failed to create display");

        // Register globals.
        let dh = display.handle();
        dh.create_global::<ChitinState, WlCompositor, ()>(4, ());
        dh.create_global::<ChitinState, WlShm, ()>(3, ());
    dh.create_global::<ChitinState, WlOutput, ()>(4, ());

        // Verify the listener is still valid (we can check its fd).
        let _fd = listener.as_raw_fd();
        assert!(_fd >= 0);

        // Drop everything to clean up.
        drop(display);
        drop(listener);
    }

    /// Test that the state struct can be created.
    #[test]
    fn test_state_creation() {
        let display: Display<ChitinState> = Display::new().expect("failed to create display");
        let dh = display.handle();
        let state = ChitinState::new(dh);
        // Just verify it exists and has the expected fields.
        let _dh = &state.dh;
    }
}
