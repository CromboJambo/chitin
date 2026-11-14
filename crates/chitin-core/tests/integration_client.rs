//! Integration test: verify the compositor socket accepts connections.

use std::os::unix::net::UnixStream;
use std::time::Duration;

#[test]
fn test_socket_accepts_connection() {
    // Set up the compositor (binds socket, creates display, registers globals).
    let setup = chitin_core::setup().expect("compositor setup failed");
    eprintln!("compositor listening on {}", setup.socket_name);

    // Build the full socket path.
    let uid = unsafe { libc::getuid() };
    let socket_path = format!("/run/user/{}/{}", uid, setup.socket_name);
    eprintln!("connecting to {}", socket_path);

    // Connect as a raw Unix socket client.
    let stream = UnixStream::connect(&socket_path)
        .unwrap_or_else(|e| panic!("failed to connect to {}: {}", socket_path, e));

    // Set a read timeout so we don't block forever.
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();

    // The compositor should send global announcements once the event loop runs.
    // Since we're not running the event loop here, we just verify the TCP-level
    // connection works (the socket is bound and accepting).
    drop(stream);

    eprintln!("integration test passed: socket accepts connections");
}
