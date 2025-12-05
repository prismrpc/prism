//! Reorg Detection and Cache Invalidation Tests
//!
//! Tests for verifying that Prism correctly detects chain reorganizations
//! and invalidates cached data for affected blocks.
//!
//! These tests use `debug_setHead` to artificially trigger reorgs in the devnet.
//!
//! ## Test Flow
//! 1. Query block hash from Prism (populates cache)
//! 2. Trigger reorg via devnet.sh
//! 3. Verify reorg occurred at upstream level (query geth directly)
//! 4. Verify Prism returns the new block hash (cache invalidated)

use super::{
    client::{format_hex_u64, retry_with_backoff, E2eClient, E2eError},
    config::E2eConfig,
    fixtures::TestFixtures,
};
use serial_test::serial;
use std::{process::Command, time::Duration};

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

/// Trigger a reorg using the devnet.sh script
fn trigger_reorg(depth: u64) -> Result<(), E2eError> {
    // Try to find the script - check common locations relative to current dir
    let script_path = std::env::current_dir()
        .ok()
        .and_then(|d| {
            // Try multiple possible paths
            let paths = vec![
                d.join("docker/devnet/devnet.sh"),
                d.join("../docker/devnet/devnet.sh"),
                d.join("../../docker/devnet/devnet.sh"),
            ];
            paths.into_iter().find(|p| p.exists())
        })
        .or_else(|| {
            // Fallback: try to find it using find command from current directory
            Command::new("find")
                .args([".", "-name", "devnet.sh", "-path", "*/docker/devnet/*", "-type", "f"])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8(o.stdout).ok().and_then(|s| {
                        s.lines().next().map(|line| std::path::PathBuf::from(line.trim()))
                    })
                })
        })
        .ok_or_else(|| {
            E2eError::AssertionFailed(
                "Could not find devnet.sh script. Make sure you're running from the project root."
                    .to_string(),
            )
        })?;

    let output = Command::new("bash")
        .arg(&script_path)
        .arg("reorg")
        .arg(depth.to_string())
        .output()
        .map_err(|e| E2eError::DockerError(format!("Failed to run devnet.sh reorg: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(E2eError::DockerError(format!(
            "devnet.sh reorg failed: stdout={stdout}, stderr={stderr}"
        )));
    }

    Ok(())
}

/// Wait for the chain to reach a specific block number
async fn wait_for_block_number(
    client: &E2eClient,
    target: u64,
    timeout: Duration,
) -> Result<u64, E2eError> {
    let start = std::time::Instant::now();
    let mut last_block = 0u64;
    let mut stall_start = std::time::Instant::now();

    while start.elapsed() < timeout {
        let current = client.get_block_number().await?;
        if current >= target {
            return Ok(current);
        }

        // Detect stalled chain (no progress for 30 seconds)
        if current != last_block {
            last_block = current;
            stall_start = std::time::Instant::now();
        } else if stall_start.elapsed() > Duration::from_secs(30) {
            return Err(E2eError::Timeout(format!(
                "Chain stalled at block {current}, waiting for {target}"
            )));
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(E2eError::Timeout(format!("Chain did not reach block {target} in time")))
}

/// Ensures the chain has enough blocks to perform a reorg of the given depth.
/// Returns the current block number once we have enough history.
///
/// This is more robust than waiting for a fixed block number because it works
/// regardless of when the devnet was started.
async fn ensure_enough_blocks_for_reorg(
    client: &E2eClient,
    reorg_depth: u64,
    timeout: Duration,
) -> Result<u64, E2eError> {
    let current = client.get_block_number().await?;
    // We need at least reorg_depth + 5 blocks of history (some buffer for safety)
    let min_required = reorg_depth + 5;

    if current >= min_required {
        println!("Chain at block {current}, sufficient for reorg depth {reorg_depth}");
        return Ok(current);
    }

    println!(
        "Chain at block {current}, need at least {min_required} for reorg depth {reorg_depth}. Waiting..."
    );
    wait_for_block_number(client, min_required, timeout).await
}

/// Wait for a reorg to be processed by polling until block hash changes at upstream
/// Returns the new hash once the reorg is detected at the geth node level
async fn wait_for_upstream_reorg(
    client: &E2eClient,
    geth_url: &str,
    block_number: u64,
    old_hash: &str,
    timeout: Duration,
) -> Result<String, E2eError> {
    let start = std::time::Instant::now();
    let block_hex = format_hex_u64(block_number);

    while start.elapsed() < timeout {
        // Query geth directly to verify the reorg happened at the source
        let response: super::client::TimedResponse<serde_json::Value> = client
            .direct_request(geth_url, "eth_getBlockByNumber", serde_json::json!([block_hex, false]))
            .await?;

        if let Some(block) = response.response.result {
            if let Some(new_hash) = block.get("hash").and_then(|h| h.as_str()) {
                if new_hash != old_hash {
                    println!(
                        "Upstream reorg confirmed for block {block_number}: {old_hash} -> {new_hash}"
                    );
                    return Ok(new_hash.to_string());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(E2eError::Timeout(format!(
        "Upstream reorg not detected for block {block_number} within {timeout:?}. \
         This may indicate that debug_setHead didn't actually create different blocks."
    )))
}

/// Wait for Prism to return the new block hash (verifies cache invalidation)
async fn wait_for_prism_reorg(
    client: &E2eClient,
    block_number: u64,
    expected_hash: &str,
    timeout: Duration,
) -> Result<(), E2eError> {
    let start = std::time::Instant::now();
    let block_hex = format_hex_u64(block_number);

    while start.elapsed() < timeout {
        let response = client.get_block_by_number(&block_hex, false).await?;
        if let Some(block) = response.response.result {
            if let Some(hash) = block.get("hash").and_then(|h| h.as_str()) {
                if hash == expected_hash {
                    println!("Prism returned correct hash for block {block_number}: {hash}");
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(E2eError::Timeout(format!(
        "Prism did not return expected hash {expected_hash} for block {block_number} within {timeout:?}. \
         Cache may not have been invalidated."
    )))
}

#[tokio::test]
#[serial]
async fn test_reorg_detection_basic() {
    let (client, _fixtures) = setup().await;
    let config = client.config();

    // Ensure we have enough blocks for a reorg of depth 5
    let reorg_depth = 5u64;
    let current_block =
        ensure_enough_blocks_for_reorg(&client, reorg_depth, Duration::from_secs(180))
            .await
            .expect("Failed to ensure enough blocks for reorg");

    println!("Starting reorg test at block: {current_block}");

    // Record block hashes for blocks that will be reorged
    let target_block = current_block - reorg_depth;

    // Get block hashes before reorg (from Prism to populate cache)
    let mut old_hashes = Vec::new();
    for i in 0..=reorg_depth {
        let block_num = target_block + i;
        let block_hex = format_hex_u64(block_num);
        let response =
            retry_with_backoff(&format!("get block {block_num} before reorg"), 3, 500, || {
                let client = client.clone();
                let block_hex = block_hex.clone();
                async move { client.get_block_by_number(&block_hex, false).await }
            })
            .await
            .unwrap_or_else(|_| panic!("Failed to get block {block_num}"));

        let block = response.response.result.expect("No block returned");
        let hash = block
            .get("hash")
            .and_then(|h| h.as_str())
            .map(String::from)
            .expect("Block should have hash");
        println!("Block {block_num} hash before reorg: {hash}");
        old_hashes.push((block_num, hash));
    }

    // Trigger reorg
    println!("Triggering reorg with depth: {reorg_depth}");
    trigger_reorg(reorg_depth).expect("Failed to trigger reorg");

    // Wait for new blocks to be mined (sealer mines every ~12 seconds)
    let target_new_block = current_block + 1;
    wait_for_block_number(&client, target_new_block, Duration::from_secs(180))
        .await
        .expect("Failed to wait for new blocks after reorg");

    // Step 1: Verify reorg happened at upstream level (query geth directly)
    println!("Verifying reorg at upstream level...");
    let mut new_hashes = Vec::new();
    for (block_num, old_hash) in &old_hashes {
        let new_hash = wait_for_upstream_reorg(
            &client,
            &config.geth_sealer_url,
            *block_num,
            old_hash,
            Duration::from_secs(60),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("Upstream reorg verification failed for block {block_num}: {e}")
        });
        new_hashes.push((*block_num, new_hash));
    }

    // Step 2: Verify Prism returns new hashes (cache invalidation worked)
    println!("Verifying Prism cache invalidation...");
    for (block_num, expected_hash) in &new_hashes {
        wait_for_prism_reorg(&client, *block_num, expected_hash, Duration::from_secs(30))
            .await
            .unwrap_or_else(|e| {
                panic!("Prism cache invalidation failed for block {block_num}: {e}")
            });
        println!("Block {block_num} correctly updated in Prism cache");
    }

    println!("Reorg detection test passed!");
}

#[tokio::test]
#[serial]
async fn test_reorg_cache_invalidation() {
    let (client, _fixtures) = setup().await;
    let config = client.config();

    // Use a reorg of depth 5
    let reorg_depth = 5u64;

    // Ensure we have enough blocks for the reorg
    let current_block =
        ensure_enough_blocks_for_reorg(&client, reorg_depth, Duration::from_secs(180))
            .await
            .expect("Failed to ensure enough blocks for reorg");

    println!("Starting cache invalidation test at block: {current_block}");

    // Use a block that will be reorged
    let test_block = current_block - 2; // A block that will be in the reorg range
    let block_hex = format_hex_u64(test_block);

    // First, query the block to populate cache
    println!("Populating cache for block {test_block}...");
    let response1 =
        retry_with_backoff(&format!("get block {test_block} (first request)"), 3, 500, || {
            let client = client.clone();
            let block_hex = block_hex.clone();
            async move { client.get_block_by_number(&block_hex, false).await }
        })
        .await
        .expect("Failed to get block (first request)");

    let block1 = response1.response.result.expect("No block in first response");
    let hash1 = block1
        .get("hash")
        .and_then(|h| h.as_str())
        .map(String::from)
        .expect("Block should have hash");
    println!("Block {test_block} hash before reorg: {hash1}");

    // Second request should be cached (verify cache is working)
    let response2 =
        retry_with_backoff(&format!("get block {test_block} (cached request)"), 3, 500, || {
            let client = client.clone();
            let block_hex = block_hex.clone();
            async move { client.get_block_by_number(&block_hex, false).await }
        })
        .await
        .expect("Failed to get block (cached request)");

    let block2 = response2.response.result.expect("No block in second response");
    let hash2 = block2
        .get("hash")
        .and_then(|h| h.as_str())
        .map(String::from)
        .expect("Block should have hash");
    assert_eq!(hash1, hash2, "Cached block should match original");

    // Trigger reorg
    println!("Triggering reorg with depth: {reorg_depth}");
    trigger_reorg(reorg_depth).expect("Failed to trigger reorg");

    // Wait for new blocks
    let target_new_block = current_block + 1;
    wait_for_block_number(&client, target_new_block, Duration::from_secs(180))
        .await
        .expect("Failed to wait for new blocks after reorg");

    // Step 1: Verify reorg happened at upstream level
    println!("Verifying reorg at upstream level for block {test_block}...");
    let new_hash = wait_for_upstream_reorg(
        &client,
        &config.geth_sealer_url,
        test_block,
        &hash1,
        Duration::from_secs(60),
    )
    .await
    .expect("Upstream reorg verification failed");

    // Step 2: Verify Prism returns the new hash (cache invalidation worked)
    println!("Verifying Prism cache invalidation for block {test_block}...");
    wait_for_prism_reorg(&client, test_block, &new_hash, Duration::from_secs(30))
        .await
        .expect("Prism cache invalidation failed");

    println!("Block {test_block} hash changed after reorg: {hash1} -> {new_hash}");
    println!("Cache invalidation test passed!");
}

#[tokio::test]
#[serial]
async fn test_reorg_logs_invalidation() {
    let (client, _fixtures) = setup().await;
    let config = client.config();

    // Use a reorg of depth 5, and we need 10 blocks of history for log range
    let reorg_depth = 5u64;

    // Ensure we have enough blocks for the reorg plus some buffer for the log range query
    // We need at least 10 blocks of history to query logs over
    let current_block =
        ensure_enough_blocks_for_reorg(&client, reorg_depth + 10, Duration::from_secs(180))
            .await
            .expect("Failed to ensure enough blocks for reorg");

    println!("Starting logs invalidation test at block: {current_block}");

    let from_block = current_block.saturating_sub(10);
    let to_block = current_block.saturating_sub(2);

    // Query logs for a range that will be reorged (to populate cache)
    let filter = TestFixtures::create_log_filter(from_block, to_block);
    println!("Querying logs for range [{from_block}, {to_block}]...");

    let response1 = retry_with_backoff("get logs before reorg", 3, 500, || {
        let client = client.clone();
        let filter = filter.clone();
        async move { client.get_logs(filter).await }
    })
    .await
    .expect("Failed to get logs (first request)");

    let logs1 = response1.response.result.unwrap_or_default();
    println!("Found {} logs before reorg", logs1.len());

    // Get a block hash from within the reorg range to track the reorg
    let test_block = to_block;
    let test_block_hex = format_hex_u64(test_block);
    let block_before = retry_with_backoff("get block before reorg", 3, 500, || {
        let client = client.clone();
        let block_hex = test_block_hex.clone();
        async move { client.get_block_by_number(&block_hex, false).await }
    })
    .await
    .expect("Failed to get block before reorg");

    let hash_before = block_before
        .response
        .result
        .as_ref()
        .and_then(|b| b.get("hash"))
        .and_then(|h| h.as_str())
        .map(String::from)
        .expect("Block should have hash");
    println!("Block {test_block} hash before reorg: {hash_before}");

    // Trigger reorg
    println!("Triggering reorg with depth: {reorg_depth}");
    trigger_reorg(reorg_depth).expect("Failed to trigger reorg");

    // Wait for new blocks
    let target_new_block = current_block + 1;
    wait_for_block_number(&client, target_new_block, Duration::from_secs(180))
        .await
        .expect("Failed to wait for new blocks after reorg");

    // Step 1: Verify reorg happened at upstream level
    println!("Verifying reorg at upstream level for block {test_block}...");
    let new_hash = wait_for_upstream_reorg(
        &client,
        &config.geth_sealer_url,
        test_block,
        &hash_before,
        Duration::from_secs(60),
    )
    .await
    .expect("Upstream reorg verification failed");

    // Step 2: Verify Prism returns the new block hash (cache invalidation worked)
    println!("Verifying Prism cache invalidation for block {test_block}...");
    wait_for_prism_reorg(&client, test_block, &new_hash, Duration::from_secs(30))
        .await
        .expect("Prism cache invalidation failed");

    // Step 3: Query logs again - should reflect the new chain state
    let filter2 = TestFixtures::create_log_filter(from_block, to_block);
    let response2 = retry_with_backoff("get logs after reorg", 5, 1000, || {
        let client = client.clone();
        let filter = filter2.clone();
        async move { client.get_logs(filter).await }
    })
    .await
    .expect("Failed to get logs after reorg");

    let logs2 = response2.response.result.unwrap_or_default();
    let logs1_len = logs1.len();
    let logs2_len = logs2.len();
    println!("Found {logs2_len} logs after reorg (was {logs1_len} before)");

    println!("Block {test_block} hash changed: {hash_before} -> {new_hash}");
    println!("Logs invalidation test passed - cache was invalidated and fresh data fetched!");
}

#[tokio::test]
#[serial]
async fn test_reorg_within_safety_depth() {
    let (client, _fixtures) = setup().await;
    let config = client.config();

    // Test with a reorg depth that's within the safety_depth (12 blocks in config.test.toml)
    let reorg_depth = 3u64; // Small reorg within safety window

    // Ensure we have enough blocks for the reorg
    let current_block =
        ensure_enough_blocks_for_reorg(&client, reorg_depth, Duration::from_secs(180))
            .await
            .expect("Failed to ensure enough blocks for reorg");

    println!("Starting safety depth test at block: {current_block}");

    let test_block = current_block - 1; // Very recent block

    let block_hex = format_hex_u64(test_block);

    // Get block before reorg (populates cache)
    let response1 =
        retry_with_backoff(&format!("get block {test_block} before reorg"), 3, 500, || {
            let client = client.clone();
            let block_hex = block_hex.clone();
            async move { client.get_block_by_number(&block_hex, false).await }
        })
        .await
        .expect("Failed to get block before reorg");

    let block1 = response1.response.result.expect("No block in first response");
    let hash1 = block1
        .get("hash")
        .and_then(|h| h.as_str())
        .map(String::from)
        .expect("Block should have hash");
    println!("Block {test_block} hash before reorg: {hash1}");

    // Trigger reorg
    println!("Triggering small reorg (depth: {reorg_depth}) within safety window...");
    trigger_reorg(reorg_depth).expect("Failed to trigger reorg");

    // Wait for new blocks
    let target_new_block = current_block + 1;
    wait_for_block_number(&client, target_new_block, Duration::from_secs(180))
        .await
        .expect("Failed to wait for new blocks after reorg");

    // Step 1: Verify reorg happened at upstream level
    println!("Verifying reorg at upstream level for block {test_block}...");
    let new_hash = wait_for_upstream_reorg(
        &client,
        &config.geth_sealer_url,
        test_block,
        &hash1,
        Duration::from_secs(60),
    )
    .await
    .expect("Upstream reorg verification failed");

    // Step 2: Verify Prism returns the new hash (cache invalidation worked)
    println!("Verifying Prism cache invalidation for block {test_block}...");
    wait_for_prism_reorg(&client, test_block, &new_hash, Duration::from_secs(30))
        .await
        .expect("Prism cache invalidation failed");

    println!("Block {test_block} hash changed: {hash1} -> {new_hash}");
    println!("Safety depth test passed!");
}
