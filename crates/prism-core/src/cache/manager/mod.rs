//! Cache manager sub-modules.
//!
//! This module contains the extracted components of the cache manager:
//! - `background`: Background task implementations for cache maintenance
//! - `config`: Configuration and error types
//! - `fetch_guard`: RAII guard for fetch coordination
//! - `utilities`: Helper functions for block parsing and diagnostics
//! - `validation`: Log validation against cached block headers

pub mod background;
pub mod config;
pub mod fetch_guard;
pub mod utilities;
pub mod validation;

// Re-export commonly used types
pub use config::{CacheManagerConfig, CacheManagerError};
pub(crate) use fetch_guard::CleanupRequest;
pub use fetch_guard::{FetchGuard, InflightFetch};

#[cfg(test)]
mod tests;
