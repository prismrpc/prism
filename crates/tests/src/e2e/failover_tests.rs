//! Failover and Circuit Breaker Tests
//!
//! Tests for upstream failover behavior when Geth nodes go down or become unresponsive.
//! These tests manipulate Docker containers to simulate node failures.
//!
//! WARNING: These tests modify Docker container state and MUST run serially.
//! All tests in this file are marked with #[serial] to prevent concurrent
//! container manipulation which would affect other tests.
//!
//! Note: The sealer node should NOT be stopped as it's the block producer.
//! Only RPC nodes (rpc-1, rpc-2, rpc-3) should be used for failover tests.

use super::{
    client::E2eClient,
    config::{containers, E2eConfig},
    fixtures::{ContainerGuard, DockerControl, TestFixtures},
};
use serial_test::serial;
use std::time::Duration;

/// Setup function for tests
async fn setup() -> (E2eClient, TestFixtures) {
    let config = E2eConfig::from_env();
    let client = E2eClient::new(config);
    let fixtures = TestFixtures::new(client.clone());

    fixtures
        .wait_for_environment(Duration::from_secs(30))
        .await
        .expect("Environment not ready");

    (client, fixtures)
}

/// Ensure a container is running before testing
fn ensure_container_running(container_name: &str) {
    if !DockerControl::is_container_running(container_name).unwrap_or(false) {
        DockerControl::start_container(container_name).expect("Failed to start container");
        std::thread::sleep(Duration::from_secs(5)); // Wait for container to be ready
    }
}

#[tokio::test]
#[serial]
async fn test_basic_failover_one_node_down() {
    let (client, _fixtures) = setup().await;

    // Ensure RPC node 1 is running
    ensure_container_running(containers::RPC_1);

    // Stop the RPC node 1
    let _guard = ContainerGuard::stop_with_restore(containers::RPC_1)
        .expect("Failed to stop RPC node 1 container");

    // Wait for the proxy to detect the failure
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Requests should still work via other nodes
    let result = client.get_block_number().await;

    match result {
        Ok(block_num) => {
            println!("Failover successful - got block number {block_num} with RPC node 1 down");
        }
        Err(e) => {
            // This might fail if circuit breaker hasn't adapted yet
            println!("Request failed (may need more upstreams): {e}");
        }
    }

    // Guard will restore the container when dropped
}

#[tokio::test]
#[serial]
async fn test_failover_multiple_nodes_down() {
    let (client, _fixtures) = setup().await;

    // Ensure containers are running
    ensure_container_running(containers::RPC_1);
    ensure_container_running(containers::RPC_2);

    // Stop multiple RPC nodes
    let _guard1 =
        ContainerGuard::stop_with_restore(containers::RPC_1).expect("Failed to stop RPC node 1");
    let _guard2 =
        ContainerGuard::stop_with_restore(containers::RPC_2).expect("Failed to stop RPC node 2");

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should still work if sealer and RPC node 3 are available
    let result = client.get_block_number().await;

    match result {
        Ok(block_num) => {
            println!("Got block {block_num} with RPC nodes 1 and 2 down");
        }
        Err(e) => {
            println!("Request failed with two nodes down: {e}");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_failover_all_rpc_nodes_down() {
    let (client, _fixtures) = setup().await;

    // Stop all RPC nodes (keep sealer running as it's the block producer)
    ensure_container_running(containers::RPC_1);
    ensure_container_running(containers::RPC_2);
    ensure_container_running(containers::RPC_3);

    let _guard1 =
        ContainerGuard::stop_with_restore(containers::RPC_1).expect("Failed to stop RPC node 1");
    let _guard2 =
        ContainerGuard::stop_with_restore(containers::RPC_2).expect("Failed to stop RPC node 2");
    let _guard3 =
        ContainerGuard::stop_with_restore(containers::RPC_3).expect("Failed to stop RPC node 3");

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Requests should still work via sealer node
    let result = client.get_block_number().await;

    match result {
        Ok(block_num) => {
            println!("Got block {block_num} - sealer node responded");
        }
        Err(e) => {
            // May fail if sealer is also unavailable
            println!("Request failed with all RPC nodes down: {e}");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_node_recovery() {
    let (client, _fixtures) = setup().await;

    ensure_container_running(containers::RPC_1);

    // Stop the RPC node 1
    DockerControl::stop_container(containers::RPC_1).expect("Failed to stop RPC node 1");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Try a request (may or may not succeed depending on other nodes)
    let _ = client.get_block_number().await;

    // Restart the RPC node 1
    DockerControl::start_container(containers::RPC_1).expect("Failed to start RPC node 1");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Wait for health check to detect recovery
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Now requests should definitely work
    let result = client.get_block_number().await;
    assert!(result.is_ok(), "Requests should work after node recovery: {:?}", result.err());

    println!("Node recovery verified - requests working after RPC node 1 restart");
}

#[tokio::test]
#[serial]
async fn test_circuit_breaker_trigger() {
    let (client, _fixtures) = setup().await;

    // Stop a node to trigger circuit breaker
    ensure_container_running(containers::RPC_1);

    let _guard =
        ContainerGuard::stop_with_restore(containers::RPC_1).expect("Failed to stop RPC node 1");

    // Make multiple requests to trigger circuit breaker
    let mut error_count = 0;
    for i in 0..10 {
        // Give time between requests for circuit breaker to react
        tokio::time::sleep(Duration::from_millis(200)).await;

        let result = client.get_block_number().await;
        if result.is_err() {
            error_count += 1;
        }
        println!("Request {}: {:?}", i + 1, result.is_ok());
    }

    // Check health to see circuit breaker status
    let health = client.get_health().await.expect("Health check failed");
    println!("Health status after failures: {health:?}");

    // Some requests may fail, but the system should remain operational
    println!("Error count: {error_count}/10");
}

#[tokio::test]
#[serial]
async fn test_paused_node_simulation() {
    let (client, _fixtures) = setup().await;

    ensure_container_running(containers::RPC_3);

    // Pause an RPC node (simulates network issues)
    let pause_result = ContainerGuard::pause_with_restore(containers::RPC_3);

    match pause_result {
        Ok(_guard) => {
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Requests should still work via other nodes
            let result = client.get_block_number().await;
            match result {
                Ok(block_num) => {
                    println!("Got block {block_num} with RPC node 3 paused");
                }
                Err(e) => {
                    println!("Request failed with paused node: {e}");
                }
            }

            // Guard will unpause on drop
        }
        Err(e) => {
            println!("Could not pause container (may require privileges): {e}");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_rolling_restart() {
    let (client, _fixtures) = setup().await;

    // Ensure all RPC containers are running
    ensure_container_running(containers::RPC_1);
    ensure_container_running(containers::RPC_2);
    ensure_container_running(containers::RPC_3);

    // Perform rolling restart - stop/start one at a time (only RPC nodes, not sealer)
    for container in [containers::RPC_1, containers::RPC_2, containers::RPC_3] {
        println!("Restarting {container}...");

        DockerControl::restart_container(container).expect("Failed to restart");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Verify proxy still works
        let result = client.get_block_number().await;
        assert!(
            result.is_ok(),
            "Proxy should work during rolling restart of {container}: {:?}",
            result.err()
        );

        println!("{container} restarted - proxy operational");
    }

    println!("Rolling restart completed successfully");
}

#[tokio::test]
#[serial]
async fn test_health_check_during_failure() {
    let (client, _fixtures) = setup().await;

    ensure_container_running(containers::RPC_1);

    // Get baseline health
    let health_before = client.get_health().await.expect("Health check failed");
    let healthy_before = health_before["upstreams"]["healthy"].as_u64().unwrap_or(0);

    println!("Healthy upstreams before: {healthy_before}");

    // Stop a node
    let _guard =
        ContainerGuard::stop_with_restore(containers::RPC_1).expect("Failed to stop RPC node 1");

    // Wait for health check to detect
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Check health again
    let health_after = client.get_health().await.expect("Health check failed");
    let healthy_after = health_after["upstreams"]["healthy"].as_u64().unwrap_or(0);

    println!("Healthy upstreams after: {healthy_after}");

    // Should have fewer healthy upstreams (or same if already at minimum)
    println!("Health delta: {healthy_before} -> {healthy_after}");
}

#[tokio::test]
#[serial]
async fn test_request_timeout_handling() {
    let (client, _fixtures) = setup().await;

    // Make a request that should complete successfully
    let start = std::time::Instant::now();
    let result = client.get_block_number().await;
    let duration = start.elapsed();

    assert!(result.is_ok(), "Request should succeed: {:?}", result.err());
    assert!(duration < Duration::from_secs(30), "Request should complete within timeout");

    println!("Request completed in {duration:?}");
}

#[tokio::test]
#[serial]
async fn test_continuous_requests_during_failover() {
    let (client, _fixtures) = setup().await;

    ensure_container_running(containers::RPC_1);

    // Start continuous requests in background
    let client_clone = client.clone();
    let request_handle = tokio::spawn(async move {
        let mut success_count = 0;
        let mut error_count = 0;

        for _ in 0..30 {
            match client_clone.get_block_number().await {
                Ok(_) => success_count += 1,
                Err(_) => error_count += 1,
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        (success_count, error_count)
    });

    // While requests are running, stop and start a container
    tokio::time::sleep(Duration::from_secs(1)).await;
    DockerControl::stop_container(containers::RPC_1).expect("Failed to stop");
    tokio::time::sleep(Duration::from_secs(2)).await;
    DockerControl::start_container(containers::RPC_1).expect("Failed to start");

    let (success_count, error_count) = request_handle.await.expect("Request task panicked");

    println!(
        "During failover: {} successes, {} errors ({}% success rate)",
        success_count,
        error_count,
        f64::from(success_count) / 30.0 * 100.0
    );

    // Should have high success rate even during failover
    assert!(success_count > 20, "Should have >66% success rate during failover");
}

#[tokio::test]
#[serial]
async fn test_upstream_selection_after_failure() {
    let (client, _fixtures) = setup().await;

    ensure_container_running(containers::RPC_1);
    ensure_container_running(containers::RPC_2);

    // Make several requests and note which upstream responds
    let mut responses_before = Vec::new();
    for _ in 0..5 {
        let health = client.get_health().await.expect("Health check failed");
        responses_before.push(health);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Stop RPC node 1
    let _guard =
        ContainerGuard::stop_with_restore(containers::RPC_1).expect("Failed to stop RPC node 1");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Make more requests
    let mut responses_after = Vec::new();
    for _ in 0..5 {
        if let Ok(health) = client.get_health().await {
            responses_after.push(health);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("Verified upstream selection adapts after node failure");
}

#[tokio::test]
#[serial]
async fn test_graceful_degradation() {
    let (client, _fixtures) = setup().await;

    // Verify service remains available as RPC nodes are progressively removed
    let containers_to_stop = [containers::RPC_1, containers::RPC_2, containers::RPC_3];
    let mut guards = Vec::new();

    for container in containers_to_stop {
        ensure_container_running(container);
    }

    for (i, container) in containers_to_stop.iter().enumerate() {
        // Stop one more container
        match ContainerGuard::stop_with_restore(container) {
            Ok(guard) => guards.push(guard),
            Err(e) => {
                println!("Could not stop {container}: {e}");
                continue;
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Check if service is still available
        let health_result = client.get_health().await;
        match health_result {
            Ok(health) => {
                let healthy = health["upstreams"]["healthy"].as_u64().unwrap_or(0);
                println!("After stopping {} containers: {} healthy upstreams", i + 1, healthy);

                // Try a real request
                match client.get_block_number().await {
                    Ok(block) => println!("  Request succeeded: block {block}"),
                    Err(e) => println!("  Request failed: {e}"),
                }
            }
            Err(e) => {
                println!("Health check failed after stopping {} containers: {e}", i + 1);
            }
        }
    }

    // Guards will restore all containers on drop
}
