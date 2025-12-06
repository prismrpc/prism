//! Logging infrastructure for the admin API.
//!
//! Provides a log buffer for capturing and querying recent log entries
//! through the admin API.

pub mod buffer;

pub use buffer::LogBuffer;
