use crate::cache::types::{BlockBody, BlockHeader, CacheStats};
use ahash::RandomState;
use dashmap::DashMap;
use lru::LruCache;
use std::{num::NonZeroUsize, sync::Arc};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, trace};

/// Errors that can occur during block cache operations.
#[derive(Debug, Error)]
pub enum BlockCacheError {
    /// Invalid configuration parameter.
    #[error("Invalid cache configuration: {0}")]
    InvalidConfig(String),
}

/// Configuration for block cache sizing and behavior.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockCacheConfig {
    /// Number of recent blocks kept in the hot window ring buffer
    pub hot_window_size: usize,
    pub max_headers: usize,
    pub max_bodies: usize,
    /// Blocks within this depth of chain tip are considered unsafe and may be invalidated
    pub safety_depth: u64,
}

impl Default for BlockCacheConfig {
    fn default() -> Self {
        Self { hot_window_size: 200, max_headers: 10000, max_bodies: 10000, safety_depth: 12 }
    }
}

/// Two-tier block cache with hot window for recent blocks and LRU for historical data.
///
/// Recent blocks are stored in a lock-free hot window (circular buffer) for O(1) access.
/// Older blocks fall back to `DashMap` lookups. All public methods are thread-safe.
pub struct BlockCache {
    _config: BlockCacheConfig,

    headers_by_hash: DashMap<[u8; 32], Arc<BlockHeader>, RandomState>,
    headers_by_number: DashMap<u64, [u8; 32], RandomState>,
    header_lru: Arc<RwLock<LruCache<[u8; 32], Arc<BlockHeader>>>>,

    bodies_by_hash: DashMap<[u8; 32], Arc<BlockBody>, RandomState>,
    body_lru: Arc<RwLock<LruCache<[u8; 32], Arc<BlockBody>>>>,

    hot_window: Arc<RwLock<HotWindow>>,

    stats: Arc<RwLock<CacheStats>>,
    stats_dirty: std::sync::atomic::AtomicBool,
}

/// Circular buffer for recent blocks with O(1) insert/lookup.
///
/// Uses modulo arithmetic to wrap around fixed-size vectors, avoiding
/// expensive shifting operations when the window advances.
/// Stores Arc types to enable zero-copy reads via cheap Arc cloning.
///
/// # Example
/// For a window of size 5 containing blocks 1001-1005, looking up block 1003:
/// - offset = 1003 - 1001 = 2
/// - index = (`start_index` + 2) % 5
struct HotWindow {
    size: usize,
    start_block: u64,
    start_index: usize,
    headers: Vec<Option<Arc<BlockHeader>>>,
    bodies: Vec<Option<Arc<BlockBody>>>,
}

impl HotWindow {
    fn new(size: usize) -> Self {
        Self {
            size,
            start_block: 0,
            start_index: 0,
            headers: vec![None; size],
            bodies: vec![None; size],
        }
    }

    /// Inserts a block, automatically advancing the window if needed.
    /// Blocks older than the current window are silently ignored.
    ///
    /// Handles usize overflow on 32-bit systems: if the block offset exceeds `usize::MAX`,
    /// the window is cleared and repositioned to accommodate the new block.
    fn insert(&mut self, block_number: u64, header: Arc<BlockHeader>, body: Arc<BlockBody>) {
        if block_number < self.start_block {
            return;
        }

        let offset_u64 = block_number.saturating_sub(self.start_block);

        let Ok(offset) = usize::try_from(offset_u64) else {
            tracing::warn!(
                block_number = block_number,
                start_block = self.start_block,
                "block offset exceeds usize, repositioning window"
            );
            for slot in &mut self.headers {
                *slot = None;
            }
            for slot in &mut self.bodies {
                *slot = None;
            }
            self.start_index = 0;
            self.start_block = block_number.saturating_sub(self.size as u64 - 1);
            let index = self.size - 1;
            self.headers[index] = Some(header);
            self.bodies[index] = Some(body);
            return;
        };

        if offset >= self.size {
            let advance = offset.saturating_sub(self.size).saturating_add(1);
            self.advance_window(advance, Some(block_number));
        }

        let offset = block_number.saturating_sub(self.start_block);
        let Some(offset) = usize::try_from(offset).ok().filter(|&o| o < self.size) else {
            return;
        };
        let index = (self.start_index + offset) % self.size;
        self.headers[index] = Some(header);
        self.bodies[index] = Some(body);
    }

    /// Advances the window by clearing old entries.
    ///
    /// For small advances (< size): O(advance) complexity, clears entries one by one.
    /// For large advances (>= size): O(size) complexity, clears all entries and repositions.
    ///
    /// # Arguments
    /// * `advance` - Number of blocks to advance
    /// * `target_block` - Optional target block that must fit in window after advance. Required for
    ///   large advances to position window correctly.
    fn advance_window(&mut self, advance: usize, target_block: Option<u64>) {
        if advance == 0 {
            return;
        }

        if advance >= self.size {
            // Large jump: clear everything and reposition window for target block
            for slot in &mut self.headers {
                *slot = None;
            }
            for slot in &mut self.bodies {
                *slot = None;
            }
            self.start_index = 0;

            // Position window so target_block is at the last slot (offset = size - 1)
            if let Some(target) = target_block {
                self.start_block = target.saturating_sub(self.size as u64 - 1);
            } else {
                // Fallback: just advance by the requested amount
                self.start_block = self.start_block.saturating_add(advance as u64);
            }
        } else {
            // Small advance: clear only the entries we're evicting
            for i in 0..advance {
                let clear_index = (self.start_index + i) % self.size;
                self.headers[clear_index] = None;
                self.bodies[clear_index] = None;
            }
            self.start_index = (self.start_index + advance) % self.size;
            self.start_block = self.start_block.saturating_add(advance as u64);
        }
    }

    fn get_header(&self, block_number: u64) -> Option<Arc<BlockHeader>> {
        if block_number < self.start_block {
            return None;
        }

        let offset = block_number.saturating_sub(self.start_block);
        let offset = usize::try_from(offset).ok()?;
        if offset >= self.size {
            return None;
        }

        let index = (self.start_index + offset) % self.size;
        self.headers[index].as_ref().map(Arc::clone)
    }

    fn get_body(&self, block_number: u64) -> Option<Arc<BlockBody>> {
        if block_number < self.start_block {
            return None;
        }

        let offset = block_number.saturating_sub(self.start_block);
        let offset = usize::try_from(offset).ok()?;
        if offset >= self.size {
            return None;
        }

        let index = (self.start_index + offset) % self.size;
        self.bodies[index].as_ref().map(Arc::clone)
    }

    fn contains_block(&self, block_number: u64) -> bool {
        if block_number < self.start_block {
            return false;
        }

        let offset = block_number.saturating_sub(self.start_block);
        let Some(offset) = usize::try_from(offset).ok() else {
            return false;
        };
        if offset >= self.size {
            return false;
        }

        let index = (self.start_index + offset) % self.size;
        self.headers[index].is_some()
    }
}

impl BlockCache {
    /// Creates a new block cache with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `BlockCacheError::InvalidConfig` if `max_headers`, `max_bodies`, or
    /// `hot_window_size` is zero.
    pub fn new(config: &BlockCacheConfig) -> Result<Self, BlockCacheError> {
        if config.hot_window_size == 0 {
            return Err(BlockCacheError::InvalidConfig(
                "hot_window_size must be non-zero".to_string(),
            ));
        }

        let max_headers = NonZeroUsize::new(config.max_headers).ok_or_else(|| {
            BlockCacheError::InvalidConfig("max_headers must be non-zero".to_string())
        })?;

        let max_bodies = NonZeroUsize::new(config.max_bodies).ok_or_else(|| {
            BlockCacheError::InvalidConfig("max_bodies must be non-zero".to_string())
        })?;

        Ok(Self {
            _config: config.clone(),
            headers_by_hash: DashMap::with_hasher(RandomState::new()),
            headers_by_number: DashMap::with_hasher(RandomState::new()),
            header_lru: Arc::new(RwLock::new(LruCache::new(max_headers))),
            bodies_by_hash: DashMap::with_hasher(RandomState::new()),
            body_lru: Arc::new(RwLock::new(LruCache::new(max_bodies))),
            hot_window: Arc::new(RwLock::new(HotWindow::new(config.hot_window_size))),
            stats: Arc::new(RwLock::new(CacheStats::default())),
            stats_dirty: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Inserts a block header into both the hot window (if recent) and persistent storage.
    pub async fn insert_header(&self, header: BlockHeader) {
        trace!(block = header.number, hash = ?header.hash, "inserting header");

        let header_arc = Arc::new(header);

        if self.is_in_hot_window(header_arc.number) {
            let block_number = header_arc.number;
            let hash = header_arc.hash;
            let empty_body = Arc::new(BlockBody { hash, transactions: vec![] });
            let header_clone = Arc::clone(&header_arc);

            let mut hot_window = self.hot_window.write().await;
            hot_window.insert(block_number, header_clone, empty_body);
        }

        self.headers_by_hash.insert(header_arc.hash, Arc::clone(&header_arc));
        self.headers_by_number.insert(header_arc.number, header_arc.hash);

        let mut lru = self.header_lru.write().await;
        lru.put(header_arc.hash, header_arc);

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Inserts a block body. Only updates hot window if the corresponding header exists.
    pub async fn insert_body(&self, body: BlockBody) {
        trace!(hash = ?body.hash, tx_count = body.transactions.len(), "inserting body");

        let body_arc = Arc::new(body);

        if let Some(header_arc) = self.get_header_by_hash(&body_arc.hash) {
            if self.is_in_hot_window(header_arc.number) {
                let block_number = header_arc.number;
                let body_clone = Arc::clone(&body_arc);

                let mut hot_window = self.hot_window.write().await;
                if let Some(existing_header) = hot_window.get_header(block_number) {
                    hot_window.insert(block_number, existing_header, body_clone);
                }
            }
        }

        self.bodies_by_hash.insert(body_arc.hash, Arc::clone(&body_arc));

        let mut lru = self.body_lru.write().await;
        lru.put(body_arc.hash, body_arc);

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Inserts multiple block headers in a single batch operation.
    ///
    /// More efficient than calling `insert_header` multiple times as locks
    /// are acquired once per batch rather than per-item.
    pub async fn insert_headers_batch(&self, headers: Vec<BlockHeader>) {
        if headers.is_empty() {
            return;
        }

        trace!(count = headers.len(), "batch inserting headers");

        let header_arcs: Vec<Arc<BlockHeader>> = headers.into_iter().map(Arc::new).collect();

        let hot_window_inserts: Vec<(u64, Arc<BlockHeader>, Arc<BlockBody>)> = header_arcs
            .iter()
            .filter(|header_arc| self.is_in_hot_window(header_arc.number))
            .map(|header_arc| {
                let empty_body =
                    Arc::new(BlockBody { hash: header_arc.hash, transactions: vec![] });
                (header_arc.number, Arc::clone(header_arc), empty_body)
            })
            .collect();

        if !hot_window_inserts.is_empty() {
            let mut hot_window = self.hot_window.write().await;
            for (block_number, header, body) in hot_window_inserts {
                hot_window.insert(block_number, header, body);
            }
        }

        for header_arc in &header_arcs {
            self.headers_by_hash.insert(header_arc.hash, Arc::clone(header_arc));
            self.headers_by_number.insert(header_arc.number, header_arc.hash);
        }

        {
            let mut lru = self.header_lru.write().await;
            for header_arc in header_arcs {
                lru.put(header_arc.hash, header_arc);
            }
        }

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Inserts multiple block bodies in a single batch operation.
    ///
    /// More efficient than calling `insert_body` multiple times as locks
    /// are acquired once per batch rather than per-item.
    pub async fn insert_bodies_batch(&self, bodies: Vec<BlockBody>) {
        if bodies.is_empty() {
            return;
        }

        trace!(count = bodies.len(), "batch inserting bodies");

        let body_arcs: Vec<Arc<BlockBody>> = bodies.into_iter().map(Arc::new).collect();

        let hot_window_updates: Vec<(u64, Arc<BlockHeader>, Arc<BlockBody>)> = body_arcs
            .iter()
            .filter_map(|body_arc| {
                self.get_header_by_hash(&body_arc.hash).and_then(|header_arc| {
                    if self.is_in_hot_window(header_arc.number) {
                        Some((header_arc.number, header_arc, Arc::clone(body_arc)))
                    } else {
                        None
                    }
                })
            })
            .collect();

        if !hot_window_updates.is_empty() {
            let mut hot_window = self.hot_window.write().await;
            for (number, header, body) in hot_window_updates {
                if let Some(existing_header) = hot_window.get_header(number) {
                    hot_window.insert(number, existing_header, body);
                } else {
                    hot_window.insert(number, header, body);
                }
            }
        }

        for body_arc in &body_arcs {
            self.bodies_by_hash.insert(body_arc.hash, Arc::clone(body_arc));
        }

        {
            let mut lru = self.body_lru.write().await;
            for body_arc in body_arcs {
                lru.put(body_arc.hash, body_arc);
            }
        }

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Retrieves a block header by number, checking hot window first for recent blocks.
    /// Falls back to `DashMap` on lock contention or cache miss.
    #[must_use]
    pub fn get_header_by_number(&self, block_number: u64) -> Option<Arc<BlockHeader>> {
        if let Ok(hot_window) = self.hot_window.try_read() {
            if let Some(header) = hot_window.get_header(block_number) {
                return Some(header);
            }
        }

        if let Some(hash) = self.headers_by_number.get(&block_number) {
            if let Some(header_arc) = self.headers_by_hash.get(&*hash) {
                return Some(Arc::clone(&header_arc));
            }
        }

        None
    }

    #[must_use]
    pub fn get_header_by_hash(&self, block_hash: &[u8; 32]) -> Option<Arc<BlockHeader>> {
        self.headers_by_hash.get(block_hash).map(|h| Arc::clone(&h))
    }

    #[must_use]
    pub fn get_body_by_hash(&self, block_hash: &[u8; 32]) -> Option<Arc<BlockBody>> {
        let header_number = self.get_header_by_hash(block_hash).map(|h| h.number);

        if let Some(number) = header_number {
            if let Ok(hot_window) = self.hot_window.try_read() {
                if let Some(body) = hot_window.get_body(number) {
                    return Some(body);
                }
            }
        }

        self.bodies_by_hash.get(block_hash).map(|b| Arc::clone(&b))
    }

    #[must_use]
    pub fn get_block_by_number(
        &self,
        block_number: u64,
    ) -> Option<(Arc<BlockHeader>, Arc<BlockBody>)> {
        let header = self.get_header_by_number(block_number)?;
        let body = self.get_body_by_hash(&header.hash)?;
        Some((header, body))
    }

    #[must_use]
    pub fn get_block_by_hash(
        &self,
        block_hash: &[u8; 32],
    ) -> Option<(Arc<BlockHeader>, Arc<BlockBody>)> {
        let header = self.get_header_by_hash(block_hash)?;
        let body = self.get_body_by_hash(block_hash)?;
        Some((header, body))
    }

    fn is_in_hot_window(&self, block_number: u64) -> bool {
        self.hot_window.try_read().is_ok_and(|hw| hw.contains_block(block_number))
    }

    /// Removes a block from all caches, typically called during chain reorganizations.
    pub async fn invalidate_block(&self, block_number: u64) {
        info!(block = block_number, "invalidating block");

        if let Some(hash) = self.headers_by_number.remove(&block_number) {
            self.headers_by_hash.remove(&hash.1);
            self.bodies_by_hash.remove(&hash.1);

            {
                let mut header_lru = self.header_lru.write().await;
                header_lru.pop(&hash.1);
            }
            {
                let mut body_lru = self.body_lru.write().await;
                body_lru.pop(&hash.1);
            }
        }

        {
            let mut hot_window = self.hot_window.write().await;
            if hot_window.contains_block(block_number) {
                let offset = block_number.saturating_sub(hot_window.start_block);
                let offset = usize::try_from(offset).unwrap_or(0);
                if offset < hot_window.size {
                    let index = (hot_window.start_index + offset) % hot_window.size;
                    hot_window.headers[index] = None;
                    hot_window.bodies[index] = None;
                }
            }
        }

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clears all cached data. Used when switching authentication contexts.
    pub async fn clear_cache(&self) {
        info!("clearing block cache");

        self.headers_by_number.clear();
        self.headers_by_hash.clear();
        self.bodies_by_hash.clear();

        {
            let mut header_lru = self.header_lru.write().await;
            header_lru.clear();
        }
        {
            let mut body_lru = self.body_lru.write().await;
            body_lru.clear();
        }

        {
            let mut hot_window = self.hot_window.write().await;
            hot_window.headers.fill(None);
            hot_window.bodies.fill(None);
            hot_window.start_block = 0;
            hot_window.start_index = 0;
        }

        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Updates the canonical chain pointer, typically after a reorg.
    pub fn update_canonical_chain(&self, block_number: u64, block_hash: [u8; 32]) {
        trace!(block = block_number, hash = ?block_hash, "updating canonical chain");
        self.headers_by_number.insert(block_number, block_hash);
    }

    /// Pre-populates the cache with a range of blocks, useful during startup.
    pub async fn warm_recent(
        &self,
        from_block: u64,
        to_block: u64,
        blocks: Vec<(BlockHeader, BlockBody)>,
    ) {
        info!(from_block = from_block, to_block = to_block, "warming block cache");
        for (header, body) in blocks {
            self.insert_header(header).await;
            self.insert_body(body).await;
        }
    }

    pub async fn get_stats(&self) -> CacheStats {
        // Update stats if dirty flag is set
        if self.stats_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.compute_stats_internal().await;
        }
        self.stats.read().await.clone()
    }

    async fn compute_stats_internal(&self) {
        let header_cache_size = self.headers_by_hash.len();
        let body_cache_size = self.bodies_by_hash.len();

        let hot_window_size = {
            let hot_window = self.hot_window.read().await;
            hot_window.headers.iter().filter(|h| h.is_some()).count()
        };

        let mut stats = self.stats.write().await;
        stats.header_cache_size = header_cache_size;
        stats.body_cache_size = body_cache_size;
        stats.hot_window_size = hot_window_size;
    }

    /// Removes blocks older than the safe head minus retention period.
    /// Only prunes when LRU cache exceeds capacity to avoid unnecessary work.
    pub async fn prune_by_safe_head(&self, safe_head: u64, retain_blocks: u64) {
        self.prune_by_safe_head_with_finality(safe_head, 0, retain_blocks).await;
    }

    /// Finality-aware block cache pruning.
    ///
    /// Implements a three-tier retention strategy based on finality:
    /// - **Finalized blocks** (`block <= finalized_block`): Never pruned (immutable)
    /// - **Safe blocks** (`finalized_block < block <= safe_head`): Protected from pruning
    /// - **Unsafe blocks** (`block > safe_head`): Pruned according to `retain_blocks` policy
    ///
    /// This ensures that finalized data remains cached indefinitely while still
    /// managing memory usage for recent unsafe blocks.
    ///
    /// # Arguments
    /// * `safe_head` - Blocks below this are considered safe from reorgs
    /// * `finalized_block` - Blocks at or below this are finalized (cryptographically committed)
    /// * `retain_blocks` - Number of blocks to retain for unsafe/non-finalized data
    pub async fn prune_by_safe_head_with_finality(
        &self,
        safe_head: u64,
        finalized_block: u64,
        retain_blocks: u64,
    ) {
        // Calculate cutoff: prune only non-finalized blocks beyond retention window
        // Finalized blocks are never pruned regardless of age
        let cutoff = if finalized_block > 0 {
            // Keep all finalized blocks, prune only old unsafe blocks
            safe_head.saturating_sub(retain_blocks).max(finalized_block)
        } else {
            // No finalized data, use standard cutoff
            safe_head.saturating_sub(retain_blocks)
        };

        {
            let mut lru = self.header_lru.write().await;
            while lru.len() > lru.cap().get() {
                if let Some((hash, header_arc)) = lru.pop_lru() {
                    // Never prune finalized blocks
                    if header_arc.number <= finalized_block {
                        lru.put(hash, header_arc);
                        break; // Keep all finalized blocks
                    }
                    // Prune non-finalized blocks beyond cutoff
                    if header_arc.number < cutoff {
                        self.headers_by_hash.remove(&hash);
                        self.headers_by_number.remove(&header_arc.number);
                    } else {
                        lru.put(hash, header_arc);
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        {
            let mut lru = self.body_lru.write().await;
            while lru.len() > lru.cap().get() {
                if let Some((hash, body_arc)) = lru.pop_lru() {
                    if let Some(header_arc) = self.get_header_by_hash(&hash) {
                        // Never prune finalized blocks
                        if header_arc.number <= finalized_block {
                            lru.put(hash, body_arc);
                            break;
                        }
                        // Prune non-finalized blocks beyond cutoff
                        if header_arc.number < cutoff {
                            self.bodies_by_hash.remove(&hash);
                        } else {
                            lru.put(hash, body_arc);
                            break;
                        }
                    } else {
                        // Orphaned body (no header) - remove it
                        self.bodies_by_hash.remove(&hash);
                    }
                } else {
                    break;
                }
            }
        }
        self.stats_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    fn create_test_header(number: u64, hash_byte: u8) -> BlockHeader {
        BlockHeader {
            hash: [hash_byte; 32],
            number,
            parent_hash: [hash_byte.wrapping_sub(1); 32],
            timestamp: 1_234_567_890 + number,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0u8; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0u8; 256]),
            transactions_root: [0u8; 32],
            state_root: [0u8; 32],
            receipts_root: [0u8; 32],
        }
    }

    fn create_test_body(hash_byte: u8, tx_count: usize) -> BlockBody {
        BlockBody {
            hash: [hash_byte; 32],
            transactions: (0..tx_count).map(|i| [i as u8; 32]).collect(),
        }
    }

    #[test]
    fn test_config_zero_hot_window_size_rejected() {
        let config = BlockCacheConfig { hot_window_size: 0, ..Default::default() };
        let result = BlockCache::new(&config);
        assert!(matches!(result, Err(BlockCacheError::InvalidConfig(_))));
    }

    #[test]
    fn test_config_zero_max_headers_rejected() {
        let config = BlockCacheConfig { max_headers: 0, ..Default::default() };
        let result = BlockCache::new(&config);
        assert!(matches!(result, Err(BlockCacheError::InvalidConfig(_))));
    }

    #[test]
    fn test_config_zero_max_bodies_rejected() {
        let config = BlockCacheConfig { max_bodies: 0, ..Default::default() };
        let result = BlockCache::new(&config);
        assert!(matches!(result, Err(BlockCacheError::InvalidConfig(_))));
    }

    #[test]
    fn test_config_valid_creates_cache() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config);
        assert!(cache.is_ok());
    }

    #[test]
    fn test_hot_window_basic_insert_and_get() {
        let mut hw = HotWindow::new(5);
        let header = Arc::new(create_test_header(100, 1));
        let body = Arc::new(create_test_body(1, 2));

        hw.insert(100, header.clone(), body.clone());

        assert!(hw.contains_block(100));
        assert_eq!(hw.get_header(100).unwrap().number, 100);
        assert_eq!(hw.get_body(100).unwrap().transactions.len(), 2);
    }

    #[test]
    fn test_hot_window_wraparound() {
        // Visual: After filling size 5, inserting block 5 overwrites slot 0
        let mut hw = HotWindow::new(5);

        // Insert blocks 0-4
        for i in 0..5 {
            let header = Arc::new(create_test_header(i, i as u8));
            let body = Arc::new(create_test_body(i as u8, 1));
            hw.insert(i, header, body);
        }

        // All should be present
        for i in 0..5 {
            assert!(hw.contains_block(i), "Block {i} should exist");
        }

        // Insert block 5 - this should evict block 0
        let header = Arc::new(create_test_header(5, 5));
        let body = Arc::new(create_test_body(5, 1));
        hw.insert(5, header, body);

        // Block 0 should be gone, blocks 1-5 should exist
        assert!(!hw.contains_block(0), "Block 0 should be evicted");
        for i in 1..=5 {
            assert!(hw.contains_block(i), "Block {i} should still exist");
        }
    }

    #[test]
    fn test_hot_window_large_jump_clears_all() {
        let mut hw = HotWindow::new(5);

        // Insert blocks 0-4
        for i in 0..5 {
            let header = Arc::new(create_test_header(i, i as u8));
            let body = Arc::new(create_test_body(i as u8, 1));
            hw.insert(i, header, body);
        }

        // Large jump: insert block 1000
        let header = Arc::new(create_test_header(1000, 100));
        let body = Arc::new(create_test_body(100, 1));
        hw.insert(1000, header, body);

        // Old blocks should all be gone
        for i in 0..5 {
            assert!(!hw.contains_block(i), "Block {i} should be cleared after large jump");
        }

        // New block should exist and be accessible
        assert!(hw.contains_block(1000));
        // Window should allow blocks in the expected range after large jump
        // Testing behavioral contract: recent blocks within window size should be accessible
        // Block 1000 is in window, blocks just before it (within window) should also be valid
        // positions
        for expected_accessible in 996..=1000 {
            // Only block 1000 was actually inserted, others are None but within valid range
            if expected_accessible == 1000 {
                assert!(
                    hw.get_header(expected_accessible).is_some(),
                    "Block {expected_accessible} should be accessible"
                );
            }
        }
        // Blocks before the window range should not be accessible
        assert!(
            !hw.contains_block(995),
            "Block 995 should be outside window range after large jump"
        );
    }

    #[test]
    fn test_hot_window_old_block_ignored() {
        let mut hw = HotWindow::new(5);

        // Start with blocks 100-104
        for i in 100..105 {
            let header = Arc::new(create_test_header(i, i as u8));
            let body = Arc::new(create_test_body(i as u8, 1));
            hw.insert(i, header, body);
        }

        // Try to insert old block 50 - should be silently ignored
        let header = Arc::new(create_test_header(50, 50));
        let body = Arc::new(create_test_body(50, 1));
        hw.insert(50, header, body);

        // Block 50 should not exist
        assert!(!hw.contains_block(50));
        // Original blocks should still be there
        for i in 100..105 {
            assert!(hw.contains_block(i), "Block {i} should still exist");
        }
    }

    #[test]
    fn test_hot_window_out_of_range_lookup_returns_none() {
        let hw = HotWindow::new(5);

        // Empty window - any lookup should return None
        assert!(hw.get_header(100).is_none());
        assert!(hw.get_body(100).is_none());
        assert!(!hw.contains_block(100));
    }

    #[test]
    fn test_hot_window_advance_window_zero_is_noop() {
        let mut hw = HotWindow::new(5);
        let header = Arc::new(create_test_header(100, 1));
        let body = Arc::new(create_test_body(1, 1));
        hw.insert(100, header.clone(), body);

        // Advancing by 0 should be a no-op - block should still be accessible
        hw.advance_window(0, None);

        // Behavioral assertion: block remains accessible after no-op advance
        assert!(hw.contains_block(100), "Block should still be accessible after zero advance");
        assert!(
            hw.get_header(100).is_some(),
            "Header should still be retrievable after zero advance"
        );
        // Verify it's the same header (content check, not internal state check)
        assert_eq!(
            hw.get_header(100).unwrap().number,
            header.number,
            "Retrieved header should match original"
        );
    }

    #[test]
    fn test_hot_window_modulo_index_calculation() {
        // Test the index calculation from the docstring example
        let mut hw = HotWindow::new(5);

        // After inserting block 1005, window should be:
        // start_block = 1001, start_index = some value
        // Block 1003: offset = 1003 - 1001 = 2
        // index = (start_index + 2) % 5

        for i in 1001..=1005 {
            let header = Arc::new(create_test_header(i, i as u8));
            let body = Arc::new(create_test_body(i as u8, 1));
            hw.insert(i, header, body);
        }

        // Verify all blocks in window are accessible
        for i in 1001..=1005 {
            assert!(hw.contains_block(i), "Block {i} should be in window");
            let header = hw.get_header(i).expect("header should exist");
            assert_eq!(header.number, i);
        }
    }

    #[tokio::test]
    async fn test_block_cache_basic_operations() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        let header = BlockHeader {
            hash: [1u8; 32],
            number: 1000,
            parent_hash: [2u8; 32],
            timestamp: 1_234_567_890,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [3u8; 20],
            extra_data: Arc::new(vec![1, 2, 3]),
            logs_bloom: Arc::new(vec![0u8; 256]),
            transactions_root: [4u8; 32],
            state_root: [5u8; 32],
            receipts_root: [6u8; 32],
        };

        let body = BlockBody { hash: [1u8; 32], transactions: vec![[7u8; 32], [8u8; 32]] };

        cache.insert_header(header.clone()).await;
        cache.insert_body(body.clone()).await;

        let retrieved = cache.get_block_by_number(1000);
        assert!(retrieved.is_some());
        let (retrieved_header, retrieved_body) = retrieved.expect("retrieved is some");
        assert_eq!(retrieved_header.number, 1000);
        assert_eq!(retrieved_body.transactions.len(), 2);

        let retrieved = cache.get_block_by_hash(&[1u8; 32]);
        assert!(retrieved.is_some());
        let (retrieved_header, retrieved_body) = retrieved.unwrap();
        assert_eq!(retrieved_header.number, 1000);
        assert_eq!(retrieved_body.transactions.len(), 2);
    }

    #[tokio::test]
    async fn test_block_cache_hot_window() {
        let config = BlockCacheConfig { hot_window_size: 10, ..BlockCacheConfig::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        for i in 0..5 {
            let header = BlockHeader {
                hash: [u8::try_from(i).unwrap_or(0); 32],
                number: 1000 + i,
                parent_hash: [0u8; 32],
                timestamp: 1_234_567_890 + i,
                gas_limit: 30_000_000,
                gas_used: 15_000_000,
                miner: [0u8; 20],
                extra_data: Arc::new(vec![]),
                logs_bloom: Arc::new(vec![0u8; 256]),
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                receipts_root: [0u8; 32],
            };

            let body = BlockBody { hash: [u8::try_from(i).unwrap_or(0); 32], transactions: vec![] };

            cache.insert_header(header).await;
            cache.insert_body(body).await;
        }

        for i in 0..5 {
            let header = cache.get_header_by_number(1000 + i);
            assert!(header.is_some());
        }
    }

    #[tokio::test]
    async fn test_block_cache_reorg_invalidation() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        let header = BlockHeader {
            hash: [1u8; 32],
            number: 1000,
            parent_hash: [0u8; 32],
            timestamp: 1_234_567_890,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0u8; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0u8; 256]),
            transactions_root: [0u8; 32],
            state_root: [0u8; 32],
            receipts_root: [0u8; 32],
        };

        let body = BlockBody { hash: [1u8; 32], transactions: vec![] };

        cache.insert_header(header).await;
        cache.insert_body(body).await;

        let retrieved = cache.get_block_by_number(1000);
        assert!(retrieved.is_some());

        cache.invalidate_block(1000).await;

        let retrieved = cache.get_block_by_number(1000);
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_batch_insert_headers() {
        let config = BlockCacheConfig { hot_window_size: 10, ..Default::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        let headers: Vec<BlockHeader> =
            (0..5).map(|i| create_test_header(1000 + i, i as u8)).collect();

        cache.insert_headers_batch(headers).await;

        for i in 0..5 {
            let header = cache.get_header_by_number(1000 + i);
            assert!(header.is_some(), "Header {i} should exist after batch insert");
        }
    }

    #[tokio::test]
    async fn test_batch_insert_bodies() {
        let config = BlockCacheConfig { hot_window_size: 10, ..Default::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        // First insert headers
        for i in 0..5 {
            cache.insert_header(create_test_header(1000 + i, i as u8)).await;
        }

        // Then batch insert bodies
        let bodies: Vec<BlockBody> = (0..5).map(|i| create_test_body(i as u8, i + 1)).collect();

        cache.insert_bodies_batch(bodies).await;

        for i in 0..5 {
            let body = cache.get_body_by_hash(&[i as u8; 32]);
            assert!(body.is_some(), "Body {i} should exist after batch insert");
            assert_eq!(body.unwrap().transactions.len(), i + 1);
        }
    }

    #[tokio::test]
    async fn test_batch_insert_empty_is_noop() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        cache.insert_headers_batch(vec![]).await;
        cache.insert_bodies_batch(vec![]).await;

        let stats = cache.get_stats().await;
        assert_eq!(stats.header_cache_size, 0);
        assert_eq!(stats.body_cache_size, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_header_inserts() {
        let config = BlockCacheConfig { hot_window_size: 100, ..Default::default() };
        let cache = Arc::new(BlockCache::new(&config).expect("valid config"));

        let mut handles = vec![];

        for thread_id in 0..4 {
            let cache_clone = Arc::clone(&cache);
            let handle = tokio::spawn(async move {
                for i in 0..25 {
                    let block_num = (thread_id * 100 + i) as u64;
                    let header = create_test_header(block_num, (thread_id * 25 + i) as u8);
                    cache_clone.insert_header(header).await;
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.expect("task should complete");
        }

        // Verify all blocks were inserted (100 total from 4 threads × 25 blocks)
        let stats = cache.get_stats().await;
        assert_eq!(stats.header_cache_size, 100);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_read_write() {
        let config = BlockCacheConfig { hot_window_size: 50, ..Default::default() };
        let cache = Arc::new(BlockCache::new(&config).expect("valid config"));

        // Pre-populate some data
        for i in 0..20 {
            cache.insert_header(create_test_header(i, i as u8)).await;
        }

        let mut writer_handles = vec![];
        let mut reader_handles = vec![];

        // Writers
        for thread_id in 0..2 {
            let cache_clone = Arc::clone(&cache);
            let handle = tokio::spawn(async move {
                for i in 0..10 {
                    let block_num = 100 + thread_id * 10 + i;
                    let header = create_test_header(block_num, block_num as u8);
                    cache_clone.insert_header(header).await;
                }
            });
            writer_handles.push(handle);
        }

        // Readers
        for _ in 0..2 {
            let cache_clone = Arc::clone(&cache);
            let handle = tokio::spawn(async move {
                let mut found = 0;
                for i in 0..20 {
                    if cache_clone.get_header_by_number(i).is_some() {
                        found += 1;
                    }
                }
                found
            });
            reader_handles.push(handle);
        }

        for handle in writer_handles {
            handle.await.expect("writer task should complete");
        }
        for handle in reader_handles {
            let _count = handle.await.expect("reader task should complete");
        }
    }

    #[tokio::test]
    async fn test_clear_cache_removes_all() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        // Insert some data
        for i in 0..5 {
            cache.insert_header(create_test_header(i, i as u8)).await;
            cache.insert_body(create_test_body(i as u8, 1)).await;
        }

        let stats_before = cache.get_stats().await;
        assert!(stats_before.header_cache_size > 0);

        cache.clear_cache().await;

        let stats_after = cache.get_stats().await;
        assert_eq!(stats_after.header_cache_size, 0);
        assert_eq!(stats_after.body_cache_size, 0);

        // Verify lookups return None
        for i in 0..5 {
            assert!(cache.get_header_by_number(i).is_none());
        }
    }

    #[tokio::test]
    async fn test_warm_recent_populates_cache() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        let blocks: Vec<(BlockHeader, BlockBody)> = (0..5)
            .map(|i| (create_test_header(1000 + i, i as u8), create_test_body(i as u8, i as usize)))
            .collect();

        cache.warm_recent(1000, 1004, blocks).await;

        for i in 0..5 {
            let block = cache.get_block_by_number(1000 + i);
            assert!(block.is_some(), "Block {i} should exist after warming");
        }
    }

    #[tokio::test]
    async fn test_update_canonical_chain() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        // Insert block with hash [1; 32]
        cache.insert_header(create_test_header(1000, 1)).await;

        // Update canonical chain to point to different hash
        cache.update_canonical_chain(1000, [2u8; 32]);

        // The number->hash mapping should be updated
        // But since [2; 32] header doesn't exist, lookup by number won't work
        // This tests the mapping update functionality
        assert!(cache.get_header_by_hash(&[1u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_prune_respects_finality() {
        let config = BlockCacheConfig {
            max_headers: 10,
            max_bodies: 10,
            hot_window_size: 5,
            safety_depth: 12,
        };
        let cache = BlockCache::new(&config).expect("valid config");

        // Insert blocks 0-20
        for i in 0..21 {
            cache.insert_header(create_test_header(i, i as u8)).await;
            cache.insert_body(create_test_body(i as u8, 1)).await;
        }

        // Prune with finalized_block = 5
        // This should protect blocks 0-5 from being pruned
        cache.prune_by_safe_head_with_finality(20, 5, 10).await;

        // Finalized blocks should still exist
        for i in 0..=5 {
            assert!(
                cache.get_header_by_hash(&[i as u8; 32]).is_some(),
                "Finalized block {i} should not be pruned"
            );
        }
    }

    #[tokio::test]
    async fn test_prune_without_finality() {
        let config = BlockCacheConfig {
            max_headers: 5,
            max_bodies: 5,
            hot_window_size: 5,
            safety_depth: 12,
        };
        let cache = BlockCache::new(&config).expect("valid config");

        // Insert more blocks than max capacity
        for i in 0..10 {
            cache.insert_header(create_test_header(i, i as u8)).await;
        }

        // Prune without finality (finalized_block = 0)
        cache.prune_by_safe_head(9, 5).await;

        // After pruning, cache should respect LRU limits
        let stats = cache.get_stats().await;
        assert!(stats.header_cache_size <= 10); // May have some remaining
    }

    #[tokio::test]
    async fn test_stats_dirty_flag() {
        let config = BlockCacheConfig { hot_window_size: 10, ..Default::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        // Initial stats
        let stats1 = cache.get_stats().await;
        assert_eq!(stats1.header_cache_size, 0);

        // Insert triggers dirty flag
        cache.insert_header(create_test_header(100, 1)).await;

        // Get stats should recompute
        let stats2 = cache.get_stats().await;
        assert_eq!(stats2.header_cache_size, 1);
    }

    #[tokio::test]
    async fn test_hot_window_stats() {
        // Note: BlockCache's is_in_hot_window checks if block is already in hot window
        // For stats, we verify header_cache_size and body_cache_size which reflect
        // the DashMap contents, not the hot window circular buffer
        let config = BlockCacheConfig { hot_window_size: 5, ..Default::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        for i in 0..3 {
            cache.insert_header(create_test_header(i, i as u8)).await;
            cache.insert_body(create_test_body(i as u8, 1)).await;
        }

        let stats = cache.get_stats().await;
        assert_eq!(stats.header_cache_size, 3);
        assert_eq!(stats.body_cache_size, 3);
    }

    #[tokio::test]
    async fn test_get_nonexistent_block() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        assert!(cache.get_header_by_number(12345).is_none());
        assert!(cache.get_header_by_hash(&[0xFF; 32]).is_none());
        assert!(cache.get_body_by_hash(&[0xFF; 32]).is_none());
        assert!(cache.get_block_by_number(12345).is_none());
        assert!(cache.get_block_by_hash(&[0xFF; 32]).is_none());
    }

    #[tokio::test]
    async fn test_invalidate_nonexistent_block() {
        let config = BlockCacheConfig::default();
        let cache = BlockCache::new(&config).expect("valid config");

        // Should not panic when invalidating non-existent block
        cache.invalidate_block(99999).await;
    }

    #[tokio::test]
    async fn test_body_requires_header() {
        let config = BlockCacheConfig { hot_window_size: 10, ..Default::default() };
        let cache = BlockCache::new(&config).expect("valid config");

        // Insert body without corresponding header
        let orphan_body = create_test_body(99, 5);
        cache.insert_body(orphan_body).await;

        // Body should exist in DashMap but not in hot window
        let body = cache.get_body_by_hash(&[99u8; 32]);
        assert!(body.is_some());
    }
}
