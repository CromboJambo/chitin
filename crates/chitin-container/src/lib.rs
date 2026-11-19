//! Containerized deployment for chitin compositor (spec 06).
//!
//! Wraps crabjar-podman with chitin-specific configuration for running
//! the compositor inside a rootless Podman container with GPU passthrough.

pub mod config;
pub mod runtime;

pub use config::ChitinContainerConfig;
pub use runtime::ChitinRuntime;
