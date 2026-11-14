//! chitin-core: Minimal Wayland compositor core.
//!
//! Protocol server, surface management, event loop.
//! Implementation follows specs/subspecs/01-compositor-core.md (to be created).

// Module structure will emerge as subspec 01 is folded down:
// - protocol/   — Wayland protocol objects and handlers
// - surface/    — Surface state machine (create → map → commit → unmap)
// - seat/       — Input device management and focus
// - output/     — Display/output management
// - event_loop/ — Main loop: socket readiness, timerfd, frame scheduling

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        let v = version();
        assert!(v.contains('.'), "version should be semver: {v}");
    }
}
