use std::sync::Arc;

use crate::{
    cache::CacheManager,
    metrics::MetricsCollector,
    types::{is_method_allowed, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
    upstream::{manager::UpstreamManager, LoadBalancerStats},
};

use super::{
    errors::ProxyError,
    handlers::{BlocksHandler, LogsHandler, TransactionsHandler},
};

/// Shared context for all proxy handlers.
///
/// Reduces Arc cloning overhead by grouping commonly shared references.
/// All handlers receive a single Arc<SharedContext> instead of individual
/// Arc references, reducing memory allocation and improving initialization efficiency.
#[derive(Clone)]
pub struct SharedContext {
    pub cache_manager: Arc<CacheManager>,
    pub upstream_manager: Arc<UpstreamManager>,
    pub metrics_collector: Arc<MetricsCollector>,
}

impl SharedContext {
    /// Forwards a request to upstream and marks the response as cache miss.
    ///
    /// JSON-RPC errors (e.g., "block not found") are forwarded as `Ok(JsonRpcResponse)`
    /// with the error field populated. Network/infrastructure failures are returned as
    /// `Err(ProxyError::Upstream)`.
    ///
    /// # Errors
    ///
    /// Returns `ProxyError::Upstream` if the upstream request fails due to network issues
    /// or all upstreams being unavailable.
    pub async fn forward_to_upstream(
        &self,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        use crate::{
            types::{CacheStatus, JsonRpcError},
            upstream::errors::UpstreamError,
        };

        let response = self.upstream_manager.send_request_auto(request).await;

        match response {
            Ok(mut response) => {
                response.cache_status = Some(CacheStatus::Miss);
                Ok(response)
            }
            Err(UpstreamError::RpcError(code, message)) => {
                self.metrics_collector.record_upstream_error_typed(
                    "upstream",
                    &UpstreamError::RpcError(code, message.clone()),
                );
                Ok(JsonRpcResponse {
                    jsonrpc: JSONRPC_VERSION_COW,
                    result: None,
                    error: Some(JsonRpcError { code, message, data: None }),
                    id: Arc::clone(&request.id),
                    cache_status: Some(CacheStatus::Miss),
                })
            }
            Err(e) => {
                self.metrics_collector.record_upstream_error_typed("upstream", &e);
                Err(e.into())
            }
        }
    }
}

/// Aggregated cache statistics across all cache types.
///
/// Combines entries from block caches (headers and bodies), transaction caches
/// (transactions and receipts), and the log store to provide a high-level view
/// of cache utilization.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of block headers and bodies currently cached
    pub block_cache_entries: usize,
    /// Number of transaction records and receipts currently cached
    pub transaction_cache_entries: usize,
    /// Number of log entries currently cached in the log store
    pub logs_cache_entries: usize,
}

/// Core proxy engine for processing Ethereum JSON-RPC requests.
///
/// Routes incoming requests to specialized handlers for cached methods
/// (`eth_getLogs`, `eth_getBlockByNumber`, etc.) or forwards them directly
/// to upstream providers. Thread-safe and designed for concurrent use via
/// shared `Arc` references to managers and collectors.
pub struct ProxyEngine {
    ctx: Arc<SharedContext>,
    logs_handler: LogsHandler,
    blocks_handler: BlocksHandler,
    transactions_handler: TransactionsHandler,
}

impl ProxyEngine {
    /// Creates a new proxy engine with the provided managers and collector.
    ///
    /// Initializes specialized handlers for logs, blocks, and transactions using
    /// a shared context pattern.
    #[must_use]
    pub fn new(
        cache_manager: Arc<CacheManager>,
        upstream_manager: Arc<UpstreamManager>,
        metrics_collector: Arc<MetricsCollector>,
    ) -> Self {
        let ctx = Arc::new(SharedContext { cache_manager, upstream_manager, metrics_collector });

        let logs_handler = LogsHandler::new(Arc::clone(&ctx));
        let blocks_handler = BlocksHandler::new(Arc::clone(&ctx));
        let transactions_handler = TransactionsHandler::new(Arc::clone(&ctx));

        Self { ctx, logs_handler, blocks_handler, transactions_handler }
    }

    /// Processes an incoming JSON-RPC request with validation and routing.
    ///
    /// Validates the request structure and checks if the method is allowed before
    /// routing to the appropriate handler or upstream provider.
    ///
    /// # Errors
    ///
    /// Returns [`ProxyError::Validation`] if the request fails validation (missing
    /// required fields, invalid format, etc.) or [`ProxyError::MethodNotSupported`]
    /// if the method is not in the allowed list.
    pub async fn process_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let method = &request.method;

        if let Err(validation_error) = request.validate() {
            self.ctx.metrics_collector.record_validation_error(method, &validation_error);
            return Err(ProxyError::Validation(validation_error));
        }

        if !is_method_allowed(method) {
            let error = ProxyError::MethodNotSupported(method.clone());
            self.ctx.metrics_collector.record_proxy_error(method, &error);
            return Err(error);
        }

        self.handle_request(request).await
    }

    /// Routes the request to the appropriate handler based on method name.
    ///
    /// Methods with caching support (`eth_getLogs`, `eth_getBlockByNumber`, etc.)
    /// are routed to specialized handlers. All other methods are forwarded directly
    /// to upstream providers without caching.
    async fn handle_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError> {
        match request.method.as_str() {
            "eth_getLogs" => self.logs_handler.handle_advanced_logs_request(request).await,
            "eth_getBlockByNumber" => {
                self.blocks_handler.handle_block_by_number_request(request).await
            }
            "eth_getBlockByHash" => self.blocks_handler.handle_block_by_hash_request(request).await,
            "eth_getTransactionByHash" => {
                self.transactions_handler.handle_transaction_by_hash_request(request).await
            }
            "eth_getTransactionReceipt" => {
                self.transactions_handler.handle_transaction_receipt_request(request).await
            }
            _ => self.ctx.forward_to_upstream(&request).await,
        }
    }

    /// Checks if the given RPC method is supported by the proxy.
    ///
    /// Delegates to the centralized allow list in `types::is_method_allowed`.
    /// This is a convenience method for external callers who want to check
    /// method support without importing the types module.
    #[must_use]
    pub fn is_method_supported(method: &str) -> bool {
        is_method_allowed(method)
    }

    /// Retrieves aggregated cache statistics from the cache manager.
    ///
    /// Combines detailed cache metrics into high-level categories for easier
    /// monitoring and reporting. Block entries include both headers and bodies,
    /// while transaction entries include both transactions and receipts.
    pub async fn get_cache_stats(&self) -> CacheStats {
        let comprehensive_stats = self.ctx.cache_manager.get_stats().await;

        CacheStats {
            block_cache_entries: comprehensive_stats.header_cache_size +
                comprehensive_stats.body_cache_size,
            transaction_cache_entries: comprehensive_stats.transaction_cache_size +
                comprehensive_stats.receipt_cache_size,
            logs_cache_entries: comprehensive_stats.log_store_size,
        }
    }

    /// Retrieves load balancer statistics from the upstream manager.
    ///
    /// Provides metrics about upstream provider selection, request distribution,
    /// health status, and response times across all configured providers.
    pub async fn get_upstream_stats(&self) -> LoadBalancerStats {
        self.ctx.upstream_manager.get_stats().await
    }

    /// Returns a reference to the cache manager.
    ///
    /// # Note
    ///
    /// This method exposes internal implementation details and is primarily
    /// intended for testing. Production code should use the public API methods
    /// like [`get_cache_stats`](Self::get_cache_stats) instead.
    #[doc(hidden)]
    #[must_use]
    pub fn get_cache_manager(&self) -> &Arc<CacheManager> {
        &self.ctx.cache_manager
    }

    /// Returns a reference to the metrics collector.
    ///
    /// Used by the server layer for:
    /// - Exposing Prometheus metrics at the `/metrics` endpoint
    /// - Recording batch request metrics
    /// - Custom metric collection in external integrations
    #[must_use]
    pub fn get_metrics_collector(&self) -> &Arc<MetricsCollector> {
        &self.ctx.metrics_collector
    }
}
