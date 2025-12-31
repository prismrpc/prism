use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::Instant};

use crate::{
    cache::{
        converter::{
            json_block_to_block_body, json_block_to_block_header, json_log_to_log_record,
            json_receipt_to_receipt_record, json_transaction_to_transaction_record,
        },
        reorg_manager::ReorgManager,
        CacheManager, FetchGuard,
    },
    types::UpstreamConfig,
    upstream::http_client::HttpClient,
    utils::hex_buffer::{parse_hex_array, parse_hex_u64, with_hex_u64},
};

use super::errors::UpstreamError;

/// Tracks WebSocket connection failures to avoid endless retry loops.
///
/// Implements exponential backoff and permanent failure detection
/// after repeated connection failures.
#[derive(Debug)]
pub struct WebSocketFailureTracker {
    consecutive_failures: u32,
    last_failure_time: Instant,
    max_consecutive_failures: u32,
    failure_reset_duration: Duration,
    permanently_failed: bool,
    last_reset_time: Instant,
}

impl Default for WebSocketFailureTracker {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: Instant::now(),
            max_consecutive_failures: 3,
            failure_reset_duration: Duration::from_secs(300),
            permanently_failed: false,
            last_reset_time: Instant::now(),
        }
    }
}

impl WebSocketFailureTracker {
    /// Records a WebSocket connection failure.
    ///
    /// Increments the consecutive failure count and marks the connection
    /// as permanently failed if threshold is exceeded.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure_time = Instant::now();

        if self.consecutive_failures >= self.max_consecutive_failures * 2 {
            self.permanently_failed = true;
        }
    }

    /// Records a successful WebSocket connection.
    ///
    /// Resets the consecutive failure count and permanent failure flag.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.permanently_failed = false;
    }

    /// Returns whether WebSocket connection retries should be stopped.
    ///
    /// Stops retrying if permanently failed or if consecutive failures
    /// exceed the threshold within the reset duration window.
    #[must_use]
    pub fn should_stop_retrying(&self) -> bool {
        if self.permanently_failed {
            return true;
        }

        if self.consecutive_failures >= self.max_consecutive_failures {
            if self.last_failure_time.elapsed() >= self.failure_reset_duration {
                return false;
            }
            return true;
        }
        false
    }

    /// Resets failure tracking if the reset duration has elapsed.
    ///
    /// Clears consecutive failures and permanent failure flag after
    /// the configured cooldown period.
    pub fn reset_if_expired(&mut self) {
        if self.consecutive_failures >= self.max_consecutive_failures &&
            self.last_failure_time.elapsed() >= self.failure_reset_duration
        {
            self.consecutive_failures = 0;
            self.permanently_failed = false;
            self.last_reset_time = Instant::now();
        }
    }

    /// Returns the current consecutive failure count.
    #[must_use]
    pub fn get_failure_count(&self) -> u32 {
        self.consecutive_failures
    }

    /// Returns whether this connection is permanently failed.
    #[must_use]
    pub fn is_permanently_failed(&self) -> bool {
        self.permanently_failed
    }
}

/// WebSocket subscription handler for chain head notifications.
///
/// Manages WebSocket connections to upstream RPC providers, subscribing
/// to `newHeads` events to proactively cache new blocks and detect reorgs.
pub struct WebSocketHandler {
    config: UpstreamConfig,
    http_client: Arc<HttpClient>,
    failure_tracker: Arc<RwLock<WebSocketFailureTracker>>,
}

impl WebSocketHandler {
    /// Creates a new WebSocket handler for the given upstream configuration.
    #[must_use]
    pub fn new(config: UpstreamConfig, http_client: Arc<HttpClient>) -> Self {
        Self {
            config,
            http_client,
            failure_tracker: Arc::new(RwLock::new(WebSocketFailureTracker::default())),
        }
    }

    /// Subscribes to `newHeads` events via WebSocket.
    ///
    /// Establishes a WebSocket connection, sends the subscription request,
    /// and processes incoming block notifications to update the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails, subscription fails, or
    /// the WebSocket stream encounters an error.
    pub async fn subscribe_to_new_heads(
        &self,
        cache_manager: Arc<CacheManager>,
        reorg_manager: Arc<ReorgManager>,
    ) -> Result<(), UpstreamError> {
        let ws_url = self.validate_and_get_ws_url()?;
        let ws_stream = self.connect_websocket(ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        self.send_subscription_message(&mut write).await?;
        self.handle_websocket_messages(&mut read, cache_manager, reorg_manager).await?;

        Ok(())
    }

    /// Returns whether a WebSocket subscription attempt should be made.
    ///
    /// Resets expired failure tracking and checks if retry limits allow
    /// another connection attempt.
    pub async fn should_attempt_websocket_subscription(&self) -> bool {
        let mut tracker = self.failure_tracker.write().await;
        tracker.reset_if_expired();
        !tracker.should_stop_retrying()
    }

    /// Records a WebSocket subscription failure.
    ///
    /// Increments failure count and logs appropriate warnings based on
    /// failure severity.
    pub async fn record_websocket_failure(&self) {
        let mut tracker = self.failure_tracker.write().await;
        tracker.record_failure();
        let failure_count = tracker.get_failure_count();

        if tracker.is_permanently_failed() {
            tracing::warn!(
                upstream = %self.config.name,
                "websocket subscription failed immediately after reset, marking as permanently failed"
            );
        } else if failure_count >= tracker.max_consecutive_failures {
            tracing::warn!(
                upstream = %self.config.name,
                failure_count = failure_count,
                retry_delay_secs = tracker.failure_reset_duration.as_secs(),
                "websocket subscription failed, stopping retries"
            );
        } else {
            tracing::debug!(
                upstream = %self.config.name,
                failure_count = failure_count,
                "websocket subscription failed"
            );
        }
    }

    /// Records a successful WebSocket subscription.
    ///
    /// Resets failure tracking and logs success.
    pub async fn record_websocket_success(&self) {
        let mut tracker = self.failure_tracker.write().await;
        tracker.record_success();
        tracing::debug!(
            upstream = %self.config.name,
            "websocket subscription succeeded, reset failure count"
        );
    }

    /// Validates and extracts the WebSocket URL from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if no WebSocket URL is configured, the URL is empty,
    /// or the URL format is invalid (must start with `ws://` or `wss://`).
    fn validate_and_get_ws_url(&self) -> Result<&str, UpstreamError> {
        let ws_url = self
            .config
            .ws_url
            .as_ref()
            .ok_or(UpstreamError::InvalidResponse("No WebSocket URL configured".to_string()))?;

        if ws_url.trim().is_empty() {
            return Err(UpstreamError::InvalidResponse("WebSocket URL is empty".to_string()));
        }

        if !ws_url.starts_with("ws://") && !ws_url.starts_with("wss://") {
            return Err(UpstreamError::InvalidResponse(format!(
                "Invalid WebSocket URL format: {ws_url}"
            )));
        }

        Ok(ws_url)
    }

    /// Establishes a WebSocket connection to the upstream provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails. Provides specific error
    /// messages for common failure cases like unsupported WebSocket protocol,
    /// method not allowed, or forbidden access.
    async fn connect_websocket(
        &self,
        ws_url: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        UpstreamError,
    > {
        tracing::info!(ws_url = ws_url, upstream = %self.config.name, "connecting to websocket");

        let result = tokio_tungstenite::connect_async(ws_url).await;

        match result {
            Ok((ws_stream, response)) => {
                tracing::info!(
                    upstream = %self.config.name,
                    status = response.status().as_u16(),
                    "websocket connected successfully"
                );
                Ok(ws_stream)
            }
            Err(e) => {
                let error_msg = e.to_string();
                tracing::error!(
                    upstream = %self.config.name,
                    error = %e,
                    "websocket connection failed"
                );

                if error_msg.contains("HTTP error: 200 OK") {
                    Err(UpstreamError::ConnectionFailed(format!(
                        "Server returned 200 OK but does not support WebSocket protocol for upstream {}",
                        self.config.name
                    )))
                } else if error_msg.contains("HTTP error: 405") {
                    Err(UpstreamError::ConnectionFailed(format!(
                        "WebSocket method not allowed for upstream {} (405 Method Not Allowed)",
                        self.config.name
                    )))
                } else if error_msg.contains("HTTP error: 403") {
                    Err(UpstreamError::ConnectionFailed(format!(
                        "WebSocket access forbidden for upstream {} (403 Forbidden)",
                        self.config.name
                    )))
                } else {
                    Err(UpstreamError::ConnectionFailed(format!(
                        "WebSocket connection failed: {e}"
                    )))
                }
            }
        }
    }

    /// Sends the `eth_subscribe` message to subscribe to `newHeads`.
    ///
    /// # Errors
    ///
    /// Returns an error if sending the subscription message fails.
    async fn send_subscription_message(
        &self,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
    ) -> Result<(), UpstreamError> {
        use tokio_tungstenite::tungstenite::Message;

        let subscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["newHeads"]
        });

        tracing::debug!(
            upstream = %self.config.name,
            message = %subscribe_msg,
            "sending subscription message"
        );

        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await
            .map_err(|e| UpstreamError::ConnectionFailed(format!("WebSocket send error: {e}")))?;

        tracing::debug!(upstream = %self.config.name, "subscription message sent successfully");
        Ok(())
    }

    /// Processes incoming WebSocket messages in a loop.
    ///
    /// Handles text messages, close frames, and errors. Dispatches
    /// text messages for further processing.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` even if the stream ends or encounters errors,
    /// as this is expected behavior for long-running subscriptions.
    async fn handle_websocket_messages(
        &self,
        read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        cache_manager: Arc<CacheManager>,
        reorg_manager: Arc<ReorgManager>,
    ) -> Result<(), UpstreamError> {
        use tokio_tungstenite::tungstenite::Message;

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    self.process_text_message(&text, &cache_manager, &reorg_manager);
                }
                Ok(Message::Close(_)) => {
                    tracing::warn!(upstream = %self.config.name, "websocket connection closed");
                    break;
                }
                Err(e) => {
                    tracing::error!(
                        upstream = %self.config.name,
                        error = %e,
                        "websocket error"
                    );
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Processes a text message received from the WebSocket.
    ///
    /// Parses the JSON and dispatches to either subscription confirmation
    /// or block notification handlers.
    fn process_text_message(
        &self,
        text: &str,
        cache_manager: &Arc<CacheManager>,
        reorg_manager: &Arc<ReorgManager>,
    ) {
        tracing::debug!(upstream = %self.config.name, message = text, "received websocket message");

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            tracing::debug!(
                upstream = %self.config.name,
                json = ?json,
                "parsed json"
            );

            if self.handle_subscription_confirmation(&json) {
                return;
            }

            self.handle_new_heads_notification(&json, cache_manager, reorg_manager);
        } else {
            tracing::warn!(upstream = %self.config.name, message = text, "failed to parse json");
        }
    }

    /// Handles subscription confirmation messages.
    ///
    /// Returns `true` if this message was a subscription confirmation,
    /// `false` otherwise.
    fn handle_subscription_confirmation(&self, json: &serde_json::Value) -> bool {
        if let Some(result) = json.get("result") {
            if result.is_string() {
                tracing::info!(
                    upstream = %self.config.name,
                    subscription_id = %result,
                    "subscription confirmed"
                );
                return true;
            }
        }
        false
    }

    /// Handles `newHeads` notification messages.
    ///
    /// Extracts block data from the notification and processes it.
    fn handle_new_heads_notification(
        &self,
        json: &serde_json::Value,
        cache_manager: &Arc<CacheManager>,
        reorg_manager: &Arc<ReorgManager>,
    ) {
        if let Some(params) = json.get("params") {
            if let Some(result) = params.get("result") {
                tracing::debug!(
                    upstream = %self.config.name,
                    result = ?result,
                    "processing result"
                );

                self.process_block_data(result, cache_manager, reorg_manager);
            } else {
                tracing::debug!(upstream = %self.config.name, "no result found in params");
            }
        } else {
            tracing::debug!(upstream = %self.config.name, "no params found in message");
        }
    }

    /// Processes block data from a `newHeads` notification.
    ///
    /// Extracts block number and hash, updates the chain tip if newer,
    /// and spawns background tasks to fetch and cache the full block.
    fn process_block_data(
        &self,
        result: &serde_json::Value,
        cache_manager: &Arc<CacheManager>,
        reorg_manager: &Arc<ReorgManager>,
    ) {
        if let (Some(number), Some(hash)) = (
            result.get("number").and_then(|v| v.as_str()),
            result.get("hash").and_then(|v| v.as_str()),
        ) {
            tracing::debug!(
                upstream = %self.config.name,
                block_number = number,
                block_hash = hash,
                "extracted block data"
            );

            if let (Some(block_number), Some(block_hash)) =
                (parse_hex_u64(number), parse_hex_array::<32>(hash))
            {
                let cache_manager_clone = cache_manager.clone();
                let reorg_manager_clone = reorg_manager.clone();
                let upstream_name = Arc::clone(&self.config.name);

                tokio::spawn(async move {
                    Self::update_chain_tip_if_newer(
                        block_number,
                        block_hash,
                        &cache_manager_clone,
                        &reorg_manager_clone,
                        &upstream_name,
                    )
                    .await;
                });

                let cache_manager_clone = cache_manager.clone();
                let config_name = Arc::clone(&self.config.name);
                let config_url = self.config.url.clone();
                let http_client = self.http_client.clone();

                tokio::spawn(async move {
                    if let Some(guard) = cache_manager_clone
                        .begin_fetch_with_timeout(block_number, Duration::from_secs(30))
                        .await
                    {
                        tracing::debug!(
                            block_number = block_number,
                            upstream = %config_name,
                            "acquired fetch lock, fetching block"
                        );
                        Self::fetch_and_cache_full_block(
                            block_number,
                            &cache_manager_clone,
                            guard,
                            &config_name,
                            &config_url,
                            &http_client,
                        );
                    } else {
                        tracing::debug!(
                            block_number = block_number,
                            upstream = %config_name,
                            "block already cached or being fetched, skipping"
                        );
                    }
                });
            } else {
                tracing::warn!(upstream = %self.config.name, "failed to parse block number or hash");
            }
        } else {
            tracing::debug!(upstream = %self.config.name, "no block number or hash found in result");
        }
    }

    /// Updates the chain tip and notifies the reorg manager of new blocks.
    ///
    /// Handles three scenarios:
    /// 1. **Newer block**: Normal progression, update tip
    /// 2. **Same height, different hash**: Reorg detected at tip
    /// 3. **Older block**: Potential chain rollback (e.g., from `debug_setHead`)
    ///
    /// The `ReorgManager` handles the logic of detecting actual reorgs and
    /// invalidating affected cache entries.
    async fn update_chain_tip_if_newer(
        block_number: u64,
        block_hash: [u8; 32],
        cache_manager: &Arc<CacheManager>,
        reorg_manager: &Arc<ReorgManager>,
        upstream_name: &str,
    ) {
        let current_tip = cache_manager.get_current_tip();

        // Always notify reorg manager - it needs to handle ALL cases:
        // - Newer block: normal progression
        // - Same height: potential reorg (different hash)
        // - Older block: potential rollback (chain went backwards)
        //
        // Previously we only notified for >= current_tip, but this missed
        // rollbacks where debug_setHead causes the chain to go backwards.
        reorg_manager.update_tip(block_number, block_hash).await;

        // Check if cleanup should be triggered for newer blocks
        if block_number > current_tip {
            cache_manager.check_and_trigger_cleanup(block_number);
        }

        // Log appropriately based on what happened
        if block_number >= current_tip {
            tracing::info!(
                block_number = block_number,
                block_hash = ?block_hash,
                upstream = upstream_name,
                current_tip = current_tip,
                is_same_height = block_number == current_tip,
                "websocket: processing block notification"
            );
        } else {
            // This is important - receiving an older block via WebSocket
            // indicates the chain may have rolled back
            tracing::warn!(
                block_number = block_number,
                block_hash = ?block_hash,
                upstream = upstream_name,
                current_tip = current_tip,
                rollback_depth = current_tip - block_number,
                "websocket: received older block (potential rollback)"
            );
        }
    }

    /// Spawns a background task to fetch and cache full block data.
    ///
    /// Acquires block header, body, transactions, and receipts from the
    /// upstream provider and stores them in the cache.
    fn fetch_and_cache_full_block(
        block_number: u64,
        cache_manager: &Arc<CacheManager>,
        guard: FetchGuard,
        upstream_name: &str,
        config_url: &str,
        http_client: &Arc<HttpClient>,
    ) {
        let upstream_name = upstream_name.to_string();
        let cache_manager_clone = cache_manager.clone();
        let http_client = http_client.clone();
        let config_url = config_url.to_string();

        tokio::spawn(async move {
            let _guard = guard;
            tracing::debug!(
                block_number = block_number,
                upstream = upstream_name,
                "fetching full block data"
            );

            if let Err(e) = Self::do_fetch_and_cache_block(
                &upstream_name,
                &config_url,
                &http_client,
                block_number,
                &cache_manager_clone,
            )
            .await
            {
                tracing::warn!(
                    block_number = block_number,
                    upstream = upstream_name,
                    error = e,
                    "failed to fully cache block"
                );
            }
        });
    }

    /// Fetches and caches a complete block from the upstream provider.
    ///
    /// Retrieves the block with transactions, then fetches receipts separately.
    async fn do_fetch_and_cache_block(
        upstream_name: &str,
        url: &str,
        http_client: &Arc<HttpClient>,
        block_number: u64,
        cache_manager: &Arc<CacheManager>,
    ) -> Result<(), String> {
        let block_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBlockByNumber",
            "params": [with_hex_u64(block_number, std::string::ToString::to_string), true]
        });

        tracing::debug!(
            block_number = block_number,
            upstream = upstream_name,
            request = ?block_req,
            "sending eth_getBlockByNumber with fullTransactions=true"
        );

        let block_response = Self::send_json_request(
            http_client,
            url,
            block_req,
            Duration::from_secs(10),
            upstream_name,
            "block",
        )
        .await
        .ok_or_else(|| "no block response".to_string())?;

        let Some(block_data) = block_response.get("result") else {
            return Err("no result in block response".to_string());
        };

        // Log transaction field structure to debug caching issues
        if let Some(tx_field) = block_data.get("transactions") {
            let tx_type = if tx_field.is_array() {
                if let Some(arr) = tx_field.as_array() {
                    if arr.is_empty() {
                        "empty_array".to_string()
                    } else if let Some(first) = arr.first() {
                        if first.is_string() {
                            format!("array_of_hashes (count: {})", arr.len())
                        } else if first.is_object() {
                            format!("array_of_objects (count: {})", arr.len())
                        } else {
                            format!("array_of_unknown (count: {})", arr.len())
                        }
                    } else {
                        "empty_array".to_string()
                    }
                } else {
                    "unexpected_type: not_array".to_string()
                }
            } else if tx_field.is_null() {
                "null".to_string()
            } else {
                format!("unexpected_type: {tx_field:?}")
            };

            tracing::info!(
                block_number = block_number,
                upstream = upstream_name,
                transactions_type = tx_type,
                "received block response with transactions field"
            );
        } else {
            tracing::warn!(
                block_number = block_number,
                upstream = upstream_name,
                "block response has no transactions field at all"
            );
        }

        Self::cache_block_data(block_number, upstream_name, cache_manager, block_data).await;

        Self::fetch_and_cache_receipts_for_block(
            http_client,
            url,
            block_number,
            upstream_name,
            cache_manager,
        )
        .await;

        tracing::debug!(
            block_number = block_number,
            upstream = upstream_name,
            "fully cached block via websocket"
        );
        Ok(())
    }

    /// Sends a JSON-RPC request and returns the parsed response.
    ///
    /// Returns `None` if the request fails, serialization fails, or an RPC error occurs.
    async fn send_json_request(
        http_client: &Arc<HttpClient>,
        url: &str,
        req: serde_json::Value,
        timeout: Duration,
        upstream_name: &str,
        what: &str,
    ) -> Option<serde_json::Value> {
        let body = match serde_json::to_vec(&req) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    what = what,
                    error = %e,
                    "failed to serialize request"
                );
                return None;
            }
        };

        let resp_bytes =
            match http_client.send_request(url, bytes::Bytes::from(body), timeout).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(
                        what = what,
                        upstream = upstream_name,
                        error = %e,
                        "failed to fetch"
                    );
                    return None;
                }
            };

        let json: serde_json::Value = match serde_json::from_slice(&resp_bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    what = what,
                    upstream = upstream_name,
                    error = %e,
                    "failed to parse response"
                );
                return None;
            }
        };

        if let Some(error) = json.get("error") {
            tracing::warn!(
                what = what,
                upstream = upstream_name,
                error = ?error,
                "rpc error"
            );
            return None;
        }

        Some(json)
    }

    /// Caches block header, body, and transactions from block data.
    async fn cache_block_data(
        block_number: u64,
        upstream_name: &str,
        cache_manager: &Arc<CacheManager>,
        block_data: &serde_json::Value,
    ) {
        if let Some(header) = json_block_to_block_header(block_data) {
            cache_manager.insert_header(header).await;
            tracing::debug!(block_number = block_number, upstream = upstream_name, "cached header");
        }

        if let Some(body) = json_block_to_block_body(block_data) {
            cache_manager.insert_body(body).await;
            tracing::debug!(block_number = block_number, upstream = upstream_name, "cached body");
        }

        // Check if transactions field exists
        match block_data.get("transactions") {
            None => {
                tracing::warn!(
                    block_number = block_number,
                    upstream = upstream_name,
                    "block data has no 'transactions' field"
                );
            }
            Some(tx_value) => {
                // Check if it's an array
                match tx_value.as_array() {
                    None => {
                        tracing::warn!(
                            block_number = block_number,
                            upstream = upstream_name,
                            tx_value_type = ?tx_value,
                            "transactions field is not an array (possibly null or transaction hashes)"
                        );
                    }
                    Some(transactions) => {
                        if transactions.is_empty() {
                            tracing::debug!(
                                block_number = block_number,
                                upstream = upstream_name,
                                "block has no transactions (empty block)"
                            );
                        } else {
                            let mut cached_count = 0;
                            let mut failed_count = 0;

                            // Check first transaction to see if we got hashes or objects
                            if let Some(first_tx) = transactions.first() {
                                if first_tx.is_string() {
                                    tracing::error!(
                                        block_number = block_number,
                                        upstream = upstream_name,
                                        transaction_count = transactions.len(),
                                        first_tx = ?first_tx,
                                        "BUG: eth_getBlockByNumber returned transaction HASHES instead of full objects! \
                                         This means fullTransactions=true parameter is not working correctly."
                                    );
                                    return;
                                }
                            }

                            for tx_json in transactions {
                                if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
                                    cache_manager.transaction_cache.insert_transaction(tx).await;
                                    cached_count += 1;
                                } else {
                                    failed_count += 1;
                                    tracing::debug!(
                                        block_number = block_number,
                                        upstream = upstream_name,
                                        tx_json = ?tx_json,
                                        "failed to convert transaction to record"
                                    );
                                }
                            }

                            if failed_count > 0 {
                                tracing::warn!(
                                    block_number = block_number,
                                    upstream = upstream_name,
                                    total_count = transactions.len(),
                                    cached_count = cached_count,
                                    failed_count = failed_count,
                                    "some transactions failed to convert (check for EIP-1559+ support or missing fields)"
                                );
                            } else if cached_count > 0 {
                                tracing::debug!(
                                    block_number = block_number,
                                    upstream = upstream_name,
                                    transaction_count = cached_count,
                                    "cached transactions"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fetches and caches receipts and logs for a block.
    ///
    /// Makes an `eth_getBlockReceipts` request and caches both receipts
    /// and extracted logs. Logs performance metrics for conversion and insertion.
    async fn fetch_and_cache_receipts_for_block(
        http_client: &Arc<HttpClient>,
        url: &str,
        block_number: u64,
        upstream_name: &str,
        cache_manager: &Arc<CacheManager>,
    ) {
        let receipts_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBlockReceipts",
            "params": [with_hex_u64(block_number, std::string::ToString::to_string)]
        });

        let Some(receipts_response) = Self::send_json_request(
            http_client,
            url,
            receipts_req,
            Duration::from_secs(10),
            upstream_name,
            "receipts",
        )
        .await
        else {
            return;
        };

        if let Some(receipts_array) = receipts_response.get("result").and_then(|r| r.as_array()) {
            let mut log_records: Vec<(crate::cache::types::LogId, crate::cache::types::LogRecord)> =
                Vec::new();
            let mut receipts_vec: Vec<crate::cache::types::ReceiptRecord> =
                Vec::with_capacity(receipts_array.len());

            let t_conv_start = Instant::now();
            for receipt_json in receipts_array {
                if let Some(logs) = receipt_json.get("logs").and_then(|v| v.as_array()) {
                    for log in logs {
                        if let Some((log_id, log_record)) = json_log_to_log_record(log) {
                            log_records.push((log_id, log_record));
                        }
                    }
                }
                if let Some(receipt) = json_receipt_to_receipt_record(receipt_json) {
                    receipts_vec.push(receipt);
                }
            }
            let conv_ms = t_conv_start.elapsed().as_millis();
            tracing::debug!(
                block_number = block_number,
                log_count = log_records.len(),
                conversion_ms = conv_ms,
                "converted logs from receipts"
            );

            tracing::debug!(
                block_number = block_number,
                upstream = upstream_name,
                receipt_count = receipts_array.len(),
                "cached receipts"
            );

            let logs_fut = async {
                if log_records.is_empty() {
                    None
                } else {
                    let t_insert = Instant::now();
                    cache_manager.insert_logs_bulk_no_stats(log_records).await;
                    let insert_ms = t_insert.elapsed().as_millis();

                    let mut stats_ms: u128 = 0;
                    if cache_manager.should_update_stats().await {
                        let t_stats = Instant::now();
                        cache_manager.update_stats_on_demand().await;
                        stats_ms = t_stats.elapsed().as_millis();
                    }
                    Some((insert_ms, stats_ms))
                }
            };

            let receipts_fut = async {
                if receipts_vec.is_empty() {
                    None
                } else {
                    let t_insert = Instant::now();
                    cache_manager.insert_receipts_bulk_no_stats(receipts_vec).await;
                    let insert_ms = t_insert.elapsed().as_millis();

                    let t_stats = Instant::now();
                    cache_manager.transaction_cache.update_stats_on_demand().await;
                    let stats_ms = t_stats.elapsed().as_millis();

                    Some((insert_ms, stats_ms))
                }
            };

            let (logs_result, receipts_result) = tokio::join!(logs_fut, receipts_fut);

            if let Some((insert_ms, stats_ms)) = logs_result {
                tracing::debug!(
                    block_number = block_number,
                    insert_ms = insert_ms,
                    stats_ms = stats_ms,
                    "inserted logs"
                );
            }
            if let Some((insert_ms, stats_ms)) = receipts_result {
                tracing::debug!(
                    block_number = block_number,
                    insert_ms = insert_ms,
                    stats_ms = stats_ms,
                    "inserted receipts"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_failure_tracker_default() {
        let tracker = WebSocketFailureTracker::default();
        assert_eq!(tracker.get_failure_count(), 0);
        assert!(!tracker.is_permanently_failed());
        assert!(!tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_records_single_failure() {
        let mut tracker = WebSocketFailureTracker::default();
        tracker.record_failure();
        assert_eq!(tracker.get_failure_count(), 1);
        assert!(!tracker.is_permanently_failed());
        assert!(!tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_records_multiple_failures() {
        let mut tracker = WebSocketFailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();
        tracker.record_failure();
        assert_eq!(tracker.get_failure_count(), 3);
        // At max_consecutive_failures (3), should stop retrying
        assert!(tracker.should_stop_retrying());
        // But not permanently failed until 2x threshold (6)
        assert!(!tracker.is_permanently_failed());
    }

    #[test]
    fn test_failure_tracker_marks_permanent_after_threshold() {
        let mut tracker = WebSocketFailureTracker::default();
        // Default max_consecutive_failures is 3, so permanent after 6
        for _ in 0..6 {
            tracker.record_failure();
        }
        assert_eq!(tracker.get_failure_count(), 6);
        assert!(tracker.is_permanently_failed());
        assert!(tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_success_resets_count() {
        let mut tracker = WebSocketFailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();
        assert_eq!(tracker.get_failure_count(), 2);

        tracker.record_success();
        assert_eq!(tracker.get_failure_count(), 0);
        assert!(!tracker.is_permanently_failed());
        assert!(!tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_success_clears_permanent_failure() {
        let mut tracker = WebSocketFailureTracker::default();
        // Reach permanent failure
        for _ in 0..6 {
            tracker.record_failure();
        }
        assert!(tracker.is_permanently_failed());

        // Success should clear it
        tracker.record_success();
        assert!(!tracker.is_permanently_failed());
        assert_eq!(tracker.get_failure_count(), 0);
    }

    #[test]
    fn test_failure_tracker_should_stop_at_threshold() {
        let mut tracker = WebSocketFailureTracker::default();

        // Below threshold - should not stop
        tracker.record_failure();
        tracker.record_failure();
        assert!(!tracker.should_stop_retrying());

        // At threshold - should stop
        tracker.record_failure();
        assert!(tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_reset_if_expired_below_threshold() {
        let mut tracker = WebSocketFailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();

        // Below threshold, reset_if_expired should not change anything
        tracker.reset_if_expired();
        assert_eq!(tracker.get_failure_count(), 2);
    }

    #[test]
    fn test_failure_tracker_reset_if_expired_at_threshold_not_expired() {
        let mut tracker = WebSocketFailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();
        tracker.record_failure();

        // At threshold but not expired (just happened)
        tracker.reset_if_expired();
        assert_eq!(tracker.get_failure_count(), 3);
        assert!(tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_permanent_failure_stops_retrying() {
        let tracker = WebSocketFailureTracker { permanently_failed: true, ..Default::default() };

        assert!(tracker.should_stop_retrying());
    }

    #[test]
    fn test_failure_tracker_custom_threshold() {
        let mut tracker = WebSocketFailureTracker {
            consecutive_failures: 0,
            last_failure_time: Instant::now(),
            max_consecutive_failures: 5,
            failure_reset_duration: Duration::from_secs(60),
            permanently_failed: false,
            last_reset_time: Instant::now(),
        };

        // Should not stop until 5 failures
        for i in 1..5 {
            tracker.record_failure();
            assert!(!tracker.should_stop_retrying(), "Should not stop at {i} failures");
        }

        tracker.record_failure();
        assert!(tracker.should_stop_retrying());

        // Should not be permanent until 10 failures (2 * 5)
        assert!(!tracker.is_permanently_failed());

        for _ in 0..5 {
            tracker.record_failure();
        }
        assert!(tracker.is_permanently_failed());
    }

    fn create_test_handler(ws_url: Option<String>) -> WebSocketHandler {
        let config = UpstreamConfig {
            name: Arc::from("test-upstream"),
            url: "http://localhost:8545".to_string(),
            ws_url,
            chain_id: 1,
            weight: 100,
            timeout_seconds: 30,
            supports_websocket: true,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
        };
        let http_client = Arc::new(HttpClient::new().unwrap());
        WebSocketHandler::new(config, http_client)
    }

    #[test]
    fn test_validate_ws_url_valid_ws() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "ws://localhost:8545");
    }

    #[test]
    fn test_validate_ws_url_valid_wss() {
        let handler = create_test_handler(Some("wss://mainnet.infura.io/ws/v3/key".to_string()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "wss://mainnet.infura.io/ws/v3/key");
    }

    #[test]
    fn test_validate_ws_url_none() {
        let handler = create_test_handler(None);
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_err());
        match result {
            Err(UpstreamError::InvalidResponse(msg)) => {
                assert!(msg.contains("No WebSocket URL configured"));
            }
            _ => panic!("Expected InvalidResponse error"),
        }
    }

    #[test]
    fn test_validate_ws_url_empty() {
        let handler = create_test_handler(Some(String::new()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_err());
        match result {
            Err(UpstreamError::InvalidResponse(msg)) => {
                assert!(msg.contains("empty"));
            }
            _ => panic!("Expected InvalidResponse error"),
        }
    }

    #[test]
    fn test_validate_ws_url_whitespace_only() {
        let handler = create_test_handler(Some("   ".to_string()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_err());
        match result {
            Err(UpstreamError::InvalidResponse(msg)) => {
                assert!(msg.contains("empty"));
            }
            _ => panic!("Expected InvalidResponse error"),
        }
    }

    #[test]
    fn test_validate_ws_url_http_protocol() {
        let handler = create_test_handler(Some("http://localhost:8545".to_string()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_err());
        match result {
            Err(UpstreamError::InvalidResponse(msg)) => {
                assert!(msg.contains("Invalid WebSocket URL format"));
            }
            _ => panic!("Expected InvalidResponse error"),
        }
    }

    #[test]
    fn test_validate_ws_url_https_protocol() {
        let handler = create_test_handler(Some("https://localhost:8545".to_string()));
        let result = handler.validate_and_get_ws_url();
        assert!(result.is_err());
        match result {
            Err(UpstreamError::InvalidResponse(msg)) => {
                assert!(msg.contains("Invalid WebSocket URL format"));
            }
            _ => panic!("Expected InvalidResponse error"),
        }
    }

    #[test]
    fn test_handle_subscription_confirmation_valid() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": "0x9ce59a13059e417087c02d3236a0b9cc"
        });

        assert!(handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_not_string_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"number": "0x100"}
        });

        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_no_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });

        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_null_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": null
        });

        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[tokio::test]
    async fn test_should_attempt_websocket_subscription_initial() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        assert!(handler.should_attempt_websocket_subscription().await);
    }

    #[tokio::test]
    async fn test_should_attempt_websocket_subscription_after_failures() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));

        // Record failures up to threshold
        for _ in 0..3 {
            handler.record_websocket_failure().await;
        }

        // Should not attempt after reaching threshold
        assert!(!handler.should_attempt_websocket_subscription().await);
    }

    #[tokio::test]
    async fn test_record_websocket_success_resets_failures() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));

        // Record some failures
        handler.record_websocket_failure().await;
        handler.record_websocket_failure().await;

        // Record success
        handler.record_websocket_success().await;

        // Should be able to attempt again
        assert!(handler.should_attempt_websocket_subscription().await);

        // Verify failure count is reset
        let tracker = handler.failure_tracker.read().await;
        assert_eq!(tracker.get_failure_count(), 0);
    }

    #[tokio::test]
    async fn test_failure_tracker_concurrent_access() {
        let ws_handler = Arc::new(create_test_handler(Some("ws://localhost:8545".to_string())));
        let mut join_handles = vec![];

        // Spawn multiple tasks recording failures concurrently
        for _ in 0..10 {
            let handler_clone = ws_handler.clone();
            join_handles.push(tokio::spawn(async move {
                handler_clone.record_websocket_failure().await;
            }));
        }

        for handle in join_handles {
            handle.await.unwrap();
        }

        // All failures should be recorded
        let tracker = ws_handler.failure_tracker.read().await;
        assert_eq!(tracker.get_failure_count(), 10);
    }

    #[test]
    fn test_handle_subscription_confirmation_array_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": ["0x1", "0x2"]
        });

        // Array result should not be treated as subscription confirmation
        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_number_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": 12345
        });

        // Numeric result should not be treated as subscription confirmation
        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_boolean_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": true
        });

        // Boolean result should not be treated as subscription confirmation
        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_empty_string_result() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": ""
        });

        assert!(handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_handle_subscription_confirmation_error_response() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32600,
                "message": "Invalid request"
            }
        });

        assert!(!handler.handle_subscription_confirmation(&json));
    }

    #[test]
    fn test_new_heads_message_structure_valid() {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0x9ce59a13059e417087c02d3236a0b9cc",
                "result": {
                    "number": "0x1234",
                    "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                    "parentHash": "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                    "timestamp": "0x5f5e100"
                }
            }
        });

        assert!(json.get("params").is_some());
        let params = json.get("params").unwrap();
        assert!(params.get("result").is_some());
        let result = params.get("result").unwrap();
        assert!(result.get("number").is_some());
        assert!(result.get("hash").is_some());
    }

    #[test]
    fn test_new_heads_message_missing_params() {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription"
        });

        assert!(json.get("params").is_none());
    }

    #[test]
    fn test_new_heads_message_missing_result_in_params() {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0x123"
            }
        });

        let params = json.get("params").unwrap();
        assert!(params.get("result").is_none());
    }

    #[test]
    fn test_new_heads_message_null_result() {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0x123",
                "result": null
            }
        });

        let params = json.get("params").unwrap();
        assert!(params.get("result").unwrap().is_null());
    }

    #[test]
    fn test_block_data_extraction_valid() {
        let result = serde_json::json!({
            "number": "0x100",
            "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
        });

        let number = result.get("number").and_then(|v| v.as_str());
        let hash = result.get("hash").and_then(|v| v.as_str());

        assert_eq!(number, Some("0x100"));
        assert!(hash.is_some());
        assert!(hash.unwrap().starts_with("0x"));
        assert_eq!(hash.unwrap().len(), 66);
    }

    #[test]
    fn test_block_data_extraction_missing_number() {
        let result = serde_json::json!({
            "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
        });

        let number = result.get("number").and_then(|v| v.as_str());
        assert!(number.is_none());
    }

    #[test]
    fn test_block_data_extraction_missing_hash() {
        let result = serde_json::json!({
            "number": "0x100"
        });

        let hash = result.get("hash").and_then(|v| v.as_str());
        assert!(hash.is_none());
    }

    #[test]
    fn test_block_data_extraction_number_as_integer() {
        let result = serde_json::json!({
            "number": 256,
            "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
        });

        let number = result.get("number").and_then(|v| v.as_str());
        assert!(number.is_none());
    }

    #[test]
    fn test_block_data_extraction_null_values() {
        let result = serde_json::json!({
            "number": null,
            "hash": null
        });

        let number = result.get("number").and_then(|v| v.as_str());
        let hash = result.get("hash").and_then(|v| v.as_str());

        assert!(number.is_none());
        assert!(hash.is_none());
    }

    #[test]
    fn test_parse_hex_u64_valid_block_numbers() {
        use crate::utils::hex_buffer::parse_hex_u64;

        assert_eq!(parse_hex_u64("0x0"), Some(0));
        assert_eq!(parse_hex_u64("0x1"), Some(1));
        assert_eq!(parse_hex_u64("0x100"), Some(256));
        assert_eq!(parse_hex_u64("0xff"), Some(255));
        assert_eq!(parse_hex_u64("0xFFFFFFFF"), Some(4_294_967_295));
        assert_eq!(parse_hex_u64("0x10000000000"), Some(1_099_511_627_776));
    }

    #[test]
    fn test_parse_hex_u64_invalid_formats() {
        use crate::utils::hex_buffer::parse_hex_u64;

        assert_eq!(parse_hex_u64(""), None);
        assert_eq!(parse_hex_u64("0x"), None);
        assert_eq!(parse_hex_u64("100"), Some(256));
        assert_eq!(parse_hex_u64("0xGHIJ"), None);
        assert_eq!(parse_hex_u64("not_hex"), None);
        assert_eq!(parse_hex_u64("xyz"), None);
    }

    #[test]
    fn test_parse_hex_u64_with_leading_zeros() {
        use crate::utils::hex_buffer::parse_hex_u64;

        assert_eq!(parse_hex_u64("0x00100"), Some(256));
        assert_eq!(parse_hex_u64("0x000000001"), Some(1));
    }

    #[test]
    fn test_parse_hex_array_valid_32_byte_hash() {
        use crate::utils::hex_buffer::parse_hex_array;

        let hash = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let result: Option<[u8; 32]> = parse_hex_array(hash);
        assert!(result.is_some());

        let bytes = result.unwrap();
        assert_eq!(bytes[0], 0x12);
        assert_eq!(bytes[1], 0x34);
        assert_eq!(bytes[31], 0xef);
    }

    #[test]
    fn test_parse_hex_array_invalid_length() {
        use crate::utils::hex_buffer::parse_hex_array;

        let short_hash = "0x1234";
        let result: Option<[u8; 32]> = parse_hex_array(short_hash);
        assert!(result.is_none());

        let long_hash = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef00";
        let result: Option<[u8; 32]> = parse_hex_array(long_hash);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_hex_array_invalid_format() {
        use crate::utils::hex_buffer::parse_hex_array;

        let no_prefix = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let result: Option<[u8; 32]> = parse_hex_array(no_prefix);
        assert!(result.is_some());

        let invalid_chars = "0xGHIJ567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let result: Option<[u8; 32]> = parse_hex_array(invalid_chars);
        assert!(result.is_none());

        let empty: Option<[u8; 32]> = parse_hex_array("");
        assert!(empty.is_none());
    }

    #[test]
    fn test_handler_stores_config_correctly() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        assert_eq!(&*handler.config.name, "test-upstream");
        assert_eq!(handler.config.url, "http://localhost:8545");
        assert_eq!(handler.config.ws_url, Some("ws://localhost:8545".to_string()));
        assert!(handler.config.supports_websocket);
    }

    #[test]
    fn test_handler_without_websocket_support() {
        let config = UpstreamConfig {
            name: Arc::from("no-ws-upstream"),
            url: "http://localhost:8545".to_string(),
            ws_url: None,
            chain_id: 1,
            weight: 100,
            timeout_seconds: 30,
            supports_websocket: false,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
        };
        let http_client = Arc::new(HttpClient::new().unwrap());
        let handler = WebSocketHandler::new(config, http_client);

        assert!(!handler.config.supports_websocket);
        assert!(handler.config.ws_url.is_none());
    }

    #[tokio::test]
    async fn test_failure_tracker_initial_state() {
        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        let tracker = handler.failure_tracker.read().await;

        assert_eq!(tracker.get_failure_count(), 0);
        assert!(!tracker.is_permanently_failed());
        assert!(!tracker.should_stop_retrying());
    }

    #[test]
    fn test_subscription_request_format() {
        let expected_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["newHeads"]
        });

        assert_eq!(expected_request["method"], "eth_subscribe");
        assert!(expected_request["params"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("newHeads")));
    }

    #[test]
    fn test_subscription_response_parsing() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": "0x9ce59a13059e417087c02d3236a0b9cc"
        });

        let handler = create_test_handler(Some("ws://localhost:8545".to_string()));
        assert!(handler.handle_subscription_confirmation(&response));
    }

    #[test]
    fn test_new_heads_with_full_block_data() {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0x9ce59a13059e417087c02d3236a0b9cc",
                "result": {
                    "baseFeePerGas": "0x7",
                    "difficulty": "0x0",
                    "extraData": "0x",
                    "gasLimit": "0x1c9c380",
                    "gasUsed": "0x0",
                    "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                    "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
                    "miner": "0x0000000000000000000000000000000000000000",
                    "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "nonce": "0x0000000000000000",
                    "number": "0x1234",
                    "parentHash": "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                    "receiptsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
                    "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
                    "stateRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "timestamp": "0x5f5e100",
                    "transactionsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
                }
            }
        });

        let params = notification.get("params").unwrap();
        let result = params.get("result").unwrap();

        assert_eq!(result.get("number").and_then(|v| v.as_str()), Some("0x1234"));
        assert!(result.get("hash").and_then(|v| v.as_str()).is_some());
        assert!(result.get("parentHash").and_then(|v| v.as_str()).is_some());
        assert!(result.get("timestamp").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn test_new_heads_minimal_block_data() {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0xabc",
                "result": {
                    "number": "0x1",
                    "hash": "0x0000000000000000000000000000000000000000000000000000000000000001"
                }
            }
        });

        let params = notification.get("params").unwrap();
        let result = params.get("result").unwrap();

        assert!(result.get("number").and_then(|v| v.as_str()).is_some());
        assert!(result.get("hash").and_then(|v| v.as_str()).is_some());
    }
}
