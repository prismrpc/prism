//! Batch Request Tests
//!
//! Tests for JSON-RPC batch request handling functionality.

use super::{
    client::{format_hex_u64, E2eClient, RpcRequest},
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

    fixtures
        .wait_for_environment(Duration::from_secs(30))
        .await
        .expect("Environment not ready");

    (client, fixtures)
}

#[tokio::test]
async fn test_batch_single_request() {
    let (client, _fixtures) = setup().await;

    // Create a batch with a single request
    let requests = vec![RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 1)];

    let (responses, duration, cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 1, "Should have exactly one response");

    let response = &responses[0];
    assert!(
        response.result.is_some() || response.error.is_some(),
        "Response should have result or error"
    );

    if let Some(result) = &response.result {
        println!("Single batch response: {result}");
    }

    println!("Duration: {duration:?}, Cache: {cache_status:?}");
}

#[tokio::test]
async fn test_batch_multiple_same_method() {
    let (client, _fixtures) = setup().await;

    // Create a batch with multiple eth_blockNumber requests
    let requests: Vec<RpcRequest> = (1..=5)
        .map(|id| RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), id))
        .collect();

    let (responses, duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 5, "Should have 5 responses");

    // All responses should have the same block number (approximately)
    let results: Vec<_> = responses.iter().filter_map(|r| r.result.as_ref()).collect();

    assert_eq!(results.len(), 5, "All requests should succeed");

    // Verify IDs are preserved
    for (i, response) in responses.iter().enumerate() {
        let expected_id = (i + 1) as u64;
        assert_eq!(response.id.as_u64(), Some(expected_id), "Response ID should match request ID");
    }

    println!("5 eth_blockNumber requests completed in {duration:?}");
}

#[tokio::test]
async fn test_batch_different_methods() {
    let (client, _fixtures) = setup().await;

    // Create a batch with different methods
    let requests = vec![
        RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 1),
        RpcRequest::with_id("eth_chainId", serde_json::json!([]), 2),
        RpcRequest::with_id("eth_gasPrice", serde_json::json!([]), 3),
    ];

    let (responses, duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 3, "Should have 3 responses");

    // Verify each response
    for response in &responses {
        // Each should have either a result or error
        assert!(
            response.result.is_some() || response.error.is_some(),
            "Response should have result or error: {response:?}"
        );
    }

    println!("Mixed batch completed in {duration:?}");
}

#[tokio::test]
async fn test_batch_with_parameters() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get block");
    let block_hex = format_hex_u64(block_num);
    let test_account = &client.config().test_account.clone();

    // Create a batch with various parameterized requests
    let requests = vec![
        RpcRequest::with_id("eth_getBlockByNumber", serde_json::json!([&block_hex, false]), 1),
        RpcRequest::with_id("eth_getBalance", serde_json::json!([test_account, "latest"]), 2),
        RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 3),
    ];

    let (responses, duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 3, "Should have 3 responses");

    // Verify getBlockByNumber response has block data
    if let Some(result) = &responses[0].result {
        assert!(result.get("number").is_some(), "Block should have number field");
        println!("Block response: number = {}", result["number"]);
    }

    // Verify getBalance response
    if let Some(result) = &responses[1].result {
        println!("Balance response: {result}");
    }

    // Verify blockNumber response
    if let Some(result) = &responses[2].result {
        println!("Block number response: {result}");
    }

    println!("Parameterized batch completed in {duration:?}");
}

#[tokio::test]
#[serial]
async fn test_batch_large() {
    let (client, _fixtures) = setup().await;

    // Create a large batch (50 requests)
    let requests: Vec<RpcRequest> = (1..=50)
        .map(|id| RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), id))
        .collect();

    let (responses, duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Large batch request failed");

    assert_eq!(responses.len(), 50, "Should have 50 responses");

    let success_count = responses.iter().filter(|r| r.result.is_some()).count();
    let error_count = responses.iter().filter(|r| r.error.is_some()).count();

    println!("Large batch (50 requests): {success_count} success, {error_count} errors");
    println!("Duration: {duration:?}");
    println!("Average per request: {:?}", duration / 50);

    // Most should succeed
    assert!(success_count >= 45, "At least 90% of requests should succeed: {success_count}/50");
}

#[tokio::test]
async fn test_batch_with_invalid_request() {
    let (client, _fixtures) = setup().await;

    // Create a batch with one valid and one invalid request
    // Note: The proxy validates methods, so an unsupported method will return an error
    let requests = vec![
        RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 1),
        RpcRequest::with_id("eth_unsupportedMethod", serde_json::json!([]), 2),
        RpcRequest::with_id("eth_chainId", serde_json::json!([]), 3),
    ];

    let (responses, _duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 3, "Should have 3 responses");

    // First should succeed
    assert!(responses[0].result.is_some(), "First request should succeed: {:?}", responses[0]);

    // Second should fail (unsupported method)
    assert!(responses[1].error.is_some(), "Second request should fail: {:?}", responses[1]);

    // Third should succeed
    assert!(responses[2].result.is_some(), "Third request should succeed: {:?}", responses[2]);

    println!("Mixed valid/invalid batch handled correctly");
}

#[tokio::test]
async fn test_batch_response_order() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get block");

    // Create requests with specific IDs to verify order preservation
    let requests = vec![
        RpcRequest::with_id("eth_chainId", serde_json::json!([]), 100),
        RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 200),
        RpcRequest::with_id(
            "eth_getBlockByNumber",
            serde_json::json!([format_hex_u64(block_num), false]),
            300,
        ),
    ];

    let (responses, _duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    // Verify IDs are preserved in order
    assert_eq!(responses[0].id.as_u64(), Some(100), "First response ID should be 100");
    assert_eq!(responses[1].id.as_u64(), Some(200), "Second response ID should be 200");
    assert_eq!(responses[2].id.as_u64(), Some(300), "Third response ID should be 300");

    println!("Response order verified correctly");
}

#[tokio::test]
async fn test_batch_empty() {
    let (client, _fixtures) = setup().await;

    // Create an empty batch
    let requests: Vec<RpcRequest> = vec![];

    let result = client.proxy_batch_request(requests).await;

    // Empty batch should either return empty response or error
    match result {
        Ok((responses, _duration, _cache_status)) => {
            assert!(responses.is_empty(), "Empty batch should return empty response");
            println!("Empty batch returned empty response array");
        }
        Err(e) => {
            println!("Empty batch returned error (acceptable): {e}");
        }
    }
}

#[tokio::test]
async fn test_batch_timing_vs_sequential() {
    let (client, _fixtures) = setup().await;

    // First, make sequential requests
    let sequential_start = std::time::Instant::now();
    for _ in 0..10 {
        let _ = client.get_block_number().await.expect("Sequential request failed");
    }
    let sequential_duration = sequential_start.elapsed();

    // Then, make a batch request
    let requests: Vec<RpcRequest> = (1..=10)
        .map(|id| RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), id))
        .collect();

    let (responses, batch_duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch request failed");

    assert_eq!(responses.len(), 10, "Should have 10 responses");

    println!("10 sequential requests: {sequential_duration:?}");
    println!("10 batched requests: {batch_duration:?}");

    // Batch should generally be faster due to reduced HTTP overhead
    // But this is a soft assertion as it depends on many factors
    if batch_duration < sequential_duration {
        println!("Batch was faster by {:?}", sequential_duration.saturating_sub(batch_duration));
    } else {
        println!("Sequential was faster (unusual but possible)");
    }
}

#[tokio::test]
#[serial]
#[allow(clippy::cast_sign_loss)]
async fn test_batch_concurrent_batches() {
    let (client, _fixtures) = setup().await;

    // Send multiple batches concurrently
    let handles: Vec<_> = (0..5)
        .map(|batch_id| {
            let client = client.clone();
            tokio::spawn(async move {
                let requests: Vec<RpcRequest> = (1..=10)
                    .map(|id| {
                        RpcRequest::with_id(
                            "eth_blockNumber",
                            serde_json::json!([]),
                            (batch_id * 100 + id) as u64,
                        )
                    })
                    .collect();
                client.proxy_batch_request(requests).await
            })
        })
        .collect();

    let mut total_responses = 0;
    let mut all_succeeded = true;

    for handle in handles {
        match handle.await {
            Ok(Ok((responses, _duration, _cache_status))) => {
                total_responses += responses.len();
                let success = responses.iter().filter(|r| r.result.is_some()).count();
                if success != responses.len() {
                    all_succeeded = false;
                }
            }
            _ => {
                all_succeeded = false;
            }
        }
    }

    assert_eq!(total_responses, 50, "Should have 50 total responses from 5 batches");
    assert!(all_succeeded, "All batch requests should succeed");

    println!("5 concurrent batches (50 total requests) completed successfully");
}

#[tokio::test]
async fn test_batch_cache_interaction() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get block");
    let block_hex = format_hex_u64(block_num);

    // First batch - should cache the block
    let requests1 = vec![
        RpcRequest::with_id("eth_getBlockByNumber", serde_json::json!([&block_hex, false]), 1),
        RpcRequest::with_id("eth_getBlockByNumber", serde_json::json!([&block_hex, false]), 2),
    ];

    let (responses1, duration1, cache_status1) =
        client.proxy_batch_request(requests1).await.expect("First batch failed");

    println!("First batch: duration={duration1:?}, cache={cache_status1:?}");

    // Second batch - should potentially benefit from cache
    let requests2 = vec![
        RpcRequest::with_id("eth_getBlockByNumber", serde_json::json!([&block_hex, false]), 1),
        RpcRequest::with_id("eth_getBlockByNumber", serde_json::json!([&block_hex, false]), 2),
    ];

    let (responses2, duration2, cache_status2) =
        client.proxy_batch_request(requests2).await.expect("Second batch failed");

    println!("Second batch: duration={duration2:?}, cache={cache_status2:?}");

    // Verify core block data is consistent (comparing hash and number only)
    // Cached responses may have fewer fields than fresh responses
    let block1 = responses1[0].result.as_ref().expect("First response should have result");
    let block2 = responses2[0].result.as_ref().expect("Second response should have result");

    assert_eq!(block1.get("hash"), block2.get("hash"), "Block hash should be consistent");
    assert_eq!(block1.get("number"), block2.get("number"), "Block number should be consistent");
}

#[tokio::test]
async fn test_batch_logs_request() {
    let (client, _fixtures) = setup().await;

    let latest = client.get_block_number().await.expect("Failed to get block number");
    let from_block = latest.saturating_sub(100);
    let to_block = latest.saturating_sub(50);

    // Create batch with log queries
    let filter = serde_json::json!({
        "fromBlock": format_hex_u64(from_block),
        "toBlock": format_hex_u64(to_block)
    });

    let requests = vec![
        RpcRequest::with_id("eth_getLogs", serde_json::json!([filter.clone()]), 1),
        RpcRequest::with_id("eth_blockNumber", serde_json::json!([]), 2),
    ];

    let (responses, duration, _cache_status) =
        client.proxy_batch_request(requests).await.expect("Batch with logs failed");

    assert_eq!(responses.len(), 2, "Should have 2 responses");

    // First response should be logs (array)
    if let Some(result) = &responses[0].result {
        assert!(result.is_array(), "Logs result should be an array");
        println!("Logs response: {} entries", result.as_array().unwrap().len());
    }

    // Second should be block number
    assert!(responses[1].result.is_some(), "Block number should succeed");

    println!("Batch with logs completed in {duration:?}");
}
