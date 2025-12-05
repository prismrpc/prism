//! Utility functions for cache manager operations.
//!
//! This module contains helper functions for:
//! - Block number parsing (hex and decimal)
//! - Block range extraction from log filter params
//! - Fetch state diagnostics and cleanup

use crate::{cache::CacheManager, types::BlockRange};
use tracing::warn;

/// Parses a block number from a string, supporting both hex and decimal formats.
///
/// This function handles the common Ethereum RPC formats for block numbers:
/// - Hex format with "0x" prefix (e.g., "0x10" = 16)
/// - Decimal format without prefix (e.g., "100" = 100)
///
/// # Arguments
///
/// * `s` - A string slice containing the block number to parse
///
/// # Returns
///
/// * `Some(u64)` - The parsed block number if the format is valid
/// * `None` - If the string is empty, contains invalid characters, or causes overflow
///
/// # Format Validation
///
/// - Hex strings must start with "0x" and contain only valid hex digits (0-9, a-f, A-F)
/// - Decimal strings must contain only digits (0-9)
/// - Leading zeros are allowed in both formats
/// - Negative numbers are not supported and will return `None`
///
/// # Examples
///
/// ```ignore
/// # use prism_core::cache::manager::utilities::parse_block_number;
/// // Hex format
/// assert_eq!(parse_block_number("0x10"), Some(16));
/// assert_eq!(parse_block_number("0xff"), Some(255));
///
/// // Decimal format
/// assert_eq!(parse_block_number("100"), Some(100));
/// assert_eq!(parse_block_number("0"), Some(0));
///
/// // Invalid formats
/// assert_eq!(parse_block_number("invalid"), None);
/// assert_eq!(parse_block_number("0xGGG"), None);
/// assert_eq!(parse_block_number(""), None);
/// ```
#[inline]
#[must_use]
pub fn parse_block_number(s: &str) -> Option<u64> {
    s.strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .or_else(|| s.parse::<u64>().ok())
}

/// Extracts a block range from `eth_getLogs` JSON-RPC filter parameters.
///
/// This function parses the `fromBlock` and `toBlock` fields from an `eth_getLogs`
/// request's filter parameters. These parameters define the block range over which
/// to search for matching logs.
///
/// # Arguments
///
/// * `params` - A JSON value expected to be an object containing filter parameters
///
/// # Expected JSON Structure
///
/// ```json
/// {
///   "fromBlock": "0x10",      // Required: starting block (hex or decimal)
///   "toBlock": "0x20",        // Optional: ending block (hex or decimal)
///   "address": "0x...",       // Other fields are ignored
///   "topics": [...]
/// }
/// ```
///
/// # Returns
///
/// * `Some(BlockRange)` - If `fromBlock` is present and can be parsed successfully. If `toBlock` is
///   absent or invalid, it defaults to the value of `fromBlock`, creating a single-block range.
/// * `None` - If any of the following conditions are met:
///   - `params` is not a JSON object
///   - `fromBlock` field is missing
///   - `fromBlock` value is not a string
///   - `fromBlock` cannot be parsed as a valid block number
///
/// # Behavior Details
///
/// - The `toBlock` field is optional; if not provided, the range becomes `[fromBlock, fromBlock]`
/// - If `toBlock` is provided but invalid, it falls back to `fromBlock` (does not return `None`)
/// - Both hex ("0x10") and decimal ("16") formats are supported via [`parse_block_number`]
///
/// # Examples
///
/// ```ignore
/// # use serde_json::json;
/// # use prism_core::cache::manager::utilities::get_block_range_from_logs;
/// // Valid range with both bounds
/// let params = json!({"fromBlock": "0x10", "toBlock": "0x20"});
/// let range = get_block_range_from_logs(&params).unwrap();
/// assert_eq!(range.from, 16);
/// assert_eq!(range.to, 32);
///
/// // Only fromBlock (creates single-block range)
/// let params = json!({"fromBlock": "0x10"});
/// let range = get_block_range_from_logs(&params).unwrap();
/// assert_eq!(range.from, 16);
/// assert_eq!(range.to, 16);
///
/// // Missing fromBlock returns None
/// let params = json!({"toBlock": "0x20"});
/// assert!(get_block_range_from_logs(&params).is_none());
/// ```
#[must_use]
pub fn get_block_range_from_logs(params: &serde_json::Value) -> Option<BlockRange> {
    let logs_params = params.as_object()?;

    let from_block = logs_params
        .get("fromBlock")
        .and_then(|v| v.as_str())
        .and_then(parse_block_number)?;

    let to_block = logs_params
        .get("toBlock")
        .and_then(|v| v.as_str())
        .and_then(parse_block_number)
        .unwrap_or(from_block);

    Some(BlockRange::new(from_block, to_block))
}

/// Returns the current fetch coordination state for debugging and diagnostics.
///
/// This function provides insight into the cache manager's fetch coordination system,
/// which prevents duplicate block fetches and tracks in-flight requests. The returned
/// counts help diagnose fetch bottlenecks, deadlocks, or coordination issues.
///
/// # Arguments
///
/// * `cache_manager` - Reference to the cache manager to inspect
///
/// # Returns
///
/// A tuple `(inflight_count, blocks_being_fetched_count)` where:
///
/// * `inflight_count` - Number of blocks currently being fetched, tracked by the inflight semaphore
///   system. This represents the actual concurrent fetch operations in progress, bounded by the
///   configured concurrency limit.
///
/// * `blocks_being_fetched_count` - Number of distinct blocks with active fetch locks. These locks
///   prevent multiple concurrent fetches of the same block number, coordinating between different
///   requests that need the same block.
///
/// # Use Cases
///
/// - **Performance monitoring**: Track fetch concurrency and identify bottlenecks
/// - **Debugging deadlocks**: Identify stuck fetches that aren't releasing properly
/// - **Testing**: Verify fetch coordination behavior in unit and integration tests
/// - **Metrics**: Export fetch state to monitoring systems
///
/// # Examples
///
/// ```ignore
/// # use prism_core::cache::CacheManager;
/// # use prism_core::cache::manager::utilities::get_fetch_state;
/// # let cache_manager = CacheManager::new(/* ... */);
/// let (inflight, locked) = get_fetch_state(&cache_manager);
/// println!("Active fetches: {}, Locked blocks: {}", inflight, locked);
///
/// // Typically: inflight <= concurrency_limit and locked >= inflight
/// assert!(inflight <= 10); // Example concurrency limit
/// ```
#[must_use]
pub fn get_fetch_state(cache_manager: &CacheManager) -> (usize, usize) {
    let inflight_count = cache_manager.inflight.len();
    let fetch_locks_count = cache_manager.blocks_being_fetched.len();
    (inflight_count, fetch_locks_count)
}

/// Forces cleanup of all fetch coordination state (TEST/RECOVERY ONLY).
///
/// This function forcibly clears all fetch coordination tracking in the cache manager,
/// including in-flight fetch counters and block fetch locks. It is designed for test
/// cleanup and emergency recovery from stuck states.
///
/// # Arguments
///
/// * `cache_manager` - Reference to the cache manager to clean up
///
/// # What Gets Cleared
///
/// This function clears two critical coordination structures:
///
/// 1. **Inflight tracking** (`cache_manager.inflight`) - Semaphore-based tracking of concurrent
///    fetch operations. Clearing this releases all fetch slots.
///
/// 2. **Fetch locks** (`cache_manager.blocks_being_fetched`) - Per-block locks that prevent
///    duplicate fetches of the same block. Clearing this allows re-fetching any block immediately.
///
/// # Warning: Production Use
///
/// **DO NOT use this function in production code.** Normal operation should rely on
/// the RAII-based `FetchGuard` pattern for automatic cleanup. This function bypasses
/// normal cleanup mechanisms and can cause race conditions if used while fetches are
/// in progress.
///
/// # Safe Use Cases
///
/// - **Test cleanup**: Resetting state between test cases
/// - **Test diagnostics**: Verifying fetch state isolation
/// - **Emergency recovery**: Clearing stuck state when normal cleanup has failed (only as a last
///   resort, after investigating the root cause)
///
/// # Side Effects
///
/// - Logs a warning with the counts of cleared entries at WARN level
/// - All waiting tasks blocked on these locks may proceed immediately
/// - Any assumptions about fetch deduplication are invalidated
///
/// # Examples
///
/// ```ignore
/// # use prism_core::cache::CacheManager;
/// # use prism_core::cache::manager::utilities::force_cleanup_fetches;
/// // In test cleanup
/// #[cfg(test)]
/// fn cleanup_after_test(cache_manager: &CacheManager) {
///     force_cleanup_fetches(cache_manager);
///     // State is now clean for next test
/// }
/// ```
pub fn force_cleanup_fetches(cache_manager: &CacheManager) {
    let inflight_count = cache_manager.inflight.len();
    let fetch_locks_count = cache_manager.blocks_being_fetched.len();

    cache_manager.inflight.clear();
    cache_manager.blocks_being_fetched.clear();

    warn!(
        inflight = inflight_count,
        fetch_locks = fetch_locks_count,
        "force cleaned up fetch state"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_block_number_hex() {
        assert_eq!(parse_block_number("0x10"), Some(16));
        assert_eq!(parse_block_number("0xff"), Some(255));
        assert_eq!(parse_block_number("0x0"), Some(0));
        assert_eq!(parse_block_number("0xABCD"), Some(43981));
    }

    #[test]
    fn test_parse_block_number_decimal() {
        assert_eq!(parse_block_number("100"), Some(100));
        assert_eq!(parse_block_number("0"), Some(0));
        assert_eq!(parse_block_number("12345678"), Some(12_345_678));
    }

    #[test]
    fn test_parse_block_number_invalid() {
        assert_eq!(parse_block_number(""), None);
        assert_eq!(parse_block_number("abc"), None);
        assert_eq!(parse_block_number("0xGGG"), None);
        assert_eq!(parse_block_number("-1"), None);
    }

    #[test]
    fn test_get_block_range_from_logs() {
        use serde_json::json;

        // Valid range
        let params = json!({"fromBlock": "0x10", "toBlock": "0x20"});
        let range = get_block_range_from_logs(&params);
        assert!(range.is_some());
        let r = range.unwrap();
        assert_eq!(r.from, 16);
        assert_eq!(r.to, 32);

        // Only fromBlock (toBlock defaults to fromBlock)
        let params = json!({"fromBlock": "0x10"});
        let range = get_block_range_from_logs(&params);
        assert!(range.is_some());
        let r = range.unwrap();
        assert_eq!(r.from, 16);
        assert_eq!(r.to, 16);

        // Missing fromBlock
        let params = json!({"toBlock": "0x20"});
        assert!(get_block_range_from_logs(&params).is_none());

        // Invalid params
        let params = json!([]);
        assert!(get_block_range_from_logs(&params).is_none());
    }
}
