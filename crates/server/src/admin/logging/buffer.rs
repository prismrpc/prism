//! Log buffer for storing recent log entries.
//!
//! Provides a ring buffer implementation for capturing and querying log entries
//! with filtering capabilities. This allows the admin API to query recent logs
//! without requiring external log aggregation infrastructure.

#![allow(clippy::missing_errors_doc)]

use std::collections::{HashSet, VecDeque};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::admin::types::{LogEntry, LogQueryParams, LogQueryResponse};

impl LogEntry {
    /// Creates a new log entry.
    #[must_use]
    pub fn new(level: String, target: String, message: String, fields: serde_json::Value) -> Self {
        Self { timestamp: Utc::now().to_rfc3339(), level, target, message, fields }
    }

    /// Parses timestamp from the entry.
    fn timestamp_parsed(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// Checks if the entry matches the given filters.
    fn matches(&self, params: &LogQueryParams) -> bool {
        // Filter by level
        if let Some(ref level_filter) = params.level {
            if !self.level.eq_ignore_ascii_case(level_filter) {
                return false;
            }
        }

        // Filter by target
        if let Some(ref target_filter) = params.target {
            if !self.target.contains(target_filter) {
                return false;
            }
        }

        // Filter by search (in message)
        if let Some(ref search) = params.search {
            let search_lower = search.to_lowercase();
            if !self.message.to_lowercase().contains(&search_lower) {
                return false;
            }
        }

        // Filter by time range
        if let Some(timestamp) = self.timestamp_parsed() {
            if let Some(ref from_str) = params.from {
                if let Ok(from) = DateTime::parse_from_rfc3339(from_str) {
                    if timestamp < from.with_timezone(&Utc) {
                        return false;
                    }
                }
            }

            if let Some(ref to_str) = params.to {
                if let Ok(to) = DateTime::parse_from_rfc3339(to_str) {
                    if timestamp > to.with_timezone(&Utc) {
                        return false;
                    }
                }
            }
        }

        true
    }
}

/// Ring buffer for storing log entries.
///
/// This buffer maintains a fixed-size deque of log entries, automatically
/// evicting the oldest entries when the maximum size is reached. It provides
/// efficient querying with support for filtering by level, target, time range,
/// and text search.
pub struct LogBuffer {
    entries: RwLock<VecDeque<LogEntry>>,
    max_size: usize,
}

impl LogBuffer {
    /// Creates a new log buffer with the specified maximum size.
    ///
    /// # Arguments
    ///
    /// * `max_size` - Maximum number of log entries to retain.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self { entries: RwLock::new(VecDeque::with_capacity(max_size)), max_size }
    }

    /// Creates a log buffer with sample data for testing/demo purposes.
    #[cfg(test)]
    #[must_use]
    pub fn with_sample_data(max_size: usize) -> Self {
        let buffer = Self::new(max_size);

        // Add some sample log entries
        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism_core::proxy".to_string(),
            "Request processed successfully".to_string(),
            serde_json::json!({"method": "eth_blockNumber", "duration_ms": 45}),
        ));

        buffer.push(LogEntry::new(
            "warn".to_string(),
            "prism_core::upstream".to_string(),
            "Upstream response time exceeded threshold".to_string(),
            serde_json::json!({"upstream": "alchemy", "latency_ms": 1200}),
        ));

        buffer.push(LogEntry::new(
            "error".to_string(),
            "prism_core::cache".to_string(),
            "Cache eviction failed".to_string(),
            serde_json::json!({"reason": "lock timeout"}),
        ));

        buffer.push(LogEntry::new(
            "debug".to_string(),
            "prism_core::metrics".to_string(),
            "Metrics collected".to_string(),
            serde_json::json!({"counter": "requests_total", "value": 1234}),
        ));

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism_server".to_string(),
            "Server started successfully".to_string(),
            serde_json::json!({"port": 3030, "bind_address": "0.0.0.0"}),
        ));

        buffer
    }

    /// Adds a log entry to the buffer.
    ///
    /// If the buffer is at capacity, the oldest entry is removed.
    pub fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.write();

        if entries.len() >= self.max_size {
            entries.pop_front();
        }

        entries.push_back(entry);
    }

    /// Queries log entries with filtering and pagination.
    ///
    /// # Arguments
    ///
    /// * `params` - Query parameters for filtering and pagination.
    ///
    /// # Returns
    ///
    /// A `LogQueryResponse` containing the matching entries, total count,
    /// and whether more entries are available.
    pub fn query(&self, params: &LogQueryParams) -> LogQueryResponse {
        let entries = self.entries.read();

        // Filter entries
        let filtered: Vec<LogEntry> = entries
            .iter()
            .rev() // Most recent first
            .filter(|entry| entry.matches(params))
            .cloned()
            .collect();

        let total = filtered.len();
        let has_more = total > params.offset + params.limit;

        // Apply pagination
        let paginated: Vec<LogEntry> =
            filtered.into_iter().skip(params.offset).take(params.limit).collect();

        LogQueryResponse { entries: paginated, total, has_more }
    }

    /// Returns all unique log targets/services.
    ///
    /// # Returns
    ///
    /// A vector of unique target strings sorted alphabetically.
    pub fn get_targets(&self) -> Vec<String> {
        let entries = self.entries.read();

        let targets: HashSet<String> = entries.iter().map(|e| e.target.clone()).collect();

        let mut targets: Vec<String> = targets.into_iter().collect();
        targets.sort();
        targets
    }

    /// Returns the current number of entries in the buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Returns whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Clears all entries from the buffer.
    pub fn clear(&self) {
        self.entries.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_log_buffer() {
        let buffer = LogBuffer::new(100);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_push_and_query() {
        let buffer = LogBuffer::new(10);

        let entry = LogEntry::new(
            "info".to_string(),
            "test".to_string(),
            "Test message".to_string(),
            serde_json::json!({}),
        );

        buffer.push(entry.clone());
        assert_eq!(buffer.len(), 1);

        let params = LogQueryParams {
            level: None,
            target: None,
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.total, 1);
        assert!(!response.has_more);
    }

    #[test]
    fn test_ring_buffer_overflow() {
        let buffer = LogBuffer::new(3);

        for i in 0..5 {
            let entry = LogEntry::new(
                "info".to_string(),
                "test".to_string(),
                format!("Message {i}"),
                serde_json::json!({}),
            );
            buffer.push(entry);
        }

        // Should only keep last 3 entries
        assert_eq!(buffer.len(), 3);

        let params = LogQueryParams {
            level: None,
            target: None,
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 3);

        // Most recent first
        assert!(response.entries[0].message.contains("Message 4"));
        assert!(response.entries[1].message.contains("Message 3"));
        assert!(response.entries[2].message.contains("Message 2"));
    }

    #[test]
    fn test_filter_by_level() {
        let buffer = LogBuffer::new(10);

        buffer.push(LogEntry::new(
            "info".to_string(),
            "test".to_string(),
            "Info message".to_string(),
            serde_json::json!({}),
        ));

        buffer.push(LogEntry::new(
            "error".to_string(),
            "test".to_string(),
            "Error message".to_string(),
            serde_json::json!({}),
        ));

        let params = LogQueryParams {
            level: Some("error".to_string()),
            target: None,
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].level, "error");
    }

    #[test]
    fn test_filter_by_target() {
        let buffer = LogBuffer::new(10);

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism::core".to_string(),
            "Core message".to_string(),
            serde_json::json!({}),
        ));

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism::server".to_string(),
            "Server message".to_string(),
            serde_json::json!({}),
        ));

        let params = LogQueryParams {
            level: None,
            target: Some("server".to_string()),
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert!(response.entries[0].target.contains("server"));
    }

    #[test]
    fn test_search_in_message() {
        let buffer = LogBuffer::new(10);

        buffer.push(LogEntry::new(
            "info".to_string(),
            "test".to_string(),
            "Request completed successfully".to_string(),
            serde_json::json!({}),
        ));

        buffer.push(LogEntry::new(
            "info".to_string(),
            "test".to_string(),
            "Connection established".to_string(),
            serde_json::json!({}),
        ));

        let params = LogQueryParams {
            level: None,
            target: None,
            search: Some("request".to_string()),
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert!(response.entries[0].message.to_lowercase().contains("request"));
    }

    #[test]
    fn test_pagination() {
        let buffer = LogBuffer::new(10);

        for i in 0..5 {
            buffer.push(LogEntry::new(
                "info".to_string(),
                "test".to_string(),
                format!("Message {i}"),
                serde_json::json!({}),
            ));
        }

        let params = LogQueryParams {
            level: None,
            target: None,
            search: None,
            from: None,
            to: None,
            limit: 2,
            offset: 1,
        };

        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 2);
        assert_eq!(response.total, 5);
        assert!(response.has_more);
    }

    #[test]
    fn test_get_targets() {
        let buffer = LogBuffer::new(10);

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism::core".to_string(),
            "Message 1".to_string(),
            serde_json::json!({}),
        ));

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism::server".to_string(),
            "Message 2".to_string(),
            serde_json::json!({}),
        ));

        buffer.push(LogEntry::new(
            "info".to_string(),
            "prism::core".to_string(),
            "Message 3".to_string(),
            serde_json::json!({}),
        ));

        let targets = buffer.get_targets();
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&"prism::core".to_string()));
        assert!(targets.contains(&"prism::server".to_string()));
    }

    #[test]
    fn test_clear() {
        let buffer = LogBuffer::new(10);

        buffer.push(LogEntry::new(
            "info".to_string(),
            "test".to_string(),
            "Message".to_string(),
            serde_json::json!({}),
        ));

        assert_eq!(buffer.len(), 1);

        buffer.clear();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
    }
}
