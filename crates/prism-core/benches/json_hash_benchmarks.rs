//! JSON hashing benchmarks using Criterion.
//!
//! Compares performance of different JSON hashing approaches:
//! - Legacy approach: `serde_json::to_string` + `DefaultHasher`
//! - Optimized approach: Direct value traversal with `AHasher`
//!
//! The optimized approach provides:
//! - 2-3x faster hashing
//! - Zero allocations for serialization
//! - Better cache locality

#![allow(clippy::items_after_statements)]

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use prism_core::{
    types::{JsonRpcError, JsonRpcResponse},
    utils::json_hash::{hash_json_response, hash_json_value},
};
use serde_json::json;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    hint::black_box,
};

fn hash_response_legacy(response: &JsonRpcResponse) -> u64 {
    let mut hasher = DefaultHasher::new();

    if let Some(ref result) = response.result {
        if let Ok(json_str) = serde_json::to_string(result) {
            json_str.hash(&mut hasher);
        }
    }

    if let Some(ref error) = response.error {
        error.code.hash(&mut hasher);
        error.message.hash(&mut hasher);
    }

    hasher.finish()
}

/// Benchmark basic JSON value hashing
fn bench_json_value_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_value_hashing");

    let simple_obj = json!({"number": "0x123", "hash": "0xabc"});
    group.bench_function("simple_object_optimized", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&simple_obj), &mut hasher);
            hasher.finish()
        });
    });

    group.bench_function("simple_object_legacy", |b| {
        b.iter(|| {
            let mut hasher = DefaultHasher::new();
            if let Ok(json_str) = serde_json::to_string(black_box(&simple_obj)) {
                json_str.hash(&mut hasher);
            }
            hasher.finish()
        });
    });

    let array = json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    group.bench_function("array_optimized", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&array), &mut hasher);
            hasher.finish()
        });
    });

    group.bench_function("array_legacy", |b| {
        b.iter(|| {
            let mut hasher = DefaultHasher::new();
            if let Ok(json_str) = serde_json::to_string(black_box(&array)) {
                json_str.hash(&mut hasher);
            }
            hasher.finish()
        });
    });

    group.finish();
}

/// Benchmark Ethereum block response hashing
fn bench_ethereum_responses(c: &mut Criterion) {
    let mut group = c.benchmark_group("ethereum_responses");

    let block_response = json!({
        "number": "0x1b4",
        "hash": "0xe670ec64341771606e55d6b4ca35a1a6b75ee3d5145a99d05921026d1527331",
        "parentHash": "0x9646252be9520f6e71339a8df9c55e4d7619deeb018d2a3f2d21fc165dde5eb5",
        "nonce": "0xe04d296d2460cfb8472af2c5fd05b5a214109c25688d3704aed5484f9a7792f2",
        "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
        "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "transactionsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
        "stateRoot": "0xd5855eb08b3387c0af375e9cdb6acfc05eb8f519e419b874b6ff2ffda7ed1dff",
        "receiptsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
        "miner": "0x4e65fda2159562a496f9f3522f89122a3088497a",
        "difficulty": "0x027f07",
        "totalDifficulty": "0x027f07",
        "extraData": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "size": "0x027f07",
        "gasLimit": "0x9f759",
        "gasUsed": "0x9f759",
        "timestamp": "0x54e34e8e",
        "transactions": [],
        "uncles": []
    });

    group.throughput(Throughput::Elements(1));

    group.bench_function("block_optimized", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&block_response), &mut hasher);
            hasher.finish()
        });
    });

    group.bench_function("block_legacy", |b| {
        b.iter(|| {
            let mut hasher = DefaultHasher::new();
            if let Ok(json_str) = serde_json::to_string(black_box(&block_response)) {
                json_str.hash(&mut hasher);
            }
            hasher.finish()
        });
    });

    group.finish();
}

/// Benchmark `JsonRpcResponse` hashing (consensus use case)
fn bench_response_hashing(c: &mut Criterion) {
    use std::borrow::Cow;

    let mut group = c.benchmark_group("response_hashing");

    let success_response = JsonRpcResponse {
        jsonrpc: Cow::Borrowed("2.0"),
        result: Some(json!({
            "blockNumber": "0x5daf3b",
            "blockHash": "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0x0987654321098765432109876543210987654321",
            "value": "0xde0b6b3a7640000",
            "gas": "0x5208",
            "gasPrice": "0x4a817c800"
        })),
        error: None,
        id: std::sync::Arc::new(json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    group.throughput(Throughput::Elements(1));

    group.bench_function("success_response_optimized", |b| {
        b.iter(|| hash_json_response(black_box(&success_response)));
    });

    group.bench_function("success_response_legacy", |b| {
        b.iter(|| hash_response_legacy(black_box(&success_response)));
    });

    let error_response = JsonRpcResponse {
        jsonrpc: Cow::Borrowed("2.0"),
        result: None,
        error: Some(JsonRpcError {
            code: -32000,
            message: "insufficient funds for transfer".to_string(),
            data: Some(json!({"required": "0x1000", "available": "0x500"})),
        }),
        id: std::sync::Arc::new(json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    group.bench_function("error_response_optimized", |b| {
        b.iter(|| hash_json_response(black_box(&error_response)));
    });

    group.bench_function("error_response_legacy", |b| {
        b.iter(|| hash_response_legacy(black_box(&error_response)));
    });

    group.finish();
}

/// Benchmark consensus scenario - hashing multiple responses
fn bench_consensus_workload(c: &mut Criterion) {
    use std::borrow::Cow;

    let mut group = c.benchmark_group("consensus_workload");

    let responses: Vec<JsonRpcResponse> = (0..3)
        .map(|i| JsonRpcResponse {
            jsonrpc: Cow::Borrowed("2.0"),
            result: Some(json!({
                "number": "0x1234",
                "hash": "0xabcd",
                "transactions": [
                    {"hash": "0x1111", "from": "0xaaaa", "to": "0xbbbb"},
                    {"hash": "0x2222", "from": "0xcccc", "to": "0xdddd"},
                ]
            })),
            error: None,
            id: std::sync::Arc::new(json!(i)),
            cache_status: None,
            serving_upstream: None,
        })
        .collect();

    group.throughput(Throughput::Elements(3));

    group.bench_function("consensus_3_responses_optimized", |b| {
        b.iter(|| {
            let mut hashes = Vec::with_capacity(3);
            for response in black_box(&responses) {
                hashes.push(hash_json_response(response));
            }
            hashes
        });
    });

    group.bench_function("consensus_3_responses_legacy", |b| {
        b.iter(|| {
            let mut hashes = Vec::with_capacity(3);
            for response in black_box(&responses) {
                hashes.push(hash_response_legacy(response));
            }
            hashes
        });
    });

    group.finish();
}

/// Benchmark object key order independence
fn bench_object_ordering(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_ordering");

    let obj1 = json!({"a": 1, "b": 2, "c": 3, "d": 4, "e": 5});
    let obj2 = json!({"e": 5, "d": 4, "c": 3, "b": 2, "a": 1});

    group.bench_function("ordered_object", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&obj1), &mut hasher);
            hasher.finish()
        });
    });

    group.bench_function("reverse_ordered_object", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&obj2), &mut hasher);
            hasher.finish()
        });
    });

    group.finish();
}

/// Benchmark nested structures
fn bench_nested_structures(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_structures");

    let deeply_nested = json!({
        "level1": {
            "level2": {
                "level3": {
                    "level4": {
                        "level5": {
                            "data": "deep value",
                            "number": 42
                        }
                    }
                }
            }
        }
    });

    group.bench_function("deeply_nested_optimized", |b| {
        b.iter(|| {
            let mut hasher = ahash::AHasher::default();
            hash_json_value(black_box(&deeply_nested), &mut hasher);
            hasher.finish()
        });
    });

    group.bench_function("deeply_nested_legacy", |b| {
        b.iter(|| {
            let mut hasher = DefaultHasher::new();
            if let Ok(json_str) = serde_json::to_string(black_box(&deeply_nested)) {
                json_str.hash(&mut hasher);
            }
            hasher.finish()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_json_value_hashing,
    bench_ethereum_responses,
    bench_response_hashing,
    bench_consensus_workload,
    bench_object_ordering,
    bench_nested_structures,
);

criterion_main!(benches);
