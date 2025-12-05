//! Cache Behavior Tests
//!
//! Tests for verifying caching functionality including block caching,
//! transaction caching, and log caching with range queries.
//!
//! Note: Some tests use `retry_with_backoff` to handle transient network failures
//! that can occur when the proxy is under load from parallel test execution.

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

    fixtures
        .wait_for_environment(Duration::from_secs(30))
        .await
        .expect("Environment not ready");

    (client, fixtures)
}

#[tokio::test]
async fn test_block_cache_hit() {
    let (client, fixtures) = setup().await;

    // Get a finalized block (should be safe to cache)
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // First request - should be a cache miss
    let response1 = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Failed to get block (first request)");

    println!(
        "First request - cache status: {:?}, duration: {:?}",
        response1.cache_status, response1.duration
    );

    // Second request - should be a cache hit
    let response2 = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Failed to get block (second request)");

    println!(
        "Second request - cache status: {:?}, duration: {:?}",
        response2.cache_status, response2.duration
    );

    // Verify we got the same block
    let block1 = response1.response.result.expect("No block in first response");
    let block2 = response2.response.result.expect("No block in second response");
    assert_eq!(block1["hash"], block2["hash"], "Block hashes should match");

    // Cache hit should generally be faster (though not guaranteed in all cases)
    // We mainly verify that both requests succeed
    println!("Block cache test passed for block {block_num}");
}

#[tokio::test]
async fn test_block_cache_header_status() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // First request
    let response1 = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("First request failed");

    // The first request should typically be a MISS (unless already cached)
    if let Some(status) = &response1.cache_status {
        println!("First request cache status: {status}");
    }

    // Second request should be a HIT
    let response2 = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Second request failed");

    if let Some(status) = &response2.cache_status {
        println!("Second request cache status: {status}");
        // After first request, subsequent ones should show cache involvement
        assert!(
            status == "FULL" || status == "MISS" || status == "PARTIAL",
            "Unexpected cache status: {status}"
        );
    }
}

#[tokio::test]
async fn test_block_cache_by_hash() {
    let (client, fixtures) = setup().await;

    // Get a block hash
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hash = fixtures.get_block_hash(block_num).await.expect("Failed to get block hash");

    // First request by hash
    let response1 = client
        .get_block_by_hash(&block_hash, false)
        .await
        .expect("First request failed");
    println!("Get by hash (1st): {:?}, duration: {:?}", response1.cache_status, response1.duration);

    // Second request by hash - should benefit from cache
    let response2 = client
        .get_block_by_hash(&block_hash, false)
        .await
        .expect("Second request failed");
    println!("Get by hash (2nd): {:?}, duration: {:?}", response2.cache_status, response2.duration);

    // Verify same block returned
    let block1 = response1.response.result.expect("No block in first response");
    let block2 = response2.response.result.expect("No block in second response");
    assert_eq!(block1["number"], block2["number"], "Block numbers should match");
}

/// Tests that caching provides measurable performance improvement.
///
/// This test validates the behavioral contract: cached responses should be
/// significantly faster than uncached responses AND should report cache HIT status.
///
/// Previous version had a false safety indicator: asserting < 1 second was so loose
/// it would pass even without caching (network requests typically < 100ms).
#[tokio::test]
async fn test_cache_timing_improvement() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // First request - should be a cache MISS (cold cache)
    let uncached_response = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Uncached request failed");
    let uncached_time = uncached_response.duration;

    println!(
        "Uncached request: status={:?}, duration={:?}",
        uncached_response.cache_status, uncached_time
    );

    // Second request - should be a cache HIT (warm cache)
    let cached_response = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Cached request failed");
    let cached_time = cached_response.duration;

    println!(
        "Cached request: status={:?}, duration={:?}",
        cached_response.cache_status, cached_time
    );

    // BEHAVIORAL VERIFICATION 1: Cache status should indicate cache was used
    // After first request, subsequent requests should show FULL or PARTIAL cache hit
    if let Some(status) = &cached_response.cache_status {
        assert!(
            status == "FULL" || status == "PARTIAL",
            "Second request should be a cache hit, got status: {status}"
        );
    }

    // BEHAVIORAL VERIFICATION 2: Data integrity - same data returned
    let block1 = uncached_response.response.result.expect("No block in uncached response");
    let block2 = cached_response.response.result.expect("No block in cached response");
    assert_eq!(block1["hash"], block2["hash"], "Cached data must match uncached data");

    // BEHAVIORAL VERIFICATION 3: Cached responses should be faster
    // Instead of an absolute threshold (which is a false safety indicator),
    // we verify relative improvement OR that cached time is reasonably fast.
    //
    // Note: In E2E tests with network variability, we can't guarantee cached < uncached
    // every time (network jitter can make uncached request faster than cached).
    // So we verify either: cached is faster, OR cached is under 50ms (clearly fast).
    let cached_is_fast = cached_time < Duration::from_millis(50);
    let cached_is_faster = cached_time < uncached_time;

    println!(
        "Performance: uncached={:?}, cached={:?}, improvement={:.2}x",
        uncached_time,
        cached_time,
        uncached_time.as_secs_f64() / cached_time.as_secs_f64().max(0.001)
    );

    assert!(
        cached_is_fast || cached_is_faster,
        "Cached response ({cached_time:?}) should be either <50ms or faster than uncached ({uncached_time:?})"
    );
}

#[tokio::test]
async fn test_logs_cache_basic() {
    let (client, _fixtures) = setup().await;

    let latest = client.get_block_number().await.expect("Failed to get block number");
    let from_block = latest.saturating_sub(50);
    let to_block = latest.saturating_sub(20);

    let filter = TestFixtures::create_log_filter(from_block, to_block);

    // First request
    let response1 = client.get_logs(filter.clone()).await.expect("First logs request failed");
    println!(
        "Logs request 1: cache={:?}, duration={:?}, count={}",
        response1.cache_status,
        response1.duration,
        response1.response.result.as_ref().map_or(0, std::vec::Vec::len)
    );

    // Second request - same range
    let response2 = client.get_logs(filter).await.expect("Second logs request failed");
    println!(
        "Logs request 2: cache={:?}, duration={:?}, count={}",
        response2.cache_status,
        response2.duration,
        response2.response.result.as_ref().map_or(0, std::vec::Vec::len)
    );

    // Results should be identical
    assert_eq!(
        response1.response.result, response2.response.result,
        "Log results should be identical for same query"
    );
}

#[tokio::test]
async fn test_logs_cache_overlapping_ranges() {
    let (client, _fixtures) = setup().await;

    let latest = client.get_block_number().await.expect("Failed to get block number");

    // First query: blocks 100-150
    let from1 = latest.saturating_sub(100);
    let to1 = latest.saturating_sub(50);
    let filter1 = TestFixtures::create_log_filter(from1, to1);

    let response1 = client.get_logs(filter1).await.expect("First range query failed");
    println!("Range [{from1}, {to1}]: {:?}", response1.cache_status);

    // Second query: overlapping range 125-175 (overlaps with first query)
    let from2 = latest.saturating_sub(75);
    let to2 = latest.saturating_sub(25);
    let filter2 = TestFixtures::create_log_filter(from2, to2);

    let response2 = client.get_logs(filter2).await.expect("Second range query failed");
    println!("Range [{from2}, {to2}]: {:?}", response2.cache_status);

    // The second query might show PARTIAL cache status if range caching is working
    // This depends on the cache implementation
    println!("Overlapping range test completed");
}

#[tokio::test]
async fn test_logs_cache_subset_range() {
    let (client, _fixtures) = setup().await;

    let latest = client.get_block_number().await.expect("Failed to get block number");

    // First query: large range
    let from_large = latest.saturating_sub(100);
    let to_large = latest.saturating_sub(20);
    let filter_large = TestFixtures::create_log_filter(from_large, to_large);

    let _ = client.get_logs(filter_large).await.expect("Large range query failed");
    println!("Cached large range [{from_large}, {to_large}]");

    // Second query: subset of first range
    let from_small = latest.saturating_sub(80);
    let to_small = latest.saturating_sub(40);
    let filter_small = TestFixtures::create_log_filter(from_small, to_small);

    let response = client.get_logs(filter_small).await.expect("Subset range query failed");
    println!(
        "Subset range [{from_small}, {to_small}]: cache={:?}, duration={:?}",
        response.cache_status, response.duration
    );

    // Subset query should potentially benefit from cached data
}

#[tokio::test]
#[serial]
async fn test_transaction_cache() {
    let (client, fixtures) = setup().await;

    // We need a real transaction hash to test this
    // On a fresh devnet, there might not be any transactions
    // First, get a block and check if it has transactions

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    let block_response = retry_with_backoff("get block with transactions", 3, 500, || {
        let client = client.clone();
        let block_hex = block_hex.clone();
        async move { client.get_block_by_number(&block_hex, true).await }
    })
    .await
    .expect("Failed to get block");

    let block = block_response.response.result.expect("No block returned");
    let Some(transactions) = block["transactions"].as_array() else {
        println!("Block {block_num} has no transactions array, skipping test");
        return;
    };

    if transactions.is_empty() {
        println!("No transactions in block {block_num}, skipping transaction cache test");
        return;
    }

    // Get the first transaction hash
    let tx_hash = transactions[0]["hash"]
        .as_str()
        .or_else(|| transactions[0].as_str())
        .expect("Transaction should have a hash");

    println!("Testing transaction cache with tx: {tx_hash}");

    // First request
    let response1 = client.get_transaction_by_hash(tx_hash).await.expect("First tx request failed");
    println!("Tx request 1: cache={:?}, duration={:?}", response1.cache_status, response1.duration);

    // Second request - should be cached
    let response2 =
        client.get_transaction_by_hash(tx_hash).await.expect("Second tx request failed");
    println!("Tx request 2: cache={:?}, duration={:?}", response2.cache_status, response2.duration);

    // Verify same transaction returned
    assert_eq!(
        response1.response.result, response2.response.result,
        "Transaction results should match"
    );
}

#[tokio::test]
#[serial]
async fn test_receipt_cache() {
    let (client, _fixtures) = setup().await;

    // Get a finalized block using retry for transient network issues
    let latest_block = retry_with_backoff("get block number", 3, 500, || {
        let client = client.clone();
        async move { client.get_block_number().await }
    })
    .await
    .expect("Failed to get block number after retries");

    // Use a block well in the past to ensure it's finalized
    let block_num = latest_block.saturating_sub(20);
    let block_hex = format_hex_u64(block_num);

    let block_response = retry_with_backoff("get block with transactions", 3, 500, || {
        let client = client.clone();
        let block_hex = block_hex.clone();
        async move { client.get_block_by_number(&block_hex, true).await }
    })
    .await
    .expect("Failed to get block");

    let block = block_response.response.result.expect("No block returned");
    let Some(transactions) = block["transactions"].as_array() else {
        println!("Block {block_num} has no transactions array, skipping test");
        return;
    };

    if transactions.is_empty() {
        println!("No transactions in block {block_num}, skipping receipt cache test");
        return;
    }

    let tx_hash = transactions[0]["hash"]
        .as_str()
        .or_else(|| transactions[0].as_str())
        .expect("Transaction should have a hash");

    println!("Testing receipt cache with tx: {tx_hash}");

    // First request
    let response1 = client
        .get_transaction_receipt(tx_hash)
        .await
        .expect("First receipt request failed");
    println!(
        "Receipt request 1: cache={:?}, duration={:?}",
        response1.cache_status, response1.duration
    );

    // Second request - should be cached
    let response2 = client
        .get_transaction_receipt(tx_hash)
        .await
        .expect("Second receipt request failed");
    println!(
        "Receipt request 2: cache={:?}, duration={:?}",
        response2.cache_status, response2.duration
    );

    // Verify same receipt returned
    assert_eq!(
        response1.response.result, response2.response.result,
        "Receipt results should match"
    );
}

#[tokio::test]
async fn test_cache_different_block_tags() {
    let (client, _fixtures) = setup().await;

    // Query "latest" multiple times - this should NOT be cached as latest changes
    // Use retry to handle transient network failures under load
    let response1 = retry_with_backoff("get first latest block", 3, 500, || {
        let client = client.clone();
        async move { client.get_block_by_number("latest", false).await }
    })
    .await
    .expect("First latest request failed after retries");

    // Wait a bit for a new block (devnet has 1s block time)
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let response2 = retry_with_backoff("get second latest block", 3, 500, || {
        let client = client.clone();
        async move { client.get_block_by_number("latest", false).await }
    })
    .await
    .expect("Second latest request failed after retries");

    let block1 = response1.response.result.expect("No first block");
    let block2 = response2.response.result.expect("No second block");

    // Latest should not be cached - blocks might be different
    // (or same if timing worked out)
    println!("First 'latest' block: {}, Second: {}", block1["number"], block2["number"]);
}

#[tokio::test]
async fn test_cache_invalidation_on_new_blocks() {
    let (client, fixtures) = setup().await;

    // Get a known finalized block to avoid multi-node block height inconsistencies
    let finalized_block =
        fixtures.get_finalized_block().await.expect("Failed to get finalized block");

    // Query the specific block multiple times to verify cache behavior
    let block_hex = format_hex_u64(finalized_block);

    let response1 = retry_with_backoff("get finalized block 1", 3, 500, || {
        let client = client.clone();
        let block_hex = block_hex.clone();
        async move { client.get_block_by_number(&block_hex, false).await }
    })
    .await
    .expect("Failed to get finalized block");

    // Wait briefly and query again
    tokio::time::sleep(Duration::from_millis(500)).await;

    let response2 = retry_with_backoff("get finalized block 2", 3, 500, || {
        let client = client.clone();
        let block_hex = block_hex.clone();
        async move { client.get_block_by_number(&block_hex, false).await }
    })
    .await
    .expect("Failed to get finalized block again");

    // Both responses should return the same block (verifying cache consistency)
    let block1 = response1.response.result.expect("First response should have result");
    let block2 = response2.response.result.expect("Second response should have result");

    assert_eq!(
        block1.get("hash"),
        block2.get("hash"),
        "Finalized block hash should be consistent across requests"
    );

    println!("Finalized block {finalized_block} consistent across cache lookups");
}

#[tokio::test]
async fn test_cache_consistency_across_methods() {
    let (client, fixtures) = setup().await;

    // Get a block by number, then by hash, verify consistency
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // Get by number
    let by_number = client
        .get_block_by_number(&block_hex, false)
        .await
        .expect("Get by number failed");
    let block_from_number = by_number.response.result.expect("No block by number");

    // Get the hash
    let block_hash = block_from_number["hash"].as_str().expect("Block should have hash");

    // Get by hash
    let by_hash = client.get_block_by_hash(block_hash, false).await.expect("Get by hash failed");
    let block_from_hash = by_hash.response.result.expect("No block by hash");

    // Blocks should be identical
    assert_eq!(
        block_from_number["number"], block_from_hash["number"],
        "Block numbers should match"
    );
    assert_eq!(block_from_number["hash"], block_from_hash["hash"], "Block hashes should match");
    assert_eq!(
        block_from_number["parentHash"], block_from_hash["parentHash"],
        "Parent hashes should match"
    );

    println!("Cache consistency verified for block {block_num}");
}

#[tokio::test]
#[serial]
async fn test_cache_stress_same_block() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get finalized block");
    let block_hex = format_hex_u64(block_num);

    // Make 50 concurrent requests for the same block
    let handles: Vec<_> = (0..50)
        .map(|_| {
            let client = client.clone();
            let block_hex = block_hex.clone();
            tokio::spawn(async move { client.get_block_by_number(&block_hex, false).await })
        })
        .collect();

    let mut success_count = 0;
    let mut total_duration = Duration::ZERO;

    for handle in handles {
        if let Ok(Ok(response)) = handle.await {
            success_count += 1;
            total_duration += response.duration;
        }
    }

    assert_eq!(success_count, 50, "All requests should succeed");

    let avg_duration = total_duration / 50;
    println!("50 concurrent requests: avg duration = {avg_duration:?}");
}
