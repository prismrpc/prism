//! Cache performance benchmarks using Criterion.
//!
//! These benchmarks measure the performance of cache-related operations including:
//! - Concurrent data structure comparison (`DashSet` vs `RwLock<HashSet>`)
//! - Fetch lock acquire/release operations
//! - High contention scenarios
//!
//! Optimizations applied to reduce outliers:
//! - Pre-allocated capacity to avoid hash table resizing during measurement
//! - `iter_batched` separates setup from measurement
//! - Rayon thread pool reuse eliminates thread spawn overhead
//! - Steady-state benchmarks measure hot-path performance

#![allow(clippy::items_after_statements)]
#![allow(clippy::expect_used)] // Acceptable in benchmark code

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use dashmap::DashSet;
use prism_core::{
    cache::{CacheManager, CacheManagerConfig},
    chain::ChainState,
};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    hint::black_box,
    sync::{Arc, RwLock},
};

/// Benchmark `DashSet` insert operations with pre-allocated capacity
fn bench_dashset_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_set_insert");

    for size in &[100usize, 1000, 10000] {
        group.throughput(Throughput::Elements(*size as u64));

        group.bench_with_input(BenchmarkId::new("DashSet", size), size, |b, &size| {
            b.iter(|| {
                let set = DashSet::<u64>::with_capacity(size);
                for i in 0..size {
                    set.insert(black_box(i as u64));
                }
                set
            });
        });

        group.bench_with_input(BenchmarkId::new("RwLock<HashSet>", size), size, |b, &size| {
            b.iter(|| {
                let set = RwLock::new(HashSet::<u64>::with_capacity(size));
                for i in 0..size {
                    set.write().expect("lock poisoned").insert(black_box(i as u64));
                }
                set
            });
        });
    }

    group.finish();
}

/// Benchmark concurrent read/write operations using Rayon thread pool.
fn bench_concurrent_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_operations");
    group.sample_size(50);

    const NUM_THREADS: usize = 4;
    const OPS_PER_THREAD: usize = 1000;
    const TOTAL_OPS: usize = NUM_THREADS * OPS_PER_THREAD;

    group.throughput(Throughput::Elements(TOTAL_OPS as u64));

    group.bench_function("DashSet_4threads_rayon", |b| {
        b.iter_batched(
            || Arc::new(DashSet::<u64>::with_capacity(TOTAL_OPS)),
            |set| {
                (0..NUM_THREADS).into_par_iter().for_each(|thread_id| {
                    for i in 0..OPS_PER_THREAD {
                        let value = (thread_id * OPS_PER_THREAD + i) as u64;
                        set.insert(black_box(value));
                        let _ = set.contains(&value);
                    }
                });
                set
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("RwLock_HashSet_4threads_rayon", |b| {
        b.iter_batched(
            || Arc::new(RwLock::new(HashSet::<u64>::with_capacity(TOTAL_OPS))),
            |set| {
                (0..NUM_THREADS).into_par_iter().for_each(|thread_id| {
                    for i in 0..OPS_PER_THREAD {
                        let value = (thread_id * OPS_PER_THREAD + i) as u64;
                        set.write().expect("lock poisoned").insert(black_box(value));
                        let _ = set.read().expect("lock poisoned").contains(&value);
                    }
                });
                set
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark fetch lock acquire/release operations.
fn bench_fetch_lock_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("fetch_lock");

    group.throughput(Throughput::Elements(1));

    group.bench_function("acquire_release_single", |b| {
        let config = CacheManagerConfig::default();
        let chain_state = Arc::new(ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&config, chain_state).expect("valid benchmark cache config"),
        );
        let mut block_id = 0u64;

        b.iter(|| {
            block_id = block_id.wrapping_add(1);
            let acquired = cache_manager.try_acquire_fetch_lock(black_box(block_id));
            if acquired {
                let _ = cache_manager.release_fetch_lock(block_id);
            }
            acquired
        });
    });

    group.finish();
}

/// Benchmark fetch lock operations with multiple iterations.
fn bench_fetch_lock_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("fetch_lock_batch");

    for batch_size in &[100usize, 1000, 10000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", batch_size),
            batch_size,
            |b, &size| {
                b.iter_batched(
                    || {
                        let config = CacheManagerConfig::default();
                        let chain_state = Arc::new(ChainState::new());
                        Arc::new(
                            CacheManager::new(&config, chain_state)
                                .expect("valid benchmark cache config"),
                        )
                    },
                    |cache_manager| {
                        for i in 0..size {
                            let block_id = i as u64;
                            if cache_manager.try_acquire_fetch_lock(black_box(block_id)) {
                                let _ = cache_manager.release_fetch_lock(block_id);
                            }
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark fetch lock under high contention.
fn bench_fetch_lock_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("fetch_lock_contention");
    group.sample_size(30);

    const NUM_THREADS: usize = 8;
    const OPS_PER_THREAD: usize = 500;
    const SHARED_BLOCKS: u64 = 50;

    group.throughput(Throughput::Elements((NUM_THREADS * OPS_PER_THREAD) as u64));

    group.bench_function("high_contention_8threads_rayon", |b| {
        b.iter_batched(
            || {
                let config = CacheManagerConfig::default();
                let chain_state = Arc::new(ChainState::new());
                (
                    Arc::new(
                        CacheManager::new(&config, chain_state)
                            .expect("valid benchmark cache config"),
                    ),
                    Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                )
            },
            |(cache_manager, acquisitions)| {
                (0..NUM_THREADS).into_par_iter().for_each(|thread_id| {
                    let mut local = 0;
                    for i in 0..OPS_PER_THREAD {
                        let block_id = ((thread_id * OPS_PER_THREAD + i) as u64) % SHARED_BLOCKS;
                        if cache_manager.try_acquire_fetch_lock(black_box(block_id)) {
                            local += 1;
                            let _ = cache_manager.release_fetch_lock(block_id);
                        }
                    }
                    acquisitions.fetch_add(local, std::sync::atomic::Ordering::Relaxed);
                });
                acquisitions.load(std::sync::atomic::Ordering::Relaxed)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Steady-state benchmarks that reuse data structures.
fn bench_steady_state_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("steady_state");

    const PRELOAD_SIZE: usize = 10000;
    let preloaded_set: Arc<DashSet<u64>> = Arc::new(DashSet::with_capacity(PRELOAD_SIZE));
    for i in 0..PRELOAD_SIZE {
        preloaded_set.insert(i as u64);
    }

    group.throughput(Throughput::Elements(1000));

    group.bench_function("dashset_contains_hot", |b| {
        let set = preloaded_set.clone();
        let mut idx = 0usize;
        b.iter(|| {
            for _ in 0..1000 {
                idx = (idx + 1) % PRELOAD_SIZE;
                let _ = set.contains(&black_box(idx as u64));
            }
        });
    });

    group.bench_function("dashset_churn_hot", |b| {
        let set = preloaded_set.clone();
        let mut idx = PRELOAD_SIZE as u64;
        b.iter(|| {
            for _ in 0..1000 {
                idx = idx.wrapping_add(1);
                set.insert(black_box(idx));
                set.remove(&black_box(idx));
            }
        });
    });

    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    let preloaded_cache =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid benchmark cache config"));

    group.bench_function("fetch_lock_reacquire_hot", |b| {
        let cache = preloaded_cache.clone();
        b.iter(|| {
            for block_id in 0u64..100 {
                if cache.try_acquire_fetch_lock(black_box(block_id)) {
                    let _ = cache.release_fetch_lock(block_id);
                }
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_dashset_insert,
    bench_concurrent_operations,
    bench_fetch_lock_operations,
    bench_fetch_lock_batch,
    bench_fetch_lock_contention,
    bench_steady_state_operations,
);

criterion_main!(benches);
