//! chitin-core: Minimal Wayland compositor core.
//!
//! Protocol server, surface management, event loop.
//! Implementation follows specs/subspecs/01-compositor-core.md.

mod compositor;
mod gpu;

pub use compositor::{ChitinState, CompositorSetup, run, setup};
pub use gpu::{GpuContext, Renderer, SurfaceFrame, GpuError};

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
