#![allow(clippy::items_after_statements)]
#![allow(clippy::expect_used)] // Acceptable in benchmark code
#![allow(clippy::semicolon_if_nothing_returned)] // Benchmark closures don't need semicolons

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use futures::future::join_all;
use prism_core::{
    cache::{
        types::{BlockBody, BlockHeader, LogFilter, LogId, LogRecord, ReceiptRecord},
        CacheManager, CacheManagerConfig,
    },
    chain::ChainState,
};
use std::{hint::black_box, sync::Arc, time::Duration};

/// Create a shared Tokio runtime for benchmarks
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
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(3))
        .sample_size(100)
        .noise_threshold(0.02)
        .confidence_level(0.95)
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// Generate a test block header
fn generate_block_header(block_number: u64) -> BlockHeader {
    let mut hash = [0u8; 32];
    hash[0..8].copy_from_slice(&block_number.to_le_bytes());

    let mut parent_hash = [0u8; 32];
    if block_number > 0 {
        parent_hash[0..8].copy_from_slice(&(block_number - 1).to_le_bytes());
    }

    BlockHeader {
        number: block_number,
        hash,
        parent_hash,
        timestamp: 1_700_000_000 + block_number,
        gas_used: 15_000_000,
        gas_limit: 30_000_000,
        miner: [0u8; 20],
        extra_data: std::sync::Arc::new(vec![]),
        logs_bloom: std::sync::Arc::new(vec![0u8; 256]),
        transactions_root: [1u8; 32],
        state_root: [2u8; 32],
        receipts_root: [3u8; 32],
    }
}

/// Generate a test block body
fn generate_block_body(block_number: u64, tx_count: usize) -> BlockBody {
    let mut hash = [0u8; 32];
    hash[0..8].copy_from_slice(&block_number.to_le_bytes());

    let transactions: Vec<[u8; 32]> = (0..tx_count)
        .map(|i| {
            let mut tx_hash = [0u8; 32];
            tx_hash[0..8].copy_from_slice(&block_number.to_le_bytes());
            tx_hash[8..16].copy_from_slice(&(i as u64).to_le_bytes());
            tx_hash
        })
        .collect();

    BlockBody { hash, transactions }
}

/// Generate a test log record with a block hash that matches the header
fn generate_log_record(block_number: u64, log_index: u32) -> LogRecord {
    let mut address = [0u8; 20];
    address[0] = (block_number % 10) as u8;

    let mut block_hash = [0u8; 32];
    block_hash[0..8].copy_from_slice(&block_number.to_le_bytes());

    let mut tx_hash = [0u8; 32];
    tx_hash[0..8].copy_from_slice(&block_number.to_le_bytes());
    tx_hash[8..12].copy_from_slice(&log_index.to_le_bytes());

    let mut topic0 = [0u8; 32];
    topic0[0] = 0xde;
    topic0[1] = 0xad;

    LogRecord::new(
        address,
        [Some(topic0), None, None, None],
        vec![0u8; 64],
        tx_hash,
        block_hash,
        log_index,
        false,
    )
}

/// Generate batch of logs for a block range
fn generate_logs_batch(
    from_block: u64,
    to_block: u64,
    logs_per_block: u32,
) -> Vec<(LogId, LogRecord)> {
    #[allow(clippy::cast_possible_truncation)]
    let mut logs =
        Vec::with_capacity(((to_block - from_block + 1) * u64::from(logs_per_block)) as usize);
    for block in from_block..=to_block {
        for log_idx in 0..logs_per_block {
            let log_id = LogId::new(block, log_idx);
            let record = generate_log_record(block, log_idx);
            logs.push((log_id, record));
        }
    }
    logs
}

/// Generate test receipt
fn generate_receipt(block_number: u64, tx_index: u32) -> ReceiptRecord {
    let mut tx_hash = [0u8; 32];
    tx_hash[0..8].copy_from_slice(&block_number.to_le_bytes());
    tx_hash[8..12].copy_from_slice(&tx_index.to_le_bytes());

    let mut block_hash = [0u8; 32];
    block_hash[0..8].copy_from_slice(&block_number.to_le_bytes());

    ReceiptRecord {
        transaction_hash: tx_hash,
        block_hash,
        block_number,
        transaction_index: tx_index,
        from: [0u8; 20],
        to: Some([1u8; 20]),
        cumulative_gas_used: 21000 * (u64::from(tx_index) + 1),
        gas_used: 21000,
        contract_address: None,
        logs: vec![LogId::new(block_number, tx_index)],
        status: 1,
        logs_bloom: vec![0u8; 256],
        effective_gas_price: Some(20_000_000_000),
        tx_type: Some(2),
        blob_gas_price: None,
    }
}

/// Async helper to populate a `CacheManager`
async fn populate_cache_manager(
    cache: &CacheManager,
    chain_state: &ChainState,
    num_blocks: u64,
    logs_per_block: u32,
) {
    // Insert blocks
    for block_num in 1..=num_blocks {
        let header = generate_block_header(block_num);
        let body = generate_block_body(block_num, 10);
        cache.insert_header(header).await;
        cache.insert_body(body).await;
    }

    // Insert logs
    let logs = generate_logs_batch(1, num_blocks, logs_per_block);
    cache.insert_logs_bulk_no_stats(logs).await;

    // Update chain state to simulate realistic conditions
    chain_state.update_tip(num_blocks, [0u8; 32]).await;
    chain_state.update_finalized(num_blocks.saturating_sub(64)).await;
}

/// Create a pre-populated `CacheManager` for benchmarks
fn create_populated_cache_manager(num_blocks: u64, logs_per_block: u32) -> Arc<CacheManager> {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    let cache = Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid config"));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(populate_cache_manager(&cache, &chain_state, num_blocks, logs_per_block));

    cache
}

/// Create a pre-populated `CacheManager` for use within an existing async context
fn create_populated_cache_manager_sync(
    _num_blocks: u64,
    _logs_per_block: u32,
) -> (Arc<CacheManager>, Arc<ChainState>) {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    let cache = Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid config"));
    (cache, chain_state)
}

// ============================================================================
// LOG VALIDATION BENCHMARKS (CRITICAL - Deferred Cleanup Pattern)
// ============================================================================

/// Benchmark log validation with deferred cleanup pattern.
/// This is the CRITICAL performance pattern that must be preserved.
fn bench_log_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_log_validation");
    // Increase sample size for stability (was 50)
    group.sample_size(100);
    let rt = create_runtime();

    // Test with varying batch sizes to verify deferred cleanup benefit
    for batch_size in &[100usize, 1000, 10000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("insert_logs_validated", batch_size),
            batch_size,
            |b, &size| {
                let logs_per_block = 10u32;
                #[allow(clippy::cast_possible_truncation)]
                let num_blocks = (size as u64) / u64::from(logs_per_block);

                b.iter_batched(
                    || {
                        // Create cache with block headers (needed for validation)
                        let cache = create_populated_cache_manager(num_blocks + 50, 0);
                        // Generate logs that will pass validation
                        let logs = generate_logs_batch(1, num_blocks, logs_per_block);
                        (cache, logs)
                    },
                    |(cache, logs)| {
                        rt.block_on(async { cache.insert_logs_validated(black_box(logs)).await })
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Also benchmark without validation for comparison
        group.bench_with_input(
            BenchmarkId::new("insert_logs_bulk", batch_size),
            batch_size,
            |b, &size| {
                let logs_per_block = 10u32;
                #[allow(clippy::cast_possible_truncation)]
                let num_blocks = (size as u64) / u64::from(logs_per_block);

                b.iter_batched(
                    || {
                        let config = CacheManagerConfig::default();
                        let chain_state = Arc::new(ChainState::new());
                        let cache = Arc::new(
                            CacheManager::new(&config, chain_state).expect("valid config"),
                        );
                        let logs = generate_logs_batch(1, num_blocks, logs_per_block);
                        (cache, logs)
                    },
                    |(cache, logs)| {
                        rt.block_on(async {
                            cache.insert_logs_bulk(black_box(logs)).await;
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ============================================================================
// BLOCK OPERATIONS BENCHMARKS
// ============================================================================

/// Benchmark block operations through `CacheManager`
fn bench_block_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_blocks");
    let rt = create_runtime();

    // Pre-populate cache with blocks
    let cache = create_populated_cache_manager(1000, 5);

    // Get block by number (hot path)
    group.bench_function("get_block_by_number_hit", |b| {
        let cache = cache.clone();
        let mut block_num = 0u64;
        b.iter(|| {
            block_num = (block_num + 1) % 1000 + 1;
            cache.get_block_by_number(black_box(block_num))
        });
    });

    // Get block by number (miss)
    group.bench_function("get_block_by_number_miss", |b| {
        let cache = cache.clone();
        b.iter(|| cache.get_block_by_number(black_box(99999)));
    });

    // Get header by number
    group.bench_function("get_header_by_number", |b| {
        let cache = cache.clone();
        let mut block_num = 0u64;
        b.iter(|| {
            block_num = (block_num + 1) % 1000 + 1;
            cache.get_header_by_number(black_box(block_num))
        });
    });

    // Get block by hash
    group.bench_function("get_block_by_hash", |b| {
        let cache = cache.clone();
        let mut block_num = 0u64;
        b.iter(|| {
            block_num = (block_num + 1) % 1000 + 1;
            let mut hash = [0u8; 32];
            hash[0..8].copy_from_slice(&block_num.to_le_bytes());
            cache.get_block_by_hash(black_box(&hash))
        });
    });

    // Insert header
    group.bench_function("insert_header", |b| {
        b.iter_batched(
            || {
                let config = CacheManagerConfig::default();
                let chain_state = Arc::new(ChainState::new());
                let cache =
                    Arc::new(CacheManager::new(&config, chain_state).expect("valid config"));
                (cache, 0u64)
            },
            |(cache, mut counter)| {
                counter += 1;
                let header = generate_block_header(counter);
                rt.block_on(async {
                    cache.insert_header(black_box(header)).await;
                });
                (cache, counter)
            },
            BatchSize::SmallInput,
        );
    });

    // Insert body
    group.bench_function("insert_body", |b| {
        b.iter_batched(
            || {
                let config = CacheManagerConfig::default();
                let chain_state = Arc::new(ChainState::new());
                let cache =
                    Arc::new(CacheManager::new(&config, chain_state).expect("valid config"));
                (cache, 0u64)
            },
            |(cache, mut counter)| {
                counter += 1;
                let body = generate_block_body(counter, 10);
                rt.block_on(async {
                    cache.insert_body(black_box(body)).await;
                });
                (cache, counter)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// LOG QUERY BENCHMARKS
// ============================================================================

/// Benchmark log query operations through `CacheManager`
fn bench_log_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_log_queries");
    // Increase sample size for stability (was 50)
    group.sample_size(100);
    let rt = create_runtime();

    // Pre-populate with logs
    let cache = create_populated_cache_manager(500, 10);

    group.bench_function("get_logs_small_range", |b| {
        let cache = cache.clone();
        b.iter(|| {
            let filter = LogFilter::new(1, 50);
            rt.block_on(async { cache.get_logs(black_box(&filter), 500).await })
        });
    });

    group.bench_function("get_logs_medium_range", |b| {
        let cache = cache.clone();
        b.iter(|| {
            let filter = LogFilter::new(1, 200);
            rt.block_on(async { cache.get_logs(black_box(&filter), 500).await })
        });
    });

    group.bench_function("get_logs_with_address_filter", |b| {
        let cache = cache.clone();
        let mut address = [0u8; 20];
        address[0] = 1;
        b.iter(|| {
            let filter = LogFilter::new(1, 100).with_address(address);
            rt.block_on(async { cache.get_logs(black_box(&filter), 500).await })
        });
    });

    group.bench_function("get_logs_with_ids", |b| {
        let cache = cache.clone();
        b.iter(|| {
            let filter = LogFilter::new(1, 50);
            rt.block_on(async { cache.get_logs_with_ids(black_box(&filter), 500).await })
        });
    });

    group.finish();
}

// ============================================================================
// TRANSACTION OPERATIONS BENCHMARKS
// ============================================================================

/// Benchmark transaction/receipt operations
fn bench_transaction_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_transactions");
    let rt = create_runtime();

    // Insert receipts
    for batch_size in &[10usize, 100, 1000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("insert_receipts_bulk", batch_size),
            batch_size,
            |b, &size| {
                b.iter_batched(
                    || {
                        let config = CacheManagerConfig::default();
                        let chain_state = Arc::new(ChainState::new());
                        let cache = Arc::new(
                            CacheManager::new(&config, chain_state).expect("valid config"),
                        );
                        #[allow(clippy::cast_possible_truncation)]
                        let receipts: Vec<ReceiptRecord> = (0..size)
                            .map(|i| generate_receipt((i / 10) as u64 + 1, (i % 10) as u32))
                            .collect();
                        (cache, receipts)
                    },
                    |(cache, receipts)| {
                        rt.block_on(async {
                            cache.insert_receipts_bulk_no_stats(black_box(receipts)).await;
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    // Get receipt (requires pre-populated cache)
    let cache = {
        let config = CacheManagerConfig::default();
        let chain_state = Arc::new(ChainState::new());
        let cache = Arc::new(CacheManager::new(&config, chain_state).expect("valid config"));

        rt.block_on(async {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let receipts: Vec<ReceiptRecord> = (0..100)
                .map(|i| generate_receipt((i / 10) as u64 + 1, (i % 10) as u32))
                .collect();
            cache.insert_receipts_bulk_no_stats(receipts).await;
        });
        cache
    };

    group.bench_function("get_receipt_hit", |b| {
        let cache = cache.clone();
        let mut idx = 0usize;
        b.iter(|| {
            idx = (idx + 1) % 100;
            let mut tx_hash = [0u8; 32];
            tx_hash[0..8].copy_from_slice(&((idx / 10) as u64 + 1).to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            tx_hash[8..12].copy_from_slice(&((idx % 10) as u32).to_le_bytes());
            cache.get_receipt(black_box(&tx_hash))
        });
    });

    group.bench_function("get_receipt_miss", |b| {
        let cache = cache.clone();
        b.iter(|| {
            let tx_hash = [0xffu8; 32];
            cache.get_receipt(black_box(&tx_hash))
        });
    });

    group.finish();
}

// ============================================================================
// CACHE INVALIDATION BENCHMARKS
// ============================================================================

/// Benchmark cache invalidation operations (critical for reorg handling)
fn bench_invalidation(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_invalidation");
    // Increase sample size for stability (was 30)
    group.sample_size(100);
    let rt = create_runtime();

    // Invalidate block with data
    group.bench_function("invalidate_block_with_data", |b| {
        b.iter_batched(
            || {
                // Create cache with some data
                create_populated_cache_manager(100, 10)
            },
            |cache| {
                rt.block_on(async {
                    // Invalidate block 50 (middle of range)
                    cache.invalidate_block(black_box(50)).await;
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Invalidate block without data (fast path)
    group.bench_function("invalidate_block_empty", |b| {
        b.iter_batched(
            || create_populated_cache_manager(50, 5),
            |cache| {
                rt.block_on(async {
                    // Invalidate block 100 (not in cache)
                    cache.invalidate_block(black_box(100)).await;
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Clear entire cache
    group.bench_function("clear_cache", |b| {
        b.iter_batched(
            || create_populated_cache_manager(100, 10),
            |cache| {
                rt.block_on(async {
                    cache.clear_cache().await;
                });
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ============================================================================
// FETCH GUARD BENCHMARKS (Extended from cache_benchmarks.rs)
// ============================================================================

/// Benchmark `FetchGuard` operations with timeout
fn bench_fetch_guard_timeout(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_fetch_guard");
    // Increase sample size for stability (was 50)
    group.sample_size(100);
    let rt = create_runtime();

    // try_begin_fetch (immediate)
    group.bench_function("try_begin_fetch", |b| {
        b.iter_batched(
            || {
                let config = CacheManagerConfig::default();
                let chain_state = Arc::new(ChainState::new());
                (Arc::new(CacheManager::new(&config, chain_state).expect("valid config")), 0u64)
            },
            |(cache, mut counter)| {
                counter += 1;
                let guard = cache.try_begin_fetch(black_box(counter));
                drop(guard);
                (cache, counter)
            },
            BatchSize::SmallInput,
        );
    });

    // begin_fetch_with_timeout (no contention)
    group.bench_function("begin_fetch_with_timeout_no_contention", |b| {
        b.iter_batched(
            || {
                let config = CacheManagerConfig::default();
                let chain_state = Arc::new(ChainState::new());
                (Arc::new(CacheManager::new(&config, chain_state).expect("valid config")), 0u64)
            },
            |(cache, mut counter)| {
                counter += 1;
                rt.block_on(async {
                    let guard = cache
                        .begin_fetch_with_timeout(
                            black_box(counter),
                            std::time::Duration::from_millis(100),
                        )
                        .await;
                    drop(guard);
                });
                (cache, counter)
            },
            BatchSize::SmallInput,
        );
    });

    // FetchGuard drop (channel-based cleanup)
    group.bench_function("fetch_guard_drop", |b| {
        let config = CacheManagerConfig::default();
        let chain_state = Arc::new(ChainState::new());
        let cache = Arc::new(CacheManager::new(&config, chain_state).expect("valid config"));

        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let guard = cache.try_begin_fetch(black_box(counter));
            drop(black_box(guard));
        });
    });

    group.finish();
}

// ============================================================================
// CONCURRENT ACCESS BENCHMARKS
// ============================================================================

/// Benchmark concurrent cache operations
#[allow(clippy::too_many_lines)]
fn bench_concurrent_cache_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_manager_concurrent");
    // Increase sample size for stability (was 20)
    group.sample_size(100);

    const NUM_THREADS: usize = 4;
    const OPS_PER_THREAD: usize = 50;

    let rt = create_multi_thread_runtime(NUM_THREADS);

    // Warm up the runtime (create cache outside async context, then use inside)
    for _ in 0..5 {
        let (cache, chain_state) = create_populated_cache_manager_sync(10, 1);
        rt.block_on(async {
            populate_cache_manager(&cache, &chain_state, 10, 1).await;
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let cache = cache.clone();
                    tokio::spawn(async move {
                        let _ = cache.get_block_by_number(1);
                    })
                })
                .collect();
            join_all(handles).await;
        });
    }

    // Concurrent block reads with barrier synchronization
    group.throughput(Throughput::Elements((NUM_THREADS * OPS_PER_THREAD) as u64));

    group.bench_function("concurrent_block_reads", |b| {
        b.iter_batched(
            || create_populated_cache_manager(200, 5),
            |cache| {
                rt.block_on(async {
                    let barrier = Arc::new(tokio::sync::Barrier::new(NUM_THREADS));

                    let handles: Vec<_> = (0..NUM_THREADS)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            let barrier = barrier.clone();
                            tokio::spawn(async move {
                                barrier.wait().await;
                                for i in 0..OPS_PER_THREAD {
                                    let block_num =
                                        ((thread_id * OPS_PER_THREAD + i) % 200 + 1) as u64;
                                    let _ = cache.get_block_by_number(block_num);
                                }
                            })
                        })
                        .collect();

                    join_all(handles).await;
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Concurrent log queries with barrier synchronization
    group.bench_function("concurrent_log_queries", |b| {
        b.iter_batched(
            || create_populated_cache_manager(200, 10),
            |cache| {
                rt.block_on(async {
                    let barrier = Arc::new(tokio::sync::Barrier::new(NUM_THREADS));

                    let handles: Vec<_> = (0..NUM_THREADS)
                        .map(|thread_id| {
                            let cache = cache.clone();
                            let barrier = barrier.clone();
                            tokio::spawn(async move {
                                barrier.wait().await;
                                for i in 0..OPS_PER_THREAD {
                                    let from = ((thread_id * 50 + i) % 150 + 1) as u64;
                                    let filter = LogFilter::new(from, from + 20);
                                    let _ = cache.get_logs(&filter, 200).await;
                                }
                            })
                        })
                        .collect();

                    join_all(handles).await;
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Mixed read/write workload with barrier synchronization
    group.bench_function("concurrent_mixed_workload", |b| {
        b.iter_batched(
            || create_populated_cache_manager(100, 5),
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
                                        let block_num =
                                            200 + (thread_id * OPS_PER_THREAD + i) as u64;
                                        let header = generate_block_header(block_num);
                                        cache.insert_header(header).await;
                                    } else {
                                        let block_num = (i % 100 + 1) as u64;
                                        let _ = cache.get_block_by_number(block_num);
                                    }
                                }
                            })
                        })
                        .collect();

                    join_all(handles).await;
                });
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// CRITERION GROUPS
// ============================================================================

// Use custom configuration for all benchmark groups to ensure stability
criterion_group! {
    name = validation_benches;
    config = criterion_config();
    targets = bench_log_validation
}

criterion_group! {
    name = block_benches;
    config = criterion_config();
    targets = bench_block_operations
}

criterion_group! {
    name = log_query_benches;
    config = criterion_config();
    targets = bench_log_queries
}

criterion_group! {
    name = transaction_benches;
    config = criterion_config();
    targets = bench_transaction_operations
}

criterion_group! {
    name = invalidation_benches;
    config = criterion_config();
    targets = bench_invalidation
}

criterion_group! {
    name = fetch_guard_benches;
    config = criterion_config();
    targets = bench_fetch_guard_timeout
}

criterion_group! {
    name = concurrent_benches;
    config = criterion_config();
    targets = bench_concurrent_cache_access
}

criterion_main!(
    validation_benches,
    block_benches,
    log_query_benches,
    transaction_benches,
    invalidation_benches,
    fetch_guard_benches,
    concurrent_benches,
);
