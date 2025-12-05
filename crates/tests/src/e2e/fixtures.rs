//! Test Fixtures for E2E Tests
//!
//! Provides reusable test data and helper functions for E2E tests.

use super::client::{format_hex_u64, E2eClient, E2eError};
use std::{process::Command, time::Duration};

/// Test fixtures and helpers for E2E tests
pub struct TestFixtures {
    client: E2eClient,
}

impl TestFixtures {
    /// Create new test fixtures
    #[must_use]
    pub fn new(client: E2eClient) -> Self {
        Self { client }
    }

    /// Get the underlying client
    #[must_use]
    pub fn client(&self) -> &E2eClient {
        &self.client
    }

    /// Wait for the test environment to be ready
    ///
    /// # Errors
    ///
    /// Returns an error if the proxy or Geth nodes do not become reachable within the timeout.
    pub async fn wait_for_environment(&self, timeout: Duration) -> Result<(), E2eError> {
        // Wait for proxy to be healthy
        self.client.wait_for_proxy_healthy(timeout).await?;

        // Optionally wait for at least one Geth node
        self.client
            .wait_for_node(&self.client.config().geth_sealer_url, timeout)
            .await?;

        Ok(())
    }

    /// Get a recent finalized block number (a few blocks behind latest)
    ///
    /// # Errors
    ///
    /// Returns an error if the block number cannot be retrieved.
    pub async fn get_finalized_block(&self) -> Result<u64, E2eError> {
        let latest = self.client.get_block_number().await?;
        // Return a block that's definitely finalized (12 blocks behind)
        Ok(latest.saturating_sub(12))
    }

    /// Get a block hash for a given block number
    ///
    /// # Errors
    ///
    /// Returns an error if the block cannot be retrieved or has no hash field.
    pub async fn get_block_hash(&self, block_number: u64) -> Result<String, E2eError> {
        let block_hex = format_hex_u64(block_number);
        let response = self.client.get_block_by_number(&block_hex, false).await?;

        let block = response.response.result.ok_or_else(|| {
            E2eError::AssertionFailed(format!("No block found for number {block_number}"))
        })?;

        block
            .get("hash")
            .and_then(|h| h.as_str())
            .map(String::from)
            .ok_or_else(|| E2eError::AssertionFailed("Block has no hash".to_string()))
    }

    /// Create a log filter for a block range
    #[must_use]
    pub fn create_log_filter(from_block: u64, to_block: u64) -> serde_json::Value {
        serde_json::json!({
            "fromBlock": format_hex_u64(from_block),
            "toBlock": format_hex_u64(to_block)
        })
    }

    /// Create a log filter with address
    #[must_use]
    pub fn create_log_filter_with_address(
        from_block: u64,
        to_block: u64,
        address: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "fromBlock": format_hex_u64(from_block),
            "toBlock": format_hex_u64(to_block),
            "address": address
        })
    }

    /// Create a log filter with topics
    #[must_use]
    pub fn create_log_filter_with_topics(
        from_block: u64,
        to_block: u64,
        topics: Vec<Option<&str>>,
    ) -> serde_json::Value {
        let topics_json: Vec<serde_json::Value> = topics
            .into_iter()
            .map(|t| t.map_or(serde_json::Value::Null, |s| serde_json::json!(s)))
            .collect();

        serde_json::json!({
            "fromBlock": format_hex_u64(from_block),
            "toBlock": format_hex_u64(to_block),
            "topics": topics_json
        })
    }
}

/// Docker container control for failover tests
pub struct DockerControl;

impl DockerControl {
    /// Stop a Docker container
    ///
    /// # Errors
    ///
    /// Returns an error if the docker command fails or the container cannot be stopped.
    pub fn stop_container(container_name: &str) -> Result<(), E2eError> {
        let output = Command::new("docker")
            .args(["stop", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker stop: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(E2eError::DockerError(format!(
                "Failed to stop container {container_name}: {stderr}"
            )));
        }

        Ok(())
    }

    /// Start a Docker container
    ///
    /// # Errors
    ///
    /// Returns an error if the docker command fails or the container cannot be started.
    pub fn start_container(container_name: &str) -> Result<(), E2eError> {
        let output = Command::new("docker")
            .args(["start", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker start: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(E2eError::DockerError(format!(
                "Failed to start container {container_name}: {stderr}"
            )));
        }

        Ok(())
    }

    /// Restart a Docker container
    ///
    /// # Errors
    ///
    /// Returns an error if the docker command fails or the container cannot be restarted.
    pub fn restart_container(container_name: &str) -> Result<(), E2eError> {
        let output = Command::new("docker")
            .args(["restart", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker restart: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(E2eError::DockerError(format!(
                "Failed to restart container {container_name}: {stderr}"
            )));
        }

        Ok(())
    }

    /// Check if a Docker container is running
    ///
    /// # Errors
    ///
    /// Returns an error if the docker inspect command fails to execute.
    pub fn is_container_running(container_name: &str) -> Result<bool, E2eError> {
        let output = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker inspect: {e}")))?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim() == "true")
    }

    /// Pause a Docker container (simulates network issues)
    ///
    /// # Errors
    ///
    /// Returns an error if the docker command fails or the container cannot be paused.
    pub fn pause_container(container_name: &str) -> Result<(), E2eError> {
        let output = Command::new("docker")
            .args(["pause", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker pause: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(E2eError::DockerError(format!(
                "Failed to pause container {container_name}: {stderr}"
            )));
        }

        Ok(())
    }

    /// Unpause a Docker container
    ///
    /// # Errors
    ///
    /// Returns an error if the docker command fails or the container cannot be unpaused.
    pub fn unpause_container(container_name: &str) -> Result<(), E2eError> {
        let output = Command::new("docker")
            .args(["unpause", container_name])
            .output()
            .map_err(|e| E2eError::DockerError(format!("Failed to run docker unpause: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(E2eError::DockerError(format!(
                "Failed to unpause container {container_name}: {stderr}"
            )));
        }

        Ok(())
    }
}

/// RAII guard for container state - ensures container is restored on drop
pub struct ContainerGuard {
    container_name: String,
    action_on_drop: ContainerAction,
}

#[derive(Clone, Copy)]
pub enum ContainerAction {
    Start,
    Unpause,
    None,
}

impl ContainerGuard {
    /// Create a guard that will start the container on drop
    ///
    /// # Errors
    ///
    /// Returns an error if the container cannot be stopped.
    pub fn stop_with_restore(container_name: &str) -> Result<Self, E2eError> {
        DockerControl::stop_container(container_name)?;
        Ok(Self {
            container_name: container_name.to_string(),
            action_on_drop: ContainerAction::Start,
        })
    }

    /// Create a guard that will unpause the container on drop
    ///
    /// # Errors
    ///
    /// Returns an error if the container cannot be paused.
    pub fn pause_with_restore(container_name: &str) -> Result<Self, E2eError> {
        DockerControl::pause_container(container_name)?;
        Ok(Self {
            container_name: container_name.to_string(),
            action_on_drop: ContainerAction::Unpause,
        })
    }

    /// Disable the restore action (e.g., if you want to manually control)
    pub fn disable_restore(&mut self) {
        self.action_on_drop = ContainerAction::None;
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        match self.action_on_drop {
            ContainerAction::Start => {
                let _ = DockerControl::start_container(&self.container_name);
            }
            ContainerAction::Unpause => {
                let _ = DockerControl::unpause_container(&self.container_name);
            }
            ContainerAction::None => {}
        }
    }
}

/// Assertion helpers for E2E tests
pub mod assertions {
    use super::{Duration, E2eError};

    /// Assert that a duration is within expected bounds
    ///
    /// # Errors
    ///
    /// Returns an error if the actual duration exceeds the expected maximum.
    pub fn assert_duration_within(
        actual: Duration,
        expected_max: Duration,
        context: &str,
    ) -> Result<(), E2eError> {
        if actual > expected_max {
            return Err(E2eError::AssertionFailed(format!(
                "{context}: duration {actual:?} exceeded expected max {expected_max:?}"
            )));
        }
        Ok(())
    }

    /// Assert that cache status is as expected
    ///
    /// # Errors
    ///
    /// Returns an error if the cache status does not match expected or is missing.
    #[allow(dead_code)]
    pub fn assert_cache_status(
        actual: Option<&str>,
        expected: &str,
        context: &str,
    ) -> Result<(), E2eError> {
        match actual {
            Some(status) if status == expected => Ok(()),
            Some(status) => Err(E2eError::AssertionFailed(format!(
                "{context}: expected cache status '{expected}', got '{status}'"
            ))),
            None => Err(E2eError::AssertionFailed(format!(
                "{context}: expected cache status '{expected}', but header was missing"
            ))),
        }
    }

    /// Assert that a value is approximately equal (for timing comparisons)
    ///
    /// # Errors
    ///
    /// Returns an error if the difference between actual and expected exceeds the tolerance.
    #[allow(dead_code)]
    pub fn assert_approximately(
        actual: f64,
        expected: f64,
        tolerance: f64,
        context: &str,
    ) -> Result<(), E2eError> {
        let diff = (actual - expected).abs();
        if diff > tolerance {
            return Err(E2eError::AssertionFailed(format!(
                "{context}: {actual} differs from {expected} by {diff} (tolerance: {tolerance})"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_filter_creation() {
        let filter = TestFixtures::create_log_filter(100, 200);
        assert_eq!(filter["fromBlock"], "0x64");
        assert_eq!(filter["toBlock"], "0xc8");
    }

    #[test]
    fn test_log_filter_with_address() {
        let filter = TestFixtures::create_log_filter_with_address(
            100,
            200,
            "0x1234567890abcdef1234567890abcdef12345678",
        );
        assert_eq!(filter["address"], "0x1234567890abcdef1234567890abcdef12345678");
    }

    #[test]
    fn test_assertions() {
        assert!(assertions::assert_duration_within(
            Duration::from_millis(100),
            Duration::from_millis(200),
            "test"
        )
        .is_ok());

        assert!(assertions::assert_duration_within(
            Duration::from_millis(300),
            Duration::from_millis(200),
            "test"
        )
        .is_err());
    }
}
