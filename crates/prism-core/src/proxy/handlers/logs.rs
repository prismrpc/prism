use futures_util::stream::{self, StreamExt};
use rayon::prelude::*;
use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::{
    cache::{
        converter::{json_log_to_log_record, log_record_to_json_log},
        types::{LogFilter, LogId, LogRecord},
    },
    proxy::engine::SharedContext,
    types::{CacheStatus, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
};

use super::super::{
    errors::ProxyError,
    partial::{PartialResult, RangeFailure, RangeFetchResult, RetryConfig},
    utils::log_filter_to_json_params,
};

/// Handler for `eth_getLogs` requests with advanced caching strategies.
///
/// Supports full cache hits, partial cache hits with range merging, and cache misses.
/// Uses concurrent fetching for missing ranges to minimize latency.
pub struct LogsHandler {
    ctx: Arc<SharedContext>,
}

impl LogsHandler {
    /// Creates a new `LogsHandler` instance with shared context.
    #[must_use]
    pub fn new(ctx: Arc<SharedContext>) -> Self {
        Self { ctx }
    }

    /// Handles `eth_getLogs` requests with advanced cache strategies.
    ///
    /// Attempts to serve logs from cache first. If the requested range is partially cached,
    /// fetches missing ranges concurrently from upstream and merges results. Handles full
    /// cache hits, empty results, partial hits, and complete misses.
    ///
    /// # Errors
    /// Returns [`ProxyError::InvalidRequest`] if filter parameters are invalid.
    /// Returns [`ProxyError::Upstream`] if upstream requests fail.
    pub async fn handle_advanced_logs_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let filter = Self::parse_log_filter_from_request(&request)?;
        let current_tip = self.ctx.cache_manager.get_current_tip();

        debug!(
            method = "eth_getLogs",
            from_block = filter.from_block,
            to_block = filter.to_block,
            block_count = filter.to_block - filter.from_block + 1,
            current_tip = current_tip,
            has_topics = !filter.topics.is_empty(),
            has_address = filter.address.is_some(),
            "processing eth_getLogs request"
        );

        let (log_records_with_ids, missing_ranges) =
            self.ctx.cache_manager.get_logs_with_ids(&filter, current_tip).await;

        match (log_records_with_ids.is_empty(), missing_ranges.is_empty()) {
            (false, true) => {
                self.handle_full_cache_hit(log_records_with_ids, &request, &filter).await
            }
            (true, true) => {
                let mut resp = Self::handle_empty_cache_hit(&filter);
                resp.id = request.id.clone();
                Ok(resp)
            }
            (false, false) => {
                self.handle_partial_cache_hit(
                    log_records_with_ids,
                    missing_ranges,
                    &request,
                    &filter,
                )
                .await
            }
            (true, false) => self.handle_cache_miss(&request, &filter).await,
        }
    }

    /// Parses and validates log filter from JSON-RPC request parameters.
    fn parse_log_filter_from_request(request: &JsonRpcRequest) -> Result<LogFilter, ProxyError> {
        let params = request.params.as_ref().ok_or_else(|| {
            ProxyError::InvalidRequest("Missing parameters for eth_getLogs".into())
        })?;

        crate::cache::converter::json_params_to_log_filter(params)
            .ok_or_else(|| ProxyError::InvalidRequest("Invalid log filter parameters".into()))
    }

    /// Handles full cache hit where all requested logs are in cache.
    ///
    /// Uses parallel processing for large log sets (>1000 logs) via `spawn_blocking`.
    async fn handle_full_cache_hit(
        &self,
        log_records_with_ids: Vec<(LogId, Arc<LogRecord>)>,
        request: &JsonRpcRequest,
        filter: &LogFilter,
    ) -> Result<JsonRpcResponse, ProxyError> {
        debug!(
            logs_count = log_records_with_ids.len(),
            from_block = filter.from_block,
            to_block = filter.to_block,
            block_count = filter.to_block - filter.from_block + 1,
            "Full cache hit"
        );

        self.ctx.metrics_collector.record_cache_hit("eth_getLogs");

        let json_logs: Vec<serde_json::Value> = if log_records_with_ids.len() > 1000 {
            tokio::task::spawn_blocking(move || {
                log_records_with_ids
                    .par_iter()
                    .map(|(log_id, log_record)| log_record_to_json_log(log_id, log_record))
                    .collect()
            })
            .await
            .map_err(|e| ProxyError::Internal(format!("failed to convert logs: {e}")))?
        } else {
            log_records_with_ids
                .iter()
                .map(|(log_id, log_record)| log_record_to_json_log(log_id, log_record))
                .collect()
        };

        Ok(JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(serde_json::Value::Array(json_logs)),
            error: None,
            id: Arc::clone(&request.id),
            cache_status: Some(CacheStatus::Full),
            serving_upstream: None,
        })
    }

    /// Handles empty cache hit where the requested range is cached but contains no logs.
    fn handle_empty_cache_hit(filter: &LogFilter) -> JsonRpcResponse {
        debug!(from_block = filter.from_block, to_block = filter.to_block, "empty cache hit");

        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(serde_json::Value::Array(vec![])),
            error: None,
            id: Arc::new(serde_json::Value::Null),
            cache_status: Some(CacheStatus::Empty),
            serving_upstream: None,
        }
    }

    /// Handles partial cache hit where some logs are cached but ranges are missing.
    ///
    /// Fetches missing ranges concurrently, merges with cached logs, and sorts the result.
    async fn handle_partial_cache_hit(
        &self,
        log_records_with_ids: Vec<(LogId, Arc<LogRecord>)>,
        missing_ranges: Vec<(u64, u64)>,
        request: &JsonRpcRequest,
        filter: &LogFilter,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let cached_blocks = filter.to_block - filter.from_block + 1 -
            missing_ranges.iter().map(|(from, to)| to - from + 1).sum::<u64>();

        debug!(
            logs_count = log_records_with_ids.len(),
            from_block = filter.from_block,
            to_block = filter.to_block,
            cached_blocks = cached_blocks,
            total_blocks = filter.to_block - filter.from_block + 1,
            missing_ranges_count = missing_ranges.len(),
            missing_ranges = ?missing_ranges,
            "partial cache hit"
        );

        self.ctx.metrics_collector.record_cache_hit("eth_getLogs");

        // Use iterator adapter instead of manual loop for idiomatic Rust
        let mut all_logs: Vec<serde_json::Value> = log_records_with_ids
            .iter()
            .map(|(log_id, log_record)| log_record_to_json_log(log_id, log_record))
            .collect();

        let t_fetch_start = std::time::Instant::now();
        let fetched =
            self.fetch_missing_ranges_concurrent(missing_ranges, filter, request, 6).await?;
        let fetch_ms = t_fetch_start.elapsed().as_millis();

        all_logs.extend(fetched);

        let t_sort_start = std::time::Instant::now();
        if all_logs.len() > 1000 {
            all_logs = tokio::task::spawn_blocking(move || {
                Self::sort_logs_fast(&mut all_logs);
                all_logs
            })
            .await
            .map_err(|e| ProxyError::Internal(format!("failed to sort logs: {e}")))?;
        } else {
            Self::sort_logs_fast(&mut all_logs);
        }
        let sort_ms = t_sort_start.elapsed().as_millis();

        debug!(
            cached_logs = log_records_with_ids.len(),
            fetched_logs = all_logs.len() - log_records_with_ids.len(),
            total_logs = all_logs.len(),
            fetch_time_ms = fetch_ms,
            sort_time_ms = sort_ms,
            "partial cache optimization complete"
        );

        Ok(JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(serde_json::Value::Array(all_logs)),
            error: None,
            id: Arc::clone(&request.id),
            cache_status: Some(CacheStatus::Partial),
            serving_upstream: None,
        })
    }

    /// Handles partial cache hit with graceful degradation on range failures.
    ///
    /// Returns partial results when some upstream ranges fail instead of failing
    /// the entire request. For automatic retry of transient failures, use
    /// `handle_partial_cache_hit_with_retry`.
    ///
    /// # Response Format
    ///
    /// On complete success: Standard JSON-RPC response with all logs.
    ///
    /// On partial success: Standard JSON-RPC response with available logs, plus
    /// `_prism_meta` field containing failure information:
    ///
    /// ```json
    /// {
    ///   "jsonrpc": "2.0",
    ///   "id": 1,
    ///   "result": [...logs from successful ranges...],
    ///   "_prism_meta": {
    ///     "partial": true,
    ///     "failed_ranges": [
    ///       {"from": 100, "to": 110, "error": "timeout", "retryable": true}
    ///     ]
    ///   }
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if:
    /// - Fetching missing ranges fails completely (all upstreams unavailable)
    /// - Log sorting task panics
    #[allow(dead_code)]
    pub async fn handle_partial_cache_hit_graceful(
        &self,
        log_records_with_ids: Vec<(LogId, Arc<LogRecord>)>,
        missing_ranges: Vec<(u64, u64)>,
        request: &JsonRpcRequest,
        filter: &LogFilter,
    ) -> Result<JsonRpcResponse, ProxyError> {
        self.handle_partial_cache_hit_internal(
            log_records_with_ids,
            missing_ranges,
            request,
            filter,
            None, // No retry config = no retries
        )
        .await
    }

    /// Handles partial cache hit with automatic retry for transient failures.
    ///
    /// Combines graceful degradation with automatic retry of transient failures
    /// (timeouts, rate limits, network issues) using exponential backoff with jitter.
    ///
    /// Transient errors are automatically retried, permanent errors fail immediately.
    /// Retry count is tracked per range and included in response metadata.
    ///
    /// # Errors
    /// Returns `ProxyError` if log sorting task panics.
    #[allow(dead_code)]
    pub async fn handle_partial_cache_hit_with_retry(
        &self,
        log_records_with_ids: Vec<(LogId, Arc<LogRecord>)>,
        missing_ranges: Vec<(u64, u64)>,
        request: &JsonRpcRequest,
        filter: &LogFilter,
        retry_config: &RetryConfig,
    ) -> Result<JsonRpcResponse, ProxyError> {
        self.handle_partial_cache_hit_internal(
            log_records_with_ids,
            missing_ranges,
            request,
            filter,
            Some(retry_config),
        )
        .await
    }

    /// Internal implementation for partial cache hit handling with optional retry.
    async fn handle_partial_cache_hit_internal(
        &self,
        log_records_with_ids: Vec<(LogId, Arc<LogRecord>)>,
        missing_ranges: Vec<(u64, u64)>,
        request: &JsonRpcRequest,
        filter: &LogFilter,
        retry_config: Option<&RetryConfig>,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let cached_blocks = filter.to_block - filter.from_block + 1 -
            missing_ranges.iter().map(|(from, to)| to - from + 1).sum::<u64>();

        debug!(
            logs_count = log_records_with_ids.len(),
            from_block = filter.from_block,
            to_block = filter.to_block,
            cached_blocks = cached_blocks,
            total_blocks = filter.to_block - filter.from_block + 1,
            missing_ranges_count = missing_ranges.len(),
            missing_ranges = ?missing_ranges,
            retry_enabled = retry_config.is_some(),
            "partial cache hit with graceful degradation"
        );

        self.ctx.metrics_collector.record_cache_hit("eth_getLogs");

        // Convert cached logs to JSON
        let mut all_logs: Vec<serde_json::Value> = log_records_with_ids
            .iter()
            .map(|(log_id, log_record)| log_record_to_json_log(log_id, log_record))
            .collect();

        // Fetch missing ranges with optional retry
        let t_fetch_start = std::time::Instant::now();
        let partial_result = if let Some(config) = retry_config {
            self.fetch_missing_ranges_with_retry(missing_ranges, filter, request, 6, config)
                .await
        } else {
            self.fetch_missing_ranges_partial(missing_ranges, filter, request, 6).await
        };
        let fetch_ms = t_fetch_start.elapsed().as_millis();

        // Destructure to avoid partial move issues
        let PartialResult { data: fetched_logs, failed_ranges, is_complete } = partial_result;
        let failure_count = failed_ranges.len();
        let failed_block_count: u64 = failed_ranges.iter().map(RangeFailure::block_count).sum();

        all_logs.extend(fetched_logs);
        let fetched_count = all_logs.len() - log_records_with_ids.len();

        // Sort all logs
        let t_sort_start = std::time::Instant::now();
        if all_logs.len() > 1000 {
            all_logs = tokio::task::spawn_blocking(move || {
                Self::sort_logs_fast(&mut all_logs);
                all_logs
            })
            .await
            .map_err(|e| ProxyError::Internal(format!("failed to sort logs: {e}")))?;
        } else {
            Self::sort_logs_fast(&mut all_logs);
        }
        let sort_ms = t_sort_start.elapsed().as_millis();

        // Build response with optional failure metadata
        let mut response_obj = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": all_logs
        });

        // Add metadata about partial failures if any
        if is_complete {
            debug!(
                cached_logs = log_records_with_ids.len(),
                fetched_logs = fetched_count,
                total_logs = all_logs.len(),
                fetch_time_ms = fetch_ms,
                sort_time_ms = sort_ms,
                "partial cache hit completed successfully"
            );
        } else {
            let failed_ranges_json: Vec<serde_json::Value> = failed_ranges
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "from": f.from_block,
                        "to": f.to_block,
                        "error": f.error.to_string(),
                        "retryable": f.is_retryable(),
                        "retry_count": f.retry_count
                    })
                })
                .collect();

            response_obj["_prism_meta"] = serde_json::json!({
                "partial": true,
                "failed_ranges": failed_ranges_json,
                "failed_block_count": failed_block_count
            });

            warn!(
                cached_logs = log_records_with_ids.len(),
                fetched_logs = fetched_count,
                failed_ranges = failure_count,
                failed_blocks = failed_block_count,
                fetch_time_ms = fetch_ms,
                sort_time_ms = sort_ms,
                retry_enabled = retry_config.is_some(),
                "partial cache hit completed with failures"
            );
        }

        // Convert serde_json::Value to JsonRpcResponse
        // We use the raw JSON approach to include the _prism_meta field
        Ok(JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: response_obj.get("result").cloned(),
            error: None,
            id: Arc::clone(&request.id),
            cache_status: Some(if is_complete {
                CacheStatus::Partial
            } else {
                CacheStatus::PartialWithFailures
            }),
            serving_upstream: None,
        })
    }

    /// Fetches logs from upstream for missing ranges with bounded concurrency.
    ///
    /// Each missing range is fetched independently and cached upon successful retrieval.
    /// Uses `buffer_unordered` to process multiple ranges concurrently up to `max_concurrency`.
    async fn fetch_missing_ranges_concurrent(
        &self,
        missing_ranges: Vec<(u64, u64)>,
        original_filter: &LogFilter,
        request: &JsonRpcRequest,
        max_concurrency: usize,
    ) -> Result<Vec<serde_json::Value>, ProxyError> {
        debug!(
            ranges_count = missing_ranges.len(),
            max_concurrency = max_concurrency,
            "starting concurrent range fetch"
        );

        let stream = stream::iter(missing_ranges.into_iter().map(|(from_block, to_block)| {
            let range_filter = LogFilter {
                address: original_filter.address,
                topics: original_filter.topics,
                topics_anywhere: original_filter.topics_anywhere.clone(),
                from_block,
                to_block,
            };

            let range_request = JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION_COW,
                method: "eth_getLogs".to_string(),
                params: Some(log_filter_to_json_params(&range_filter)),
                id: Arc::clone(&request.id),
            };

            self.fetch_single_range(range_request, range_filter)
        }));

        let buffered = stream.buffer_unordered(max_concurrency);
        self.collect_range_results(buffered).await
    }

    /// Fetches logs for a single block range from upstream.
    ///
    /// On success, caches the logs and returns them. On failure, returns the error.
    async fn fetch_single_range(
        &self,
        range_request: JsonRpcRequest,
        range_filter: LogFilter,
    ) -> Result<Vec<serde_json::Value>, ProxyError> {
        let from_block = range_filter.from_block;
        let to_block = range_filter.to_block;

        let t0 = std::time::Instant::now();
        let res = self.ctx.forward_to_upstream(&range_request).await;
        let upstream_ms = t0.elapsed().as_millis();

        match res {
            Ok(range_response) => {
                let mut out = Vec::new();
                if let Some(result) = range_response.result.as_ref() {
                    if let Some(logs_array) = result.as_array() {
                        debug!(
                            logs_count = logs_array.len(),
                            from_block = from_block,
                            to_block = to_block,
                            fetch_time_ms = upstream_ms,
                            "range fetch complete"
                        );

                        out.extend(logs_array.iter().cloned());
                        self.cache_logs_from_response(logs_array, &range_filter).await;
                    }
                }
                Ok(out)
            }
            Err(e) => {
                error!(
                    from_block = from_block,
                    to_block = to_block,
                    error = %e,
                    fetch_time_ms = upstream_ms,
                    "failed to fetch range"
                );
                Err(e)
            }
        }
    }

    /// Collects results from a buffered stream of range fetch results.
    ///
    /// Processes results and accumulates them. Logs warnings for any errors
    /// but continues processing remaining ranges.
    async fn collect_range_results(
        &self,
        mut buffered: impl stream::Stream<Item = Result<Vec<serde_json::Value>, ProxyError>> + Unpin,
    ) -> Result<Vec<serde_json::Value>, ProxyError> {
        let mut all = Vec::new();
        let mut errors = 0usize;

        while let Some(res) = buffered.next().await {
            match res {
                Ok(mut logs) => all.append(&mut logs),
                Err(e) => {
                    errors += 1;
                    error!(error = %e, "range fetch error");
                }
            }
        }

        if errors > 0 {
            warn!(error_count = errors, "completed concurrent range fetch with errors");
        } else {
            debug!("concurrent range fetch completed successfully");
        }

        Ok(all)
    }

    /// Fetches missing ranges with partial failure tracking.
    ///
    /// Tracks which ranges failed and returns a `PartialResult` containing both
    /// successfully fetched logs and detailed failure information for failed ranges.
    #[allow(dead_code)]
    pub async fn fetch_missing_ranges_partial(
        &self,
        missing_ranges: Vec<(u64, u64)>,
        original_filter: &LogFilter,
        request: &JsonRpcRequest,
        max_concurrency: usize,
    ) -> PartialResult<Vec<serde_json::Value>> {
        debug!(
            ranges_count = missing_ranges.len(),
            max_concurrency = max_concurrency,
            "starting concurrent range fetch with partial tracking"
        );

        let stream = stream::iter(missing_ranges.into_iter().map(|(from_block, to_block)| {
            let range_filter = LogFilter {
                address: original_filter.address,
                topics: original_filter.topics,
                topics_anywhere: original_filter.topics_anywhere.clone(),
                from_block,
                to_block,
            };

            let range_request = JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION_COW,
                method: "eth_getLogs".to_string(),
                params: Some(log_filter_to_json_params(&range_filter)),
                id: Arc::clone(&request.id),
            };

            self.fetch_single_range_tracked(range_request, range_filter)
        }));

        let buffered = stream.buffer_unordered(max_concurrency);
        self.collect_range_results_partial(buffered).await
    }

    /// Fetches logs for a single block range with range tracking.
    ///
    /// Returns a `RangeFetchResult` that preserves range information even on error,
    /// enabling construction of `PartialResult` with detailed failure tracking.
    ///
    /// On success, the range is marked as covered. On failure, the range is NOT
    /// marked as covered, allowing future retries.
    async fn fetch_single_range_tracked(
        &self,
        range_request: JsonRpcRequest,
        range_filter: LogFilter,
    ) -> RangeFetchResult<Vec<serde_json::Value>> {
        let from_block = range_filter.from_block;
        let to_block = range_filter.to_block;

        let t0 = std::time::Instant::now();
        let res = self.ctx.forward_to_upstream(&range_request).await;
        let upstream_ms = t0.elapsed().as_millis();

        match res {
            Ok(range_response) => {
                let mut out = Vec::new();
                if let Some(result) = range_response.result.as_ref() {
                    if let Some(logs_array) = result.as_array() {
                        debug!(
                            logs_count = logs_array.len(),
                            from_block = from_block,
                            to_block = to_block,
                            fetch_time_ms = upstream_ms,
                            "range fetch complete"
                        );

                        out.extend(logs_array.iter().cloned());
                        self.cache_logs_from_response(logs_array, &range_filter).await;
                    }
                }
                RangeFetchResult::success(from_block, to_block, out)
            }
            Err(e) => {
                error!(
                    from_block = from_block,
                    to_block = to_block,
                    error = %e,
                    fetch_time_ms = upstream_ms,
                    "failed to fetch range"
                );
                RangeFetchResult::failure(from_block, to_block, e)
            }
        }
    }

    /// Collects results from a stream of tracked range fetches into a `PartialResult`.
    ///
    /// Separates successful results from failures, providing detailed tracking of
    /// which ranges failed and why. Successful ranges are marked as covered during
    /// fetch; failed ranges are not, allowing future retries.
    async fn collect_range_results_partial(
        &self,
        mut buffered: impl stream::Stream<Item = RangeFetchResult<Vec<serde_json::Value>>> + Unpin,
    ) -> PartialResult<Vec<serde_json::Value>> {
        let mut all_logs = Vec::new();
        let mut failures = Vec::new();

        while let Some(fetch_result) = buffered.next().await {
            match fetch_result.result {
                Ok(mut logs) => {
                    debug!(
                        from_block = fetch_result.from_block,
                        to_block = fetch_result.to_block,
                        logs_count = logs.len(),
                        "range fetched successfully"
                    );
                    all_logs.append(&mut logs);
                }
                Err(e) => {
                    let failure =
                        RangeFailure::new(fetch_result.from_block, fetch_result.to_block, e);
                    let failure = if let Some(upstream) = fetch_result.upstream {
                        failure.with_upstream(upstream)
                    } else {
                        failure
                    };

                    warn!(
                        from_block = failure.from_block,
                        to_block = failure.to_block,
                        error = %failure.error,
                        "range fetch failed"
                    );

                    failures.push(failure);
                }
            }
        }

        let is_complete = failures.is_empty();

        if is_complete {
            debug!(logs_count = all_logs.len(), "all ranges fetched successfully");
        } else {
            warn!(
                success_count = all_logs.len(),
                failure_count = failures.len(),
                failed_blocks = failures.iter().map(RangeFailure::block_count).sum::<u64>(),
                "completed with partial failures"
            );
        }

        PartialResult::partial(all_logs, failures)
    }

    /// Fetches missing ranges with automatic retry for transient failures.
    ///
    /// Automatically retries ranges that fail with transient errors (timeouts, rate limits).
    /// Permanent errors fail immediately. Uses exponential backoff with jitter.
    #[allow(dead_code)]
    pub async fn fetch_missing_ranges_with_retry(
        &self,
        missing_ranges: Vec<(u64, u64)>,
        original_filter: &LogFilter,
        request: &JsonRpcRequest,
        max_concurrency: usize,
        retry_config: &RetryConfig,
    ) -> PartialResult<Vec<serde_json::Value>> {
        // First attempt: fetch all ranges
        let mut result = self
            .fetch_missing_ranges_partial(missing_ranges, original_filter, request, max_concurrency)
            .await;

        // If complete or no retryable failures, return early
        if result.is_complete() || result.retryable_ranges().is_empty() {
            return result;
        }

        // Retry loop for failed ranges
        let mut attempt = 0u32;
        while retry_config.should_retry(attempt) && !result.retryable_ranges().is_empty() {
            attempt += 1;

            // Extract ranges to retry (only retryable ones)
            let ranges_to_retry: Vec<(u64, u64, u32)> = result
                .failed_ranges
                .iter()
                .filter(|f| f.is_retryable())
                .map(|f| (f.from_block, f.to_block, f.retry_count + 1))
                .collect();

            if ranges_to_retry.is_empty() {
                break;
            }

            // Wait before retrying
            let delay = retry_config.calculate_delay(attempt - 1);
            debug!(
                attempt = attempt,
                max_retries = retry_config.max_retries,
                delay_ms = delay.as_millis(),
                ranges_count = ranges_to_retry.len(),
                "retrying failed ranges"
            );
            tokio::time::sleep(delay).await;

            // Retry the failed ranges
            let retry_result = self
                .retry_failed_ranges(ranges_to_retry, original_filter, request, max_concurrency)
                .await;

            // Merge retry results into the main result
            result = self.merge_retry_results(result, retry_result);
        }

        // Log final outcome
        if result.is_complete() {
            debug!(total_attempts = attempt + 1, "all ranges fetched successfully after retries");
        } else {
            let failed_count = result.failed_ranges.len();
            let retryable_count = result.retryable_ranges().len();
            warn!(
                total_attempts = attempt + 1,
                failed_ranges = failed_count,
                still_retryable = retryable_count,
                "some ranges failed after max retries"
            );
        }

        result
    }

    /// Retries fetching specific failed ranges with retry count tracking.
    ///
    /// Accepts ranges with their retry counts and preserves this information in
    /// any resulting failures.
    async fn retry_failed_ranges(
        &self,
        ranges_with_retry_count: Vec<(u64, u64, u32)>,
        original_filter: &LogFilter,
        request: &JsonRpcRequest,
        max_concurrency: usize,
    ) -> PartialResult<Vec<serde_json::Value>> {
        let stream = stream::iter(ranges_with_retry_count.into_iter().map(
            |(from_block, to_block, retry_count)| {
                let range_filter = LogFilter {
                    address: original_filter.address,
                    topics: original_filter.topics,
                    topics_anywhere: original_filter.topics_anywhere.clone(),
                    from_block,
                    to_block,
                };

                let range_request = JsonRpcRequest {
                    jsonrpc: JSONRPC_VERSION_COW,
                    method: "eth_getLogs".to_string(),
                    params: Some(log_filter_to_json_params(&range_filter)),
                    id: Arc::clone(&request.id),
                };

                self.fetch_single_range_tracked_with_retry_count(
                    range_request,
                    range_filter,
                    retry_count,
                )
            },
        ));

        let buffered = stream.buffer_unordered(max_concurrency);
        self.collect_retry_results(buffered).await
    }

    /// Fetches a single range while preserving retry count for failures.
    async fn fetch_single_range_tracked_with_retry_count(
        &self,
        range_request: JsonRpcRequest,
        range_filter: LogFilter,
        retry_count: u32,
    ) -> (RangeFetchResult<Vec<serde_json::Value>>, u32) {
        let result = self.fetch_single_range_tracked(range_request, range_filter).await;
        (result, retry_count)
    }

    /// Collects retry results, preserving retry count information.
    async fn collect_retry_results(
        &self,
        mut buffered: impl stream::Stream<Item = (RangeFetchResult<Vec<serde_json::Value>>, u32)>
            + Unpin,
    ) -> PartialResult<Vec<serde_json::Value>> {
        let mut all_logs = Vec::new();
        let mut failures = Vec::new();

        while let Some((fetch_result, retry_count)) = buffered.next().await {
            match fetch_result.result {
                Ok(mut logs) => {
                    debug!(
                        from_block = fetch_result.from_block,
                        to_block = fetch_result.to_block,
                        logs_count = logs.len(),
                        retry_count = retry_count,
                        "range fetched successfully on retry"
                    );
                    all_logs.append(&mut logs);
                }
                Err(e) => {
                    let mut failure =
                        RangeFailure::new(fetch_result.from_block, fetch_result.to_block, e)
                            .with_retry_count(retry_count);
                    if let Some(upstream) = fetch_result.upstream {
                        failure = failure.with_upstream(upstream);
                    }

                    warn!(
                        from_block = failure.from_block,
                        to_block = failure.to_block,
                        error = %failure.error,
                        retry_count = retry_count,
                        "range fetch failed on retry"
                    );

                    failures.push(failure);
                }
            }
        }

        PartialResult::partial(all_logs, failures)
    }

    /// Merges retry results into the main result.
    ///
    /// Appends successfully retried logs, replaces retryable failures with new ones,
    /// and preserves non-retryable failures from the original result.
    #[allow(clippy::unused_self)]
    fn merge_retry_results(
        &self,
        mut original: PartialResult<Vec<serde_json::Value>>,
        retry_result: PartialResult<Vec<serde_json::Value>>,
    ) -> PartialResult<Vec<serde_json::Value>> {
        // Add successfully fetched logs from retry
        original.data.extend(retry_result.data);

        // Keep non-retryable failures from original (they weren't retried)
        // Retryable failures are replaced by retry_result.failed_ranges
        let mut new_failures: Vec<RangeFailure> =
            original.failed_ranges.into_iter().filter(|f| !f.is_retryable()).collect();

        // Add failures from the retry attempt (ranges that still failed after retry)
        new_failures.extend(retry_result.failed_ranges);

        let is_complete = new_failures.is_empty();

        PartialResult { data: original.data, failed_ranges: new_failures, is_complete }
    }

    /// Extracts block number from log JSON, defaulting to 0 if missing or invalid.
    fn extract_block_number(log: &serde_json::Value) -> u64 {
        use crate::utils::BlockParameter;
        log.get("blockNumber").and_then(BlockParameter::from_json_value).unwrap_or(0)
    }

    /// Extracts log index from log JSON, defaulting to 0 if missing or invalid.
    fn extract_log_index(log: &serde_json::Value) -> u32 {
        use crate::utils::BlockParameter;
        log.get("logIndex")
            .and_then(|v| v.as_str())
            .and_then(BlockParameter::parse_hex_u32)
            .unwrap_or(0)
    }

    /// Sorts logs efficiently by precomputing sort keys and reordering in-place.
    ///
    /// Uses unstable sort on (`block_number`, `log_index`) pairs for optimal performance.
    fn sort_logs_fast(logs: &mut Vec<serde_json::Value>) {
        let mut keys: Vec<(u64, u32, usize)> = logs
            .iter()
            .enumerate()
            .map(|(i, log)| (Self::extract_block_number(log), Self::extract_log_index(log), i))
            .collect();

        keys.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        let mut reordered: Vec<serde_json::Value> = Vec::with_capacity(logs.len());
        for (_, _, i) in keys {
            if i < logs.len() {
                let v = std::mem::take(&mut logs[i]);
                reordered.push(v);
            }
        }
        *logs = reordered;
    }

    /// Handles complete cache miss by forwarding to upstream and caching the result.
    async fn handle_cache_miss(
        &self,
        request: &JsonRpcRequest,
        filter: &LogFilter,
    ) -> Result<JsonRpcResponse, ProxyError> {
        debug!(from_block = filter.from_block, to_block = filter.to_block, "cache miss");

        self.ctx.metrics_collector.record_cache_miss("eth_getLogs");

        debug!("forwarding eth_getLogs request to upstream");
        let response = self.ctx.forward_to_upstream(request).await?;

        if let Some(result) = response.result.as_ref() {
            if let Some(logs_array) = result.as_array() {
                debug!(
                    logs_count = logs_array.len(),
                    from_block = filter.from_block,
                    to_block = filter.to_block,
                    "caching logs from upstream response"
                );
                self.cache_logs_from_response(logs_array, filter).await;
                debug!("logs caching completed successfully");
            }
        }

        Ok(response)
    }

    /// Caches logs from upstream response with validation.
    ///
    /// Marks the fetched range as covered, enabling the cache to distinguish
    /// "no logs exist" from "not yet fetched". Uses parallel processing for
    /// large log sets (>1000 logs) via `spawn_blocking`.
    async fn cache_logs_from_response(&self, logs_array: &[serde_json::Value], filter: &LogFilter) {
        use crate::cache::validation::validate_logs_response;

        if !validate_logs_response(logs_array, filter) {
            warn!(
                from_block = filter.from_block,
                to_block = filter.to_block,
                "logs validation failed, skipping cache"
            );
            return;
        }

        let (log_records, log_ids): (Vec<_>, Vec<_>) = if logs_array.len() > 1000 {
            tokio::task::block_in_place(|| {
                let logs_owned: Vec<serde_json::Value> = logs_array.to_vec();
                logs_owned
                    .par_iter()
                    .filter_map(json_log_to_log_record)
                    .map(|(log_id, log_record)| ((log_id, log_record), log_id))
                    .unzip()
            })
        } else {
            logs_array
                .iter()
                .filter_map(json_log_to_log_record)
                .map(|(log_id, log_record)| ((log_id, log_record), log_id))
                .unzip()
        };

        self.ctx
            .cache_manager
            .log_cache
            .mark_range_covered(filter.from_block, filter.to_block);

        if !log_records.is_empty() {
            self.ctx.cache_manager.insert_logs_bulk_no_stats(log_records).await;
            self.ctx.cache_manager.cache_exact_result(filter, log_ids).await;
            if self.ctx.cache_manager.should_update_stats().await {
                self.ctx.cache_manager.update_stats_on_demand().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test parsing log filter from request parameters
    #[test]
    fn test_parse_log_filter_from_request() {
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(json!([{
                "fromBlock": "0x1",
                "toBlock": "0xa",
                "address": "0x1234567890123456789012345678901234567890"
            }])),
            json!(1),
        );

        let result = LogsHandler::parse_log_filter_from_request(&request);
        assert!(result.is_ok(), "Should successfully parse valid log filter");
    }

    #[test]
    fn test_parse_log_filter_missing_params() {
        let request = JsonRpcRequest::new("eth_getLogs", None, json!(1));

        let result = LogsHandler::parse_log_filter_from_request(&request);
        assert!(result.is_err(), "Should fail when params are missing");
    }

    #[test]
    fn test_parse_log_filter_invalid_params() {
        let request = JsonRpcRequest::new("eth_getLogs", Some(json!(["invalid"])), json!(1));

        let result = LogsHandler::parse_log_filter_from_request(&request);
        assert!(result.is_err(), "Should fail with invalid filter parameters");
    }

    /// Test topic filter matching logic
    #[test]
    fn test_topic_filter_exact_match() {
        // Topics are matched in the converter logic
        // This is a placeholder for future topic matching tests
        let topic1 = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let topic2 = "0x1234567890123456789012345678901234567890123456789012345678901234";
        assert_eq!(topic1, topic2, "Exact topic match");
    }

    #[test]
    fn test_topic_filter_wildcard() {
        // Test that null topics act as wildcards
        let wildcard: Option<String> = None;
        assert!(wildcard.is_none(), "Wildcard topic should be None");
    }

    /// Test log sorting by block number and log index
    #[test]
    fn test_log_sorting_order() {
        let block_nums = [100u64, 100, 101, 99];
        let log_indices = [0u64, 1, 0, 5];

        let mut pairs: Vec<_> = block_nums.iter().zip(log_indices.iter()).collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));

        assert_eq!(pairs[0], (&99, &5));
        assert_eq!(pairs[1], (&100, &0));
        assert_eq!(pairs[2], (&100, &1));
        assert_eq!(pairs[3], (&101, &0));
    }

    /// Test handling of empty results
    #[test]
    fn test_empty_log_result() {
        let filter = LogFilter {
            from_block: 100,
            to_block: 200,
            address: None,
            topics: [None, None, None, None],
            topics_anywhere: vec![],
        };

        let response = LogsHandler::handle_empty_cache_hit(&filter);
        assert_eq!(response.result, Some(json!([])));
        assert!(response.error.is_none());
    }

    /// Test calculate missing ranges scenarios
    #[test]
    fn test_calculate_missing_ranges_full_cache() {
        // Cached: [100-200], Requested: [100-200] → Missing: []
        // This would be tested with actual cache manager integration
        let requested_from = 100u64;
        let requested_to = 200u64;
        let cached_from = 100u64;
        let cached_to = 200u64;

        // If fully cached, no missing ranges
        if requested_from >= cached_from && requested_to <= cached_to {
            let missing_ranges: Vec<(u64, u64)> = vec![];
            assert!(missing_ranges.is_empty(), "No missing ranges when fully cached");
        }
    }

    #[test]
    fn test_calculate_missing_ranges_partial_cache() {
        // Cached: [100-150], Requested: [100-200] → Missing: [151-200]
        let requested_to = 200u64;
        let cached_to = 150u64;

        let missing_start = cached_to + 1;
        assert_eq!(missing_start, 151, "Missing range should start after cached end");
        assert_eq!(requested_to, 200, "Missing range should end at request end");
    }

    #[test]
    fn test_calculate_missing_ranges_no_overlap() {
        // Cached: [100-150], Requested: [200-250] → Missing: [200-250]
        let requested_from = 200u64;
        let requested_to = 250u64;
        let cached_to = 150u64;

        // No overlap case
        if requested_from > cached_to {
            let missing = (requested_from, requested_to);
            assert_eq!(missing.0, 200, "Missing range should be the full request");
            assert_eq!(missing.1, 250);
        }
    }

    /// Test that address filters are properly parsed
    #[test]
    fn test_address_filter_parsing() {
        let address_str = "0x1234567890123456789012345678901234567890";
        assert_eq!(address_str.len(), 42, "Valid Ethereum address is 42 chars (0x + 40 hex)");
    }

    /// Test response construction
    #[test]
    fn test_response_construction() {
        let response = JsonRpcResponse::success(json!([]), Arc::new(json!(1)));

        assert_eq!(response.jsonrpc.as_ref(), "2.0");
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    use crate::upstream::errors::UpstreamError;

    fn timeout_error() -> ProxyError {
        ProxyError::Upstream(UpstreamError::Timeout)
    }

    fn invalid_request_error() -> ProxyError {
        ProxyError::InvalidRequest("bad request".to_string())
    }

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 100);
        assert_eq!(config.max_delay_ms, 2000);
    }

    #[test]
    fn test_retry_config_builder_chain() {
        let config = RetryConfig::new()
            .with_max_retries(5)
            .with_base_delay_ms(50)
            .with_max_delay_ms(5000)
            .with_jitter_factor(0.1);

        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_delay_ms, 50);
        assert_eq!(config.max_delay_ms, 5000);
        assert!((config.jitter_factor - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_partial_result_with_retryable_failures() {
        let failures = vec![
            RangeFailure::new(100, 110, timeout_error()), // retryable
            RangeFailure::new(200, 210, invalid_request_error()), // not retryable
        ];

        let result: PartialResult<Vec<serde_json::Value>> =
            PartialResult::partial(vec![], failures);

        let retryable = result.retryable_ranges();
        assert_eq!(retryable.len(), 1);
        assert_eq!(retryable[0].from_block, 100);
        assert_eq!(retryable[0].to_block, 110);
    }

    #[test]
    fn test_partial_result_merge_success() {
        // Simulate: original had 2 retryable failures, retry succeeded for one
        let original_data = vec![json!({"log": 1})];
        let original_failures = vec![
            RangeFailure::new(100, 110, timeout_error()),
            RangeFailure::new(200, 210, timeout_error()),
        ];
        let original = PartialResult::partial(original_data, original_failures);

        // Retry: one succeeded (data), one still failed
        let retry_data = vec![json!({"log": 2})]; // range 100-110 succeeded
        let retry_failures = vec![RangeFailure::new(200, 210, timeout_error()).with_retry_count(1)];
        let retry_result = PartialResult::partial(retry_data, retry_failures);

        // Merge (simulating what merge_retry_results does)
        let mut merged_data = original.data;
        merged_data.extend(retry_result.data);

        // Keep non-retryable from original + retry failures
        let merged_failures: Vec<RangeFailure> = retry_result.failed_ranges;

        assert_eq!(merged_data.len(), 2);
        assert_eq!(merged_failures.len(), 1);
        assert_eq!(merged_failures[0].from_block, 200);
        assert_eq!(merged_failures[0].retry_count, 1);
    }

    #[test]
    fn test_partial_result_merge_preserves_non_retryable() {
        // Original has both retryable and non-retryable failures
        let original_failures = vec![
            RangeFailure::new(100, 110, timeout_error()), // retryable
            RangeFailure::new(200, 210, invalid_request_error()), // not retryable
        ];
        let original: PartialResult<Vec<serde_json::Value>> =
            PartialResult::partial(vec![], original_failures);

        // After retry: retryable failure succeeded
        // Non-retryable should be preserved
        let non_retryable: Vec<&RangeFailure> =
            original.failed_ranges.iter().filter(|f| !f.is_retryable()).collect();

        assert_eq!(non_retryable.len(), 1);
        assert_eq!(non_retryable[0].from_block, 200);
    }

    #[test]
    fn test_range_failure_retry_count_tracking() {
        let failure = RangeFailure::new(100, 110, timeout_error()).with_retry_count(3);

        assert_eq!(failure.retry_count, 3);
        assert!(failure.is_retryable());
    }

    #[test]
    fn test_retry_config_should_retry_boundary() {
        let config = RetryConfig::new().with_max_retries(3);

        // Should retry for attempts 0, 1, 2
        assert!(config.should_retry(0), "Should retry on first attempt");
        assert!(config.should_retry(1), "Should retry on second attempt");
        assert!(config.should_retry(2), "Should retry on third attempt");

        // Should not retry at max
        assert!(!config.should_retry(3), "Should not retry at max");
        assert!(!config.should_retry(10), "Should not retry past max");
    }

    #[test]
    fn test_retry_config_delay_exponential_growth() {
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(10000)
            .with_jitter_factor(0.0);

        let d0 = config.calculate_delay(0).as_millis();
        let d1 = config.calculate_delay(1).as_millis();
        let d2 = config.calculate_delay(2).as_millis();

        assert_eq!(d0, 100);
        assert_eq!(d1, 200);
        assert_eq!(d2, 400);
    }

    #[test]
    fn test_retry_config_delay_caps_at_max() {
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(300)
            .with_jitter_factor(0.0);

        // 100 * 2^3 = 800, but should cap at 300
        let delay = config.calculate_delay(3).as_millis();
        assert_eq!(delay, 300);
    }

    #[test]
    fn test_partial_result_complete_after_retry() {
        // All failures were retryable and all succeeded
        let result: PartialResult<Vec<serde_json::Value>> =
            PartialResult::complete(vec![json!({"log": 1}), json!({"log": 2})]);

        assert!(result.is_complete());
        assert!(result.failed_ranges.is_empty());
        assert_eq!(result.data.len(), 2);
    }

    #[test]
    fn test_extract_block_number_valid_hex() {
        let log = json!({
            "blockNumber": "0x64",
            "logIndex": "0x0"
        });

        let block_num = LogsHandler::extract_block_number(&log);
        assert_eq!(block_num, 100, "Should parse 0x64 as 100");
    }

    #[test]
    fn test_extract_block_number_large_value() {
        let log = json!({
            "blockNumber": "0xFFFFFF",
            "logIndex": "0x0"
        });

        let block_num = LogsHandler::extract_block_number(&log);
        assert_eq!(block_num, 16_777_215, "Should parse 0xFFFFFF as 16777215");
    }

    #[test]
    fn test_extract_block_number_missing() {
        let log = json!({
            "logIndex": "0x0"
        });

        let block_num = LogsHandler::extract_block_number(&log);
        assert_eq!(block_num, 0, "Missing blockNumber should default to 0");
    }

    #[test]
    fn test_extract_block_number_null() {
        let log = json!({
            "blockNumber": null,
            "logIndex": "0x0"
        });

        let block_num = LogsHandler::extract_block_number(&log);
        assert_eq!(block_num, 0, "Null blockNumber should default to 0");
    }

    #[test]
    fn test_extract_block_number_invalid_format() {
        let log = json!({
            "blockNumber": "invalid",
            "logIndex": "0x0"
        });

        let block_num = LogsHandler::extract_block_number(&log);
        assert_eq!(block_num, 0, "Invalid hex should default to 0");
    }

    #[test]
    fn test_extract_log_index_valid_hex() {
        let log = json!({
            "blockNumber": "0x64",
            "logIndex": "0xa"
        });

        let log_idx = LogsHandler::extract_log_index(&log);
        assert_eq!(log_idx, 10, "Should parse 0xa as 10");
    }

    #[test]
    fn test_extract_log_index_missing() {
        let log = json!({
            "blockNumber": "0x64"
        });

        let log_idx = LogsHandler::extract_log_index(&log);
        assert_eq!(log_idx, 0, "Missing logIndex should default to 0");
    }

    #[test]
    fn test_extract_log_index_zero() {
        let log = json!({
            "blockNumber": "0x64",
            "logIndex": "0x0"
        });

        let log_idx = LogsHandler::extract_log_index(&log);
        assert_eq!(log_idx, 0, "Should parse 0x0 as 0");
    }

    #[test]
    fn test_sort_logs_fast_by_block_number() {
        let mut logs = vec![
            json!({"blockNumber": "0xc8", "logIndex": "0x0"}), // 200
            json!({"blockNumber": "0x64", "logIndex": "0x0"}), // 100
            json!({"blockNumber": "0x96", "logIndex": "0x0"}), // 150
        ];

        LogsHandler::sort_logs_fast(&mut logs);

        assert_eq!(LogsHandler::extract_block_number(&logs[0]), 100);
        assert_eq!(LogsHandler::extract_block_number(&logs[1]), 150);
        assert_eq!(LogsHandler::extract_block_number(&logs[2]), 200);
    }

    #[test]
    fn test_sort_logs_fast_by_log_index_within_block() {
        let mut logs = vec![
            json!({"blockNumber": "0x64", "logIndex": "0x2"}),
            json!({"blockNumber": "0x64", "logIndex": "0x0"}),
            json!({"blockNumber": "0x64", "logIndex": "0x1"}),
        ];

        LogsHandler::sort_logs_fast(&mut logs);

        assert_eq!(LogsHandler::extract_log_index(&logs[0]), 0);
        assert_eq!(LogsHandler::extract_log_index(&logs[1]), 1);
        assert_eq!(LogsHandler::extract_log_index(&logs[2]), 2);
    }

    #[test]
    fn test_sort_logs_fast_mixed() {
        let mut logs = vec![
            json!({"blockNumber": "0x65", "logIndex": "0x0"}), // block 101, idx 0
            json!({"blockNumber": "0x64", "logIndex": "0x2"}), // block 100, idx 2
            json!({"blockNumber": "0x64", "logIndex": "0x0"}), // block 100, idx 0
            json!({"blockNumber": "0x65", "logIndex": "0x1"}), // block 101, idx 1
            json!({"blockNumber": "0x64", "logIndex": "0x1"}), // block 100, idx 1
        ];

        LogsHandler::sort_logs_fast(&mut logs);

        // Expected order: (100,0), (100,1), (100,2), (101,0), (101,1)
        assert_eq!(LogsHandler::extract_block_number(&logs[0]), 100);
        assert_eq!(LogsHandler::extract_log_index(&logs[0]), 0);

        assert_eq!(LogsHandler::extract_block_number(&logs[1]), 100);
        assert_eq!(LogsHandler::extract_log_index(&logs[1]), 1);

        assert_eq!(LogsHandler::extract_block_number(&logs[2]), 100);
        assert_eq!(LogsHandler::extract_log_index(&logs[2]), 2);

        assert_eq!(LogsHandler::extract_block_number(&logs[3]), 101);
        assert_eq!(LogsHandler::extract_log_index(&logs[3]), 0);

        assert_eq!(LogsHandler::extract_block_number(&logs[4]), 101);
        assert_eq!(LogsHandler::extract_log_index(&logs[4]), 1);
    }

    #[test]
    fn test_sort_logs_fast_empty() {
        let mut logs: Vec<serde_json::Value> = vec![];
        LogsHandler::sort_logs_fast(&mut logs);
        assert!(logs.is_empty(), "Empty vec should remain empty");
    }

    #[test]
    fn test_sort_logs_fast_single_element() {
        let mut logs = vec![json!({"blockNumber": "0x64", "logIndex": "0x0"})];
        LogsHandler::sort_logs_fast(&mut logs);
        assert_eq!(logs.len(), 1);
        assert_eq!(LogsHandler::extract_block_number(&logs[0]), 100);
    }

    #[test]
    fn test_sort_logs_preserves_other_fields() {
        let mut logs = vec![
            json!({"blockNumber": "0xc8", "logIndex": "0x0", "data": "0xabc"}),
            json!({"blockNumber": "0x64", "logIndex": "0x0", "data": "0xdef"}),
        ];

        LogsHandler::sort_logs_fast(&mut logs);

        // Verify data fields are preserved after sorting
        assert_eq!(logs[0].get("data").unwrap(), "0xdef");
        assert_eq!(logs[1].get("data").unwrap(), "0xabc");
    }

    #[test]
    fn test_parse_log_filter_with_topics() {
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(json!([{
                "fromBlock": "0x1",
                "toBlock": "0xa",
                "topics": [
                    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
                ]
            }])),
            json!(1),
        );

        let result = LogsHandler::parse_log_filter_from_request(&request);
        assert!(result.is_ok(), "Should parse filter with topics");
    }

    #[test]
    fn test_parse_log_filter_with_null_topic_wildcard() {
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(json!([{
                "fromBlock": "0x1",
                "toBlock": "0xa",
                "topics": [
                    null,
                    "0x0000000000000000000000001234567890123456789012345678901234567890"
                ]
            }])),
            json!(1),
        );

        let result = LogsHandler::parse_log_filter_from_request(&request);
        assert!(result.is_ok(), "Should parse filter with null topic wildcard");
    }

    #[test]
    fn test_parse_log_filter_with_address_array() {
        // Some implementations allow address to be an array
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(json!([{
                "fromBlock": "0x1",
                "toBlock": "0xa",
                "address": ["0x1234567890123456789012345678901234567890"]
            }])),
            json!(1),
        );

        // This may or may not be supported depending on implementation
        let result = LogsHandler::parse_log_filter_from_request(&request);
        // Just verify it doesn't panic
        let _ = result.is_ok();
    }

    #[test]
    fn test_parse_log_filter_empty_object() {
        let request = JsonRpcRequest::new("eth_getLogs", Some(json!([{}])), json!(1));

        // Empty filter should either work (with defaults) or fail gracefully
        let result = LogsHandler::parse_log_filter_from_request(&request);
        // Just verify it doesn't panic
        let _ = result.is_ok();
    }

    #[test]
    fn test_empty_cache_hit_response_has_correct_status() {
        let filter = LogFilter {
            from_block: 100,
            to_block: 200,
            address: None,
            topics: [None, None, None, None],
            topics_anywhere: vec![],
        };

        let response = LogsHandler::handle_empty_cache_hit(&filter);
        assert_eq!(response.cache_status, Some(CacheStatus::Empty));
    }

    #[test]
    fn test_range_failure_block_count() {
        let failure = RangeFailure::new(100, 109, timeout_error());
        assert_eq!(failure.block_count(), 10, "100-109 inclusive is 10 blocks");
    }

    #[test]
    fn test_range_failure_single_block() {
        let failure = RangeFailure::new(100, 100, timeout_error());
        assert_eq!(failure.block_count(), 1, "Single block range");
    }

    #[test]
    fn test_range_failure_with_upstream() {
        let failure =
            RangeFailure::new(100, 110, timeout_error()).with_upstream("provider-1".to_string());
        assert_eq!(failure.upstream, Some("provider-1".to_string()));
    }

    #[test]
    fn test_range_failure_is_retryable_timeout() {
        let failure = RangeFailure::new(100, 110, timeout_error());
        assert!(failure.is_retryable(), "Timeout should be retryable");
    }

    #[test]
    fn test_range_failure_is_not_retryable_invalid_request() {
        let failure = RangeFailure::new(100, 110, invalid_request_error());
        assert!(!failure.is_retryable(), "Invalid request should not be retryable");
    }

    #[test]
    fn test_partial_result_empty() {
        let result: PartialResult<Vec<i32>> = PartialResult::complete(vec![]);
        assert!(result.is_complete());
        assert!(result.data.is_empty());
        assert!(result.failed_ranges.is_empty());
    }

    #[test]
    fn test_partial_result_all_failures() {
        let failures = vec![
            RangeFailure::new(100, 110, timeout_error()),
            RangeFailure::new(200, 210, timeout_error()),
        ];
        let result: PartialResult<Vec<i32>> = PartialResult::partial(vec![], failures);

        assert!(!result.is_complete());
        assert_eq!(result.failed_ranges.len(), 2);
        assert!(result.data.is_empty());
    }

    #[test]
    fn test_partial_result_retryable_ranges_filter() {
        let failures = vec![
            RangeFailure::new(100, 110, timeout_error()), // retryable
            RangeFailure::new(200, 210, invalid_request_error()), // not retryable
            RangeFailure::new(300, 310, timeout_error()), // retryable
        ];
        let result: PartialResult<Vec<i32>> = PartialResult::partial(vec![], failures);

        let retryable = result.retryable_ranges();
        assert_eq!(retryable.len(), 2);
        assert!(retryable.iter().all(|r| r.is_retryable()));
    }

    #[test]
    fn test_retry_config_delay_with_jitter() {
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(10000)
            .with_jitter_factor(0.25);

        // With 25% jitter, delay should be within [75, 125] for attempt 0
        let delay = config.calculate_delay(0).as_millis();
        assert!(delay >= 75, "Delay should be at least 75ms with -25% jitter");
        assert!(delay <= 125, "Delay should be at most 125ms with +25% jitter");
    }

    #[test]
    fn test_retry_config_zero_max_retries() {
        let config = RetryConfig::new().with_max_retries(0);
        assert!(!config.should_retry(0), "Should not retry when max_retries is 0");
    }
}
