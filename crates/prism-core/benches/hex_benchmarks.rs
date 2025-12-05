//! Hex formatting benchmarks using Criterion.
//!
//! Compares performance of different hex encoding approaches:
//! - Traditional `hex::encode` with `format!`
//! - Optimized `format_hex` with thread-local buffer reuse
//! - Zero-allocation callback-based `with_hex_format`

#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_possible_truncation)] // Acceptable: intentional byte wrapping in test data

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use prism_core::utils::hex_buffer::{format_hex, with_address, with_hash32, with_hex_format};
use std::hint::black_box;

/// Benchmark hex encoding for different data sizes
fn bench_hex_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("hex_encoding");

    let sizes: [usize; 5] = [20, 32, 64, 256, 1024];

    for size in &sizes {
        let data: Vec<u8> = (0..*size).map(|i| i as u8).collect();
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("hex_encode_format", size), &data, |b, data| {
            b.iter(|| format!("0x{}", hex::encode(black_box(data))));
        });

        group.bench_with_input(BenchmarkId::new("format_hex", size), &data, |b, data| {
            b.iter(|| format_hex(black_box(data)));
        });

        group.bench_with_input(BenchmarkId::new("with_hex_format", size), &data, |b, data| {
            b.iter(|| {
                with_hex_format(black_box(data), |hex_str| {
                    black_box(hex_str.len());
                });
            });
        });
    }

    group.finish();
}

/// Benchmark specialized hash and address formatters
fn bench_specialized_formatters(c: &mut Criterion) {
    let mut group = c.benchmark_group("specialized_hex");

    let hash: [u8; 32] = [0xAB; 32];
    group.throughput(Throughput::Bytes(32));

    group.bench_function("with_hash32", |b| {
        b.iter(|| {
            with_hash32(black_box(&hash), |hex_str| {
                black_box(hex_str.len());
            });
        });
    });

    group.bench_function("format_hex_32bytes", |b| {
        b.iter(|| format_hex(black_box(&hash)));
    });

    let address: [u8; 20] = [0xCC; 20];
    group.throughput(Throughput::Bytes(20));

    group.bench_function("with_address", |b| {
        b.iter(|| {
            with_address(black_box(&address), |hex_str| {
                black_box(hex_str.len());
            });
        });
    });

    group.bench_function("format_hex_20bytes", |b| {
        b.iter(|| format_hex(black_box(&address)));
    });

    group.finish();
}

/// Benchmark repeated encoding (tests buffer reuse effectiveness)
fn bench_repeated_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("repeated_encoding");

    const ITERATIONS: usize = 1000;
    let data: [u8; 32] = [0xAB; 32];

    group.throughput(Throughput::Elements(ITERATIONS as u64));

    group.bench_function("format_hex_1000x", |b| {
        b.iter(|| {
            for _ in 0..ITERATIONS {
                let _ = format_hex(black_box(&data));
            }
        });
    });

    group.bench_function("hex_encode_format_1000x", |b| {
        b.iter(|| {
            for _ in 0..ITERATIONS {
                let _ = format!("0x{}", hex::encode(black_box(&data)));
            }
        });
    });

    group.bench_function("with_hex_format_1000x", |b| {
        b.iter(|| {
            for _ in 0..ITERATIONS {
                with_hex_format(black_box(&data), |hex_str| {
                    black_box(hex_str.len());
                });
            }
        });
    });

    group.finish();
}

/// Benchmark encoding varying-size data (simulates real-world mixed workload)
fn bench_mixed_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_sizes");

    let data_20: [u8; 20] = [0xAA; 20];
    let data_32: [u8; 32] = [0xBB; 32];
    let data_64: [u8; 64] = [0xCC; 64];
    let data_256: [u8; 256] = [0xDD; 256];

    const ITERATIONS: usize = 250;
    group.throughput(Throughput::Elements((ITERATIONS * 4) as u64));

    group.bench_function("format_hex_mixed", |b| {
        b.iter(|| {
            for _ in 0..ITERATIONS {
                let _ = format_hex(black_box(&data_20));
                let _ = format_hex(black_box(&data_32));
                let _ = format_hex(black_box(&data_64));
                let _ = format_hex(black_box(&data_256));
            }
        });
    });

    group.bench_function("hex_encode_mixed", |b| {
        b.iter(|| {
            for _ in 0..ITERATIONS {
                let _ = format!("0x{}", hex::encode(black_box(&data_20)));
                let _ = format!("0x{}", hex::encode(black_box(&data_32)));
                let _ = format!("0x{}", hex::encode(black_box(&data_64)));
                let _ = format!("0x{}", hex::encode(black_box(&data_256)));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_hex_encoding,
    bench_specialized_formatters,
    bench_repeated_encoding,
    bench_mixed_sizes,
);

criterion_main!(benches);
