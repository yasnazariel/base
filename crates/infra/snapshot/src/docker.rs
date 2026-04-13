//! Docker container lifecycle management via CLI.

use std::path::Path;

use eyre::{bail, Result, WrapErr};
use tokio::process::Command;
use tracing::{debug, info};

/// Docker client for stopping and starting containers.
#[derive(Debug, Clone)]
pub struct DockerClient {
    container_name: String,
    stop_timeout_secs: u64,
}

impl DockerClient {
    /// Creates a new client for the given container.
    pub const fn new(container_name: String, stop_timeout_secs: u64) -> Self {
        Self { container_name, stop_timeout_secs }
    }

    /// Stops the container with a graceful timeout (SIGTERM → SIGKILL).
    pub async fn stop(&self) -> Result<()> {
        info!(container = %self.container_name, timeout = self.stop_timeout_secs, "stopping container");

        let output = Command::new("docker")
            .args(["stop", "-t", &self.stop_timeout_secs.to_string(), &self.container_name])
            .output()
            .await
            .wrap_err("failed to execute docker stop")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("docker stop failed: {stderr}");
        }

        self.wait_for_state("exited").await?;
        info!(container = %self.container_name, "container stopped");
        Ok(())
    }

    /// Starts the container.
    pub async fn start(&self) -> Result<()> {
        info!(container = %self.container_name, "starting container");

        let output = Command::new("docker")
            .args(["start", &self.container_name])
            .output()
            .await
            .wrap_err("failed to execute docker start")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("docker start failed: {stderr}");
        }

        self.wait_for_state("running").await?;
        info!(container = %self.container_name, "container started");
        Ok(())
    }

    /// Returns the current container state (running, exited, etc.).
    pub async fn container_state(&self) -> Result<String> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.State.Status}}",
                &self.container_name,
            ])
            .output()
            .await
            .wrap_err("failed to execute docker inspect")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("docker inspect failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Checks if the Docker socket exists at the given path.
    pub fn verify_socket(socket_path: &Path) -> Result<()> {
        if !socket_path.exists() {
            bail!("docker socket not found at {}", socket_path.display());
        }
        Ok(())
    }

    async fn wait_for_state(&self, expected: &str) -> Result<()> {
        for attempt in 0..30 {
            let state = self.container_state().await?;
            if state == expected {
                return Ok(());
            }
            debug!(
                container = %self.container_name,
                current = %state,
                expected,
                attempt,
                "waiting for container state",
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        bail!(
            "container {} did not reach state '{expected}' within 30s",
            self.container_name
        );
    }
}
