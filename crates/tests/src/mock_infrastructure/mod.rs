//! Mock Infrastructure for Testing Prism RPC Aggregator
//!
//! This module provides reusable mock types for testing upstream interactions
//! without requiring real network connections.
//!
//! ## Components
//!
//! - `RpcMockBuilder`: Wraps mockito to provide Ethereum-specific RPC mocking
//! - `MockWebSocketServer`: Provides a mock WebSocket server for subscription testing
//! - Test helpers for common scenarios
//!
//! ## Usage
//!
//! ```ignore
//! use tests::mock_infrastructure::{RpcMockBuilder, BlockResponseBuilder};
//!
//! let mut mock = RpcMockBuilder::new().await;
//! mock.mock_get_block(100, BlockResponseBuilder::new(100).build());
//!
//! // Use mock.url() to connect your client
//! ```

pub mod rpc_mock;
pub mod test_helpers;
pub mod websocket_mock;

pub use rpc_mock::{BlockResponseBuilder, LogResponseBuilder, RpcMockBuilder};
pub use test_helpers::*;
pub use websocket_mock::MockWebSocketServer;
