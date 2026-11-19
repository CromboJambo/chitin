//! Chitin container configuration.
//!
//! Defines the configuration for running chitin-compositor inside a Podman
//! container with GPU passthrough, virtual display, and remote access support.

use serde::{Deserialize, Serialize};

/// Configuration for running chitin in a containerized environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChitinContainerConfig {
    /// Container name (unique per instance).
    pub container_name: String,
    /// Base image to use (built from Dockerfile.chitin).
    pub image: String,
    /// GPU devices to pass through for rendering.
    pub gpu_devices: Vec<String>,
    /// Virtual display resolution (used when no physical display is available).
    pub virtual_resolution: Resolution,
    /// Remote desktop configuration.
    pub remote: Option<RemoteConfig>,
    /// Host paths to mount into the container.
    pub mounts: Vec<MountSpec>,
    /// Environment variables to set in the container.
    pub env_vars: Vec<(String, String)>,
    /// Whether to use host networking (required for RustDesk remote access).
    pub host_networking: bool,
    /// Resource limits.
    pub resource_limits: Option<ResourceLimits>,
}

/// Display resolution specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    /// 1080p — default virtual display resolution.
    pub fn fhd() -> Self {
        Self { width: 1920, height: 1080 }
    }

    /// 4K — high-resolution virtual display.
    pub fn uhd() -> Self {
        Self { width: 3840, height: 2160 }
    }
}

impl Default for Resolution {
    fn default() -> Self {
        Self::fhd()
    }
}

/// Remote desktop access configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Remote desktop protocol (currently only RustDesk is planned).
    pub protocol: RemoteProtocol,
    /// Port for remote access (default 21115 for RustDesk).
    pub port: u16,
}

/// Supported remote desktop protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteProtocol {
    /// RustDesk — open-source remote desktop with E2E encryption.
    RustDesk,
    /// NoVNC — browser-based VNC access.
    NoVnc,
}

impl Default for RemoteProtocol {
    fn default() -> Self {
        Self::RustDesk
    }
}

/// Host path mount specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSpec {
    /// Path on the host machine.
    pub host_path: String,
    /// Path inside the container.
    pub container_path: String,
    /// Read-only mount.
    pub read_only: bool,
}

/// Resource limits for the container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// CPU shares (relative weight).
    pub cpu_shares: Option<i32>,
    /// Memory limit in megabytes.
    pub memory_limit_mb: Option<i32>,
}

impl ChitinContainerConfig {
    /// Create a default configuration for running chitin on the local machine.
    pub fn new(container_name: &str) -> Self {
        // Auto-detect available DRM devices.
        let gpu_devices = detect_drm_devices();

        Self {
            container_name: container_name.to_string(),
            image: "chitin-compositor:latest".to_string(),
            gpu_devices,
            virtual_resolution: Resolution::fhd(),
            remote: Some(RemoteConfig {
                protocol: RemoteProtocol::RustDesk,
                port: 21115,
            }),
            mounts: vec![
                // Mount the Wayland runtime directory for socket access.
                MountSpec {
                    host_path: format!("{}/wayland", std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/0".into())),
                    container_path: "/tmp/.X11-unix".to_string(),
                    read_only: false,
                },
            ],
            env_vars: vec![
                ("WAYLAND_DISPLAY".to_string(), "wayland-0".to_string()),
                ("DISPLAY".to_string(), ":0".to_string()),
            ],
            host_networking: true,
            resource_limits: None,
        }
    }

    /// Set the virtual display resolution.
    pub fn with_resolution(mut self, resolution: Resolution) -> Self {
        self.virtual_resolution = resolution;
        self
    }

    /// Enable remote desktop access.
    pub fn with_remote(mut self, config: RemoteConfig) -> Self {
        self.remote = Some(config);
        self
    }

    /// Disable remote desktop access.
    pub fn without_remote(mut self) -> Self {
        self.remote = None;
        self
    }

    /// Add a host path mount.
    pub fn with_mount(mut self, spec: MountSpec) -> Self {
        self.mounts.push(spec);
        self
    }

    /// Set resource limits.
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = Some(limits);
        self
    }

    /// Build a crabjar-podman SandboxConfig from this configuration.
    pub fn to_podman_config(&self) -> crabjar_podman::SandboxConfig {
        let mut config = crabjar_podman::SandboxConfig::new(
            &self.container_name,
            vec![
                "/usr/local/bin/chitin-compositor".to_string(),
                "--virtual-resolution".to_string(),
                format!("{}x{}", self.virtual_resolution.width, self.virtual_resolution.height),
            ],
        );

        // GPU passthrough with privileged mode for DRM/KMS access.
        if !self.gpu_devices.is_empty() {
            config = config.with_gpu_passthrough(self.gpu_devices.clone(), true);
        }

        // Host networking for remote access and Wayland socket sharing.
        if self.host_networking {
            config.network = Some(crabjar_podman::NetworkConfig {
                network_name: "host".to_string(),
                port_mappings: vec![],
            });
        }

        // Mounts.
        for mount in &self.mounts {
            config.mounts.push((mount.host_path.clone(), mount.container_path.clone()));
        }

        // Environment variables.
        for (key, value) in &self.env_vars {
            config.env_vars.push((key.clone(), value.clone()));
        }

        // Resource limits.
        if let Some(limits) = &self.resource_limits {
            config.resource_limits = Some(crabjar_podman::ResourceLimits {
                cpu_shares: limits.cpu_shares,
                memory_limit_mb: limits.memory_limit_mb,
            });
        }

        config
    }
}

/// Detect available DRM devices on the host system.
fn detect_drm_devices() -> Vec<String> {
    let mut devices = Vec::new();
    let dri_dir = std::path::Path::new("/dev/dri");

    if let Ok(entries) = std::fs::read_dir(dri_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Look for card* and renderD* devices.
            if name.starts_with("card") || name.starts_with("renderD") {
                devices.push(format!("/dev/dri/{}", name));
            }
        }
    }

    devices
}
