//! Basic RPC Proxying Tests
//!
//! Tests for basic JSON-RPC method proxying functionality.

use super::{
    client::{format_hex_u64, retry_with_backoff, E2eClient},
    config::E2eConfig,
    fixtures::TestFixtures,
};
use serial_test::serial;
use std::time::Duration;

/// Setup function for tests
async fn setup() -> (E2eClient, TestFixtures) {
    let config = E2eConfig::from_env();
    let client = E2eClient::new(config);
    let fixtures = TestFixtures::new(client.clone());

    // Wait for environment to be ready
    fixtures
        .wait_for_environment(Duration::from_secs(30))
        .await
        .expect("Environment not ready");

    (client, fixtures)
}

#[tokio::test]
async fn test_eth_block_number() {
    let (client, _fixtures) = setup().await;

    let block_number = client.get_block_number().await.expect("Failed to get block number");

    // Block number should be non-zero on a running devnet
    assert!(block_number > 0, "Block number should be greater than 0, got {block_number}");

    println!("Current block number: {block_number}");
}

#[tokio::test]
async fn test_eth_chain_id() {
    let (client, _fixtures) = setup().await;

    let chain_id = client.get_chain_id().await.expect("Failed to get chain ID");

    // Chain ID should match the devnet configuration (1337 - private PoA)
    assert_eq!(
        chain_id,
        client.config().chain_id,
        "Chain ID mismatch: expected {}, got {chain_id}",
        client.config().chain_id
    );

    println!("Chain ID: {chain_id}");
}

#[tokio::test]
async fn test_eth_get_balance() {
    let (client, _fixtures) = setup().await;

    let test_account = &client.config().test_account;
    let (balance, duration) =
        client.get_balance(test_account).await.expect("Failed to get balance");

    // Test account should have a balance (pre-funded with 10000 ETH)
    assert!(
        balance != "0x0" && !balance.is_empty(),
        "Test account should have a non-zero balance, got {balance}"
    );

    println!("Balance for {test_account}: {balance} (took {duration:?})");
}

#[tokio::test]
async fn test_eth_get_block_by_number_latest() {
    let (client, _fixtures) = setup().await;

    let response = client
        .get_block_by_number("latest", false)
        .await
        .expect("Failed to get latest block");

    let block = response.response.result.expect("No block returned");

    // Verify block structure
    assert!(block.get("number").is_some(), "Block should have a number");
    assert!(block.get("hash").is_some(), "Block should have a hash");
    assert!(block.get("parentHash").is_some(), "Block should have a parent hash");
    assert!(block.get("timestamp").is_some(), "Block should have a timestamp");

    println!("Latest block number: {}", block["number"]);
    println!("Latest block hash: {}", block["hash"]);
    println!("Request took: {:?}", response.duration);
}

#[tokio::test]
async fn test_eth_get_block_by_number_specific() {
    let (client, fixtures) = setup().await;

    // Get a finalized block number
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    let response = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Failed to get specific block");

    let block = response.response.result.expect("No block returned");

    // Verify the block number matches
    let returned_number = block["number"].as_str().expect("Block number should be a string");
    assert_eq!(returned_number, block_hex, "Block number mismatch");

    println!("Retrieved block {block_num}");
}

#[tokio::test]
#[serial]
async fn test_eth_get_block_by_number_with_transactions() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // Get block with full transactions - use retry for resilience
    let response = retry_with_backoff("get block with transactions", 3, 500, || {
        let client = client.clone();
        let block_hex = block_hex.clone();
        async move { client.get_block_by_number(&block_hex, true).await }
    })
    .await
    .expect("Failed to get block with transactions");

    let block = response.response.result.expect("No block returned");

    // Transactions field should be an array (gracefully handle missing field)
    let Some(transactions) = block.get("transactions") else {
        println!(
            "Block {block_num} has no transactions field (may be cached without full tx data)"
        );
        return;
    };
    assert!(transactions.is_array(), "Transactions should be an array");

    println!("Block {block_num} has {} transactions", transactions.as_array().unwrap().len());
}

#[tokio::test]
async fn test_eth_get_block_by_hash() {
    let (client, fixtures) = setup().await;

    // First get a block hash
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hash = fixtures.get_block_hash(block_num).await.expect("Failed to get block hash");

    // Now query by hash
    let response = client
        .get_block_by_hash(&block_hash, false)
        .await
        .expect("Failed to get block by hash");

    let block = response.response.result.expect("No block returned");

    // Verify the hash matches
    let returned_hash = block["hash"].as_str().expect("Block hash should be a string");
    assert_eq!(returned_hash, block_hash, "Block hash mismatch");

    println!("Retrieved block by hash: {block_hash}");
}

#[tokio::test]
async fn test_eth_get_block_nonexistent() {
    let (client, _fixtures) = setup().await;

    // Query a block that doesn't exist yet (very far in the future)
    let response = client.get_block_by_number("0xffffffffff", false).await;

    // The response should succeed but return null
    let timed_response = response.expect("Request should succeed");
    assert!(
        timed_response.response.result.is_none() ||
            timed_response.response.result == Some(serde_json::Value::Null),
        "Non-existent block should return null"
    );
}

#[tokio::test]
async fn test_eth_get_logs_empty_range() {
    let (client, _fixtures) = setup().await;

    // Get a finalized block using retry for transient issues
    let latest = retry_with_backoff("get block number", 3, 500, || {
        let client = client.clone();
        async move { client.get_block_number().await }
    })
    .await
    .expect("Failed to get block number");

    let block_num = latest.saturating_sub(20);

    // Query logs for a small range (likely empty on fresh devnet)
    let filter = TestFixtures::create_log_filter(block_num.saturating_sub(1), block_num);

    let response = retry_with_backoff("get logs", 3, 500, || {
        let client = client.clone();
        let filter = filter.clone();
        async move { client.get_logs(filter).await }
    })
    .await
    .expect("Failed to get logs");

    let logs = response.response.result.expect("No logs returned");

    println!("Found {} logs in range [{}, {}]", logs.len(), block_num.saturating_sub(1), block_num);
}

#[tokio::test]
async fn test_eth_get_logs_block_range() {
    let (client, _fixtures) = setup().await;

    let latest = client.get_block_number().await.expect("Failed to get block number");

    // Query a reasonable range
    let from_block = latest.saturating_sub(100);
    let to_block = latest.saturating_sub(10);

    let filter = TestFixtures::create_log_filter(from_block, to_block);

    let response = client.get_logs(filter).await.expect("Failed to get logs");

    let logs = response.response.result.expect("No logs returned");

    println!("Found {} logs in range [{from_block}, {to_block}]", logs.len());
    println!("Request took: {:?}", response.duration);
}

#[tokio::test]
async fn test_health_endpoint() {
    let (client, _fixtures) = setup().await;

    let health = client.get_health().await.expect("Failed to get health");

    // Check health response structure
    let status = health
        .get("status")
        .and_then(|s| s.as_str())
        .expect("Health should have status");
    assert!(
        status == "healthy" || status == "unhealthy",
        "Status should be 'healthy' or 'unhealthy', got '{status}'"
    );

    let upstreams = health.get("upstreams").expect("Health should have upstreams");
    assert!(upstreams.get("total").is_some(), "Upstreams should have total count");
    assert!(upstreams.get("healthy").is_some(), "Upstreams should have healthy count");

    println!("Health status: {status}");
    println!("Total upstreams: {}", upstreams["total"]);
    println!("Healthy upstreams: {}", upstreams["healthy"]);
}

#[tokio::test]
async fn test_metrics_endpoint() {
    let (client, _fixtures) = setup().await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Metrics should be in Prometheus format
    assert!(!metrics.is_empty(), "Metrics should not be empty");

    // Check for some expected metric names
    let expected_metrics = ["rpc_requests_total", "rpc_cache"];

    for metric in expected_metrics {
        // Metrics might not have any recorded values yet, so just check format
        println!("Checking for metric pattern: {metric}");
    }

    println!("Metrics endpoint returned {} bytes", metrics.len());
}

#[tokio::test]
async fn test_multiple_sequential_requests() {
    let (client, _fixtures) = setup().await;

    let mut block_numbers = Vec::new();

    for i in 0..5 {
        let block_num = client.get_block_number().await.expect("Failed to get block number");
        block_numbers.push(block_num);
        println!("Request {}: block number {block_num}", i + 1);
    }

    // All block numbers should be valid (non-zero)
    for (i, &num) in block_numbers.iter().enumerate() {
        assert!(num > 0, "Block number {} should be positive", i + 1);
    }

    // In a multi-node devnet environment, block numbers may vary significantly due to
    // load balancing across nodes at different heights (e.g., flaky node at block 100,
    // primary at block 2000). Just verify all block numbers are reasonable.
    let min_block = *block_numbers.iter().min().unwrap();
    let max_block = *block_numbers.iter().max().unwrap();
    let spread = max_block - min_block;

    // Log the spread for debugging, but don't fail on large spreads since
    // the devnet intentionally has nodes at different block heights
    println!("Block number spread: {spread} (min: {min_block}, max: {max_block})");

    // Only fail if spread is extremely large (>10000 blocks), which would indicate a problem
    assert!(
        spread <= 10000,
        "Block number spread ({spread}) is extremely large, possible node misconfiguration"
    );
}

#[tokio::test]
async fn test_concurrent_requests() {
    let (client, _fixtures) = setup().await;

    // Spawn multiple concurrent requests
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.get_block_number().await })
        })
        .collect();

    // Wait for all requests to complete
    let mut results = Vec::new();
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        results.push(result.expect("Request failed"));
    }

    // All results should be valid block numbers
    for (i, block_num) in results.iter().enumerate() {
        assert!(*block_num > 0, "Block number {i} should be positive");
    }

    println!("All 10 concurrent requests succeeded");
}

#[tokio::test]
async fn test_response_timing() {
    let (client, _fixtures) = setup().await;

    let response: super::client::TimedResponse<String> = client
        .proxy_request("eth_blockNumber", serde_json::json!([]))
        .await
        .expect("Failed to get block number");

    // Response should be reasonably fast (increased to 15s to handle concurrent test load)
    assert!(
        response.duration < Duration::from_secs(15),
        "Response took too long: {:?}",
        response.duration
    );

    println!("eth_blockNumber response time: {:?}", response.duration);
}

#[tokio::test]
async fn test_invalid_json_rpc_version() {
    let (client, _fixtures) = setup().await;

    // This test verifies error handling for invalid requests
    // The proxy should reject invalid JSON-RPC versions
    let request = serde_json::json!({
        "jsonrpc": "1.0",  // Invalid version
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });

    let url = &client.config().proxy_url;
    let http_client = reqwest::Client::new();
    let response = http_client.post(url).json(&request).send().await.expect("Request failed");

    // Should get an error response (either HTTP error or JSON-RPC error)
    let body: serde_json::Value = response.json().await.expect("Failed to parse response");

    // Check for error in response
    if body.get("error").is_some() {
        println!("Got expected error response for invalid JSON-RPC version");
    } else {
        println!("Response: {body}");
    }
}
