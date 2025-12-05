//! Mock WebSocket Server for Testing
//!
//! Provides a configurable WebSocket server for testing subscription behavior
//! without requiring a real Ethereum node.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::RwLock,
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

/// A mock WebSocket server for testing.
pub struct MockWebSocketServer {
    addr: SocketAddr,
    message_queue: Arc<RwLock<VecDeque<Message>>>,
    received_messages: Arc<RwLock<Vec<String>>>,
    server_handle: JoinHandle<()>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl MockWebSocketServer {
    /// Creates a new mock WebSocket server on a random available port.
    ///
    /// # Errors
    ///
    /// Returns an error if the server cannot bind to a local port or retrieve the bound address.
    pub async fn new() -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let message_queue = Arc::new(RwLock::new(VecDeque::new()));
        let received_messages = Arc::new(RwLock::new(Vec::new()));
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);

        let server_handle = Self::spawn_server(
            listener,
            message_queue.clone(),
            received_messages.clone(),
            shutdown_tx.subscribe(),
        );

        Ok(Self { addr, message_queue, received_messages, server_handle, shutdown_tx })
    }

    fn spawn_server(
        listener: TcpListener,
        message_queue: Arc<RwLock<VecDeque<Message>>>,
        received_messages: Arc<RwLock<Vec<String>>>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        if let Ok((stream, _)) = result {
                            let queue = message_queue.clone();
                            let received = received_messages.clone();
                            tokio::spawn(Self::handle_connection(stream, queue, received));
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        })
    }

    async fn handle_connection(
        stream: TcpStream,
        message_queue: Arc<RwLock<VecDeque<Message>>>,
        received_messages: Arc<RwLock<Vec<String>>>,
    ) {
        let Ok(ws_stream) = accept_async(stream).await else { return };

        let (mut write, mut read) = ws_stream.split();

        loop {
            // Send any queued messages
            {
                let mut queue = message_queue.write().await;
                while let Some(msg) = queue.pop_front() {
                    if write.send(msg).await.is_err() {
                        return;
                    }
                }
            }

            // Process incoming messages with timeout
            tokio::select! {
                Some(result) = read.next() => {
                    match result {
                        Ok(Message::Text(text)) => {
                            received_messages.write().await.push(text.to_string());
                        }
                        Ok(Message::Close(_)) | Err(_) => {
                            break;
                        }
                        _ => ()
                    }
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    /// Returns the WebSocket URL for connecting to this server.
    #[must_use]
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Enqueues a message to be sent to connected clients.
    pub async fn enqueue_message(&self, msg: Message) {
        self.message_queue.write().await.push_back(msg);
    }

    /// Enqueues a text message to be sent to connected clients.
    pub async fn enqueue_text(&self, text: impl Into<String>) {
        self.enqueue_message(Message::Text(text.into().into())).await;
    }

    /// Sends a subscription confirmation message.
    pub async fn send_subscription_confirmation(&self, subscription_id: &str) {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": subscription_id
        });
        self.enqueue_text(msg.to_string()).await;
    }

    /// Sends a `newHeads` notification.
    pub async fn send_new_heads(&self, block_number: u64, block_hash: &str) {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": "0x9ce59a13059e417087c02d3236a0b9cc",
                "result": {
                    "number": format!("0x{:x}", block_number),
                    "hash": block_hash,
                    "parentHash": format!("0x{:064x}", block_number.saturating_sub(1)),
                    "timestamp": format!("0x{:x}", 1_600_000_000 + block_number),
                    "gasLimit": "0x1c9c380",
                    "gasUsed": "0x0",
                    "baseFeePerGas": "0x7"
                }
            }
        });
        self.enqueue_text(msg.to_string()).await;
    }

    /// Sends a complete `newHeads` notification with full block data.
    pub async fn send_new_heads_full(&self, block_number: u64) {
        let block_hash = format!("0x{block_number:064x}");
        self.send_new_heads(block_number, &block_hash).await;
    }

    /// Sends a close frame to disconnect clients.
    pub async fn send_close(&self) {
        self.enqueue_message(Message::Close(None)).await;
    }

    /// Waits for a subscription request to be received.
    pub async fn wait_for_subscription(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            let received = self.received_messages.read().await;
            if received.iter().any(|msg| msg.contains("eth_subscribe")) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Returns all received messages.
    pub async fn get_received_messages(&self) -> Vec<String> {
        self.received_messages.read().await.clone()
    }

    /// Clears all received messages.
    pub async fn clear_received_messages(&self) {
        self.received_messages.write().await.clear();
    }

    /// Shuts down the server.
    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        self.server_handle.abort();
    }
}

impl Drop for MockWebSocketServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        self.server_handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;

    #[tokio::test]
    async fn test_mock_websocket_server_creation() {
        let server = MockWebSocketServer::new().await.unwrap();
        assert!(server.url().starts_with("ws://127.0.0.1:"));
    }

    #[tokio::test]
    async fn test_mock_websocket_server_connection() {
        let server = MockWebSocketServer::new().await.unwrap();
        let url = server.url();

        // Connect to the server
        let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
        drop(ws_stream);
    }

    #[tokio::test]
    async fn test_mock_websocket_send_and_receive() {
        let server = MockWebSocketServer::new().await.unwrap();
        let url = server.url();

        // Queue a message before connecting
        server.send_subscription_confirmation("0xabc123").await;

        // Connect and receive the message
        let (mut ws_stream, _) = connect_async(&url).await.expect("Failed to connect");

        // Give the server time to send the message
        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(Ok(msg)) = ws_stream.next().await {
            if let Message::Text(text) = msg {
                assert!(text.contains("0xabc123"));
            } else {
                panic!("Expected text message");
            }
        }
    }

    #[tokio::test]
    async fn test_mock_websocket_new_heads() {
        let server = MockWebSocketServer::new().await.unwrap();

        // Queue a newHeads notification
        server.send_new_heads_full(1000).await;

        let url = server.url();
        let (mut ws_stream, _) = connect_async(&url).await.expect("Failed to connect");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(Ok(Message::Text(text))) = ws_stream.next().await {
            assert!(text.contains("eth_subscription"));
            assert!(text.contains("0x3e8")); // 1000 in hex
        }
    }

    #[tokio::test]
    async fn test_mock_websocket_receives_client_messages() {
        let server = MockWebSocketServer::new().await.unwrap();
        let url = server.url();

        let (mut ws_stream, _) = connect_async(&url).await.expect("Failed to connect");

        // Send a subscription request
        let sub_request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["newHeads"]
        });

        ws_stream.send(Message::Text(sub_request.to_string().into())).await.unwrap();

        // Wait for the server to receive it
        assert!(server.wait_for_subscription(Duration::from_secs(2)).await);
    }
}
