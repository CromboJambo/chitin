//! Chitin compositor binary.
//!
//! Minimal Wayland compositor for subspec 01: binds a socket, exposes
//! wl_compositor and wl_shm globals, accepts clients, and runs an event loop.

use std::process;

fn main() {
    // Initialize tracing subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chitin_core=debug".into()),
        )
        .try_init();

    tracing::info!("chitin compositor starting (subspec 01)");

    if let Err(e) = chitin_core::run() {
        tracing::error!("compositor exited with error: {}", e);
        process::exit(1);
    }
}
