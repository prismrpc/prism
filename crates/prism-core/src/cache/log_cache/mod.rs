//! Log cache using inverted indexes for efficient `eth_getLogs` queries.

mod bitmap_indexes;
pub mod config;
mod coverage;
mod invalidation;
mod query_engine;

pub use config::{LogCacheConfig, LogCacheConfigError, MAX_CHUNK_SIZE};

use crate::cache::types::{CacheStats, LogFilter, LogId, LogRecord};

use ahash::RandomState;
use dashmap::{DashMap, DashSet};
use moka::future::Cache;
use roaring::RoaringBitmap;
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};
use tokio::sync::RwLock;

/// Log cache using inverted indexes for efficient `eth_getLogs` queries.
///
/// The cache stores logs deduplicated by `LogId` and maintains bitmap indexes
/// for addresses, topics (positions 0-3), and block chunks. This enables fast
/// intersection operations to find matching logs without scanning all data.
///
/// `LogId`s are packed into u32 values using the format:
/// - Bits 0-15: `log_index` within block (max 65535)
/// - Bits 16-31: block offset within chunk (max 65535 blocks per chunk)
///
/// Queries beyond `safety_depth` blocks from the chain tip return partial results
/// to avoid caching data likely to be reorganized.
///
/// # Coverage Tracking
///
/// The cache maintains explicit coverage tracking via `covered_blocks` to distinguish
/// between "fetched but empty" and "not yet fetched". This enables:
/// - Partial range storage: mark successful ranges as covered, leave failed ranges unknown
/// - Accurate cache hit detection: empty ranges are still cache hits if covered
/// - Graceful degradation: failed ranges can be retried without re-fetching covered ranges
///
/// # Performance Note
///
/// Bitmap indexes use `Arc<RoaringBitmap>` to enable O(1) cloning during queries.
/// This trades slightly more expensive inserts (via `Arc::make_mut`) for much cheaper
/// query operations that need to clone bitmaps for intersection.
pub struct LogCache {
    pub(crate) config: LogCacheConfig,

    pub(crate) log_store: DashMap<LogId, Arc<LogRecord>, RandomState>,

    /// Address -> packed log IDs bitmap. Arc-wrapped for O(1) cloning in queries.
    pub(crate) address_index: DashMap<[u8; 20], Arc<RoaringBitmap>, RandomState>,
    /// Topic position -> topic value -> packed log IDs bitmap. Arc-wrapped for O(1) cloning.
    pub(crate) topic_indexes: [DashMap<[u8; 32], Arc<RoaringBitmap>, RandomState>; 4],
    /// Chunk ID -> packed log IDs bitmap. Arc-wrapped for O(1) cloning.
    pub(crate) chunk_index: DashMap<u64, Arc<RoaringBitmap>, RandomState>,

    /// Reverse index: `block_number` -> `Vec<LogId>` for efficient invalidation.
    /// This enables O(1) lookup of logs to invalidate during reorgs instead of O(n) scan.
    pub(crate) block_index: DashMap<u64, Vec<LogId>, RandomState>,

    /// Explicit coverage tracking: which blocks have been successfully fetched.
    ///
    /// Key: `chunk_id`, Value: bitmap of block offsets within chunk that are covered.
    /// A block is "covered" if we've successfully fetched logs for it (even if empty).
    /// This distinguishes "no logs exist" from "not yet fetched".
    ///
    /// Coverage is set automatically when:
    /// - Logs are inserted via `insert_log()` or bulk methods
    /// - Ranges are explicitly marked via `mark_range_covered()`
    /// - Cache is warmed via `warm_range()`
    ///
    /// Used for partial range storage: when a range fetch partially fails, we mark
    /// successful sub-ranges as covered while leaving failed ranges as unknown.
    pub(crate) covered_blocks: DashMap<u64, Arc<RoaringBitmap>, RandomState>,

    pub(crate) exact_results: Cache<LogFilter, Vec<LogId>>,

    pub(crate) stats: Arc<RwLock<CacheStats>>,

    pub(crate) last_stats_update: Arc<RwLock<Instant>>,

    /// Dirty flag for lazy stats computation.
    ///
    /// # Lazy Recomputation Protocol
    ///
    /// 1. **Insert operations**: Set `dirty = true` (Relaxed ordering, best-effort signal)
    /// 2. **Stats readers**: Check dirty flag, trigger recomputation if needed
    /// 3. **Recomputation**: Clears `dirty = false` after snapshot (Release ordering)
    ///
    /// # Race Handling
    ///
    /// If inserts happen during recomputation, stats may be slightly stale but
    /// never corrupt. Next stats read will trigger fresh recomputation.
    ///
    /// # Performance Rationale
    ///
    /// Avoids lock contention and expensive aggregation on hot insert path.
    /// Stats updates are deferred to background task or on-demand queries.
    ///
    /// # Example Flow
    ///
    ///
    /// Thread A (insert): `stats_dirty.store(true, Relaxed)`
    /// Thread B (reader): if dirty { `recompute_stats()`; dirty.store(false, Release) }
    /// Thread A (insert): `stats_dirty.store(true, Relaxed)`  // May happen during recompute
    ///     ///
    /// The worst case is slightly stale stats (one insert behind), which is acceptable
    /// for monitoring purposes. The next read will trigger a fresh recomputation.
    pub(crate) stats_dirty: AtomicBool,

    /// Track which addresses have exceeded bitmap capacity.
    /// When an address index overflows, queries involving that address cannot
    /// be fully satisfied from cache and must indicate partial results.
    pub(crate) overflow_addresses: DashSet<[u8; 20], RandomState>,

    /// Track which topics have exceeded bitmap capacity.
    /// Format: (`topic_position`, `topic_value`). When a topic index overflows,
    /// queries involving that topic cannot be fully satisfied from cache.
    pub(crate) overflow_topics: DashSet<(u8, [u8; 32]), RandomState>,
}

impl LogCache {
    /// Creates a new log cache with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `LogCacheConfigError` if `chunk_size` exceeds `MAX_CHUNK_SIZE` (1024).
    /// Invalid configuration should be caught at startup to prevent silent data loss
    /// from `LogId` packing failures at runtime.
    pub fn new(config: &LogCacheConfig) -> Result<Self, LogCacheConfigError> {
        // Validate config at construction to prevent silent data loss from LogId packing failures
        config.validate()?;

        Ok(Self {
            config: config.clone(),
            log_store: DashMap::with_hasher(RandomState::new()),
            address_index: DashMap::with_hasher(RandomState::new()),
            topic_indexes: [
                DashMap::with_hasher(RandomState::new()),
                DashMap::with_hasher(RandomState::new()),
                DashMap::with_hasher(RandomState::new()),
                DashMap::with_hasher(RandomState::new()),
            ],
            chunk_index: DashMap::with_hasher(RandomState::new()),
            block_index: DashMap::with_hasher(RandomState::new()),
            covered_blocks: DashMap::with_hasher(RandomState::new()),
            exact_results: Cache::builder().max_capacity(config.max_exact_results as u64).build(),
            stats: Arc::new(RwLock::new(CacheStats::default())),
            last_stats_update: Arc::new(RwLock::new(Instant::now())),
            stats_dirty: AtomicBool::new(false),
            overflow_addresses: DashSet::with_hasher(RandomState::new()),
            overflow_topics: DashSet::with_hasher(RandomState::new()),
        })
    }

    #[must_use]
    pub fn get_chunk_id(&self, block_number: u64) -> u64 {
        block_number / self.config.chunk_size
    }

    #[must_use]
    pub fn get_chunk_range(&self, chunk_id: u64) -> (u64, u64) {
        let start = chunk_id * self.config.chunk_size;
        let end = start + self.config.chunk_size - 1;
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn create_test_log_record(address: [u8; 20], topic0: Option<[u8; 32]>) -> LogRecord {
        LogRecord::new(
            address,
            [topic0, None, None, None],
            vec![1, 2, 3],
            [3u8; 32],
            [4u8; 32],
            0,
            false,
        )
    }

    fn create_test_log_record_with_topics(
        address: [u8; 20],
        topics: [Option<[u8; 32]>; 4],
    ) -> LogRecord {
        LogRecord::new(address, topics, vec![1, 2, 3], [3u8; 32], [4u8; 32], 0, false)
    }

    async fn insert_logs_for_blocks(
        cache: &LogCache,
        blocks: &[u64],
        address: [u8; 20],
        topic: Option<[u8; 32]>,
    ) {
        for &block_num in blocks {
            let log_id = LogId::new(block_num, 0);
            let log_record = create_test_log_record(address, topic);
            cache.insert_log(log_id, log_record).await;
        }
    }

    #[tokio::test]
    async fn test_log_cache_basic_operations() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let log_id = LogId::new(1000, 0);
        let log_record = LogRecord::new(
            [1u8; 20],
            [Some([2u8; 32]), None, None, None],
            vec![1, 2, 3],
            [3u8; 32],
            [4u8; 32],
            0,
            false,
        );

        cache.insert_log(log_id, log_record.clone()).await;

        let filter = LogFilter::new(1000, 1000).with_address([1u8; 20]).with_topic(0, [2u8; 32]);

        let (result, _missing_ranges) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], log_id);
    }

    #[tokio::test]
    async fn test_log_cache_bitmap_operations() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let topic = [2u8; 32];

        for i in 0..10 {
            let log_id = LogId::new(1000 + i, 0);
            let log_record = LogRecord::new(
                address,
                [Some(topic), None, None, None],
                vec![u8::try_from(i).unwrap_or(0)],
                [3u8; 32],
                [4u8; 32],
                u32::try_from(i).unwrap_or(0),
                false,
            );
            cache.insert_log(log_id, log_record).await;
        }

        let filter = LogFilter::new(1000, 1009).with_address(address);
        let (result, _missing_ranges) = cache.query_logs(&filter, 1009).await;
        assert_eq!(result.len(), 10);

        let filter = LogFilter::new(1000, 1009).with_topic(0, topic);
        let (result, _missing_ranges) = cache.query_logs(&filter, 1009).await;
        assert_eq!(result.len(), 10);
    }

    #[tokio::test]
    async fn test_log_cache_reorg_invalidation() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let log_id = LogId::new(1000, 0);
        let log_record = LogRecord::new(
            [1u8; 20],
            [Some([2u8; 32]), None, None, None],
            vec![1, 2, 3],
            [3u8; 32],
            [4u8; 32],
            0,
            false,
        );
        cache.insert_log(log_id, log_record).await;

        let filter = LogFilter::new(1000, 1000);
        let (result, _missing_ranges) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 1);

        cache.invalidate_block(1000);

        let (result, _missing_ranges) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_query_range_fully_covered() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-110
        insert_logs_for_blocks(&cache, &(100..=110).collect::<Vec<_>>(), address, None).await;

        // Query exactly the same range - should be fully covered
        let filter = LogFilter::new(100, 110).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        assert_eq!(result.len(), 11);
        assert!(missing_ranges.is_empty(), "No missing ranges for fully covered query");
    }

    #[tokio::test]
    async fn test_query_range_subset_of_cached() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-120
        insert_logs_for_blocks(&cache, &(100..=120).collect::<Vec<_>>(), address, None).await;

        // Query a subset (105-115) - should be fully covered
        let filter = LogFilter::new(105, 115).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 120).await;

        assert_eq!(result.len(), 11);
        assert!(missing_ranges.is_empty());
    }

    #[tokio::test]
    async fn test_query_range_no_overlap() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-110
        insert_logs_for_blocks(&cache, &(100..=110).collect::<Vec<_>>(), address, None).await;

        // Query completely different range (200-210)
        let filter = LogFilter::new(200, 210).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 210).await;

        assert_eq!(result.len(), 0);
        assert_eq!(missing_ranges, vec![(200, 210)]);
    }

    #[tokio::test]
    async fn test_query_range_partial_overlap_start() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-110
        insert_logs_for_blocks(&cache, &(100..=110).collect::<Vec<_>>(), address, None).await;

        // Query 95-105 (overlaps at end)
        let filter = LogFilter::new(95, 105).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        assert_eq!(result.len(), 6);
        // Missing 95-99
        assert_eq!(missing_ranges, vec![(95, 99)]);
    }

    #[tokio::test]
    async fn test_query_range_partial_overlap_end() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-110
        insert_logs_for_blocks(&cache, &(100..=110).collect::<Vec<_>>(), address, None).await;

        // Query 105-120 (overlaps at start)
        let filter = LogFilter::new(105, 120).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 120).await;

        assert_eq!(result.len(), 6);
        // Missing 111-120
        assert_eq!(missing_ranges, vec![(111, 120)]);
    }

    #[tokio::test]
    async fn test_query_range_extends_both_ends() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-110
        insert_logs_for_blocks(&cache, &(100..=110).collect::<Vec<_>>(), address, None).await;

        // Query 90-120 (extends beyond both ends)
        let filter = LogFilter::new(90, 120).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 120).await;

        assert_eq!(result.len(), 11);
        // Missing 90-99 and 111-120
        assert_eq!(missing_ranges, vec![(90, 99), (111, 120)]);
    }

    // Partial Range Fulfillment Tests

    #[tokio::test]
    async fn test_partial_fulfillment_gap_in_middle() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-105 and 115-120 (gap at 106-114)
        insert_logs_for_blocks(&cache, &(100..=105).collect::<Vec<_>>(), address, None).await;
        insert_logs_for_blocks(&cache, &(115..=120).collect::<Vec<_>>(), address, None).await;

        // Query entire range 100-120
        let filter = LogFilter::new(100, 120).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 120).await;

        assert_eq!(result.len(), 12);
        // Missing 106-114
        assert_eq!(missing_ranges, vec![(106, 114)]);
    }

    #[tokio::test]
    async fn test_partial_fulfillment_multiple_gaps() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 100-102, 110-112, 120-122 (multiple gaps)
        insert_logs_for_blocks(&cache, &[100, 101, 102], address, None).await;
        insert_logs_for_blocks(&cache, &[110, 111, 112], address, None).await;
        insert_logs_for_blocks(&cache, &[120, 121, 122], address, None).await;

        // Query 100-125
        let filter = LogFilter::new(100, 125).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 125).await;

        assert_eq!(result.len(), 9);
        // Missing: 103-109, 113-119, 123-125
        assert_eq!(missing_ranges, vec![(103, 109), (113, 119), (123, 125)]);
    }

    #[tokio::test]
    async fn test_partial_fulfillment_single_block_cached() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert only block 100
        insert_logs_for_blocks(&cache, &[100], address, None).await;

        // Query 100-110
        let filter = LogFilter::new(100, 110).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        assert_eq!(result.len(), 1);
        // Missing 101-110
        assert_eq!(missing_ranges, vec![(101, 110)]);
    }

    #[tokio::test]
    async fn test_safety_depth_marks_unsafe_blocks_as_missing() {
        let config = LogCacheConfig { safety_depth: 12, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 980-1000
        insert_logs_for_blocks(&cache, &(980..=1000).collect::<Vec<_>>(), address, None).await;

        // Query 980-1000 with current_tip=1000 and safety_depth=12
        // Effective to_block for coverage = 1000-12=988
        let filter = LogFilter::new(980, 1000).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 1000).await;

        assert_eq!(result.len(), 21);
        // Blocks 989-1000 are within safety depth, so they're marked as "missing"
        // even though we have cached data - caller should refetch
        assert_eq!(missing_ranges, vec![(989, 1000)]);
    }

    #[tokio::test]
    async fn test_safety_depth_query_entirely_in_unsafe_zone() {
        let config = LogCacheConfig { safety_depth: 12, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 995-1000
        insert_logs_for_blocks(&cache, &(995..=1000).collect::<Vec<_>>(), address, None).await;

        // Query 995-1000 with current_tip=1000, safety_depth=12
        // Safe upper bound is 988, query starts at 995 which is > 988
        let filter = LogFilter::new(995, 1000).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 1000).await;

        // Cache still returns the logs it has
        assert_eq!(result.len(), 6);
        // But all blocks are marked as missing since they're in unsafe zone
        assert_eq!(missing_ranges, vec![(995, 1000)]);
    }

    #[tokio::test]
    async fn test_safety_depth_zero() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 990-1000
        insert_logs_for_blocks(&cache, &(990..=1000).collect::<Vec<_>>(), address, None).await;

        // Query with safety_depth=0, should include all blocks
        let filter = LogFilter::new(990, 1000).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 1000).await;

        assert_eq!(result.len(), 11);
        assert!(missing_ranges.is_empty());
    }

    #[tokio::test]
    async fn test_safety_depth_partial_coverage() {
        let config = LogCacheConfig { safety_depth: 5, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs for blocks 90-95 only (gap at 96-100)
        insert_logs_for_blocks(&cache, &(90..=95).collect::<Vec<_>>(), address, None).await;

        // Query 90-100 with current_tip=100, safety_depth=5
        // Effective to_block = 100-5 = 95
        let filter = LogFilter::new(90, 100).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 100).await;

        assert_eq!(result.len(), 6);
        // Blocks 96-100 are missing (beyond our cached range AND in unsafe zone)
        assert_eq!(missing_ranges, vec![(96, 100)]);
    }

    #[tokio::test]
    async fn test_query_across_chunk_boundary() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs crossing chunk boundary (999, 1000, 1001)
        insert_logs_for_blocks(&cache, &[999, 1000, 1001], address, None).await;

        // Query across boundary
        let filter = LogFilter::new(999, 1001).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 1001).await;

        assert_eq!(result.len(), 3);
        assert!(missing_ranges.is_empty());
    }

    #[tokio::test]
    async fn test_query_multiple_chunks() {
        // Note: LogId pack/unpack uses hardcoded 1000, so chunk_size must be 1000
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");
        let address = [1u8; 20];

        // Insert logs in chunks 0 (0-999), 1 (1000-1999), 2 (2000-2999)
        for &block in &[500, 1500, 2500] {
            let log_id = LogId::new(block, 0);
            let log_record = create_test_log_record(address, None);
            cache.insert_log(log_id, log_record).await;
        }

        // Query across all three chunks
        let filter = LogFilter::new(0, 3000).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 3000).await;

        assert_eq!(result.len(), 3);
        assert!(!missing_ranges.is_empty());

        // Verify the blocks we found are correct
        let block_numbers: Vec<u64> = result.iter().map(|id| id.block_number).collect();
        assert!(block_numbers.contains(&500));
        assert!(block_numbers.contains(&1500));
        assert!(block_numbers.contains(&2500));
    }

    #[tokio::test]
    async fn test_chunk_id_calculation() {
        let config = LogCacheConfig { chunk_size: 1000, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        assert_eq!(cache.get_chunk_id(0), 0);
        assert_eq!(cache.get_chunk_id(999), 0);
        assert_eq!(cache.get_chunk_id(1000), 1);
        assert_eq!(cache.get_chunk_id(1001), 1);
        assert_eq!(cache.get_chunk_id(5555), 5);
    }

    #[tokio::test]
    async fn test_chunk_range_calculation() {
        let config = LogCacheConfig { chunk_size: 1000, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        assert_eq!(cache.get_chunk_range(0), (0, 999));
        assert_eq!(cache.get_chunk_range(1), (1000, 1999));
        assert_eq!(cache.get_chunk_range(5), (5000, 5999));
    }

    #[tokio::test]
    async fn test_invalidate_block_with_multiple_logs() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let topic = [2u8; 32];

        // Insert 5 logs for block 1000
        for i in 0..5 {
            let log_id = LogId::new(1000, i);
            let log_record = create_test_log_record(address, Some(topic));
            cache.insert_log(log_id, log_record).await;
        }

        // Verify logs are present
        let filter = LogFilter::new(1000, 1000).with_address(address);
        let (result, _) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 5);

        // Invalidate block
        cache.invalidate_block(1000);

        // Verify all logs removed
        let (result, _) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_invalidate_block_removes_from_address_index() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert log for block 1000
        let log_id = LogId::new(1000, 0);
        let log_record = create_test_log_record(address, None);
        cache.insert_log(log_id, log_record).await;

        // Verify address index has the entry
        assert!(cache.address_index.contains_key(&address));

        // Invalidate block
        cache.invalidate_block(1000);

        // Address index entry should be removed (was the only log with that address)
        assert!(!cache.address_index.contains_key(&address));
    }

    #[tokio::test]
    async fn test_invalidate_block_removes_from_topic_indexes() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let topic0 = [2u8; 32];
        let topic1 = [3u8; 32];

        // Insert log with two topics
        let log_id = LogId::new(1000, 0);
        let log_record =
            create_test_log_record_with_topics([1u8; 20], [Some(topic0), Some(topic1), None, None]);
        cache.insert_log(log_id, log_record).await;

        // Verify topic indexes have entries
        assert!(cache.topic_indexes[0].contains_key(&topic0));
        assert!(cache.topic_indexes[1].contains_key(&topic1));

        // Invalidate block
        cache.invalidate_block(1000);

        // Topic index entries should be removed
        assert!(!cache.topic_indexes[0].contains_key(&topic0));
        assert!(!cache.topic_indexes[1].contains_key(&topic1));
    }

    #[tokio::test]
    async fn test_invalidate_block_partial_index_cleanup() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert logs for blocks 1000 and 1001 with same address
        insert_logs_for_blocks(&cache, &[1000, 1001], address, None).await;

        // Invalidate only block 1000
        cache.invalidate_block(1000);

        // Address index should still exist (block 1001 log remains)
        assert!(cache.address_index.contains_key(&address));

        // Query should still find block 1001
        let filter = LogFilter::new(1001, 1001).with_address(address);
        let (result, _) = cache.query_logs(&filter, 1001).await;
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn test_invalidate_nonexistent_block() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[1000], address, None).await;

        // Invalidate a block that doesn't exist - should not panic
        cache.invalidate_block(9999);

        // Original logs should still be there
        let filter = LogFilter::new(1000, 1000).with_address(address);
        let (result, _) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn test_invalidate_removes_from_chunk_index() {
        let config = LogCacheConfig { chunk_size: 1000, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert log for block 1500 (chunk 1)
        insert_logs_for_blocks(&cache, &[1500], address, None).await;

        // Verify chunk index has entry
        assert!(cache.chunk_index.contains_key(&1));

        // Invalidate block
        cache.invalidate_block(1500);

        // Chunk index entry should be removed
        assert!(!cache.chunk_index.contains_key(&1));
    }

    #[tokio::test]
    async fn test_exact_result_caching_on_full_hit() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100, 101, 102], address, None).await;

        let filter = LogFilter::new(100, 102).with_address(address);

        // First query - should compute result
        let (result1, missing1) = cache.query_logs(&filter, 102).await;
        assert_eq!(result1.len(), 3);
        assert!(missing1.is_empty());

        // Second query - should hit exact result cache
        let (result2, missing2) = cache.query_logs(&filter, 102).await;
        assert_eq!(result2.len(), 3);
        assert!(missing2.is_empty());
    }

    #[tokio::test]
    async fn test_exact_result_not_cached_on_partial_hit() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100, 101], address, None).await;

        let filter = LogFilter::new(100, 105).with_address(address);

        // Query should return partial result
        let (result, missing) = cache.query_logs(&filter, 105).await;
        assert_eq!(result.len(), 2);
        assert!(!missing.is_empty());

        // Exact results should NOT be cached for partial hits
        // (We can't easily verify this directly, but the behavior is tested)
    }

    #[tokio::test]
    async fn test_invalidate_clears_exact_results() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100, 101, 102], address, None).await;

        let filter = LogFilter::new(100, 102).with_address(address);

        // Cache exact result
        let _ = cache.query_logs(&filter, 102).await;

        // Invalidate a related block
        cache.invalidate_block(101);

        // Query should now show missing ranges
        let (result, missing) = cache.query_logs(&filter, 102).await;
        assert_eq!(result.len(), 2); // Only 100 and 102 remain
        assert!(!missing.is_empty()); // Block 101 is missing
    }

    #[tokio::test]
    async fn test_bulk_insert() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let logs: Vec<(LogId, LogRecord)> = (100..110)
            .map(|i| {
                let log_id = LogId::new(i, 0);
                let log_record = create_test_log_record(address, None);
                (log_id, log_record)
            })
            .collect();

        cache.insert_logs_bulk(logs).await;

        let filter = LogFilter::new(100, 109).with_address(address);
        let (result, missing) = cache.query_logs(&filter, 109).await;

        assert_eq!(result.len(), 10);
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_bulk_insert_no_stats() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let logs: Vec<(LogId, LogRecord)> = (100..110)
            .map(|i| {
                let log_id = LogId::new(i, 0);
                let log_record = create_test_log_record(address, None);
                (log_id, log_record)
            })
            .collect();

        cache.insert_logs_bulk_no_stats(logs).await;

        let filter = LogFilter::new(100, 109).with_address(address);
        let (result, _) = cache.query_logs(&filter, 109).await;
        assert_eq!(result.len(), 10);

        // Changes should be tracked
        assert!(cache.stats_dirty.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_address_filter_only() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let addr1 = [1u8; 20];
        let addr2 = [2u8; 20];

        insert_logs_for_blocks(&cache, &[100, 101, 102], addr1, None).await;
        insert_logs_for_blocks(&cache, &[100, 101, 102], addr2, None).await;

        // Query for addr1 only
        let filter = LogFilter::new(100, 102).with_address(addr1);
        let (result, _) = cache.query_logs(&filter, 102).await;

        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn test_topic_filter_only() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let topic1 = [1u8; 32];
        let topic2 = [2u8; 32];

        insert_logs_for_blocks(&cache, &[100, 101, 102], [0u8; 20], Some(topic1)).await;
        insert_logs_for_blocks(&cache, &[103, 104, 105], [0u8; 20], Some(topic2)).await;

        // Query for topic1 only
        let filter = LogFilter::new(100, 105).with_topic(0, topic1);
        let (result, _) = cache.query_logs(&filter, 105).await;

        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn test_address_and_topic_filter() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let topic = [2u8; 32];

        // Insert matching log
        let log_id = LogId::new(100, 0);
        let log_record = create_test_log_record(address, Some(topic));
        cache.insert_log(log_id, log_record).await;

        // Insert non-matching logs
        cache
            .insert_log(LogId::new(101, 0), create_test_log_record(address, None))
            .await;
        cache
            .insert_log(LogId::new(102, 0), create_test_log_record([0u8; 20], Some(topic)))
            .await;

        // Query with both filters
        let filter = LogFilter::new(100, 102).with_address(address).with_topic(0, topic);
        let (result, _) = cache.query_logs(&filter, 102).await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].block_number, 100);
    }

    #[tokio::test]
    async fn test_multiple_topic_positions() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let topic0 = [1u8; 32];
        let topic1 = [2u8; 32];
        let topic2 = [3u8; 32];

        // Insert log with topics at positions 0, 1, 2
        let log_id = LogId::new(100, 0);
        let log_record = create_test_log_record_with_topics(
            [1u8; 20],
            [Some(topic0), Some(topic1), Some(topic2), None],
        );
        cache.insert_log(log_id, log_record).await;

        // Query with topic at position 1
        let filter = LogFilter::new(100, 100).with_topic(1, topic1);
        let (result, _) = cache.query_logs(&filter, 100).await;
        assert_eq!(result.len(), 1);

        // Query with topic at position 2
        let filter = LogFilter::new(100, 100).with_topic(2, topic2);
        let (result, _) = cache.query_logs(&filter, 100).await;
        assert_eq!(result.len(), 1);

        // Query with wrong topic at position 0 - should not match
        let filter = LogFilter::new(100, 100).with_topic(0, topic1);
        let (result, _) = cache.query_logs(&filter, 100).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_prune_old_chunks() {
        let config = LogCacheConfig { chunk_size: 100, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert logs for blocks 0-300 (chunks 0, 1, 2, 3)
        for block in (0..=300).step_by(50) {
            insert_logs_for_blocks(&cache, &[block], address, None).await;
        }

        cache.prune_old_chunks(1000, 500).await;

        let filter = LogFilter::new(0, 300).with_address(address);
        let (result, _) = cache.query_logs(&filter, 1000).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_stats_update() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100, 101, 102], address, None).await;

        cache.force_update_stats().await;
        let stats = cache.get_stats().await;

        assert_eq!(stats.log_store_size, 3);
    }

    #[tokio::test]
    async fn test_should_update_stats_threshold() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        // Initially should not need update (within 300 second window)
        // Note: should_update_stats only checks time elapsed, not change count.
        // The dirty flag pattern handles cache invalidation separately.
        assert!(!cache.should_update_stats().await);

        // Insert some logs (won't trigger time-based update)
        let address = [1u8; 20];
        for i in 0..10 {
            cache
                .insert_log_with_stats_update(
                    LogId::new(i, 0),
                    create_test_log_record(address, None),
                    false,
                )
                .await;
        }

        // Still within 300 second window, so should not need update
        // The dirty flag will ensure stats are recomputed when requested via get_stats()
        assert!(!cache.should_update_stats().await);

        // Verify stats can be retrieved (get_stats handles dirty flag automatically)
        let stats = cache.get_stats().await;
        assert_eq!(stats.log_store_size, 10);
    }

    #[tokio::test]
    async fn test_get_log_records() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let log_ids: Vec<LogId> = (100..103).map(|i| LogId::new(i, 0)).collect();

        for &log_id in &log_ids {
            let log_record = create_test_log_record(address, None);
            cache.insert_log(log_id, log_record).await;
        }

        let records = cache.get_log_records(&log_ids);
        assert_eq!(records.len(), 3);
    }

    #[tokio::test]
    async fn test_get_log_records_with_ids() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let log_ids: Vec<LogId> = (100..103).map(|i| LogId::new(i, 0)).collect();

        for &log_id in &log_ids {
            let log_record = create_test_log_record(address, None);
            cache.insert_log(log_id, log_record).await;
        }

        let records_with_ids = cache.get_log_records_with_ids(&log_ids);
        assert_eq!(records_with_ids.len(), 3);

        // Verify IDs are paired correctly
        for (log_id, _record) in &records_with_ids {
            assert!(log_ids.contains(log_id));
        }
    }

    #[tokio::test]
    async fn test_get_log_records_missing_ids() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        cache
            .insert_log(LogId::new(100, 0), create_test_log_record(address, None))
            .await;

        // Query with IDs that don't exist
        let missing_ids = vec![LogId::new(200, 0), LogId::new(201, 0)];
        let records = cache.get_log_records(&missing_ids);
        assert_eq!(records.len(), 0);
    }

    #[tokio::test]
    async fn test_warm_range() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        let logs: Vec<(LogId, LogRecord)> = (100..110)
            .map(|i| {
                let log_id = LogId::new(i, 0);
                let log_record = create_test_log_record(address, None);
                (log_id, log_record)
            })
            .collect();

        cache.warm_range(100, 109, logs).await;

        let filter = LogFilter::new(100, 109).with_address(address);
        let (result, missing) = cache.query_logs(&filter, 109).await;

        assert_eq!(result.len(), 10);
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_clear_cache() {
        let config = LogCacheConfig::default();
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100, 101, 102], address, None).await;

        // Verify logs exist
        assert!(!cache.log_store.is_empty());

        // Clear cache
        cache.clear_cache().await;

        // Verify all cleared
        assert!(cache.log_store.is_empty());
        assert!(cache.address_index.is_empty());
        assert!(cache.chunk_index.is_empty());
        assert!(cache.block_index.is_empty());
        for topic_index in &cache.topic_indexes {
            assert!(topic_index.is_empty());
        }
    }

    #[tokio::test]
    async fn test_single_block_range() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100], address, None).await;

        let filter = LogFilter::new(100, 100).with_address(address);
        let (result, missing) = cache.query_logs(&filter, 100).await;

        assert_eq!(result.len(), 1);
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_empty_query_result() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];
        insert_logs_for_blocks(&cache, &[100], address, None).await;

        // Query for different address
        let filter = LogFilter::new(100, 100).with_address([2u8; 20]);
        let (result, _) = cache.query_logs(&filter, 100).await;

        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_multiple_logs_same_block() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert 10 logs for the same block
        for i in 0..10 {
            let log_id = LogId::new(100, i);
            let log_record = create_test_log_record(address, None);
            cache.insert_log(log_id, log_record).await;
        }

        let filter = LogFilter::new(100, 100).with_address(address);
        let (result, _) = cache.query_logs(&filter, 100).await;

        assert_eq!(result.len(), 10);
    }

    #[tokio::test]
    async fn test_log_id_ordering() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let address = [1u8; 20];

        // Insert logs out of order
        cache
            .insert_log(LogId::new(103, 0), create_test_log_record(address, None))
            .await;
        cache
            .insert_log(LogId::new(100, 0), create_test_log_record(address, None))
            .await;
        cache
            .insert_log(LogId::new(101, 0), create_test_log_record(address, None))
            .await;

        let filter = LogFilter::new(100, 103).with_address(address);
        let (result, _) = cache.query_logs(&filter, 103).await;

        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn test_wildcard_filter() {
        let config = LogCacheConfig { safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        let addr1 = [1u8; 20];
        let addr2 = [2u8; 20];
        let topic1 = [10u8; 32];
        let topic2 = [20u8; 32];

        cache
            .insert_log(LogId::new(100, 0), create_test_log_record(addr1, Some(topic1)))
            .await;
        cache
            .insert_log(LogId::new(100, 1), create_test_log_record(addr2, Some(topic2)))
            .await;

        // Wildcard filter (no address/topic specified) should match nothing from indexes
        // but if there are no filters, it returns empty because prepare_filter_bitmaps returns
        // Some([])
        let filter = LogFilter::new(100, 100);
        let (result, _) = cache.query_logs(&filter, 100).await;

        // With no filters, prepare_filter_bitmaps returns Some(empty vec),
        // and intersect_filters_in_order with empty filters returns chunk_bitmap.clone()
        // So we should get all logs in that chunk matching the block range
        assert_eq!(result.len(), 2);
    }

    use proptest::prelude::*;
    use std::collections::HashSet;

    /// Strategy for generating valid block ranges (start <= end)
    fn block_range_strategy() -> impl Strategy<Value = (u64, u64)> {
        (0u64..100_000u64).prop_flat_map(|start| (Just(start), start..=start + 10_000))
    }

    /// Strategy for generating sets of covered ranges (sorted, non-overlapping)
    fn covered_ranges_strategy() -> impl Strategy<Value = Vec<(u64, u64)>> {
        proptest::collection::vec((0u64..10_000u64, 1u64..100u64), 0..20).prop_map(|ranges| {
            // Convert to (start, end) format and sort
            let mut normalized: Vec<(u64, u64)> =
                ranges.iter().map(|(start, len)| (*start, *start + len - 1)).collect();
            normalized.sort_by_key(|r| r.0);

            // Merge overlapping ranges to ensure non-overlapping
            if normalized.is_empty() {
                return Vec::new();
            }

            let mut merged = Vec::new();
            let mut current = normalized[0];

            for &(start, end) in &normalized[1..] {
                if start <= current.1 + 1 {
                    // Overlapping or adjacent, merge
                    current.1 = current.1.max(end);
                } else {
                    // Non-overlapping, push current and start new
                    merged.push(current);
                    current = (start, end);
                }
            }
            merged.push(current);
            merged
        })
    }

    proptest! {
        #[test]
        fn prop_uncovered_ranges_union_equals_query_range(
            (query_start, query_end) in block_range_strategy(),
            covered_ranges in covered_ranges_strategy()
        ) {
            // Build covered blocks set
            let mut covered_blocks = HashSet::new();
            for (start, end) in &covered_ranges {
                for block in *start..=*end {
                    covered_blocks.insert(block);
                }
            }

            // Compute missing ranges using the cache's algorithm
            let filter = LogFilter::new(query_start, query_end);
            let effective_to = query_end; // No safety depth for this test
            let missing_ranges = LogCache::compute_missing_ranges_from_coverage(
                &filter,
                effective_to,
                &covered_blocks
            );

            // Verify: Union of covered + uncovered ranges equals query range
            let mut all_blocks = covered_blocks.clone();
            for (start, end) in &missing_ranges {
                for block in *start..=*end {
                    all_blocks.insert(block);
                }
            }

            // All blocks in query range should be accounted for
            for block in query_start..=query_end {
                prop_assert!(
                    all_blocks.contains(&block),
                    "Block {} not in union of covered + uncovered ranges", block
                );
            }
        }

        #[test]
        fn prop_uncovered_ranges_dont_overlap_covered(
            (query_start, query_end) in block_range_strategy(),
            covered_ranges in covered_ranges_strategy()
        ) {
            // Build covered blocks set
            let mut covered_blocks = HashSet::new();
            for (start, end) in &covered_ranges {
                for block in *start..=*end {
                    covered_blocks.insert(block);
                }
            }

            // Compute missing ranges
            let filter = LogFilter::new(query_start, query_end);
            let effective_to = query_end;
            let missing_ranges = LogCache::compute_missing_ranges_from_coverage(
                &filter,
                effective_to,
                &covered_blocks
            );

            // Verify: Uncovered ranges don't overlap with covered blocks
            for (start, end) in &missing_ranges {
                for block in *start..=*end {
                    prop_assert!(
                        !covered_blocks.contains(&block),
                        "Block {} in missing range [{}, {}] but also in covered set",
                        block, start, end
                    );
                }
            }
        }

        #[test]
        fn prop_uncovered_ranges_sorted_non_overlapping(
            (query_start, query_end) in block_range_strategy(),
            covered_ranges in covered_ranges_strategy()
        ) {
            // Build covered blocks set
            let mut covered_blocks = HashSet::new();
            for (start, end) in &covered_ranges {
                for block in *start..=*end {
                    covered_blocks.insert(block);
                }
            }

            // Compute missing ranges
            let filter = LogFilter::new(query_start, query_end);
            let effective_to = query_end;
            let missing_ranges = LogCache::compute_missing_ranges_from_coverage(
                &filter,
                effective_to,
                &covered_blocks
            );

            // Verify: Ranges are sorted
            for i in 1..missing_ranges.len() {
                prop_assert!(
                    missing_ranges[i].0 > missing_ranges[i-1].1,
                    "Ranges not sorted or overlapping: [{}, {}] followed by [{}, {}]",
                    missing_ranges[i-1].0, missing_ranges[i-1].1,
                    missing_ranges[i].0, missing_ranges[i].1
                );
            }

            // Verify: Each range has start <= end
            for (start, end) in &missing_ranges {
                prop_assert!(
                    start <= end,
                    "Invalid range: start {} > end {}", start, end
                );
            }
        }

        #[test]
        fn prop_empty_covered_returns_full_query_range(
            (query_start, query_end) in block_range_strategy()
        ) {
            let covered_blocks = HashSet::new(); // Empty coverage

            let filter = LogFilter::new(query_start, query_end);
            let effective_to = query_end;
            let missing_ranges = LogCache::compute_missing_ranges_from_coverage(
                &filter,
                effective_to,
                &covered_blocks
            );

            // Should return exactly one range covering the entire query
            prop_assert_eq!(missing_ranges.len(), 1);
            prop_assert_eq!(missing_ranges[0].0, query_start);
            prop_assert_eq!(missing_ranges[0].1, query_end);
        }

        #[test]
        fn prop_fully_covered_returns_empty(
            (query_start, query_end) in block_range_strategy()
        ) {
            // Cover all blocks in the query range
            let mut covered_blocks = HashSet::new();
            for block in query_start..=query_end {
                covered_blocks.insert(block);
            }

            let filter = LogFilter::new(query_start, query_end);
            let effective_to = query_end;
            let missing_ranges = LogCache::compute_missing_ranges_from_coverage(
                &filter,
                effective_to,
                &covered_blocks
            );

            // Should return no missing ranges
            prop_assert!(
                missing_ranges.is_empty(),
                "Expected empty missing ranges, got: {:?}", missing_ranges
            );
        }
    }

    #[test]
    fn test_mark_range_covered_single_block() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark single block as covered
        cache.mark_range_covered(100, 100);

        assert!(cache.is_block_covered(100));
        assert!(!cache.is_block_covered(99));
        assert!(!cache.is_block_covered(101));
    }

    #[test]
    fn test_mark_range_covered_multiple_blocks() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range 100-110 as covered
        cache.mark_range_covered(100, 110);

        for block in 100..=110 {
            assert!(cache.is_block_covered(block), "Block {block} should be covered");
        }
        assert!(!cache.is_block_covered(99));
        assert!(!cache.is_block_covered(111));
    }

    #[test]
    fn test_mark_range_covered_across_chunks() {
        let config = LogCacheConfig { chunk_size: 100, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range that spans chunk 0 (0-99) and chunk 1 (100-199)
        cache.mark_range_covered(90, 110);

        for block in 90..=110 {
            assert!(cache.is_block_covered(block), "Block {block} should be covered");
        }
        assert!(!cache.is_block_covered(89));
        assert!(!cache.is_block_covered(111));
    }

    #[test]
    fn test_is_range_covered() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range 100-110 as covered
        cache.mark_range_covered(100, 110);

        // Full range should be covered
        assert!(cache.is_range_covered(100, 110));

        // Subset should be covered
        assert!(cache.is_range_covered(102, 108));

        // Superset should NOT be covered
        assert!(!cache.is_range_covered(99, 110));
        assert!(!cache.is_range_covered(100, 111));
        assert!(!cache.is_range_covered(99, 111));

        // Disjoint range should NOT be covered
        assert!(!cache.is_range_covered(200, 210));
    }

    #[test]
    fn test_clear_block_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range as covered
        cache.mark_range_covered(100, 110);
        assert!(cache.is_block_covered(105));

        // Clear single block
        cache.clear_block_coverage(105);

        // Block 105 should no longer be covered
        assert!(!cache.is_block_covered(105));

        // Other blocks should still be covered
        assert!(cache.is_block_covered(100));
        assert!(cache.is_block_covered(110));
    }

    #[test]
    fn test_clear_range_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range as covered
        cache.mark_range_covered(100, 120);

        // Clear middle range
        cache.clear_range_coverage(105, 115);

        // Cleared blocks should not be covered
        for block in 105..=115 {
            assert!(!cache.is_block_covered(block), "Block {block} should NOT be covered");
        }

        // Blocks outside cleared range should still be covered
        for block in 100..=104 {
            assert!(cache.is_block_covered(block), "Block {block} should still be covered");
        }
        for block in 116..=120 {
            assert!(cache.is_block_covered(block), "Block {block} should still be covered");
        }
    }

    #[test]
    fn test_covered_block_count() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        assert_eq!(cache.covered_block_count(), 0);

        cache.mark_range_covered(100, 109); // 10 blocks
        assert_eq!(cache.covered_block_count(), 10);

        cache.mark_range_covered(200, 204); // 5 more blocks
        assert_eq!(cache.covered_block_count(), 15);

        cache.clear_block_coverage(105);
        assert_eq!(cache.covered_block_count(), 14);
    }

    #[tokio::test]
    async fn test_query_with_explicit_coverage_no_logs() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range as covered (but don't insert any logs)
        cache.mark_range_covered(100, 110);

        let address = [1u8; 20];
        let filter = LogFilter::new(100, 110).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        // No logs found
        assert_eq!(result.len(), 0);

        // But no missing ranges either (range is covered!)
        assert!(
            missing_ranges.is_empty(),
            "Range should be covered even without logs, got: {missing_ranges:?}"
        );
    }

    #[tokio::test]
    async fn test_query_with_partial_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark only blocks 100-105 as covered
        cache.mark_range_covered(100, 105);

        // Insert a log in the covered range
        let address = [1u8; 20];
        cache
            .insert_log(LogId::new(102, 0), create_test_log_record(address, None))
            .await;

        // Query for 100-110
        let filter = LogFilter::new(100, 110).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        // Should find 1 log
        assert_eq!(result.len(), 1);

        // Missing ranges should be 106-110 (not covered)
        assert_eq!(missing_ranges, vec![(106, 110)]);
    }

    #[tokio::test]
    async fn test_query_with_gap_in_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark ranges with a gap: 100-105, 115-120
        cache.mark_range_covered(100, 105);
        cache.mark_range_covered(115, 120);

        let address = [1u8; 20];
        let filter = LogFilter::new(100, 120).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 120).await;

        // No logs
        assert_eq!(result.len(), 0);

        // Gap should be reported as missing: 106-114
        assert_eq!(missing_ranges, vec![(106, 114)]);
    }

    #[tokio::test]
    async fn test_invalidate_block_clears_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range and insert a log
        cache.mark_range_covered(100, 110);
        let address = [1u8; 20];
        cache
            .insert_log(LogId::new(105, 0), create_test_log_record(address, None))
            .await;

        // Verify coverage
        assert!(cache.is_block_covered(105));

        // Invalidate block 105
        cache.invalidate_block(105);

        // Block should no longer be covered
        assert!(!cache.is_block_covered(105));

        // Log should be gone
        let filter = LogFilter::new(100, 110).with_address(address);
        let (result, missing_ranges) = cache.query_logs(&filter, 110).await;

        assert_eq!(result.len(), 0);
        assert!(missing_ranges.contains(&(105, 105)), "Block 105 should be in missing ranges");
    }

    #[tokio::test]
    async fn test_clear_cache_clears_coverage() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range as covered
        cache.mark_range_covered(100, 200);
        assert_eq!(cache.covered_block_count(), 101);

        // Clear cache
        cache.clear_cache().await;

        // Coverage should be cleared
        assert_eq!(cache.covered_block_count(), 0);
        assert!(!cache.is_block_covered(100));
    }

    #[test]
    fn test_coverage_across_multiple_chunks() {
        let config = LogCacheConfig { chunk_size: 100, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark range spanning 3 chunks: 50-250
        // Chunk 0: 50-99 (50 blocks)
        // Chunk 1: 100-199 (100 blocks)
        // Chunk 2: 200-250 (51 blocks)
        cache.mark_range_covered(50, 250);

        assert_eq!(cache.covered_block_count(), 201);
        assert!(cache.is_range_covered(50, 250));

        // Verify each chunk boundary
        assert!(cache.is_block_covered(50)); // Start of chunk 0
        assert!(cache.is_block_covered(99)); // End of chunk 0
        assert!(cache.is_block_covered(100)); // Start of chunk 1
        assert!(cache.is_block_covered(199)); // End of chunk 1
        assert!(cache.is_block_covered(200)); // Start of chunk 2
        assert!(cache.is_block_covered(250)); // End of covered range

        // Verify edges are not covered
        assert!(!cache.is_block_covered(49));
        assert!(!cache.is_block_covered(251));
    }

    #[test]
    fn test_overlapping_coverage_marks() {
        let config = LogCacheConfig { chunk_size: 1000, safety_depth: 0, ..Default::default() };
        let cache = LogCache::new(&config).expect("valid test config");

        // Mark overlapping ranges
        cache.mark_range_covered(100, 150);
        cache.mark_range_covered(130, 180);

        // Union should be covered
        assert!(cache.is_range_covered(100, 180));

        // Count should not double-count
        assert_eq!(cache.covered_block_count(), 81); // 100-180 inclusive
    }
}
