//! Benchmarks for consensus engine to verify performance doesn't regress after refactoring.
//!
//! Run benchmarks:
//! ```bash
//! cargo bench --bench consensus_engine_benchmarks
//! ```
//!
//! Save baseline before refactoring:
//! ```bash
//! cargo bench --bench consensus_engine_benchmarks -- --save-baseline pre-refactor
//! ```
//!
//! Compare after refactoring:
//! ```bash
//! cargo bench --bench consensus_engine_benchmarks -- --baseline pre-refactor
//! ```
#![allow(clippy::expect_used)] // Acceptable in benchmark code
#![allow(clippy::semicolon_if_nothing_returned)] // Benchmark closures don't need semicolons

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use prism_core::{
    types::{JsonRpcResponse, JSONRPC_VERSION_COW},
    upstream::consensus::{
        config::ConsensusConfig,
        engine::ConsensusEngine,
        types::{ResponseGroup, ResponseType},
    },
    utils::json_hash::{hash_json_response, hash_json_response_filtered},
};
use std::{collections::HashMap, hint::black_box, sync::Arc, time::Duration};

/// Create a shared Tokio runtime for benchmarks
fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Configure Criterion for stable, reproducible benchmarks
fn criterion_config() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(3))
        .sample_size(100)
        .noise_threshold(0.02)
        .confidence_level(0.95)
}

// ============================================================================
// Test Data Generation
// ============================================================================

/// Generate a test response with meaningful content
fn generate_response(block_number: u64, tx_count: usize, include_result: bool) -> JsonRpcResponse {
    let result = if include_result {
        Some(serde_json::json!({
            "number": format!("0x{:x}", block_number),
            "hash": format!("0x{:064x}", block_number),
            "parentHash": format!("0x{:064x}", block_number.saturating_sub(1)),
            "nonce": "0x0000000000000000",
            "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
            "logsBloom": "0x".to_string() + &"00".repeat(256),
            "transactionsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "stateRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "receiptsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "miner": "0x0000000000000000000000000000000000000000",
            "difficulty": "0x1",
            "totalDifficulty": "0x1",
            "extraData": "0x",
            "size": "0x100",
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x0",
            "timestamp": format!("0x{:x}", 1_700_000_000 + block_number),
            "transactions": (0..tx_count).map(|i| format!("0x{:064x}", block_number * 1000 + i as u64)).collect::<Vec<_>>(),
            "uncles": []
        }))
    } else {
        None
    };

    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result,
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    }
}

/// Generate an error response
fn generate_error_response(code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: None,
        error: Some(prism_core::types::JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        }),
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    }
}

/// Generate an empty/null response
fn generate_empty_response() -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::Value::Null),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    }
}

/// Generate a set of responses with varying agreement levels
fn generate_responses_with_agreement(
    total: usize,
    agreement_count: usize,
    block_number: u64,
) -> Vec<(Arc<str>, JsonRpcResponse)> {
    let mut responses = Vec::with_capacity(total);

    // Agreement group - same response
    let agreed_response = generate_response(block_number, 10, true);
    for i in 0..agreement_count {
        responses.push((Arc::from(format!("upstream_{i}")), agreed_response.clone()));
    }

    // Disagreement group - different responses
    for i in agreement_count..total {
        responses.push((
            Arc::from(format!("upstream_{i}")),
            generate_response(block_number + i as u64, 10, true),
        ));
    }

    responses
}

/// Generate responses with mixed types (`NonEmpty`, `Empty`, `Error`)
fn generate_mixed_responses(total: usize) -> Vec<(Arc<str>, JsonRpcResponse)> {
    let mut responses = Vec::with_capacity(total);

    for i in 0..total {
        let response = match i % 3 {
            0 => generate_response(100 + i as u64, 5, true), // NonEmpty
            1 => generate_empty_response(),                  // Empty
            _ => generate_error_response(-32000, "execution reverted"), // Error
        };
        responses.push((Arc::from(format!("upstream_{i}")), response));
    }

    responses
}

/// Generate a large JSON value for size estimation benchmarks
fn generate_large_json(depth: usize, breadth: usize) -> serde_json::Value {
    if depth == 0 {
        return serde_json::json!("leaf_value");
    }

    let mut obj = serde_json::Map::new();
    for i in 0..breadth {
        obj.insert(format!("key_{i}"), generate_large_json(depth - 1, breadth));
    }
    serde_json::Value::Object(obj)
}

// ============================================================================
// Response Type Classification Benchmarks
// ============================================================================

fn bench_response_type_classification(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_type_classification");

    // NonEmpty response
    let non_empty = generate_response(100, 10, true);
    group.bench_function("non_empty", |b| {
        b.iter(|| ResponseType::from_response(black_box(&non_empty)));
    });

    // Empty response
    let empty = generate_empty_response();
    group.bench_function("empty", |b| b.iter(|| ResponseType::from_response(black_box(&empty))));

    // Error response
    let error = generate_error_response(-32000, "execution reverted");
    group.bench_function("error", |b| b.iter(|| ResponseType::from_response(black_box(&error))));

    // Large response
    let large = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(generate_large_json(4, 5)),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };
    group.bench_function("large_response", |b| {
        b.iter(|| ResponseType::from_response(black_box(&large)));
    });

    group.finish();
}

// ============================================================================
// Response Hashing Benchmarks
// ============================================================================

fn bench_response_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_hashing");

    // Small response
    let small = generate_response(100, 2, true);
    group.bench_function("small_response", |b| b.iter(|| hash_json_response(black_box(&small))));

    // Medium response (10 transactions)
    let medium = generate_response(100, 10, true);
    group.bench_function("medium_response", |b| b.iter(|| hash_json_response(black_box(&medium))));

    // Large response (100 transactions)
    let large = generate_response(100, 100, true);
    group.bench_function("large_response", |b| b.iter(|| hash_json_response(black_box(&large))));

    // Very large response (nested JSON)
    let very_large = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(generate_large_json(5, 6)),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };
    group.bench_function("very_large_nested", |b| {
        b.iter(|| hash_json_response(black_box(&very_large)));
    });

    group.finish();
}

// ============================================================================
// Filtered Hashing Benchmarks
// ============================================================================

fn bench_filtered_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("filtered_hashing");

    let response = generate_response(100, 20, true);

    // No filters
    let empty_filters: Vec<String> = vec![];
    group.bench_function("no_filters", |b| {
        b.iter(|| hash_json_response_filtered(black_box(&response), black_box(&empty_filters)));
    });

    // Single filter
    let single_filter = vec!["timestamp".to_string()];
    group.bench_function("single_filter", |b| {
        b.iter(|| hash_json_response_filtered(black_box(&response), black_box(&single_filter)));
    });

    // Multiple filters
    let multiple_filters =
        vec!["timestamp".to_string(), "requestsHash".to_string(), "transactions.*".to_string()];
    group.bench_function("multiple_filters", |b| {
        b.iter(|| hash_json_response_filtered(black_box(&response), black_box(&multiple_filters)));
    });

    // Wildcard filter
    let wildcard_filter = vec!["*.blockTimestamp".to_string()];
    group.bench_function("wildcard_filter", |b| {
        b.iter(|| hash_json_response_filtered(black_box(&response), black_box(&wildcard_filter)));
    });

    group.finish();
}

// ============================================================================
// Response Grouping Implementation (mirrors engine.rs for benchmarking)
// ============================================================================

/// Local implementation of `group_responses` (since engine.rs version is pub(crate))
/// This mirrors the exact logic from `ConsensusEngine::group_responses`
fn group_responses(
    responses: Vec<(Arc<str>, JsonRpcResponse)>,
    ignore_paths: Option<&[String]>,
) -> Vec<ResponseGroup> {
    let mut groups: HashMap<u64, ResponseGroup> = HashMap::with_capacity(responses.len());

    for (upstream_name, response) in responses {
        let hash = match ignore_paths {
            Some(paths) if !paths.is_empty() => hash_json_response_filtered(&response, paths),
            _ => hash_json_response(&response),
        };
        let response_type = ResponseType::from_response(&response);
        let response_size = response.result.as_ref().map_or(0, estimate_json_size);

        groups
            .entry(hash)
            .and_modify(|group| {
                group.upstreams.push(Arc::clone(&upstream_name));
                group.count += 1;
            })
            .or_insert_with(|| ResponseGroup {
                response_hash: hash,
                upstreams: vec![upstream_name],
                count: 1,
                response: response.clone(),
                response_type,
                response_size,
            });
    }

    groups.into_values().collect()
}

/// Local implementation of `responses_match` (mirrors engine.rs)
fn responses_match(resp1: &JsonRpcResponse, resp2: &JsonRpcResponse) -> bool {
    hash_json_response(resp1) == hash_json_response(resp2)
}

// ============================================================================
// Response Grouping Benchmarks (CRITICAL)
// ============================================================================

fn bench_response_grouping(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_grouping");

    // 3 responses, all agree
    group.bench_function("3_all_agree", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(3, 3, 100),
            |responses| group_responses(black_box(responses), None),
            BatchSize::SmallInput,
        )
    });

    // 3 responses, 2 agree
    group.bench_function("3_two_agree", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(3, 2, 100),
            |responses| group_responses(black_box(responses), None),
            BatchSize::SmallInput,
        )
    });

    // 3 responses, all disagree
    group.bench_function("3_all_disagree", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(3, 1, 100),
            |responses| group_responses(black_box(responses), None),
            BatchSize::SmallInput,
        )
    });

    // 5 responses, 3 agree
    group.bench_function("5_three_agree", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(5, 3, 100),
            |responses| group_responses(black_box(responses), None),
            BatchSize::SmallInput,
        )
    });

    // 10 responses, various agreement
    group.bench_function("10_mixed", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(10, 5, 100),
            |responses| group_responses(black_box(responses), None),
            BatchSize::SmallInput,
        )
    });

    // With ignore paths
    let ignore_paths = vec!["timestamp".to_string(), "requestsHash".to_string()];
    group.bench_function("3_with_ignore_paths", |b| {
        b.iter_batched(
            || generate_responses_with_agreement(3, 3, 100),
            |responses| group_responses(black_box(responses), Some(&ignore_paths)),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ============================================================================
// JSON Size Estimation Benchmarks
// ============================================================================

fn bench_json_size_estimation(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_size_estimation");

    // Small object
    let small = serde_json::json!({
        "key": "value",
        "number": 42
    });
    group.bench_function("small_object", |b| b.iter(|| estimate_json_size(black_box(&small))));

    // Block response
    let block = generate_response(100, 10, true);
    if let Some(result) = &block.result {
        group
            .bench_function("block_response", |b| b.iter(|| estimate_json_size(black_box(result))));
    }

    // Large array
    let large_array =
        serde_json::json!((0..1000).map(|i| format!("0x{i:064x}")).collect::<Vec<_>>());
    group.bench_function("large_array_1000", |b| {
        b.iter(|| estimate_json_size(black_box(&large_array)))
    });

    // Deeply nested
    let nested = generate_large_json(6, 4);
    group.bench_function("deeply_nested", |b| b.iter(|| estimate_json_size(black_box(&nested))));

    group.finish();
}

/// Re-implement `estimate_json_size` for benchmarking (since it's private in engine.rs)
fn estimate_json_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        serde_json::Value::Number(n) => n.to_string().len(),
        serde_json::Value::String(s) => s.len() + 2,
        serde_json::Value::Array(arr) => {
            2 + arr.len().saturating_sub(1) + arr.iter().map(estimate_json_size).sum::<usize>()
        }
        serde_json::Value::Object(obj) => {
            2 + obj.len().saturating_sub(1) +
                obj.iter().map(|(k, v)| k.len() + 3 + estimate_json_size(v)).sum::<usize>()
        }
    }
}

// ============================================================================
// Preference Sorting Benchmarks
// ============================================================================

fn bench_preference_sorting(c: &mut Criterion) {
    let mut group = c.benchmark_group("preference_sorting");

    // Generate response groups for sorting
    #[allow(clippy::items_after_statements)]
    fn generate_groups(count: usize) -> Vec<ResponseGroup> {
        (0..count)
            .map(|i| {
                let response = match i % 3 {
                    0 => generate_response(100 + i as u64, 5, true),
                    1 => generate_empty_response(),
                    _ => generate_error_response(-32000, "error"),
                };
                let response_type = ResponseType::from_response(&response);
                ResponseGroup {
                    response_hash: hash_json_response(&response),
                    upstreams: vec![Arc::from(format!("upstream_{i}"))],
                    count: (count - i),
                    response: response.clone(),
                    response_type,
                    response_size: estimate_json_size(
                        response.result.as_ref().unwrap_or(&serde_json::Value::Null),
                    ),
                }
            })
            .collect()
    }

    // No preferences
    let config_no_prefs = ConsensusConfig::default();
    group.bench_function("no_preferences_5_groups", |b| {
        b.iter_batched(
            || generate_groups(5),
            |mut groups| {
                apply_preferences(black_box(&mut groups), black_box(&config_no_prefs));
                groups
            },
            BatchSize::SmallInput,
        )
    });

    // With prefer_non_empty
    let config_prefer_non_empty = ConsensusConfig { prefer_non_empty: true, ..Default::default() };
    group.bench_function("prefer_non_empty_5_groups", |b| {
        b.iter_batched(
            || generate_groups(5),
            |mut groups| {
                apply_preferences(black_box(&mut groups), black_box(&config_prefer_non_empty));
                groups
            },
            BatchSize::SmallInput,
        )
    });

    // With all preferences
    let config_all_prefs = ConsensusConfig {
        prefer_non_empty: true,
        prefer_larger_responses: true,
        ..Default::default()
    };
    group.bench_function("all_preferences_5_groups", |b| {
        b.iter_batched(
            || generate_groups(5),
            |mut groups| {
                apply_preferences(black_box(&mut groups), black_box(&config_all_prefs));
                groups
            },
            BatchSize::SmallInput,
        )
    });

    // Larger set
    group.bench_function("all_preferences_10_groups", |b| {
        b.iter_batched(
            || generate_groups(10),
            |mut groups| {
                apply_preferences(black_box(&mut groups), black_box(&config_all_prefs));
                groups
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

/// Re-implement `apply_preferences` for benchmarking
fn apply_preferences(groups: &mut [ResponseGroup], config: &ConsensusConfig) {
    groups.sort_by(|a, b| {
        if config.prefer_non_empty {
            let type_cmp = a.response_type.priority_order().cmp(&b.response_type.priority_order());
            if type_cmp != std::cmp::Ordering::Equal {
                return type_cmp;
            }
        }

        let count_cmp = b.count.cmp(&a.count);
        if count_cmp != std::cmp::Ordering::Equal {
            return count_cmp;
        }

        if config.prefer_larger_responses {
            return b.response_size.cmp(&a.response_size);
        }

        std::cmp::Ordering::Equal
    });
}

// ============================================================================
// Consensus Engine Creation Benchmark
// ============================================================================

fn bench_consensus_engine_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_engine_creation");

    // Default config
    group.bench_function("default_config", |b| {
        b.iter(|| ConsensusEngine::new(black_box(ConsensusConfig::default())))
    });

    // With misbehavior tracking enabled
    let config_with_misbehavior =
        ConsensusConfig { dispute_threshold: Some(5), ..Default::default() };
    group.bench_function("with_misbehavior_tracking", |b| {
        b.iter(|| ConsensusEngine::new(black_box(config_with_misbehavior.clone())))
    });

    // Full config
    let mut full_config = ConsensusConfig {
        enabled: true,
        prefer_non_empty: true,
        prefer_larger_responses: true,
        ..Default::default()
    };
    full_config.dispute_threshold = Some(5);
    full_config.ignore_fields.insert(
        "eth_getBlockByNumber".to_string(),
        vec!["timestamp".to_string(), "requestsHash".to_string()],
    );
    group.bench_function("full_config", |b| {
        b.iter(|| ConsensusEngine::new(black_box(full_config.clone())))
    });

    group.finish();
}

// ============================================================================
// Config Operations Benchmarks
// ============================================================================

fn bench_config_operations(c: &mut Criterion) {
    let rt = create_runtime();
    let mut group = c.benchmark_group("config_operations");

    let engine = ConsensusEngine::new(ConsensusConfig::default());

    // is_enabled check
    group.bench_function("is_enabled", |b| b.iter(|| rt.block_on(engine.is_enabled())));

    // requires_consensus check
    group.bench_function("requires_consensus_match", |b| {
        b.iter(|| rt.block_on(engine.requires_consensus(black_box("eth_getBlockByNumber"))))
    });

    group.bench_function("requires_consensus_no_match", |b| {
        b.iter(|| rt.block_on(engine.requires_consensus(black_box("eth_call"))))
    });

    // get_config
    group.bench_function("get_config", |b| b.iter(|| rt.block_on(engine.get_config())));

    group.finish();
}

// ============================================================================
// Mixed Response Type Benchmarks
// ============================================================================

fn bench_mixed_response_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_response_processing");

    // Process mixed responses (NonEmpty, Empty, Error)
    for count in [3, 5, 10] {
        group.bench_with_input(
            BenchmarkId::new("group_mixed_types", count),
            &count,
            |b, &count| {
                b.iter_batched(
                    || generate_mixed_responses(count),
                    |responses| group_responses(black_box(responses), None),
                    BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

// ============================================================================
// Response Comparison Benchmark
// ============================================================================

fn bench_response_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_comparison");

    let resp1 = generate_response(100, 10, true);
    let resp2 = generate_response(100, 10, true);
    let resp3 = generate_response(101, 10, true);

    // Same responses
    group.bench_function("same_responses", |b| {
        b.iter(|| responses_match(black_box(&resp1), black_box(&resp2)))
    });

    // Different responses
    group.bench_function("different_responses", |b| {
        b.iter(|| responses_match(black_box(&resp1), black_box(&resp3)))
    });

    group.finish();
}

// ============================================================================
// Scaling Benchmarks
// ============================================================================

fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");
    // Increase sample size for better statistical stability (was 50)
    group.sample_size(100);

    // Response grouping with increasing response counts
    for count in [3, 5, 10, 20, 50] {
        group.bench_with_input(BenchmarkId::new("group_responses", count), &count, |b, &count| {
            b.iter_batched(
                || generate_responses_with_agreement(count, count / 2 + 1, 100),
                |responses| group_responses(black_box(responses), None),
                BatchSize::SmallInput,
            )
        });
    }

    // Hashing with increasing transaction counts
    for tx_count in [1, 10, 50, 100, 500] {
        let response = generate_response(100, tx_count, true);
        group.bench_with_input(
            BenchmarkId::new("hash_response_tx_count", tx_count),
            &response,
            |b, response| b.iter(|| hash_json_response(black_box(response))),
        );
    }

    group.finish();
}

// ============================================================================
// Main Benchmark Groups
// ============================================================================

// Use custom configuration for stable, reproducible benchmarks
criterion_group! {
    name = benches;
    config = criterion_config();
    targets =
        bench_response_type_classification,
        bench_response_hashing,
        bench_filtered_hashing,
        bench_response_grouping,
        bench_json_size_estimation,
        bench_preference_sorting,
        bench_consensus_engine_creation,
        bench_config_operations,
        bench_mixed_response_processing,
        bench_response_comparison,
        bench_scaling
}

criterion_main!(benches);
