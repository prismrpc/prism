//! Cache endpoint handlers.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]

use axum::{extract::State, response::IntoResponse, Json};

use crate::admin::{
    prometheus::parse_time_range,
    types::{
        BlockCacheStats, CacheHitByMethod, CacheHitDataPoint, CacheSettingsResponse, CacheStats,
        LogCacheStats, MemoryAllocation, TimeRangeQuery, TransactionCacheStats,
    },
    AdminState,
};

/// Formats bytes into human-readable string.
fn format_bytes(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = KB * 1024;
    const GB: usize = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Calculates hit rate as a percentage from hits and misses.
///
/// Returns 0.0 if total (hits + misses) is zero to avoid division by zero.
fn calculate_hit_rate(hits: u64, misses: u64) -> f64 {
    let total = hits + misses;
    if total > 0 {
        (hits as f64 / total as f64) * 100.0
    } else {
        0.0
    }
}

/// GET /admin/cache/stats
///
/// Returns current cache statistics.
#[utoipa::path(
    get,
    path = "/admin/cache/stats",
    tag = "Cache",
    responses(
        (status = 200, description = "Current cache statistics", body = CacheStats)
    )
)]
pub async fn get_stats(State(state): State<AdminState>) -> impl IntoResponse {
    let cache_stats_raw = state.proxy_engine.get_cache_manager().get_stats().await;

    // Calculate hit rates using helper
    let block_hit_rate =
        calculate_hit_rate(cache_stats_raw.block_cache_hits, cache_stats_raw.block_cache_misses);

    // Combined transaction/receipt hit rate
    let combined_tx_hits =
        cache_stats_raw.transaction_cache_hits + cache_stats_raw.receipt_cache_hits;
    let combined_tx_misses =
        cache_stats_raw.transaction_cache_misses + cache_stats_raw.receipt_cache_misses;
    let combined_tx_hit_rate = calculate_hit_rate(combined_tx_hits, combined_tx_misses);

    // Log partial fulfillment rate (partial hits as percentage of all log requests)
    let log_total = cache_stats_raw.log_cache_hits +
        cache_stats_raw.log_cache_misses +
        cache_stats_raw.log_cache_partial_hits;
    let log_partial_rate = if log_total > 0 {
        (cache_stats_raw.log_cache_partial_hits as f64 / log_total as f64) * 100.0
    } else {
        0.0
    };

    // Estimate memory usage (rough approximation based on entry counts)
    // Block header ~500 bytes, body ~2KB average, transaction ~300 bytes, receipt ~200 bytes
    let block_memory =
        cache_stats_raw.header_cache_size * 500 + cache_stats_raw.body_cache_size * 2048;
    let tx_memory =
        cache_stats_raw.transaction_cache_size * 300 + cache_stats_raw.receipt_cache_size * 200;

    let cache_stats = CacheStats {
        block_cache: BlockCacheStats {
            hot_window_size: cache_stats_raw.hot_window_size,
            lru_entries: cache_stats_raw.header_cache_size + cache_stats_raw.body_cache_size,
            memory_usage: format_bytes(block_memory),
            hit_rate: block_hit_rate,
        },
        log_cache: LogCacheStats {
            chunk_count: cache_stats_raw.exact_result_count,
            indexed_blocks: cache_stats_raw.log_store_size,
            memory_usage: format_bytes(cache_stats_raw.bitmap_memory_usage),
            partial_fulfillment: log_partial_rate,
        },
        transaction_cache: TransactionCacheStats {
            entries: cache_stats_raw.transaction_cache_size,
            receipt_entries: cache_stats_raw.receipt_cache_size,
            memory_usage: format_bytes(tx_memory),
            hit_rate: combined_tx_hit_rate,
        },
    };

    Json(cache_stats)
}

/// GET /admin/cache/hit-rate
///
/// Returns cache hit rate over time.
#[utoipa::path(
    get,
    path = "/admin/cache/hit-rate",
    tag = "Cache",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range: 24h, 7d, 30d (default: 24h)")
    ),
    responses(
        (status = 200, description = "Cache hit rate time series data", body = Vec<CacheHitDataPoint>)
    )
)]
pub async fn get_hit_rate(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for historical data if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_cache_hit_rate(time_range).await {
            Ok(data) if !data.is_empty() => return Json(data),
            Ok(_) => {
                tracing::debug!("no cache hit rate data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for cache hit rate: {}", e);
            }
        }
    }

    // Return empty data if Prometheus unavailable or query failed
    let data: Vec<CacheHitDataPoint> = vec![CacheHitDataPoint {
        timestamp: chrono::Utc::now().to_rfc3339(),
        hit: 0.0,
        partial: 0.0,
        miss: 100.0,
    }];

    Json(data)
}

/// GET /admin/cache/hit-by-method
///
/// Returns cache hit rate by RPC method.
#[utoipa::path(
    get,
    path = "/admin/cache/hit-by-method",
    tag = "Cache",
    responses(
        (status = 200, description = "Cache hit rate breakdown by RPC method", body = Vec<CacheHitByMethod>)
    )
)]
pub async fn get_hit_by_method(State(state): State<AdminState>) -> impl IntoResponse {
    let cache_stats_raw = state.proxy_engine.get_cache_manager().get_stats().await;

    // Calculate hit rates per method type using helper
    let block_hit_rate =
        calculate_hit_rate(cache_stats_raw.block_cache_hits, cache_stats_raw.block_cache_misses);

    // For logs, partial hits count as cache hits (data was partially served from cache)
    let log_hits = cache_stats_raw.log_cache_hits + cache_stats_raw.log_cache_partial_hits;
    let log_hit_rate = calculate_hit_rate(log_hits, cache_stats_raw.log_cache_misses);

    let tx_hit_rate = calculate_hit_rate(
        cache_stats_raw.transaction_cache_hits,
        cache_stats_raw.transaction_cache_misses,
    );
    let receipt_hit_rate = calculate_hit_rate(
        cache_stats_raw.receipt_cache_hits,
        cache_stats_raw.receipt_cache_misses,
    );

    let data: Vec<CacheHitByMethod> = vec![
        CacheHitByMethod { method: "eth_getBlockByNumber".to_string(), hit_rate: block_hit_rate },
        CacheHitByMethod { method: "eth_getLogs".to_string(), hit_rate: log_hit_rate },
        CacheHitByMethod { method: "eth_getTransactionByHash".to_string(), hit_rate: tx_hit_rate },
        CacheHitByMethod {
            method: "eth_getTransactionReceipt".to_string(),
            hit_rate: receipt_hit_rate,
        },
    ];

    Json(data)
}

/// GET /admin/cache/memory-allocation
///
/// Returns memory usage breakdown.
#[utoipa::path(
    get,
    path = "/admin/cache/memory-allocation",
    tag = "Cache",
    responses(
        (status = 200, description = "Memory allocation breakdown by cache type", body = Vec<MemoryAllocation>)
    )
)]
pub async fn get_memory_allocation(State(state): State<AdminState>) -> impl IntoResponse {
    let cache_stats_raw = state.proxy_engine.get_cache_manager().get_stats().await;

    // Estimate memory usage (rough approximation based on entry counts)
    // Block header ~500 bytes, body ~2KB average
    let block_memory =
        (cache_stats_raw.header_cache_size * 500 + cache_stats_raw.body_cache_size * 2048) as u64;

    // Transaction ~300 bytes, receipt ~200 bytes
    let tx_memory = (cache_stats_raw.transaction_cache_size * 300 +
        cache_stats_raw.receipt_cache_size * 200) as u64;

    let allocations = vec![
        MemoryAllocation {
            label: "Block Cache".to_string(),
            value: block_memory,
            color: "bg-primary".to_string(),
        },
        MemoryAllocation {
            label: "Log Cache".to_string(),
            value: cache_stats_raw.bitmap_memory_usage as u64,
            color: "bg-accent".to_string(),
        },
        MemoryAllocation {
            label: "Transaction Cache".to_string(),
            value: tx_memory,
            color: "bg-secondary".to_string(),
        },
    ];

    Json(allocations)
}

/// GET /admin/cache/settings
///
/// Returns current cache configuration.
#[utoipa::path(
    get,
    path = "/admin/cache/settings",
    tag = "Cache",
    responses(
        (status = 200, description = "Current cache configuration", body = CacheSettingsResponse)
    )
)]
pub async fn get_settings(State(state): State<AdminState>) -> impl IntoResponse {
    let config = &state.config.cache.manager_config;

    // Calculate estimated max sizes from config entry counts
    // Using same estimation factors as get_stats():
    // Block header ~500 bytes, body ~2KB, transaction ~300 bytes, receipt ~200 bytes
    let block_max_bytes =
        config.block_cache.max_headers * 500 + config.block_cache.max_bodies * 2048;
    let tx_max_bytes = config.transaction_cache.max_transactions * 300 +
        config.transaction_cache.max_receipts * 200;

    // Log cache: rough estimate based on bitmap entries and exact results
    // Each exact result ~1KB, each bitmap entry ~100 bytes
    let log_max_bytes =
        config.log_cache.max_exact_results * 1024 + config.log_cache.max_bitmap_entries * 100;

    let settings = CacheSettingsResponse {
        retain_blocks: config.retain_blocks as usize,
        block_cache_max_size: format_bytes(block_max_bytes),
        log_cache_max_size: format_bytes(log_max_bytes),
        transaction_cache_max_size: format_bytes(tx_max_bytes),
        cleanup_interval: config.cleanup_interval_seconds,
    };

    Json(settings)
}

/// POST /admin/cache/clear
///
/// Clears specific cache types.
#[utoipa::path(
    post,
    path = "/admin/cache/clear",
    tag = "Cache",
    request_body = crate::admin::types::ClearCacheRequest,
    responses(
        (status = 200, description = "Cache cleared successfully", body = crate::admin::types::SuccessResponse),
        (status = 400, description = "Invalid cache type specified")
    )
)]
pub async fn clear_cache(
    State(state): State<AdminState>,
    axum::extract::Json(request): axum::extract::Json<crate::admin::types::ClearCacheRequest>,
) -> Result<axum::Json<crate::admin::types::SuccessResponse>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;

    let cache_manager = state.proxy_engine.get_cache_manager();

    for cache_type in &request.cache_types {
        match cache_type.as_str() {
            "block" => {
                cache_manager.block_cache.clear_cache().await;
                tracing::info!("cleared block cache");
            }
            "transaction" => {
                cache_manager.transaction_cache.clear_cache().await;
                tracing::info!("cleared transaction cache");
            }
            "logs" => {
                cache_manager.log_cache.clear_cache().await;
                tracing::info!("cleared log cache");
            }
            "all" => {
                cache_manager.clear_cache().await;
                tracing::info!("cleared all caches");
            }
            unknown => {
                return Err((StatusCode::BAD_REQUEST, format!("Unknown cache type: {unknown}")));
            }
        }
    }

    Ok(axum::Json(crate::admin::types::SuccessResponse { success: true }))
}

/// PUT /admin/cache/settings
///
/// Updates cache configuration at runtime.
#[utoipa::path(
    put,
    path = "/admin/cache/settings",
    tag = "Cache",
    request_body = crate::admin::types::UpdateCacheSettingsRequest,
    responses(
        (status = 200, description = "Cache settings updated", body = crate::admin::types::SuccessResponse),
        (status = 501, description = "Runtime configuration updates not yet implemented")
    )
)]
pub async fn update_settings(
    State(_state): State<AdminState>,
    axum::extract::Json(request): axum::extract::Json<
        crate::admin::types::UpdateCacheSettingsRequest,
    >,
) -> Result<axum::Json<crate::admin::types::SuccessResponse>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;

    // Note: The current CacheManager config is stored in Arc<AppConfig> which is immutable.
    // This implementation demonstrates the handler structure, but full runtime config updates
    // would require making the config mutable or using atomic updates.
    //
    // For now, we'll return an error indicating this feature requires additional infrastructure.

    if request.retain_blocks.is_some() || request.cleanup_interval.is_some() {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "Runtime cache configuration updates require additional infrastructure. \
             Current config is immutable. Consider implementing an RwLock wrapper for \
             CacheManagerConfig in the AdminState."
                .to_string(),
        ));
    }

    Ok(axum::Json(crate::admin::types::SuccessResponse { success: true }))
}
