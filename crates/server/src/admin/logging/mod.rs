//! Logging infrastructure for the admin API.
//!
//! Provides a log buffer for capturing and querying recent log entries
//! through the admin API, along with a tracing layer for capturing logs.

pub mod buffer;
pub mod layer;

pub use buffer::LogBuffer;
pub use layer::LogBufferLayer;
