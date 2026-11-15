//! Minimal Wayland test client for chitin compositor.
//!
//! Verifies the full xdg_wm_base protocol round-trip:
//! 1. Connect to display
//! 2. Bind wl_compositor, wl_shm, xdg_wm_base via registry
//! 3. Create surface → xdg_surface → xdg_toplevel
//! 4. Allocate SHM buffer with colored pattern
//! 5. Attach + commit on configure
//! 6. Exit after receiving configure (proves compositor processed the map)
//!
//! Run: cargo run --example test-client
//! Requires: chitin-compositor running with WAYLAND_DISPLAY set

use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsFd;

use wayland_client::{
    delegate_noop,
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

struct State {
    running: bool,
    base_surface: Option<wl_surface::WlSurface>,
    buffer: Option<wl_buffer::WlBuffer>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    xdg_surface: Option<(xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel)>,
    configured_count: u32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match &interface[..] {
                "wl_compositor" => {
                    let compositor =
                        registry.bind::<wl_compositor::WlCompositor, _, _>(name, 1, qh, ());
                    let surface = compositor.create_surface(qh, ());
                    state.base_surface = Some(surface);

                    if state.wm_base.is_some() && state.xdg_surface.is_none() {
                        state.init_xdg_surface(qh);
                    }
                }
                "wl_shm" => {
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ());

                    let (init_w, init_h) = (320u32, 200u32);

                    // Create a temp file with a colored pattern.
                    let mut file = tempfile::tempfile().unwrap();
                    draw(&mut file, init_w, init_h);
                    let pool = shm.create_pool(file.as_fd(), (init_w * init_h * 4) as i32, qh, ());
                    let buffer = pool.create_buffer(
                        0,
                        init_w as i32,
                        init_h as i32,
                        (init_w * 4) as i32,
                        wl_shm::Format::Argb8888,
                        qh,
                        (),
                    );
                    state.buffer = Some(buffer.clone());

                    // If we already got configured, attach now.
                    if state.configured_count > 0 {
                        let surface = state.base_surface.as_ref().unwrap();
                        surface.attach(Some(&buffer), 0, 0);
                        surface.commit();
                    }
                }
                "xdg_wm_base" => {
                    let wm_base = registry.bind::<xdg_wm_base::XdgWmBase, _, _>(name, 1, qh, ());
                    state.wm_base = Some(wm_base);

                    if state.base_surface.is_some() && state.xdg_surface.is_none() {
                        state.init_xdg_surface(qh);
                    }
                }
                _ => {}
            }
        }
    }
}

// Ignore events from these object types.
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

fn draw(file: &mut File, width: u32, height: u32) {
    let mut buf = std::io::BufWriter::new(file);
    for _y in 0..height {
        for x in 0..width {
            // Vertical color bands: red | green | blue
            let (r, g, b) = if x < width / 3 {
                (255u8, 0, 0)
            } else if x < 2 * width / 3 {
                (0, 255u8, 0)
            } else {
                (0, 0, 255u8)
            };
            let a = 255u8;
            buf.write_all(&[b, g, r, a]).unwrap(); // BGRA for Argb8888 on little-endian
        }
    }
    buf.flush().unwrap();
}

impl State {
    fn init_xdg_surface(&mut self, qh: &QueueHandle<State>) {
        let wm_base = self.wm_base.as_ref().unwrap();
        let base_surface = self.base_surface.as_ref().unwrap();

        let xdg_surface = wm_base.get_xdg_surface(base_surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_title("chitin test client".into());
        toplevel.set_app_id("chitin-test-client".into());

        base_surface.commit();

        self.xdg_surface = Some((xdg_surface, toplevel));
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.configured_count += 1;
            println!(
                "📋 Received xdg_surface.configure (serial={}, #{})",
                serial, state.configured_count
            );
            xdg_surface.ack_configure(serial);

            // Attach buffer if we have one.
            let surface = state.base_surface.as_ref().unwrap();
            if let Some(ref buffer) = state.buffer {
                surface.attach(Some(buffer), 0, 0);
                surface.commit();
                println!("   Attached buffer and committed");
            }

            // After the first configure, we're done — protocol round-trip verified.
            if state.configured_count >= 1 {
                println!("\n✅ SUCCESS: Received {} configure event(s) from compositor", state.configured_count);
                println!("   The xdg_wm_base protocol round-trip is working!");
                state.running = false;
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => {
                println!("🔒 Compositor requested close");
                state.running = false;
            }
            xdg_toplevel::Event::Configure { .. } => {
                println!("📋 Received xdg_toplevel.configure");
            }
            _ => {}
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init()
        .ok();

    println!("Connecting to Wayland display...");
    let conn = Connection::connect_to_env().expect("Failed to connect to Wayland display");
    println!("Connected!");

    let mut event_queue = conn.new_event_queue();
    let qhandle = event_queue.handle();

    let display = conn.display();
    display.get_registry(&qhandle, ());

    let mut state = State {
        running: true,
        base_surface: None,
        buffer: None,
        wm_base: None,
        xdg_surface: None,
        configured_count: 0,
    };

    println!("Starting test client — waiting for configure events...");

    // Run the event loop with a timeout.
    let start = std::time::Instant::now();
    while state.running {
        if start.elapsed() > std::time::Duration::from_secs(10) {
            println!("\n⚠️  TIMEOUT: Test did not complete within 10s");
            break;
        }

        match event_queue.blocking_dispatch(&mut state) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Event dispatch error: {:?}", e);
                break;
            }
        }
    }

    // One final dispatch to flush any pending requests (like the buffer commit)
    // before we exit. This ensures the compositor receives all our messages.
    match event_queue.flush() {
        Ok(_) => println!("   Flushed pending requests to compositor"),
        Err(e) => eprintln!("   Flush warning: {:?}", e),
    }
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("\nTest client exiting.");
}
