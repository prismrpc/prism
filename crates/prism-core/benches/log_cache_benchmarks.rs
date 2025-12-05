//! `LogCache` performance benchmarks using Criterion.
//!
//! These benchmarks establish a performance baseline before refactoring `log_cache.rs`
//! (Issue #12 from code review). They measure:
//!
//! - Insert operations (single and bulk)
//! - Query operations with varying filter complexity
//! - Coverage tracking (mark/check ranges)
//! - Invalidation operations (reorg handling)
//! - Concurrent access patterns
//! - Memory efficiency (bitmap operations)
//!
//! Run with: `cargo bench --bench log_cache_benchmarks`
//!
//! After refactoring, compare results to ensure no performance regression.

#![allow(clippy::items_after_statements)]
#![allow(clippy::expect_used)] // Acceptable in benchmark code
#![allow(clippy::semicolon_if_nothing_returned)] // Benchmark closures don't need semicolons

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use futures::future::join_all;
use prism_core::cache::{
    log_cache::{LogCache, LogCacheConfig},
    types::{LogFilter, LogId, LogRecord},
};
use std::{hint::black_box, sync::Arc, time::Duration};

/// Create a shared Tokio runtime for benchmarks
/// This avoids runtime creation overhead in benchmark iterations
fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Create a multi-threaded runtime for concurrent benchmarks
fn create_multi_thread_runtime(num_threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_threads)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Configure Criterion for stable, reproducible benchmarks
fn criterion_config() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(15))
        .warm_up_time(Duration::from_secs(3))
        .sample_size(100)
        .noise_threshold(0.02)
        .confidence_level(0.95)
}

/// Generate a test log record with realistic data
#[allow(clippy::cast_possible_truncation)]
fn generate_log_record(block_number: u64, log_index: u32, address_seed: u8) -> LogRecord {
    let mut address = [0u8; 20];
    address[0] = address_seed;
    address[19] = (block_number % 256) as u8;

    let mut topic0 = [0u8; 32];
    topic0[0] = 0xde;
    topic0[1] = 0xad;
    topic0[31] = address_seed;

    let mut topic1 = [0u8; 32];
    topic1[0] = (log_index % 256) as u8;

    let mut tx_hash = [0u8; 32];
    tx_hash[0..8].copy_from_slice(&block_number.to_le_bytes());
    tx_hash[8..12].copy_from_slice(&log_index.to_le_bytes());

    let mut block_hash = [0u8; 32];
    block_hash[0..8].copy_from_slice(&block_number.to_le_bytes());

    LogRecord::new(
        address,
        [Some(topic0), Some(topic1), None, None],
        vec![0u8; 64], // Typical log data size
        tx_hash,
        block_hash,
        log_index,
        false,
    )
}

/// Generate batch of logs for a block range
#[allow(clippy::cast_possible_truncation)]
fn generate_logs_batch(
    from_block: u64,
    to_block: u64,
    logs_per_block: u32,
) -> Vec<(LogId, LogRecord)> {
    let mut logs =
        Vec::with_capacity(((to_block - from_block + 1) * u64::from(logs_per_block)) as usize);
    for block in from_block..=to_block {
        for log_idx in 0..logs_per_block {
            let log_id = LogId::new(block, log_idx);
            let record = generate_log_record(block, log_idx, (block % 10) as u8);
            logs.push((log_id, record));
        }
    }
    logs
}

/// Create a pre-populated `LogCache` for query benchmarks
fn create_populated_cache(num_blocks: u64, logs_per_block: u32) -> LogCache {
    let config = LogCacheConfig::default();
    let cache = LogCache::new(&config).expect("valid config");

    // Use tokio runtime for async operations
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let logs = generate_logs_batch(1, num_blocks, logs_per_block);
    rt.block_on(async {
        cache.insert_logs_bulk_no_stats(logs).await;
    });

    cache
}

// ============================================================================
// INSERT BENCHMARKS
// ============================================================================

/// Benchmark single log insertion
fn bench_insert_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_insert_single");
    let rt = create_runtime();

    group.throughput(Throughput::Elements(1));

    group.bench_function("insert_log", |b| {
        let mut counter = 0u64;
        b.iter_batched(
            || {
                counter += 1;
                let config = LogCacheConfig::default();
                let cache = LogCache::new(&config).expect("valid config");
                let log_id = LogId::new(counter, 0);
                let record = generate_log_record(counter, 0, 1);
                (cache, log_id, record)
            },
            |(cache, log_id, record)| {
                rt.block_on(cache.insert_log(black_box(log_id), black_box(record)));
                cache
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark bulk log insertion with varying batch sizes
fn bench_insert_bulk(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_insert_bulk");
    let rt = create_runtime();

    for batch_size in &[100usize, 1000, 10000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // Benchmark without stats update (pure insert performance)
        group.bench_with_input(
            BenchmarkId::new("insert_logs_bulk_no_stats", batch_size),
            batch_size,
            |b, &size| {
                let logs_per_block = 10u32;
                #[allow(clippy::cast_possible_truncation)]
                let num_blocks = (size as u64) / u64::from(logs_per_block);

                b.iter_batched(
                    || {
                        let config = LogCacheConfig::default();
                        let cache = LogCache::new(&config).expect("valid config");
                        let logs = generate_logs_batch(1, num_blocks, logs_per_block);
                        (cache, logs)
                    },
                    |(cache, logs)| {
                        rt.block_on(cache.insert_logs_bulk_no_stats(black_box(logs)));
                        cache
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Benchmark with stats update (includes lock contention overhead)
        group.bench_with_input(
            BenchmarkId::new("insert_logs_bulk_with_stats", batch_size),
            batch_size,
            |b, &size| {
                let logs_per_block = 10u32;
                #[allow(clippy::cast_possible_truncation)]
                let num_blocks = (size as u64) / u64::from(logs_per_block);

                b.iter_batched(
                    || {
                        let config = LogCacheConfig::default();
                        let cache = LogCache::new(&config).expect("valid config");
                        let logs = generate_logs_batch(1, num_blocks, logs_per_block);
                        (cache, logs)
                    },
                    |(cache, logs)| {
                        rt.block_on(cache.insert_logs_bulk(black_box(logs)));
                        cache
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ============================================================================
// QUERY BENCHMARKS
// ============================================================================

/// Benchmark query operations with varying filter complexity
fn bench_query_logs(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_query");
    // Increase sample size for statistical stability
    group.sample_size(100);
    let rt = create_runtime();
    let current_tip = 1000u64;

    // Query with address filter only - fresh cache per iteration for isolation
    group.bench_function("query_address_only", |b| {
        let mut address = [0u8; 20];
        address[0] = 1; // Match address_seed = 1

        b.iter_batched(
            || create_populated_cache(1000, 10),
            |cache| {
                let filter = LogFilter::new(1, 100).with_address(address);
                rt.block_on(cache.query_logs(black_box(&filter), current_tip))
            },
            BatchSize::LargeInput,
        );
    });

    // Query with address + topic filter
    group.bench_function("query_address_and_topic", |b| {
        let mut address = [0u8; 20];
        address[0] = 1;

        let mut topic0 = [0u8; 32];
        topic0[0] = 0xde;
        topic0[1] = 0xad;
        topic0[31] = 1;

        b.iter_batched(
            || create_populated_cache(1000, 10),
            |cache| {
                let filter = LogFilter::new(1, 100).with_address(address).with_topic(0, topic0);
                rt.block_on(cache.query_logs(black_box(&filter), current_tip))
            },
            BatchSize::LargeInput,
        );
    });

    // Query with wide block range
    group.bench_function("query_wide_range", |b| {
        let mut address = [0u8; 20];
        address[0] = 1;

        b.iter_batched(
            || create_populated_cache(1000, 10),
            |cache| {
                let filter = LogFilter::new(1, 500).with_address(address);
                rt.block_on(cache.query_logs(black_box(&filter), current_tip))
            },
            BatchSize::LargeInput,
        );
    });

    // Query with no filters (chunk scan)
    group.bench_function("query_no_filters", |b| {
        b.iter_batched(
            || create_populated_cache(1000, 10),
            |cache| {
                let filter = LogFilter::new(1, 50);
                rt.block_on(cache.query_logs(black_box(&filter), current_tip))
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

/// Benchmark log retrieval by IDs
fn bench_get_log_records(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_retrieval");

    for batch_size in &[10usize, 100, 1000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("get_log_records", batch_size),
            batch_size,
            |b, &size| {
                // Create log IDs to fetch
                let log_ids: Vec<LogId> =
                    (0..size as u64).map(|i| LogId::new(i % 1000 + 1, (i % 10) as u32)).collect();

                b.iter_batched(
                    || create_populated_cache(1000, 10),
                    |cache| cache.get_log_records(black_box(&log_ids)),
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

// ============================================================================
// COVERAGE TRACKING BENCHMARKS
// ============================================================================

/// Benchmark coverage tracking operations
fn bench_coverage_tracking(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_coverage");

    // Benchmark marking ranges as covered
    group.bench_function("mark_range_covered_small", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                LogCache::new(&config).expect("valid config")
            },
            |cache| {
                cache.mark_range_covered(black_box(1), black_box(100));
                cache
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("mark_range_covered_large", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                LogCache::new(&config).expect("valid config")
            },
            |cache| {
                cache.mark_range_covered(black_box(1), black_box(10000));
                cache
            },
            BatchSize::SmallInput,
        );
    });

    // Benchmark checking if range is covered
    let config = LogCacheConfig::default();
    let cache_with_coverage = LogCache::new(&config).expect("valid config");
    cache_with_coverage.mark_range_covered(1, 5000);

    group.bench_function("is_range_covered_hit", |b| {
        b.iter(|| cache_with_coverage.is_range_covered(black_box(100), black_box(200)));
    });

    group.bench_function("is_range_covered_partial", |b| {
        b.iter(|| cache_with_coverage.is_range_covered(black_box(4900), black_box(5100)));
    });

    group.bench_function("is_block_covered", |b| {
        b.iter(|| cache_with_coverage.is_block_covered(black_box(500)));
    });

    group.finish();
}

// ============================================================================
// INVALIDATION BENCHMARKS
// ============================================================================

/// Benchmark block invalidation (critical for reorg handling)
fn bench_invalidation(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_invalidation");
    // Increase sample size for stability
    group.sample_size(100);
    let rt = create_runtime();

    // Benchmark invalidating a single block
    for logs_per_block in &[10u32, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("invalidate_block", logs_per_block),
            logs_per_block,
            |b, &lpb| {
                b.iter_batched(
                    || {
                        // Create cache with some data
                        let config = LogCacheConfig::default();
                        let cache = LogCache::new(&config).expect("valid config");
                        let logs = generate_logs_batch(1, 100, lpb);
                        rt.block_on(cache.insert_logs_bulk_no_stats(logs));
                        cache
                    },
                    |cache| {
                        // Invalidate block 50 (middle of range)
                        cache.invalidate_block(black_box(50));
                        cache
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    // Benchmark invalidating block with no logs (fast path)
    group.bench_function("invalidate_block_empty", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                let cache = LogCache::new(&config).expect("valid config");
                let logs = generate_logs_batch(1, 50, 10);
                rt.block_on(cache.insert_logs_bulk_no_stats(logs));
                cache
            },
            |cache| {
                // Invalidate block 100 (not in cache)
                cache.invalidate_block(black_box(100));
                cache
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// CONCURRENT ACCESS BENCHMARKS
// ============================================================================

/// Benchmark concurrent read/write operations
#[allow(clippy::too_many_lines)]
fn bench_concurrent_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_concurrent");
    // Increase sample size for better statistical stability in concurrent scenarios
    group.sample_size(100);

    const NUM_THREADS: usize = 4;
    const OPS_PER_THREAD: usize = 100;

    let rt = create_multi_thread_runtime(NUM_THREADS);

    // Warm up the runtime before benchmarks to reduce variance
    rt.block_on(async {
        for _ in 0..5 {
            let cache = Arc::new(LogCache::new(&LogCacheConfig::default()).expect("valid config"));
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let cache = cache.clone();
                    tokio::spawn(async move {
                        cache.insert_log(LogId::new(1, 0), generate_log_record(1, 0, 0)).await;
                    })
                })
                .collect();
            join_all(handles).await;
        }
    });

    // Concurrent inserts with barrier synchronization
    group.throughput(Throughput::Elements((NUM_THREADS * OPS_PER_THREAD) as u64));

    group.bench_function("concurrent_inserts", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                Arc::new(LogCache::new(&config).expect("valid config"))
            },
            |cache| {
                rt.block_on(async {
                    // Use barrier for synchronized start - reduces timing variance
                    let barrier = Arc::new(tokio::sync::Barrier::new(NUM_THREADS));

                    let handles: Vec<_> = (0..NUM_THREADS)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            let barrier = barrier.clone();
                            tokio::spawn(async move {
                                // Wait for all threads to be ready
                                barrier.wait().await;

                                for i in 0..OPS_PER_THREAD {
                                    let block = (thread_id * OPS_PER_THREAD + i) as u64;
                                    let log_id = LogId::new(block, 0);
                                    #[allow(clippy::cast_possible_truncation)]
                                    let record = generate_log_record(block, 0, thread_id as u8);
                                    cache.insert_log(log_id, record).await;
                                }
                            })
                        })
                        .collect();

                    // Use join_all for proper concurrent completion
                    join_all(handles).await;
                });
                cache
            },
            BatchSize::SmallInput,
        );
    });

    // Concurrent reads (queries) with barrier synchronization
    group.bench_function("concurrent_queries", |b| {
        b.iter_batched(
            || Arc::new(create_populated_cache(500, 10)),
            |cache| {
                rt.block_on(async {
                    let barrier = Arc::new(tokio::sync::Barrier::new(NUM_THREADS));

                    let handles: Vec<_> = (0..NUM_THREADS)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            let barrier = barrier.clone();
                            tokio::spawn(async move {
                                barrier.wait().await;

                                #[allow(clippy::cast_possible_truncation)]
                                for i in 0..OPS_PER_THREAD {
                                    let from = ((thread_id * 100 + i) % 400) as u64 + 1;
                                    let to = from + 50;
                                    let mut address = [0u8; 20];
                                    address[0] = (thread_id % 10) as u8;
                                    let filter = LogFilter::new(from, to).with_address(address);
                                    let _ = cache.query_logs(&filter, 500).await;
                                }
                            })
                        })
                        .collect();

                    join_all(handles).await;
                });
                cache
            },
            BatchSize::SmallInput,
        );
    });

    // Mixed read/write workload with barrier synchronization
    group.bench_function("concurrent_mixed", |b| {
        b.iter_batched(
            || Arc::new(create_populated_cache(200, 5)),
            |cache| {
                rt.block_on(async {
                    let barrier = Arc::new(tokio::sync::Barrier::new(NUM_THREADS));

                    let handles: Vec<_> = (0..NUM_THREADS)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            let barrier = barrier.clone();
                            let is_writer = thread_id % 2 == 0;
                            tokio::spawn(async move {
                                barrier.wait().await;

                                for i in 0..OPS_PER_THREAD {
                                    if is_writer {
                                        let block = 300 + (thread_id * OPS_PER_THREAD + i) as u64;
                                        let log_id = LogId::new(block, 0);
                                        #[allow(clippy::cast_possible_truncation)]
                                        #[allow(clippy::cast_possible_truncation)]
                                        let record = generate_log_record(block, 0, thread_id as u8);
                                        cache.insert_log(log_id, record).await;
                                    } else {
                                        let from = (i % 100) as u64 + 1;
                                        let filter = LogFilter::new(from, from + 20);
                                        let _ = cache.query_logs(&filter, 500).await;
                                    }
                                }
                            })
                        })
                        .collect();

                    join_all(handles).await;
                });
                cache
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// BITMAP OPERATIONS BENCHMARKS
// ============================================================================

/// Benchmark bitmap-specific operations (Arc<RoaringBitmap> patterns)
fn bench_bitmap_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_bitmap");
    let rt = create_runtime();

    // Benchmark with high cardinality addresses (many unique addresses)
    group.bench_function("insert_high_cardinality_addresses", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                LogCache::new(&config).expect("valid config")
            },
            |cache| {
                rt.block_on(async {
                    // 100 logs with 100 different addresses
                    for i in 0..100u64 {
                        let mut address = [0u8; 20];
                        address[0..8].copy_from_slice(&i.to_le_bytes());
                        let log_id = LogId::new(i + 1, 0);
                        let record = LogRecord::new(
                            address,
                            [None, None, None, None],
                            vec![],
                            [0u8; 32],
                            [0u8; 32],
                            0,
                            false,
                        );
                        cache.insert_log(log_id, record).await;
                    }
                });
                cache
            },
            BatchSize::SmallInput,
        );
    });

    // Benchmark with low cardinality addresses (few unique addresses, many logs each)
    group.bench_function("insert_low_cardinality_addresses", |b| {
        b.iter_batched(
            || {
                let config = LogCacheConfig::default();
                LogCache::new(&config).expect("valid config")
            },
            |cache| {
                rt.block_on(async {
                    // 100 logs with only 5 different addresses
                    for i in 0..100u64 {
                        let mut address = [0u8; 20];
                        address[0] = (i % 5) as u8; // Only 5 unique addresses
                        let log_id = LogId::new(i + 1, 0);
                        let record = LogRecord::new(
                            address,
                            [None, None, None, None],
                            vec![],
                            [0u8; 32],
                            [0u8; 32],
                            0,
                            false,
                        );
                        cache.insert_log(log_id, record).await;
                    }
                });
                cache
            },
            BatchSize::SmallInput,
        );
    });

    // Benchmark query with bitmap intersection - fresh cache per iteration
    group.bench_function("bitmap_intersection_query", |b| {
        let mut topic0 = [0u8; 32];
        topic0[0] = 0xde;
        topic0[1] = 0xad;
        topic0[31] = 1;

        b.iter_batched(
            || create_populated_cache(500, 20),
            |cache| {
                let filter = LogFilter::new(1, 200).with_topic(0, topic0);
                rt.block_on(cache.query_logs(black_box(&filter), 500))
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ============================================================================
// MEMORY EFFICIENCY BENCHMARKS
// ============================================================================

/// Benchmark memory-related operations
fn bench_memory_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_cache_memory");
    // Increase sample size for better stability (was 20)
    group.sample_size(50);
    let rt = create_runtime();

    // Benchmark stats computation (includes bitmap memory calculation)
    // Use fresh cache per iteration for isolation
    group.bench_function("update_stats", |b| {
        b.iter_batched(
            || create_populated_cache(1000, 10),
            |cache| {
                rt.block_on(async {
                    cache.force_update_stats().await;
                    cache.get_stats().await
                })
            },
            BatchSize::LargeInput,
        );
    });

    // Benchmark clear_cache (memory cleanup)
    group.bench_function("clear_cache", |b| {
        b.iter_batched(
            || create_populated_cache(500, 10),
            |cache| {
                rt.block_on(cache.clear_cache());
                cache
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ============================================================================
// CRITERION GROUPS
// ============================================================================

// Use custom configuration for all benchmark groups to ensure stability
criterion_group! {
    name = insert_benches;
    config = criterion_config();
    targets = bench_insert_single, bench_insert_bulk
}

criterion_group! {
    name = query_benches;
    config = criterion_config();
    targets = bench_query_logs, bench_get_log_records
}

criterion_group! {
    name = coverage_benches;
    config = criterion_config();
    targets = bench_coverage_tracking
}

criterion_group! {
    name = invalidation_benches;
    config = criterion_config();
    targets = bench_invalidation
}

criterion_group! {
    name = concurrent_benches;
    config = criterion_config();
    targets = bench_concurrent_access
}

criterion_group! {
    name = bitmap_benches;
    config = criterion_config();
    targets = bench_bitmap_operations
}

criterion_group! {
    name = memory_benches;
    config = criterion_config();
    targets = bench_memory_efficiency
}

criterion_main!(
    insert_benches,
    query_benches,
    coverage_benches,
    invalidation_benches,
    concurrent_benches,
    bitmap_benches,
    memory_benches,
);
