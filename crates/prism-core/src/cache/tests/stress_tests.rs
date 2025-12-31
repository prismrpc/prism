#![allow(
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::cast_lossless
)]

use crate::{
    cache::types::{LogId, PackingError},
    chain::ChainState,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn stress_test_logid_max_log_index() {
    const MAX_LOG_INDEX: u32 = 0x3F_FFFF;

    let log_id = LogId::new(100, MAX_LOG_INDEX);
    let packed = log_id.pack();
    assert!(packed.is_ok(), "MAX_LOG_INDEX should pack successfully");

    let packed_val = packed.unwrap();
    let unpacked = LogId::unpack_with_chunk(packed_val, 0);
    assert_eq!(unpacked.log_index, MAX_LOG_INDEX);

    let log_id_overflow = LogId { block_number: 100, log_index: MAX_LOG_INDEX + 1 };
    let result = log_id_overflow.pack();
    assert!(
        matches!(result, Err(PackingError::LogIndexTooLarge { log_index }) if log_index == MAX_LOG_INDEX + 1),
        "log_index > MAX should return LogIndexTooLarge error"
    );

    assert!(log_id_overflow.try_pack().is_none());
}

#[test]
fn stress_test_logid_chunk_boundaries() {
    const CHUNK_SIZE: u64 = LogId::DEFAULT_CHUNK_SIZE;

    let log_at_999 = LogId::new(999, 0);
    let packed_999 = log_at_999.pack().unwrap();
    let unpacked_999 = LogId::unpack_with_chunk(packed_999, 0);
    assert_eq!(unpacked_999.block_number, 999);

    let log_at_1000 = LogId::new(1000, 0);
    let packed_1000 = log_at_1000.pack().unwrap();
    let unpacked_1000 = LogId::unpack_with_chunk(packed_1000, 1);
    assert_eq!(unpacked_1000.block_number, 1000);

    let log_at_1001 = LogId::new(1001, 0);
    let packed_1001 = log_at_1001.pack().unwrap();
    let unpacked_1001 = LogId::unpack_with_chunk(packed_1001, 1);
    assert_eq!(unpacked_1001.block_number, 1001);

    assert_eq!(999 / CHUNK_SIZE, 0, "Block 999 should be in chunk 0");
    assert_eq!(1000 / CHUNK_SIZE, 1, "Block 1000 should be in chunk 1");
    assert_eq!(1001 / CHUNK_SIZE, 1, "Block 1001 should be in chunk 1");
}

#[test]
fn stress_test_logid_max_block_offset() {
    let max_chunk_size: u64 = LogId::MAX_CHUNK_SIZE;

    let log_id_valid = LogId::new(1023, 0);
    let result = log_id_valid.pack_with_chunk_size(max_chunk_size);
    assert!(result.is_ok(), "block_offset=1023 should pack with chunk_size=1024");

    let packed = result.unwrap();
    let unpacked = LogId::unpack(packed);
    assert_eq!(unpacked.block_number, 1023);
    assert_eq!(unpacked.log_index, 0);

    let log_id_wrap = LogId::new(1024, 0);
    let result = log_id_wrap.pack_with_chunk_size(max_chunk_size);
    assert!(result.is_ok(), "block_number=1024 % chunk_size=1024 = 0, should pack successfully");
}

#[test]
fn stress_test_logid_chunk_size_overflow() {
    let huge_chunk_size: u64 = LogId::MAX_CHUNK_SIZE + 1;
    let log_id = LogId::new(100, 50);

    let result = log_id.pack_with_chunk_size(huge_chunk_size);
    assert!(
        matches!(result, Err(PackingError::ChunkSizeTooLarge { chunk_size, .. }) if chunk_size == huge_chunk_size),
        "chunk_size > MAX_CHUNK_SIZE should return ChunkSizeTooLarge error"
    );

    assert!(log_id.try_pack_with_chunk_size(huge_chunk_size).is_none());

    let very_large_chunk: u64 = u64::from(u32::MAX) + 1;
    let result = log_id.pack_with_chunk_size(very_large_chunk);
    assert!(matches!(result, Err(PackingError::ChunkSizeTooLarge { .. })));
}

#[test]
fn stress_test_logid_rapid_cross_chunk_inserts() {
    const NUM_CHUNKS: u64 = 10;
    const LOGS_PER_BLOCK: u32 = 100;
    const CHUNK_SIZE: u64 = LogId::DEFAULT_CHUNK_SIZE;

    let mut all_packed: Vec<(u64, u32, u32)> = Vec::new();

    for chunk_id in 0..NUM_CHUNKS {
        let base_block = chunk_id * CHUNK_SIZE;

        for block_offset in [0, CHUNK_SIZE / 2, CHUNK_SIZE - 1] {
            let block_num = base_block + block_offset;

            for log_idx in 0..LOGS_PER_BLOCK {
                let log_id = LogId::new(block_num, log_idx);
                let packed = log_id.pack().expect("Should pack within valid bounds");
                all_packed.push((chunk_id, packed, log_idx));
            }
        }
    }

    for (chunk_id, packed, expected_log_idx) in all_packed {
        let unpacked = LogId::unpack_with_chunk(packed, chunk_id);
        assert_eq!(
            unpacked.log_index, expected_log_idx,
            "Log index mismatch for chunk {} packed value {}",
            chunk_id, packed
        );
    }
}

#[test]
fn stress_test_logid_combined_max_values() {
    const MAX_LOG_INDEX: u32 = 0x3F_FFFF;
    const MAX_BLOCK_OFFSET: u32 = 1023;

    let chunk_size: u64 = (MAX_BLOCK_OFFSET + 1) as u64;

    let log_id = LogId { block_number: u64::from(MAX_BLOCK_OFFSET), log_index: MAX_LOG_INDEX };

    let packed = log_id.pack_with_chunk_size(chunk_size);
    assert!(packed.is_ok(), "Combined max values should pack successfully");

    let packed_val = packed.unwrap();
    assert_eq!(packed_val, u32::MAX, "Combined max values should produce u32::MAX");

    let unpacked = LogId::unpack_with_chunk_and_size(packed_val, 0, chunk_size);
    assert_eq!(unpacked.block_number, u64::from(MAX_BLOCK_OFFSET));
    assert_eq!(unpacked.log_index, MAX_LOG_INDEX);
}

#[tokio::test]
async fn stress_test_concurrent_chain_state_updates() {
    use tokio::task;

    const NUM_ITERATIONS: usize = 5;
    const NUM_UPDATE_TIP_TASKS: usize = 40;
    const NUM_FORCE_UPDATE_TASKS: usize = 30;
    const NUM_FINALIZED_TASKS: usize = 30;
    const MAX_BLOCK: u64 = 10000;

    for iteration in 0..NUM_ITERATIONS {
        let state = Arc::new(ChainState::new());
        // Pre-set tip high enough for finalized updates
        state.force_update_tip(MAX_BLOCK, [0u8; 32]).await;

        let invariant_violations = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];

        // Spawn update_tip tasks (only advances)
        for i in 0..NUM_UPDATE_TIP_TASKS {
            let state_clone = state.clone();
            handles.push(task::spawn(async move {
                let block = ((i as u64 * 100) % MAX_BLOCK) + 1;
                #[allow(clippy::cast_possible_truncation)]
                let hash = [block as u8; 32];
                state_clone.update_tip(block, hash).await;
            }));
        }

        // Spawn force_update_tip tasks (can rollback)
        for i in 0..NUM_FORCE_UPDATE_TASKS {
            let state_clone = state.clone();
            handles.push(task::spawn(async move {
                // Alternate between high and low blocks to stress rollback paths
                let block = if i % 2 == 0 {
                    ((i as u64 * 50) % MAX_BLOCK) + 1
                } else {
                    MAX_BLOCK - (i as u64 * 10)
                };
                #[allow(clippy::cast_possible_truncation)]
                let hash = [block as u8; 32];
                state_clone.force_update_tip(block, hash).await;
            }));
        }

        // Spawn update_finalized tasks
        for i in 0..NUM_FINALIZED_TASKS {
            let state_clone = state.clone();
            let violations = invariant_violations.clone();
            handles.push(task::spawn(async move {
                let block = ((i as u64 * 100) % (MAX_BLOCK / 2)) + 1;
                let _ = state_clone.update_finalized(block).await;

                // Verify invariant: finalized <= tip
                let finalized = state_clone.finalized_block();
                let tip = state_clone.current_tip();
                if finalized > tip {
                    violations.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        // Verify no invariant violations occurred
        let violations = invariant_violations.load(Ordering::SeqCst);
        assert_eq!(
            violations, 0,
            "Iteration {}: {} invariant violations (finalized > tip)",
            iteration, violations
        );

        // Final consistency check
        let (final_tip, final_hash) = state.current_tip_with_hash();
        #[allow(clippy::cast_possible_truncation)]
        let expected_hash = [final_tip as u8; 32];
        assert_eq!(
            final_hash, expected_hash,
            "Iteration {}: final state inconsistent - tip={}, hash={:?}",
            iteration, final_tip, final_hash
        );
    }
}

#[tokio::test]
async fn stress_test_reorg_storm() {
    use tokio::task;

    const NUM_ITERATIONS: usize = 5;
    const NUM_REORG_TASKS: usize = 50;

    for iteration in 0..NUM_ITERATIONS {
        let state = Arc::new(ChainState::new());
        let consistency_violations = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];

        // Simulate reorg storm: tasks forcing tip to oscillate between blocks
        for i in 0..NUM_REORG_TASKS {
            let state_clone = state.clone();
            let violations = consistency_violations.clone();

            handles.push(task::spawn(async move {
                // Alternate between different "chains" simulating reorg contention
                let chain_a_tip = 1000 + (i as u64 % 10);
                let chain_b_tip = 995 + (i as u64 % 15);

                // Force to chain A
                #[allow(clippy::cast_possible_truncation)]
                state_clone.force_update_tip(chain_a_tip, [chain_a_tip as u8; 32]).await;

                // Read and verify consistency
                let (tip, hash) = state_clone.current_tip_with_hash();
                #[allow(clippy::cast_possible_truncation)]
                if hash != [tip as u8; 32] {
                    violations.fetch_add(1, Ordering::SeqCst);
                }

                // Force to chain B (simulates reorg)
                #[allow(clippy::cast_possible_truncation)]
                state_clone.force_update_tip(chain_b_tip, [chain_b_tip as u8; 32]).await;

                // Verify consistency again
                let (tip, hash) = state_clone.current_tip_with_hash();
                #[allow(clippy::cast_possible_truncation)]
                if hash != [tip as u8; 32] {
                    violations.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        let violations = consistency_violations.load(Ordering::SeqCst);
        assert_eq!(
            violations, 0,
            "Iteration {}: {} consistency violations during reorg storm",
            iteration, violations
        );
    }
}

#[tokio::test]
async fn stress_test_read_write_contention_during_reorgs() {
    use tokio::task;

    const NUM_WRITERS: usize = 20;
    const NUM_READERS: usize = 50;
    const READS_PER_TASK: usize = 100;

    let state = Arc::new(ChainState::new());
    let consistency_violations = Arc::new(AtomicUsize::new(0));
    let total_reads = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn writers that continuously update/force-update tip
    for i in 0..NUM_WRITERS {
        let state_clone = state.clone();
        handles.push(task::spawn(async move {
            for j in 0..10 {
                let block = ((i * 100 + j) as u64) % 5000 + 1;
                #[allow(clippy::cast_possible_truncation)]
                let hash = [block as u8; 32];
                if j % 2 == 0 {
                    state_clone.update_tip(block, hash).await;
                } else {
                    state_clone.force_update_tip(block, hash).await;
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    // Spawn readers that verify consistency
    for _ in 0..NUM_READERS {
        let state_clone = state.clone();
        let violations = consistency_violations.clone();
        let reads = total_reads.clone();

        handles.push(task::spawn(async move {
            for _ in 0..READS_PER_TASK {
                let (tip, hash) = state_clone.current_tip_with_hash();

                // Property: hash must match tip (each writer uses hash = [block as u8; 32])
                if tip > 0 {
                    #[allow(clippy::cast_possible_truncation)]
                    let expected_hash = [tip as u8; 32];
                    if hash != expected_hash {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }
                }
                reads.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    let violations = consistency_violations.load(Ordering::SeqCst);
    let reads = total_reads.load(Ordering::Relaxed);

    assert_eq!(violations, 0, "Found {} consistency violations across {} reads", violations, reads);
    assert!(reads > 0, "No reads were performed");
}

#[tokio::test]
async fn stress_test_finalized_tip_invariant_under_contention() {
    use tokio::task;

    const NUM_ITERATIONS: usize = 10;
    const NUM_TASKS: usize = 50;

    for iteration in 0..NUM_ITERATIONS {
        let state = Arc::new(ChainState::new());
        // Set initial tip
        state.force_update_tip(1000, [0xAA; 32]).await;

        let invariant_violations = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        // Mix of tasks: some lowering tip, some raising finalized
        for i in 0..NUM_TASKS {
            let state_clone = state.clone();
            let violations = invariant_violations.clone();

            handles.push(task::spawn(async move {
                if i % 3 == 0 {
                    // Try to lower tip (creates race with finalized updates)
                    let new_tip = 500 + (i as u64 % 100);
                    #[allow(clippy::cast_possible_truncation)]
                    state_clone.force_update_tip(new_tip, [new_tip as u8; 32]).await;
                } else if i % 3 == 1 {
                    // Try to raise finalized
                    let new_finalized = 100 + (i as u64 * 5);
                    let _ = state_clone.update_finalized(new_finalized).await;
                } else {
                    // Try to raise tip
                    let new_tip = 2000 + (i as u64 * 10);
                    #[allow(clippy::cast_possible_truncation)]
                    let _ = state_clone.update_tip(new_tip, [new_tip as u8; 32]).await;
                }

                // Check invariant after each operation
                let finalized = state_clone.finalized_block();
                let tip = state_clone.current_tip();
                if finalized > tip {
                    violations.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        let violations = invariant_violations.load(Ordering::SeqCst);
        assert_eq!(
            violations, 0,
            "Iteration {}: {} violations of finalized <= tip invariant",
            iteration, violations
        );

        // Final invariant check
        let finalized = state.finalized_block();
        let tip = state.current_tip();
        assert!(
            finalized <= tip,
            "Final state violates invariant: finalized={} > tip={}",
            finalized,
            tip
        );
    }
}

#[tokio::test]
async fn stress_test_finalized_never_exceeds_tip_race() {
    use tokio::task;

    const NUM_ITERATIONS: usize = 20;

    for iteration in 0..NUM_ITERATIONS {
        let state = Arc::new(ChainState::new());

        // Start with tip at 1000
        state.force_update_tip(1000, [0u8; 32]).await;

        let mut handles: Vec<task::JoinHandle<()>> = vec![];

        // Task 1: Try to set finalized to 500
        let state1 = state.clone();
        handles.push(task::spawn(async move {
            let _ = state1.update_finalized(500).await;
        }));

        // Task 2: Try to advance tip (normal update, not force)
        let state2 = state.clone();
        handles.push(task::spawn(async move {
            let _ = state2.update_tip(1200, [0u8; 32]).await;
        }));

        // Task 3: Try to set finalized higher
        let state3 = state.clone();
        handles.push(task::spawn(async move {
            let _ = state3.update_finalized(800).await;
        }));

        for handle in handles {
            let _ = handle.await;
        }

        // Verify invariant holds when using normal update methods
        let finalized = state.finalized_block();
        let tip = state.current_tip();

        assert!(
            finalized <= tip,
            "Iteration {}: invariant violated - finalized={} > tip={}",
            iteration,
            finalized,
            tip
        );
    }
}

fn stress_test_response(result: Option<serde_json::Value>) -> crate::types::JsonRpcResponse {
    use crate::types::JSONRPC_VERSION_COW;
    crate::types::JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result,
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    }
}

#[test]
fn stress_test_consensus_all_different_responses() {
    use crate::upstream::consensus::quorum;

    // Create responses where every upstream returns a different value
    let responses: Vec<(Arc<str>, crate::types::JsonRpcResponse)> = (0..10)
        .map(|i| {
            let response = stress_test_response(Some(serde_json::json!({
                "number": format!("0x{:x}", i * 100),
                "hash": format!("0x{:064x}", i)
            })));
            (Arc::from(format!("upstream_{}", i)), response)
        })
        .collect();

    // Group responses - should get 10 groups (all different)
    let groups = quorum::group_responses(responses, None);

    assert_eq!(groups.len(), 10, "All different responses should create 10 groups");

    // Each group should have exactly 1 upstream
    for group in &groups {
        assert_eq!(group.count, 1, "Each group should have exactly 1 upstream when all different");
    }
}

#[test]
fn stress_test_consensus_partial_agreement() {
    use crate::upstream::consensus::quorum;

    let agreeing_response =
        stress_test_response(Some(serde_json::json!({"number": "0x100", "hash": "0xabc"})));

    let mut responses: Vec<(Arc<str>, crate::types::JsonRpcResponse)> = Vec::new();

    // 5 upstreams agree
    for i in 0..5 {
        responses.push((Arc::from(format!("agreeing_{}", i)), agreeing_response.clone()));
    }

    // 5 upstreams all return different values
    for i in 0..5 {
        let different_response = stress_test_response(Some(serde_json::json!({
            "number": format!("0x{:x}", (i + 1) * 200),
            "hash": format!("0x{:064x}", i + 1000)
        })));
        responses.push((Arc::from(format!("different_{}", i)), different_response));
    }

    let groups = quorum::group_responses(responses, None);

    // Should have 6 groups: 1 with 5 upstreams, 5 with 1 upstream each
    assert_eq!(groups.len(), 6, "Should have 6 groups (1 agreeing + 5 different)");

    // Find the majority group
    let majority_group = groups.iter().max_by_key(|g| g.count).unwrap();
    assert_eq!(majority_group.count, 5, "Majority group should have 5 upstreams");
}

#[test]
fn stress_test_consensus_empty_responses() {
    use crate::upstream::consensus::quorum;

    // Create mix of empty and non-empty responses
    let empty_response = stress_test_response(Some(serde_json::json!(null)));
    let non_empty_response = stress_test_response(Some(serde_json::json!({"data": "value"})));

    let responses = vec![
        (Arc::from("upstream1"), empty_response.clone()),
        (Arc::from("upstream2"), empty_response.clone()),
        (Arc::from("upstream3"), empty_response.clone()),
        (Arc::from("upstream4"), non_empty_response.clone()),
        (Arc::from("upstream5"), non_empty_response.clone()),
    ];

    let groups = quorum::group_responses(responses, None);

    // Should have 2 groups: empty (3) and non-empty (2)
    assert_eq!(groups.len(), 2, "Should have 2 groups (empty and non-empty)");

    let empty_group = groups.iter().find(|g| g.count == 3).unwrap();
    let non_empty_group = groups.iter().find(|g| g.count == 2).unwrap();

    assert_eq!(empty_group.upstreams.len(), 3);
    assert_eq!(non_empty_group.upstreams.len(), 2);
}

#[test]
fn stress_test_consensus_error_responses() {
    use crate::{
        types::{JsonRpcError, JSONRPC_VERSION_COW},
        upstream::consensus::quorum,
    };

    // Create error responses with same error code
    let error_response = crate::types::JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: None,
        error: Some(JsonRpcError {
            code: -32000,
            message: "execution reverted".to_string(),
            data: None,
        }),
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    // Create error responses with different error codes
    let different_error = crate::types::JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: None,
        error: Some(JsonRpcError {
            code: -32601,
            message: "method not found".to_string(),
            data: None,
        }),
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let success_response = stress_test_response(Some(serde_json::json!({"success": true})));

    let responses = vec![
        (Arc::from("upstream1"), error_response.clone()),
        (Arc::from("upstream2"), error_response.clone()),
        (Arc::from("upstream3"), different_error),
        (Arc::from("upstream4"), success_response.clone()),
        (Arc::from("upstream5"), success_response.clone()),
    ];

    let groups = quorum::group_responses(responses, None);

    // Should have 3 groups: same error (2), different error (1), success (2)
    assert_eq!(groups.len(), 3, "Should have 3 distinct response groups");
}

#[test]
fn stress_test_consensus_many_upstreams() {
    use crate::upstream::consensus::quorum;

    const NUM_UPSTREAMS: usize = 100;
    const NUM_ITERATIONS: usize = 10;

    for iteration in 0..NUM_ITERATIONS {
        let mut responses: Vec<(Arc<str>, crate::types::JsonRpcResponse)> =
            Vec::with_capacity(NUM_UPSTREAMS);

        // Create pattern: 40% response A, 30% response B, 20% response C, 10% unique
        for i in 0..NUM_UPSTREAMS {
            let response_value = match i % 10 {
                0..=3 => "0x100",                      // 40%
                4..=6 => "0x200",                      // 30%
                7..=8 => "0x300",                      // 20%
                _ => &format!("0x{:x}", 1000 + i)[..], // 10% unique
            };

            let response =
                stress_test_response(Some(serde_json::json!({"number": response_value})));
            responses.push((Arc::from(format!("upstream_{}", i)), response));
        }

        let groups = quorum::group_responses(responses, None);

        // Should have at least 3 main groups + some unique ones
        assert!(
            groups.len() >= 3,
            "Iteration {}: Should have at least 3 groups, got {}",
            iteration,
            groups.len()
        );

        // Verify total upstreams across all groups equals input count
        let total: usize = groups.iter().map(|g| g.count).sum();
        assert_eq!(
            total, NUM_UPSTREAMS,
            "Iteration {}: Total upstreams in groups ({}) != input count ({})",
            iteration, total, NUM_UPSTREAMS
        );

        // Find largest group (should be ~40%)
        let max_group = groups.iter().max_by_key(|g| g.count).unwrap();
        assert!(
            max_group.count >= 35 && max_group.count <= 45,
            "Iteration {}: Largest group should be ~40 upstreams, got {}",
            iteration,
            max_group.count
        );
    }
}

#[test]
fn stress_test_consensus_with_ignore_paths() {
    use crate::upstream::consensus::quorum;

    // Responses that differ only in timestamp field
    let response1 = stress_test_response(Some(serde_json::json!({
        "number": "0x100",
        "hash": "0xabc",
        "timestamp": "0x12345"
    })));

    let response2 = stress_test_response(Some(serde_json::json!({
        "number": "0x100",
        "hash": "0xabc",
        "timestamp": "0x54321"  // Different timestamp
    })));

    let responses = vec![
        (Arc::from("upstream1"), response1.clone()),
        (Arc::from("upstream2"), response2.clone()),
        (Arc::from("upstream3"), response1.clone()),
    ];

    // Without ignore_paths: should be 2 groups (different timestamps)
    let groups_without_filter = quorum::group_responses(responses.clone(), None);
    assert_eq!(
        groups_without_filter.len(),
        2,
        "Without filter: responses with different timestamps should be in different groups"
    );

    // With ignore_paths for timestamp: should be 1 group
    // Note: paths are relative to the result object, so "timestamp" not "result.timestamp"
    let ignore_paths = vec!["timestamp".to_string()];
    let groups_with_filter = quorum::group_responses(responses, Some(ignore_paths.as_slice()));
    assert_eq!(
        groups_with_filter.len(),
        1,
        "With timestamp filter: all responses should be in one group"
    );
    assert_eq!(groups_with_filter[0].count, 3);
}
