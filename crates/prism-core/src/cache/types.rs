//! Cache type definitions and data structures for efficient storage and indexing.
//!
//! This module provides the core type definitions used throughout the cache system,
//! including compact storage formats, bitmap indexing primitives, and filter matching.
//!
//! # Key Types
//!
//! - **[`LogId`]**: Compact log identifier with packed bit representation for bitmap indexing
//! - **[`LogRecord`]**: Normalized log entry stored once per unique log
//! - **[`LogFilter`]**: Query filter with address/topic matching and canonical key generation
//! - **[`BlockHeader`]** / **[`BlockBody`]**: Block metadata and transaction references
//! - **[`TransactionRecord`]** / **[`ReceiptRecord`]**: Transaction and receipt storage
//!
//! # Packed `LogId` Format
//!
//! [`LogId`] uses a space-efficient packing format to enable bitmap indexing with Roaring bitmaps:
//!
//! ```text
//! ┌─────────────────────┬───────────────────────────────┐
//! │  Block Offset (10)  │      Log Index (22)           │
//! │     bits 31-22      │         bits 21-0             │
//! └─────────────────────┴───────────────────────────────┘
//! ```
//!
//! - **Bits 31-22 (10 bits)**: Block offset within chunk (0-1023, supports up to 1024 block chunks)
//! - **Bits 21-0 (22 bits)**: Log index within block (0-4,194,303 logs per block)
//!
//! This packing enables:
//! - Efficient range queries using bitmap operations (AND, OR, XOR)
//! - O(1) conversion between `LogId` and packed `u32` representation
//! - Compact storage: millions of log IDs in a few MB of bitmap memory
//!
//! ## Example: Packing and Unpacking
//!
//! ```
//! use prism_core::cache::types::LogId;
//!
//! // Create a LogId for block 12345, log index 7
//! let log_id = LogId::new(12345, 7);
//!
//! // Pack into u32 for bitmap storage (using default 1000-block chunks)
//! // Block 12345 is in chunk 12 (12345 / 1000), with offset 345 (12345 % 1000)
//! let packed = log_id.pack().unwrap();
//! assert_eq!(packed, (345 << 22) | 7);
//!
//! // Unpack from bitmap with chunk context
//! let unpacked = LogId::unpack_with_chunk(packed, 12);
//! assert_eq!(unpacked.block_number, 12345);
//! assert_eq!(unpacked.log_index, 7);
//! ```
//!
//! # Filter Matching
//!
//! [`LogFilter`] provides flexible filtering with:
//! - **Address matching**: Exact match on contract address
//! - **Positional topic matching**: Match specific topics at positions 0-3
//! - **Anywhere topic matching**: Match topics appearing at any position
//! - **Hash-based keys**: Efficient filter deduplication using `ahash`
//!
//! See [`LogRecord::matches_filter`] for matching logic details.

use serde::{Deserialize, Serialize};
use std::{
    fmt::Write,
    hash::{Hash, Hasher},
    sync::Arc,
};
use thiserror::Error;

/// Finality status of a cached item based on block position relative to finalized checkpoint.
///
/// Used to classify cached blocks and determine appropriate retention policies:
/// - Finalized items can be cached indefinitely (immutable)
/// - Safe items can be cached with long TTLs (unlikely to reorg)
/// - Unsafe items need short TTLs (subject to reorgs)
/// - Unknown items should be treated conservatively
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalityStatus {
    /// Block is beyond the finalized checkpoint - immutable and safe to cache indefinitely
    Finalized,
    /// Block is beyond `safety_depth` but not yet finalized - unlikely to reorg
    Safe,
    /// Block is within `safety_depth` from chain tip - subject to reorganizations
    Unsafe,
    /// Cannot determine finality status (missing block info)
    Unknown,
}

/// Error type for `LogId` packing operations
#[derive(Debug, Error)]
pub enum PackingError {
    #[error("Log index {log_index} exceeds maximum value of {}", LogId::MAX_LOG_INDEX)]
    LogIndexTooLarge { log_index: u32 },

    #[error("Block offset {block_offset} exceeds maximum value of {}", LogId::MAX_BLOCK_OFFSET)]
    BlockOffsetTooLarge { block_offset: u32 },

    #[error("Chunk size {chunk_size} exceeds maximum value of {max}")]
    ChunkSizeTooLarge { chunk_size: u64, max: u64 },
}

/// Error type for filter operations
#[derive(Debug, Error)]
pub enum FilterError {
    #[error("Failed to format filter key: {0}")]
    FormatError(#[from] std::fmt::Error),
}

/// Unique identifier for a log entry across the entire chain
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogId {
    pub block_number: u64,
    pub log_index: u32,
}

impl LogId {
    /// Default chunk size used for packing/unpacking block offsets.
    /// This must match the `chunk_size` configuration in `LogCacheConfig`.
    pub const DEFAULT_CHUNK_SIZE: u64 = 1000;

    const MAX_BLOCK_OFFSET: u32 = 1023;
    const MAX_LOG_INDEX: u32 = 0x3F_FFFF;

    #[must_use]
    pub fn new(block_number: u64, log_index: u32) -> Self {
        debug_assert!(
            log_index <= Self::MAX_LOG_INDEX,
            "Log index {} exceeds maximum of {}",
            log_index,
            Self::MAX_LOG_INDEX
        );

        Self { block_number, log_index }
    }

    /// Pack into a single u32 for efficient storage (roaring bitmap limitation)
    /// We store the block offset within the chunk (0-999) and `log_index`
    /// Returns an error if values exceed the maximum allowed ranges
    /// # Errors
    ///
    /// Returns an error if values exceed the maximum allowed ranges
    pub fn pack(&self) -> Result<u32, PackingError> {
        self.pack_with_chunk_size(Self::DEFAULT_CHUNK_SIZE)
    }

    /// Maximum chunk size that can be used with the packing format.
    /// Limited to 1024 because block offsets are stored in 10 bits (max value 1023).
    pub const MAX_CHUNK_SIZE: u64 = 1024;

    /// Pack with a custom chunk size. This is used when the cache configuration
    /// uses a non-default chunk size.
    /// # Errors
    ///
    /// Returns an error if values exceed the maximum allowed ranges
    pub fn pack_with_chunk_size(&self, chunk_size: u64) -> Result<u32, PackingError> {
        // Validate chunk_size doesn't exceed packing format limit (10 bits = max 1023)
        // A chunk_size of 1024 produces offsets 0-1023, which exactly fits MAX_BLOCK_OFFSET
        if chunk_size > Self::MAX_CHUNK_SIZE {
            return Err(PackingError::ChunkSizeTooLarge { chunk_size, max: Self::MAX_CHUNK_SIZE });
        }

        // SAFETY: block_number % chunk_size is always < chunk_size <= 1024
        // Since chunk_size <= 1024, the modulo result is at most 1023, which fits in u32
        #[allow(clippy::cast_possible_truncation)]
        let block_offset = (self.block_number % chunk_size) as u32;

        // This check is now redundant (chunk_size <= 1024 guarantees offset <= 1023)
        // but kept for defense-in-depth
        if block_offset > Self::MAX_BLOCK_OFFSET {
            return Err(PackingError::BlockOffsetTooLarge { block_offset });
        }

        if self.log_index > Self::MAX_LOG_INDEX {
            return Err(PackingError::LogIndexTooLarge { log_index: self.log_index });
        }

        Ok((block_offset << 22) | self.log_index)
    }

    /// Alternative: A "safe" pack that returns None on overflow
    /// Use this when you want to handle errors more gracefully
    #[must_use]
    pub fn try_pack(&self) -> Option<u32> {
        self.try_pack_with_chunk_size(Self::DEFAULT_CHUNK_SIZE)
    }

    /// Try to pack with a custom chunk size.
    #[must_use]
    pub fn try_pack_with_chunk_size(&self, chunk_size: u64) -> Option<u32> {
        // Validate chunk_size doesn't exceed packing format limit
        if chunk_size > Self::MAX_CHUNK_SIZE {
            return None;
        }

        // SAFETY: block_number % chunk_size is always < chunk_size <= 1024
        #[allow(clippy::cast_possible_truncation)]
        let block_offset = (self.block_number % chunk_size) as u32;

        if block_offset <= Self::MAX_BLOCK_OFFSET && self.log_index <= Self::MAX_LOG_INDEX {
            Some((block_offset << 22) | self.log_index)
        } else {
            None
        }
    }

    /// Unpack from a single u32
    /// Note: This only gives the block offset within chunk, full block number must be reconstructed
    #[must_use]
    pub fn unpack(packed: u32) -> Self {
        Self { block_number: u64::from(packed >> 22), log_index: packed & 0x3F_FFFF }
    }

    /// Unpack with chunk context to get the full block number
    #[must_use]
    pub fn unpack_with_chunk(packed: u32, chunk_id: u64) -> Self {
        Self::unpack_with_chunk_and_size(packed, chunk_id, Self::DEFAULT_CHUNK_SIZE)
    }

    /// Unpack with chunk context and custom chunk size to get the full block number
    #[must_use]
    pub fn unpack_with_chunk_and_size(packed: u32, chunk_id: u64, chunk_size: u64) -> Self {
        let block_offset = u64::from(packed >> 22);
        let full_block_number = chunk_id * chunk_size + block_offset;
        Self { block_number: full_block_number, log_index: packed & 0x3F_FFFF }
    }
}

/// Compact log record stored once per log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub address: [u8; 20],
    pub topics: [Option<[u8; 32]>; 4],
    pub data: Vec<u8>,
    pub transaction_hash: [u8; 32],
    pub block_hash: [u8; 32],
    pub transaction_index: u32,
    pub removed: bool,
}

impl LogRecord {
    #[must_use]
    pub fn new(
        address: [u8; 20],
        topics: [Option<[u8; 32]>; 4],
        data: Vec<u8>,
        transaction_hash: [u8; 32],
        block_hash: [u8; 32],
        transaction_index: u32,
        removed: bool,
    ) -> Self {
        Self { address, topics, data, transaction_hash, block_hash, transaction_index, removed }
    }

    #[must_use]
    pub fn topic(&self, index: usize) -> Option<&[u8; 32]> {
        if index < 4 {
            self.topics[index].as_ref()
        } else {
            None
        }
    }

    /// Check if this log matches a filter
    ///
    /// Matching rules:
    /// - `address`: If specified, log address must match exactly
    /// - `topics[0-3]`: Positional matching - if specified, log's topic at that index must match
    /// - `topics_anywhere`: Each topic must appear in ANY of the log's topic positions (0-3)
    ///
    /// All conditions are combined with AND logic.
    #[must_use]
    pub fn matches_filter(&self, filter: &LogFilter) -> bool {
        // Check address filter
        if let Some(filter_addr) = filter.address {
            if self.address != filter_addr {
                return false;
            }
        }

        // Check positional topic filters (topics[0-3])
        for (i, filter_topic) in filter.topics.iter().enumerate() {
            if let Some(filter_topic) = filter_topic {
                if let Some(log_topic) = self.topic(i) {
                    if log_topic != filter_topic {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        }

        // Check topics_anywhere: each must appear in at least one position
        for anywhere_topic in &filter.topics_anywhere {
            let found =
                self.topics.iter().any(|log_topic| log_topic.as_ref() == Some(anywhere_topic));
            if !found {
                return false;
            }
        }

        true
    }
}

/// Log filter for querying
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogFilter {
    pub address: Option<[u8; 20]>,
    pub topics: [Option<[u8; 32]>; 4],
    pub topics_anywhere: Vec<[u8; 32]>,
    pub from_block: u64,
    pub to_block: u64,
}

impl LogFilter {
    #[must_use]
    pub fn new(from_block: u64, to_block: u64) -> Self {
        Self { address: None, topics: [None; 4], topics_anywhere: Vec::new(), from_block, to_block }
    }

    #[must_use]
    pub fn with_address(mut self, address: [u8; 20]) -> Self {
        self.address = Some(address);
        self
    }

    #[must_use]
    pub fn with_topic(mut self, index: usize, topic: [u8; 32]) -> Self {
        if index < 4 {
            self.topics[index] = Some(topic);
        }
        self
    }

    /// Add a topic that can appear at any position
    #[must_use]
    pub fn with_topic_anywhere(mut self, topic: [u8; 32]) -> Self {
        self.topics_anywhere.push(topic);
        self
    }

    /// Create a canonical key for this filter - optimized version with pre-allocated buffer
    /// Returns an error if the string buffer fails to write.
    /// # Errors
    ///
    /// Returns an error if the string buffer fails to write.
    pub fn canonical_key(&self) -> Result<String, FilterError> {
        let estimated_size = 50 +
            self.address.map_or(0, |_| 50) +
            self.topics.iter().filter_map(|t| t.as_ref()).count() * 70 +
            self.topics_anywhere.len() * 70;

        let mut key = String::with_capacity(estimated_size);

        write!(&mut key, "{}:{}", self.from_block, self.to_block)?;

        if let Some(addr) = self.address {
            key.push_str(":addr_");
            for byte in &addr {
                write!(&mut key, "{byte:02x}")?;
            }
        }

        for (i, topic) in self.topics.iter().enumerate() {
            if let Some(t) = topic {
                write!(&mut key, ":t{i}_0x")?;
                for byte in t {
                    write!(&mut key, "{byte:02x}")?;
                }
            }
        }

        // Include topics_anywhere in canonical key
        for topic in &self.topics_anywhere {
            key.push_str(":ta_0x");
            for byte in topic {
                write!(&mut key, "{byte:02x}")?;
            }
        }

        Ok(key)
    }

    /// Get a hash-based key for `HashMap` lookups (avoids string allocation).
    ///
    /// Uses `ahash` for 2-3x faster hashing compared to `DefaultHasher`.
    #[must_use]
    pub fn hash_key(&self) -> u64 {
        use ahash::AHasher;
        let mut hasher = AHasher::default();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl Hash for LogFilter {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.from_block.hash(state);
        self.to_block.hash(state);

        if let Some(addr) = self.address {
            addr.hash(state);
        } else {
            0u8.hash(state);
        }

        for topic in &self.topics {
            if let Some(t) = topic {
                t.hash(state);
            } else {
                0u8.hash(state);
            }
        }

        // Include topics_anywhere in hash to prevent cache key collisions
        // for filters that differ only in this field
        self.topics_anywhere.hash(state);
    }
}

impl PartialEq for LogFilter {
    fn eq(&self, other: &Self) -> bool {
        self.from_block == other.from_block &&
            self.to_block == other.to_block &&
            self.address == other.address &&
            self.topics == other.topics &&
            self.topics_anywhere == other.topics_anywhere
    }
}

impl Eq for LogFilter {}

impl Default for LogFilter {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

/// Block header information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub hash: [u8; 32],
    pub number: u64,
    pub parent_hash: [u8; 32],
    pub timestamp: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub miner: [u8; 20],

    pub extra_data: Arc<Vec<u8>>,
    pub logs_bloom: Arc<Vec<u8>>,
    pub transactions_root: [u8; 32],
    pub state_root: [u8; 32],
    pub receipts_root: [u8; 32],
}

/// Block body (transactions only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBody {
    pub hash: [u8; 32],
    pub transactions: Vec<[u8; 32]>,
}

/// Transaction record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub hash: [u8; 32],
    pub block_hash: Option<[u8; 32]>,
    pub block_number: Option<u64>,
    pub transaction_index: Option<u32>,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value: [u8; 32],
    pub gas_price: [u8; 32],
    pub gas_limit: u64,
    pub nonce: u64,
    pub data: Vec<u8>,
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

/// Transaction receipt with log references
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptRecord {
    pub transaction_hash: [u8; 32],
    pub block_hash: [u8; 32],
    pub block_number: u64,
    pub transaction_index: u32,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub contract_address: Option<[u8; 20]>,
    pub logs: Vec<LogId>,
    pub status: u64,
    pub logs_bloom: Vec<u8>,
    /// EIP-1559: Effective gas price paid
    pub effective_gas_price: Option<u64>,
    /// Transaction type (0x0 = legacy, 0x1 = access list, 0x2 = EIP-1559, 0x3 = EIP-4844)
    pub tx_type: Option<u8>,
    /// EIP-4844: Blob gas price (for blob transactions)
    pub blob_gas_price: Option<u64>,
}

/// Cache statistics for monitoring
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub log_store_size: usize,
    pub header_cache_size: usize,
    pub body_cache_size: usize,
    pub transaction_cache_size: usize,
    pub receipt_cache_size: usize,
    pub exact_result_count: usize,
    pub total_log_ids_cached: usize,
    pub bitmap_memory_usage: usize,
    pub hot_window_size: usize,

    // Hit/miss tracking for block cache
    pub block_cache_hits: u64,
    pub block_cache_misses: u64,

    // Hit/miss tracking for transaction cache
    pub transaction_cache_hits: u64,
    pub transaction_cache_misses: u64,

    // Hit/miss tracking for receipt cache
    pub receipt_cache_hits: u64,
    pub receipt_cache_misses: u64,

    // Hit/miss tracking for log cache
    pub log_cache_hits: u64,
    pub log_cache_misses: u64,
    pub log_cache_partial_hits: u64,
}

/// Reorg information
#[derive(Debug, Clone)]
pub struct ReorgInfo {
    pub old_tip: u64,
    pub new_tip: u64,
    pub divergence_block: u64,
    pub old_blocks: Vec<u64>,
    pub new_blocks: Vec<u64>,
}

impl ReorgInfo {
    #[must_use]
    pub fn new(old_tip: u64, new_tip: u64, divergence_block: u64) -> Self {
        Self { old_tip, new_tip, divergence_block, old_blocks: Vec::new(), new_blocks: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a LogRecord for testing
    fn make_log(address: [u8; 20], topics: [Option<[u8; 32]>; 4]) -> LogRecord {
        LogRecord {
            address,
            topics,
            data: vec![],
            transaction_hash: [0u8; 32],
            block_hash: [0u8; 32],
            transaction_index: 0,
            removed: false,
        }
    }

    // --- LogRecord::matches_filter tests ---

    #[test]
    fn test_matches_filter_no_criteria() {
        let log = make_log([1u8; 20], [Some([0xAA; 32]), None, None, None]);
        let filter = LogFilter::new(0, 100);

        assert!(log.matches_filter(&filter), "Empty filter should match any log");
    }

    #[test]
    fn test_matches_filter_address_match() {
        let log = make_log([1u8; 20], [None, None, None, None]);
        let filter = LogFilter::new(0, 100).with_address([1u8; 20]);

        assert!(log.matches_filter(&filter));
    }

    #[test]
    fn test_matches_filter_address_mismatch() {
        let log = make_log([1u8; 20], [None, None, None, None]);
        let filter = LogFilter::new(0, 100).with_address([2u8; 20]);

        assert!(!log.matches_filter(&filter));
    }

    #[test]
    fn test_matches_filter_positional_topic_match() {
        let topic0 = [0xAA; 32];
        let topic1 = [0xBB; 32];
        let log = make_log([1u8; 20], [Some(topic0), Some(topic1), None, None]);

        // Match topic at position 0
        let filter = LogFilter::new(0, 100).with_topic(0, topic0);
        assert!(log.matches_filter(&filter));

        // Match topic at position 1
        let filter = LogFilter::new(0, 100).with_topic(1, topic1);
        assert!(log.matches_filter(&filter));

        // Match both positions
        let filter = LogFilter::new(0, 100).with_topic(0, topic0).with_topic(1, topic1);
        assert!(log.matches_filter(&filter));
    }

    #[test]
    fn test_matches_filter_positional_topic_mismatch() {
        let log = make_log([1u8; 20], [Some([0xAA; 32]), None, None, None]);

        // Wrong value at position 0
        let filter = LogFilter::new(0, 100).with_topic(0, [0xBB; 32]);
        assert!(!log.matches_filter(&filter));

        // Filter expects topic at position 1, but log has None there
        let filter = LogFilter::new(0, 100).with_topic(1, [0xAA; 32]);
        assert!(!log.matches_filter(&filter));
    }

    #[test]
    fn test_matches_filter_topics_anywhere_single_match() {
        let topic_a = [0xAA; 32];
        let topic_b = [0xBB; 32];

        // Log has topic_a at position 0
        let log = make_log([1u8; 20], [Some(topic_a), Some(topic_b), None, None]);

        // topics_anywhere should find topic_a
        let filter = LogFilter::new(0, 100).with_topic_anywhere(topic_a);
        assert!(log.matches_filter(&filter), "topic_a should be found at position 0");

        // topics_anywhere should find topic_b
        let filter = LogFilter::new(0, 100).with_topic_anywhere(topic_b);
        assert!(log.matches_filter(&filter), "topic_b should be found at position 1");
    }

    #[test]
    fn test_matches_filter_topics_anywhere_any_position() {
        let target = [0xFF; 32];

        // Target topic at position 0
        let log0 = make_log([1u8; 20], [Some(target), None, None, None]);
        let filter = LogFilter::new(0, 100).with_topic_anywhere(target);
        assert!(log0.matches_filter(&filter), "Should match at position 0");

        // Target topic at position 1
        let log1 = make_log([1u8; 20], [Some([0xAA; 32]), Some(target), None, None]);
        assert!(log1.matches_filter(&filter), "Should match at position 1");

        // Target topic at position 2
        let log2 = make_log([1u8; 20], [None, None, Some(target), None]);
        assert!(log2.matches_filter(&filter), "Should match at position 2");

        // Target topic at position 3
        let log3 = make_log([1u8; 20], [None, None, None, Some(target)]);
        assert!(log3.matches_filter(&filter), "Should match at position 3");
    }

    #[test]
    fn test_matches_filter_topics_anywhere_not_found() {
        let log = make_log([1u8; 20], [Some([0xAA; 32]), Some([0xBB; 32]), None, None]);

        // Look for a topic that doesn't exist
        let filter = LogFilter::new(0, 100).with_topic_anywhere([0xFF; 32]);
        assert!(!log.matches_filter(&filter), "Topic not in log should not match");
    }

    #[test]
    fn test_matches_filter_topics_anywhere_multiple_all_must_match() {
        let topic_a = [0xAA; 32];
        let topic_b = [0xBB; 32];
        let topic_c = [0xCC; 32];

        // Log has topics A and B
        let log = make_log([1u8; 20], [Some(topic_a), Some(topic_b), None, None]);

        // Filter requires A and B - should match
        let filter =
            LogFilter::new(0, 100).with_topic_anywhere(topic_a).with_topic_anywhere(topic_b);
        assert!(log.matches_filter(&filter), "Both A and B are present");

        // Filter requires A, B, and C - should NOT match (C is missing)
        let filter = LogFilter::new(0, 100)
            .with_topic_anywhere(topic_a)
            .with_topic_anywhere(topic_b)
            .with_topic_anywhere(topic_c);
        assert!(!log.matches_filter(&filter), "C is missing, should not match");
    }

    #[test]
    fn test_matches_filter_combined_positional_and_anywhere() {
        let event_sig = [0x11; 32]; // topic[0] = event signature
        let sender = [0x22; 32]; // topic[1] = sender
        let receiver = [0x33; 32]; // topic[2] = receiver

        let log = make_log([1u8; 20], [Some(event_sig), Some(sender), Some(receiver), None]);

        // Positional: require specific event signature at topic[0]
        // Anywhere: require receiver to be somewhere (but we don't care where)
        let filter = LogFilter::new(0, 100).with_topic(0, event_sig).with_topic_anywhere(receiver);
        assert!(log.matches_filter(&filter));

        // Wrong event signature should fail
        let filter = LogFilter::new(0, 100).with_topic(0, [0xFF; 32]).with_topic_anywhere(receiver);
        assert!(!log.matches_filter(&filter));

        // Missing anywhere topic should fail
        let filter =
            LogFilter::new(0, 100).with_topic(0, event_sig).with_topic_anywhere([0xFF; 32]);
        assert!(!log.matches_filter(&filter));
    }

    #[test]
    fn test_matches_filter_combined_address_and_topics_anywhere() {
        let target_addr = [0x42; 20];
        let topic = [0xAA; 32];

        let log = make_log(target_addr, [Some(topic), None, None, None]);

        // Both address and topic_anywhere must match
        let filter = LogFilter::new(0, 100).with_address(target_addr).with_topic_anywhere(topic);
        assert!(log.matches_filter(&filter));

        // Wrong address should fail
        let filter = LogFilter::new(0, 100).with_address([0xFF; 20]).with_topic_anywhere(topic);
        assert!(!log.matches_filter(&filter));
    }

    // --- LogFilter Hash and PartialEq tests ---

    #[test]
    fn test_log_filter_hash_includes_topics_anywhere() {
        let filter1 = LogFilter::new(0, 100);
        let filter2 = LogFilter::new(0, 100).with_topic_anywhere([0xAA; 32]);

        // Different topics_anywhere should produce different hashes
        assert_ne!(filter1.hash_key(), filter2.hash_key());
    }

    #[test]
    fn test_log_filter_eq_includes_topics_anywhere() {
        let filter1 = LogFilter::new(0, 100);
        let filter2 = LogFilter::new(0, 100).with_topic_anywhere([0xAA; 32]);
        let filter3 = LogFilter::new(0, 100).with_topic_anywhere([0xAA; 32]);

        assert_ne!(filter1, filter2, "Different topics_anywhere should not be equal");
        assert_eq!(filter2, filter3, "Same topics_anywhere should be equal");
    }

    #[test]
    fn test_log_filter_canonical_key_includes_topics_anywhere() {
        let filter = LogFilter::new(0, 100)
            .with_topic_anywhere([0xAA; 32])
            .with_topic_anywhere([0xBB; 32]);

        let key = filter.canonical_key().unwrap();

        assert!(key.contains(":ta_0x"), "Canonical key should include topics_anywhere prefix");
        assert!(key.contains("aaaa"), "Canonical key should include topic bytes");
    }
}
