//! Container runtime for chitin compositor.
//!
//! Manages the lifecycle of chitin containers using crabjar-podman:
//! build, run, stop, status, and logs.

use crate::config::ChitinContainerConfig;
use crabjar_podman::{PodmanError, PodmanExecutor};
use serde_json::Value;
use std::io;
use thiserror::Error;

/// Errors from chitin container operations.
#[derive(Debug, Error)]
pub enum ChitinRuntimeError {
    #[error("podman error: {0}")]
    Podman(#[from] PodmanError),

    #[error("container build failed: {0}")]
    BuildFailed(String),

    #[error("container not found: {0}")]
    ContainerNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// Runtime for managing chitin containers.
pub struct ChitinRuntime {
    executor: PodmanExecutor,
}

impl ChitinRuntime {
    /// Create a new runtime instance.
    pub fn new() -> Result<Self, ChitinRuntimeError> {
        let executor = PodmanExecutor::new()?;
        Ok(Self { executor })
    }

    /// Build the chitin container image from Dockerfile.chitin.
    pub fn build_image(&self, dockerfile_dir: &str) -> Result<(), ChitinRuntimeError> {
        tracing::info!("building chitin container image from {}", dockerfile_dir);

        let output = self.executor.run_raw(&[
            "build".to_string(),
            "-t".to_string(),
            "chitin-compositor:latest".to_string(),
            "-f".to_string(),
            format!("{}/Dockerfile.chitin", dockerfile_dir),
            dockerfile_dir.to_string(),
        ])?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChitinRuntimeError::BuildFailed(format!(
                "build failed:\n{}\n{}",
                stdout, stderr
            )));
        }

        tracing::info!("chitin container image built successfully");
        Ok(())
    }

    /// Start a chitin container with the given configuration.
    pub fn start(&self, config: &ChitinContainerConfig) -> Result<String, ChitinRuntimeError> {
        let podman_config = config.to_podman_config();

        // Remove existing container if it exists (clean restart).
        if self.executor.container_exists(&config.container_name)? {
            tracing::info!("removing existing container {}", config.container_name);
            self.executor.stop_container(&config.container_name).ok();
            self.executor.remove_container(&config.container_name)?;
        }

        // Create and start the container.
        let container_id = self.executor.create_container(&podman_config)?;
        self.executor.start_container(&container_id)?;

        tracing::info!(
            "chitin container started: {} (id: {})",
            config.container_name,
            container_id
        );

        Ok(container_id)
    }

    /// Stop a running chitin container.
    pub fn stop(&self, container_name: &str) -> Result<(), ChitinRuntimeError> {
        if !self.executor.container_exists(container_name)? {
            return Err(ChitinRuntimeError::ContainerNotFound(container_name.to_string()));
        }

        self.executor.stop_container(container_name)?;
        tracing::info!("chitin container stopped: {}", container_name);
        Ok(())
    }

    /// Remove a chitin container (must be stopped first).
    pub fn remove(&self, container_name: &str) -> Result<(), ChitinRuntimeError> {
        if !self.executor.container_exists(container_name)? {
            return Err(ChitinRuntimeError::ContainerNotFound(container_name.to_string()));
        }

        self.executor.remove_container(container_name)?;
        tracing::info!("chitin container removed: {}", container_name);
        Ok(())
    }

    /// Get the status of a chitin container.
    pub fn status(&self, container_name: &str) -> Result<Value, ChitinRuntimeError> {
        if !self.executor.container_exists(container_name)? {
            return Err(ChitinRuntimeError::ContainerNotFound(container_name.to_string()));
        }

        let info = self.executor.inspect_container(container_name)?;
        Ok(info)
    }

    /// Get logs from a running chitin container.
    pub fn logs(&self, container_name: &str, tail_lines: usize) -> Result<String, ChitinRuntimeError> {
        if !self.executor.container_exists(container_name)? {
            return Err(ChitinRuntimeError::ContainerNotFound(container_name.to_string()));
        }

        let output = self.executor.run_raw(&[
            "logs".to_string(),
            "--tail".to_string(),
            tail_lines.to_string(),
            container_name.to_string(),
        ])?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Check if a chitin container is running.
    pub fn is_running(&self, container_name: &str) -> Result<bool, ChitinRuntimeError> {
        if !self.executor.container_exists(container_name)? {
            return Ok(false);
        }

        let info = self.executor.inspect_container(container_name)?;
        let state = info["State"]["Running"].as_bool().unwrap_or(false);
        Ok(state)
    }
}
