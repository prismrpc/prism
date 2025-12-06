use crate::cache::types::{CacheStats, LogRecord, ReceiptRecord, TransactionRecord};
use ahash::RandomState;
use dashmap::DashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, trace};

/// Errors that occur during transaction cache initialization.
#[derive(Debug, Error)]
pub enum TransactionCacheError {
    /// Invalid configuration parameter (typically zero capacity).
    #[error("Invalid cache configuration: {0}")]
    InvalidConfig(String),
}

/// Configuration for transaction and receipt cache sizing.
///
/// Controls LRU capacity for both transaction and receipt caches independently,
/// plus safety depth for determining when cached data is unlikely to change due to reorgs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TransactionCacheConfig {
    /// Maximum number of transactions to cache (default: 50,000)
    pub max_transactions: usize,
    /// Maximum number of receipts to cache (default: 50,000)
    pub max_receipts: usize,
    /// Minimum block depth before considering data safe from reorgs (default: 12)
    pub safety_depth: u64,
}

impl Default for TransactionCacheConfig {
    fn default() -> Self {
        Self { max_transactions: 50000, max_receipts: 50000, safety_depth: 12 }
    }
}

/// Cache for transactions and receipts with explicit capacity management.
///
/// This cache uses `DashMap` for fast concurrent lookups with explicit pruning
/// when capacity is exceeded. Pruning evicts from oldest blocks first, which is
/// optimal for blockchain data where newer blocks are more frequently accessed.
///
/// Additionally maintains a block-to-transactions index to support `eth_getBlockByNumber`
/// queries and efficient reorg handling.
pub struct TransactionCache {
    // Transaction storage with capacity limit
    transactions_by_hash: DashMap<[u8; 32], Arc<TransactionRecord>, RandomState>,
    max_transactions: usize,

    // Receipt storage with capacity limit
    receipts_by_hash: DashMap<[u8; 32], Arc<ReceiptRecord>, RandomState>,
    max_receipts: usize,

    // Block number → transaction hashes for efficient block-level queries
    block_transactions: DashMap<u64, Vec<[u8; 32]>, RandomState>,

    stats: Arc<RwLock<CacheStats>>,

    /// Atomic hit counter for transactions
    tx_hits: std::sync::atomic::AtomicU64,
    /// Atomic miss counter for transactions
    tx_misses: std::sync::atomic::AtomicU64,
    /// Atomic hit counter for receipts
    receipt_hits: std::sync::atomic::AtomicU64,
    /// Atomic miss counter for receipts
    receipt_misses: std::sync::atomic::AtomicU64,
}

impl TransactionCache {
    /// Creates a new transaction cache with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `TransactionCacheError::InvalidConfig` if `max_transactions` or `max_receipts`
    /// is zero.
    pub fn new(config: &TransactionCacheConfig) -> Result<Self, TransactionCacheError> {
        if config.max_transactions == 0 {
            return Err(TransactionCacheError::InvalidConfig(
                "max_transactions must be non-zero".to_string(),
            ));
        }

        if config.max_receipts == 0 {
            return Err(TransactionCacheError::InvalidConfig(
                "max_receipts must be non-zero".to_string(),
            ));
        }

        Ok(Self {
            transactions_by_hash: DashMap::with_hasher(RandomState::new()),
            max_transactions: config.max_transactions,
            receipts_by_hash: DashMap::with_hasher(RandomState::new()),
            max_receipts: config.max_receipts,
            block_transactions: DashMap::with_hasher(RandomState::new()),
            stats: Arc::new(RwLock::new(CacheStats::default())),
            tx_hits: std::sync::atomic::AtomicU64::new(0),
            tx_misses: std::sync::atomic::AtomicU64::new(0),
            receipt_hits: std::sync::atomic::AtomicU64::new(0),
            receipt_misses: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Inserts a transaction into the cache and updates the block index.
    ///
    /// Pending transactions (those without a `block_number`) won't be added to the block index
    /// but will still be cached by hash. Automatically prunes oldest blocks when capacity
    /// is exceeded.
    pub async fn insert_transaction(&self, transaction: TransactionRecord) {
        trace!(hash = ?transaction.hash, "inserting transaction");

        let tx_arc = Arc::new(transaction);
        self.transactions_by_hash.insert(tx_arc.hash, Arc::clone(&tx_arc));

        if let Some(block_number) = tx_arc.block_number {
            self.block_transactions.entry(block_number).or_default().push(tx_arc.hash);
        }

        // Prune if over capacity
        if self.transactions_by_hash.len() > self.max_transactions {
            self.prune_transactions();
        }

        self.update_stats().await;
    }

    /// Inserts a receipt into the cache. Automatically prunes when capacity is exceeded.
    pub async fn insert_receipt(&self, receipt: ReceiptRecord) {
        trace!(hash = ?receipt.transaction_hash, "inserting receipt");

        let receipt_arc = Arc::new(receipt);
        self.receipts_by_hash.insert(receipt_arc.transaction_hash, receipt_arc);

        // Prune if over capacity
        if self.receipts_by_hash.len() > self.max_receipts {
            self.prune_receipts();
        }

        self.update_stats().await;
    }

    /// Bulk inserts receipts without updating stats after each insert.
    ///
    /// This is significantly faster than calling `insert_receipt` in a loop because stats
    /// are only updated once at the end. Use this when warming the cache or processing
    /// large batches of receipts from upstream providers. Pruning is deferred until after
    /// all inserts complete.
    #[allow(clippy::unused_async)] // Keep async for API consistency
    pub async fn insert_receipts_bulk_no_stats(&self, receipts: Vec<ReceiptRecord>) {
        for receipt in receipts {
            let receipt_arc = Arc::new(receipt);
            self.receipts_by_hash.insert(receipt_arc.transaction_hash, receipt_arc);
        }

        // Prune if over capacity after bulk insert
        if self.receipts_by_hash.len() > self.max_receipts {
            self.prune_receipts();
        }
    }

    pub async fn update_stats_on_demand(&self) {
        self.update_stats().await;
    }

    #[must_use]
    pub fn get_transaction(&self, tx_hash: &[u8; 32]) -> Option<Arc<TransactionRecord>> {
        if let Some(tx) = self.transactions_by_hash.get(tx_hash) {
            self.tx_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(Arc::clone(&tx))
        } else {
            self.tx_misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    #[must_use]
    pub fn get_receipt(&self, tx_hash: &[u8; 32]) -> Option<Arc<ReceiptRecord>> {
        if let Some(rcpt) = self.receipts_by_hash.get(tx_hash) {
            self.receipt_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(Arc::clone(&rcpt))
        } else {
            self.receipt_misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    /// Returns a receipt with all log references resolved to actual log records.
    ///
    /// Receipts store only `LogId` references - this method resolves them to full
    /// `LogRecord` objects via the log cache. More convenient than manual resolution.
    #[must_use]
    pub fn get_receipt_with_logs(
        &self,
        tx_hash: &[u8; 32],
        log_cache: &crate::cache::log_cache::LogCache,
    ) -> Option<(Arc<ReceiptRecord>, Vec<Arc<LogRecord>>)> {
        let receipt_arc = self.get_receipt(tx_hash)?;
        let log_records = log_cache.get_log_records(&receipt_arc.logs);
        Some((receipt_arc, log_records))
    }

    /// Updates where a transaction appears in the blockchain.
    ///
    /// Used to update pending transactions when they're confirmed, or to fix transaction
    /// locations after a reorg. Pass `None` values to unset the location (e.g., moving
    /// a transaction back to pending state).
    pub fn update_transaction_location(
        &self,
        tx_hash: [u8; 32],
        block_hash: Option<[u8; 32]>,
        block_number: Option<u64>,
        transaction_index: Option<u32>,
    ) {
        if let Some(tx_arc) = self.transactions_by_hash.get(&tx_hash).map(|r| Arc::clone(&r)) {
            // Clone the record, update it, and re-insert as a new Arc
            let mut transaction = (*tx_arc).clone();
            transaction.block_hash = block_hash;
            transaction.block_number = block_number;
            transaction.transaction_index = transaction_index;

            self.transactions_by_hash.insert(tx_hash, Arc::new(transaction));
        }
    }

    /// Removes all transactions and receipts for a given block.
    ///
    /// Called during chain reorgs to purge data from blocks that are no longer canonical.
    pub async fn invalidate_block(&self, block_number: u64) {
        info!(block = block_number, "invalidating transactions");

        if let Some((_, tx_hashes)) = self.block_transactions.remove(&block_number) {
            for tx_hash in tx_hashes {
                self.transactions_by_hash.remove(&tx_hash);
                self.receipts_by_hash.remove(&tx_hash);
            }
        }

        self.update_stats().await;
    }

    /// Clears all cached data.
    ///
    /// Used when authentication changes require a full cache purge to prevent
    /// data leakage between different API keys or access contexts.
    pub async fn clear_cache(&self) {
        info!("Clearing all transaction cache data");

        self.transactions_by_hash.clear();
        self.receipts_by_hash.clear();
        self.block_transactions.clear();

        self.update_stats().await;
    }

    /// Returns all cached transactions for a block.
    ///
    /// Supports `eth_getBlockByNumber` queries by leveraging the block-to-transactions
    /// index. Returns empty vec if block not indexed.
    #[must_use]
    pub fn get_block_transactions(&self, block_number: u64) -> Vec<Arc<TransactionRecord>> {
        if let Some(tx_hashes) = self.block_transactions.get(&block_number) {
            let mut transactions = Vec::with_capacity(tx_hashes.len());

            for tx_hash in tx_hashes.iter() {
                if let Some(transaction_arc) = self.get_transaction(tx_hash) {
                    transactions.push(transaction_arc);
                }
            }

            transactions
        } else {
            Vec::new()
        }
    }

    /// Returns all cached receipts for a block.
    ///
    /// Uses the block-to-transactions index. Returns empty vec if block not indexed.
    #[must_use]
    pub fn get_block_receipts(&self, block_number: u64) -> Vec<Arc<ReceiptRecord>> {
        if let Some(tx_hashes) = self.block_transactions.get(&block_number) {
            let mut receipts = Vec::with_capacity(tx_hashes.len());

            for tx_hash in tx_hashes.iter() {
                if let Some(receipt_arc) = self.get_receipt(tx_hash) {
                    receipts.push(receipt_arc);
                }
            }

            receipts
        } else {
            Vec::new()
        }
    }

    /// Pre-populates the cache with transactions and receipts from a block range.
    ///
    /// Useful for cache warming on startup or after a reorg to restore previously
    /// cached data.
    pub async fn warm_range(
        &self,
        from_block: u64,
        to_block: u64,
        transactions: Vec<TransactionRecord>,
        receipts: Vec<ReceiptRecord>,
    ) {
        info!("Warming transaction cache for blocks {} to {}", from_block, to_block);

        for transaction in transactions {
            self.insert_transaction(transaction).await;
        }

        for receipt in receipts {
            self.insert_receipt(receipt).await;
        }
    }

    pub async fn get_stats(&self) -> CacheStats {
        // Update stats including hit/miss counters
        self.update_stats().await;
        self.stats.read().await.clone()
    }

    async fn update_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.transaction_cache_size = self.transactions_by_hash.len();
        stats.receipt_cache_size = self.receipts_by_hash.len();
        stats.transaction_cache_hits = self.tx_hits.load(std::sync::atomic::Ordering::Relaxed);
        stats.transaction_cache_misses = self.tx_misses.load(std::sync::atomic::Ordering::Relaxed);
        stats.receipt_cache_hits = self.receipt_hits.load(std::sync::atomic::Ordering::Relaxed);
        stats.receipt_cache_misses = self.receipt_misses.load(std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns the current transaction hit count.
    #[must_use]
    pub fn transaction_hit_count(&self) -> u64 {
        self.tx_hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the current transaction miss count.
    #[must_use]
    pub fn transaction_miss_count(&self) -> u64 {
        self.tx_misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the current receipt hit count.
    #[must_use]
    pub fn receipt_hit_count(&self) -> u64 {
        self.receipt_hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the current receipt miss count.
    #[must_use]
    pub fn receipt_miss_count(&self) -> u64 {
        self.receipt_misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Prunes transactions to stay within capacity by evicting from oldest blocks first.
    ///
    /// This strategy is optimal for blockchain caches since newer blocks are accessed
    /// more frequently than older ones.
    fn prune_transactions(&self) {
        let to_evict = self.transactions_by_hash.len().saturating_sub(self.max_transactions);
        if to_evict == 0 {
            return;
        }

        // Collect and sort block numbers (oldest first)
        let mut blocks: Vec<u64> = self.block_transactions.iter().map(|e| *e.key()).collect();
        blocks.sort_unstable();

        let mut evicted = 0;
        for block in blocks {
            if evicted >= to_evict {
                break;
            }

            // Get hashes for this block
            let hashes: Vec<[u8; 32]> = self
                .block_transactions
                .get(&block)
                .map(|entry| entry.clone())
                .unwrap_or_default();

            for hash in hashes {
                if evicted >= to_evict {
                    break;
                }
                if self.transactions_by_hash.remove(&hash).is_some() {
                    evicted += 1;
                }
            }

            // Clean up block index - remove evicted hashes
            self.block_transactions.alter(&block, |_, mut v| {
                v.retain(|h| self.transactions_by_hash.contains_key(h));
                v
            });

            // Remove empty block entries
            if self.block_transactions.get(&block).is_some_and(|v| v.is_empty()) {
                self.block_transactions.remove(&block);
            }
        }
    }

    /// Prunes receipts to stay within capacity.
    ///
    /// Since receipts don't have a block index, we simply remove arbitrary entries
    /// until under capacity. This is acceptable since receipt lookups are by tx hash.
    fn prune_receipts(&self) {
        let to_evict = self.receipts_by_hash.len().saturating_sub(self.max_receipts);
        if to_evict == 0 {
            return;
        }

        // Collect keys to evict (take first N from iterator)
        let keys_to_evict: Vec<[u8; 32]> =
            self.receipts_by_hash.iter().take(to_evict).map(|e| *e.key()).collect();

        for key in keys_to_evict {
            self.receipts_by_hash.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cache::types::LogId;

    use super::*;

    #[tokio::test]
    async fn test_transaction_cache_basic_operations() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let transaction = TransactionRecord {
            hash: [1u8; 32],
            block_hash: Some([2u8; 32]),
            block_number: Some(1000),
            transaction_index: Some(0),
            from: [3u8; 20],
            to: Some([4u8; 20]),
            value: [5u8; 32],
            gas_price: [6u8; 32],
            gas_limit: 21000,
            nonce: 0,
            data: vec![1, 2, 3],
            v: 27,
            r: [7u8; 32],
            s: [8u8; 32],
        };

        let receipt = ReceiptRecord {
            transaction_hash: [1u8; 32],
            block_hash: [2u8; 32],
            block_number: 1000,
            transaction_index: 0,
            from: [3u8; 20],
            to: Some([4u8; 20]),
            cumulative_gas_used: 21000,
            gas_used: 21000,
            contract_address: None,
            logs: vec![LogId::new(1000, 0)],
            status: 1,
            logs_bloom: vec![0u8; 256],
            effective_gas_price: Some(8),
            tx_type: Some(2),
            blob_gas_price: None,
        };

        cache.insert_transaction(transaction.clone()).await;
        cache.insert_receipt(receipt.clone()).await;

        let retrieved_tx = cache.get_transaction(&[1u8; 32]);
        assert!(retrieved_tx.is_some());
        let retrieved_tx = retrieved_tx.unwrap();
        assert_eq!(retrieved_tx.block_number, Some(1000));

        let retrieved_receipt = cache.get_receipt(&[1u8; 32]);
        assert!(retrieved_receipt.is_some());
        let retrieved_receipt = retrieved_receipt.unwrap();
        assert_eq!(retrieved_receipt.block_number, 1000);

        let block_txs = cache.get_block_transactions(1000);
        assert_eq!(block_txs.len(), 1);
        assert_eq!(block_txs[0].hash, [1u8; 32]);

        let block_receipts = cache.get_block_receipts(1000);
        assert_eq!(block_receipts.len(), 1);
        assert_eq!(block_receipts[0].transaction_hash, [1u8; 32]);
    }

    #[tokio::test]
    async fn test_transaction_cache_reorg_invalidation() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let transaction = TransactionRecord {
            hash: [1u8; 32],
            block_hash: Some([2u8; 32]),
            block_number: Some(1000),
            transaction_index: Some(0),
            from: [3u8; 20],
            to: Some([4u8; 20]),
            value: [5u8; 32],
            gas_price: [6u8; 32],
            gas_limit: 21000,
            nonce: 0,
            data: vec![],
            v: 27,
            r: [7u8; 32],
            s: [8u8; 32],
        };

        let receipt = ReceiptRecord {
            transaction_hash: [1u8; 32],
            block_hash: [2u8; 32],
            block_number: 1000,
            transaction_index: 0,
            from: [3u8; 20],
            to: Some([4u8; 20]),
            cumulative_gas_used: 21000,
            gas_used: 21000,
            contract_address: None,
            logs: vec![],
            status: 1,
            logs_bloom: vec![0u8; 256],
            effective_gas_price: None,
            tx_type: None,
            blob_gas_price: None,
        };

        cache.insert_transaction(transaction).await;
        cache.insert_receipt(receipt).await;

        assert!(cache.get_transaction(&[1u8; 32]).is_some());
        assert!(cache.get_receipt(&[1u8; 32]).is_some());

        cache.invalidate_block(1000).await;

        assert!(cache.get_transaction(&[1u8; 32]).is_none());
        assert!(cache.get_receipt(&[1u8; 32]).is_none());
    }

    #[tokio::test]
    async fn test_transaction_cache_location_update() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let transaction = TransactionRecord {
            hash: [1u8; 32],
            block_hash: None,
            block_number: None,
            transaction_index: None,
            from: [3u8; 20],
            to: Some([4u8; 20]),
            value: [5u8; 32],
            gas_price: [6u8; 32],
            gas_limit: 21000,
            nonce: 0,
            data: vec![],
            v: 27,
            r: [7u8; 32],
            s: [8u8; 32],
        };

        cache.insert_transaction(transaction).await;

        cache.update_transaction_location([1u8; 32], Some([2u8; 32]), Some(1000), Some(0));

        let retrieved = cache.get_transaction(&[1u8; 32]);
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.block_hash, Some([2u8; 32]));
        assert_eq!(retrieved.block_number, Some(1000));
        assert_eq!(retrieved.transaction_index, Some(0));
    }

    #[allow(clippy::cast_possible_truncation)]
    fn create_test_transaction(hash: [u8; 32], block_number: Option<u64>) -> TransactionRecord {
        TransactionRecord {
            hash,
            block_hash: block_number.map(|bn| [bn as u8; 32]),
            block_number,
            transaction_index: Some(0),
            from: [1u8; 20],
            to: Some([2u8; 20]),
            value: [0u8; 32],
            gas_price: [1u8; 32],
            gas_limit: 21000,
            nonce: 0,
            data: vec![],
            v: 27,
            r: [3u8; 32],
            s: [4u8; 32],
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn create_test_receipt(tx_hash: [u8; 32], block_number: u64) -> ReceiptRecord {
        ReceiptRecord {
            transaction_hash: tx_hash,
            block_hash: [block_number as u8; 32],
            block_number,
            transaction_index: 0,
            from: [1u8; 20],
            to: Some([2u8; 20]),
            cumulative_gas_used: 21000,
            gas_used: 21000,
            contract_address: None,
            logs: vec![],
            status: 1,
            logs_bloom: vec![0u8; 256],
            effective_gas_price: Some(10),
            tx_type: Some(2),
            blob_gas_price: None,
        }
    }

    #[tokio::test]
    async fn test_transaction_eviction_respects_capacity() {
        // Create cache with capacity of 2 transactions
        let config =
            TransactionCacheConfig { max_transactions: 2, max_receipts: 2, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert 3 transactions - should evict oldest block
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1002))).await;

        // Verify capacity is respected
        assert_eq!(cache.transactions_by_hash.len(), 2);

        // Oldest transaction (block 1000) should be evicted
        assert!(cache.get_transaction(&[1u8; 32]).is_none());

        // Newer transactions should remain
        assert!(cache.get_transaction(&[2u8; 32]).is_some());
        assert!(cache.get_transaction(&[3u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_transaction_eviction_cleans_block_index() {
        // Create cache with capacity of 2 transactions
        let config =
            TransactionCacheConfig { max_transactions: 2, max_receipts: 2, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions in different blocks
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1002))).await;

        // Block index for evicted block should be cleaned up
        assert!(!cache.block_transactions.contains_key(&1000));

        // Remaining blocks should still be indexed
        assert!(cache.block_transactions.contains_key(&1001));
        assert!(cache.block_transactions.contains_key(&1002));
    }

    #[tokio::test]
    async fn test_receipt_eviction_respects_capacity() {
        // Create cache with capacity of 2 receipts
        let config =
            TransactionCacheConfig { max_transactions: 10, max_receipts: 2, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert 3 receipts - should evict to maintain capacity
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;
        cache.insert_receipt(create_test_receipt([2u8; 32], 1001)).await;
        cache.insert_receipt(create_test_receipt([3u8; 32], 1002)).await;

        // Verify capacity is respected
        assert_eq!(cache.receipts_by_hash.len(), 2);
    }

    #[tokio::test]
    async fn test_get_does_not_affect_cache_content() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let tx = create_test_transaction([1u8; 32], Some(1000));
        cache.insert_transaction(tx).await;

        // Get transaction multiple times
        let _ = cache.get_transaction(&[1u8; 32]);
        let _ = cache.get_transaction(&[1u8; 32]);
        let _ = cache.get_transaction(&[1u8; 32]);

        // Verify cache still contains exactly one entry
        assert_eq!(cache.transactions_by_hash.len(), 1);
        assert!(cache.transactions_by_hash.contains_key(&[1u8; 32]));
    }

    #[tokio::test]
    async fn test_eviction_removes_from_block_index() {
        // Create cache with capacity of 1 transaction
        let config =
            TransactionCacheConfig { max_transactions: 1, max_receipts: 1, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transaction in block 1000
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        assert!(cache.block_transactions.contains_key(&1000));

        // Insert another transaction to trigger eviction
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;

        // Block 1000 index should be cleaned up after eviction
        assert!(!cache.block_transactions.contains_key(&1000));
        assert!(cache.block_transactions.contains_key(&1001));
    }

    #[tokio::test]
    async fn test_block_index_pending_transaction_not_indexed() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert pending transaction (block_number = None)
        let pending_tx = create_test_transaction([1u8; 32], None);
        cache.insert_transaction(pending_tx).await;

        // Verify transaction is cached
        assert!(cache.get_transaction(&[1u8; 32]).is_some());

        // Verify no block index entry (pending transactions aren't indexed by block)
        assert_eq!(cache.block_transactions.len(), 0);
    }

    #[tokio::test]
    async fn test_block_index_confirmed_transaction_is_indexed() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert confirmed transaction
        let tx = create_test_transaction([1u8; 32], Some(1000));
        cache.insert_transaction(tx).await;

        // Verify block index has entry
        assert!(cache.block_transactions.contains_key(&1000));

        // Verify block index contains the transaction hash
        let tx_hashes = cache.block_transactions.get(&1000).unwrap();
        assert_eq!(tx_hashes.len(), 1);
        assert_eq!(tx_hashes[0], [1u8; 32]);
    }

    #[tokio::test]
    async fn test_block_index_multiple_transactions_same_block() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert multiple transactions in the same block
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1000))).await;

        // Verify block index has all three transactions
        let tx_hashes = cache.block_transactions.get(&1000).unwrap();
        assert_eq!(tx_hashes.len(), 3);
        assert!(tx_hashes.contains(&[1u8; 32]));
        assert!(tx_hashes.contains(&[2u8; 32]));
        assert!(tx_hashes.contains(&[3u8; 32]));
    }

    #[tokio::test]
    async fn test_block_index_get_block_transactions_returns_all() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1001))).await;

        // Get transactions for block 1000
        let block_txs = cache.get_block_transactions(1000);
        assert_eq!(block_txs.len(), 2);

        // Verify correct transactions returned
        let hashes: Vec<[u8; 32]> = block_txs.iter().map(|tx| tx.hash).collect();
        assert!(hashes.contains(&[1u8; 32]));
        assert!(hashes.contains(&[2u8; 32]));
    }

    #[tokio::test]
    async fn test_block_index_get_block_receipts_returns_all() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions and receipts
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;
        cache.insert_receipt(create_test_receipt([2u8; 32], 1000)).await;

        // Get receipts for block 1000
        let block_receipts = cache.get_block_receipts(1000);
        assert_eq!(block_receipts.len(), 2);

        // Verify correct receipts returned
        let hashes: Vec<[u8; 32]> = block_receipts.iter().map(|r| r.transaction_hash).collect();
        assert!(hashes.contains(&[1u8; 32]));
        assert!(hashes.contains(&[2u8; 32]));
    }

    #[tokio::test]
    async fn test_block_index_nonexistent_block_returns_empty() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Query non-existent block
        let block_txs = cache.get_block_transactions(9999);
        assert_eq!(block_txs.len(), 0);

        let block_receipts = cache.get_block_receipts(9999);
        assert_eq!(block_receipts.len(), 0);
    }

    #[tokio::test]
    async fn test_block_index_update_location_updates_record() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert pending transaction
        let tx = create_test_transaction([1u8; 32], None);
        cache.insert_transaction(tx).await;

        // Verify not in block index
        assert_eq!(cache.block_transactions.len(), 0);

        // Update location to confirm transaction
        cache.update_transaction_location([1u8; 32], Some([10u8; 32]), Some(1000), Some(5));

        // Verify transaction location is updated
        let updated_tx = cache.get_transaction(&[1u8; 32]).unwrap();
        assert_eq!(updated_tx.block_number, Some(1000));
        assert_eq!(updated_tx.block_hash, Some([10u8; 32]));
        assert_eq!(updated_tx.transaction_index, Some(5));
    }

    #[tokio::test]
    async fn test_reorg_invalidate_block_removes_all_transactions() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert multiple transactions in block 1000
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1000))).await;

        // Verify transactions are cached
        assert!(cache.get_transaction(&[1u8; 32]).is_some());
        assert!(cache.get_transaction(&[2u8; 32]).is_some());
        assert!(cache.get_transaction(&[3u8; 32]).is_some());

        // Invalidate block
        cache.invalidate_block(1000).await;

        // Verify all transactions removed
        assert!(cache.get_transaction(&[1u8; 32]).is_none());
        assert!(cache.get_transaction(&[2u8; 32]).is_none());
        assert!(cache.get_transaction(&[3u8; 32]).is_none());
    }

    #[tokio::test]
    async fn test_reorg_invalidate_block_removes_all_receipts() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions and receipts in block 1000
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;
        cache.insert_receipt(create_test_receipt([2u8; 32], 1000)).await;

        // Verify receipts are cached
        assert!(cache.get_receipt(&[1u8; 32]).is_some());
        assert!(cache.get_receipt(&[2u8; 32]).is_some());

        // Invalidate block
        cache.invalidate_block(1000).await;

        // Verify all receipts removed
        assert!(cache.get_receipt(&[1u8; 32]).is_none());
        assert!(cache.get_receipt(&[2u8; 32]).is_none());
    }

    #[tokio::test]
    async fn test_reorg_invalidate_block_updates_stats() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1000))).await;

        // Get stats before invalidation
        let stats_before = cache.get_stats().await;
        assert_eq!(stats_before.transaction_cache_size, 2);

        // Invalidate block
        cache.invalidate_block(1000).await;

        // Get stats after invalidation
        let stats_after = cache.get_stats().await;
        assert_eq!(stats_after.transaction_cache_size, 0);
    }

    #[tokio::test]
    async fn test_reorg_invalidate_nonexistent_block_is_noop() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transaction in block 1000
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;

        // Invalidate different block
        cache.invalidate_block(9999).await;

        // Verify original transaction still exists
        assert!(cache.get_transaction(&[1u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_reorg_invalidate_preserves_other_blocks() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions in multiple blocks
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;
        cache.insert_transaction(create_test_transaction([3u8; 32], Some(1002))).await;

        // Invalidate only block 1001
        cache.invalidate_block(1001).await;

        // Verify block 1001 transaction removed
        assert!(cache.get_transaction(&[2u8; 32]).is_none());

        // Verify other blocks preserved
        assert!(cache.get_transaction(&[1u8; 32]).is_some());
        assert!(cache.get_transaction(&[3u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_bulk_insert_receipts_no_stats() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Create bulk receipts
        let receipts = vec![
            create_test_receipt([1u8; 32], 1000),
            create_test_receipt([2u8; 32], 1000),
            create_test_receipt([3u8; 32], 1000),
        ];

        // Insert bulk without stats updates
        cache.insert_receipts_bulk_no_stats(receipts).await;

        // Verify receipts are cached
        assert!(cache.get_receipt(&[1u8; 32]).is_some());
        assert!(cache.get_receipt(&[2u8; 32]).is_some());
        assert!(cache.get_receipt(&[3u8; 32]).is_some());

        // Stats might be outdated until update_stats_on_demand
        cache.update_stats_on_demand().await;
        let stats = cache.get_stats().await;
        assert_eq!(stats.receipt_cache_size, 3);
    }

    #[tokio::test]
    async fn test_bulk_insert_stats_updated_on_demand() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert receipts in bulk
        let receipts =
            vec![create_test_receipt([1u8; 32], 1000), create_test_receipt([2u8; 32], 1000)];
        cache.insert_receipts_bulk_no_stats(receipts).await;

        // Update stats manually
        cache.update_stats_on_demand().await;

        // Verify stats are correct
        let stats = cache.get_stats().await;
        assert_eq!(stats.receipt_cache_size, 2);
    }

    #[tokio::test]
    async fn test_clear_cache_removes_all_data() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert various data
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;
        cache.insert_receipt(create_test_receipt([2u8; 32], 1001)).await;

        // Verify data exists
        assert!(cache.get_transaction(&[1u8; 32]).is_some());
        assert!(cache.get_receipt(&[1u8; 32]).is_some());

        // Clear cache
        cache.clear_cache().await;

        // Verify everything removed
        assert!(cache.get_transaction(&[1u8; 32]).is_none());
        assert!(cache.get_transaction(&[2u8; 32]).is_none());
        assert!(cache.get_receipt(&[1u8; 32]).is_none());
        assert!(cache.get_receipt(&[2u8; 32]).is_none());
    }

    #[tokio::test]
    async fn test_clear_cache_removes_block_index() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transactions
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;

        // Verify block index has entries
        assert!(cache.block_transactions.contains_key(&1000));
        assert!(cache.block_transactions.contains_key(&1001));

        // Clear cache
        cache.clear_cache().await;

        // Verify block index is empty
        assert_eq!(cache.block_transactions.len(), 0);
    }

    #[tokio::test]
    async fn test_clear_cache_updates_stats() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert data
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;

        // Clear cache
        cache.clear_cache().await;

        // Verify stats are zeroed
        let stats = cache.get_stats().await;
        assert_eq!(stats.transaction_cache_size, 0);
        assert_eq!(stats.receipt_cache_size, 0);
    }

    #[tokio::test]
    async fn test_capacity_enforced_on_transaction_inserts() {
        let config =
            TransactionCacheConfig { max_transactions: 3, max_receipts: 10, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert more transactions than capacity
        for i in 1..=5 {
            cache
                .insert_transaction(create_test_transaction([i; 32], Some(1000 + u64::from(i))))
                .await;
        }

        // Capacity is enforced - should have at most 3 transactions
        assert_eq!(cache.transactions_by_hash.len(), 3);

        // Oldest blocks should be evicted (blocks 1001, 1002 evicted, 1003-1005 remain)
        assert!(cache.get_transaction(&[1u8; 32]).is_none());
        assert!(cache.get_transaction(&[2u8; 32]).is_none());
        assert!(cache.get_transaction(&[3u8; 32]).is_some());
        assert!(cache.get_transaction(&[4u8; 32]).is_some());
        assert!(cache.get_transaction(&[5u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_capacity_enforced_on_receipt_inserts() {
        let config =
            TransactionCacheConfig { max_transactions: 10, max_receipts: 3, safety_depth: 12 };
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert more receipts than capacity
        for i in 1..=5 {
            cache.insert_receipt(create_test_receipt([i; 32], 1000 + u64::from(i))).await;
        }

        // Capacity is enforced - should have at most 3 receipts
        assert_eq!(cache.receipts_by_hash.len(), 3);
    }

    #[tokio::test]
    async fn test_config_zero_transaction_capacity_returns_error() {
        let config =
            TransactionCacheConfig { max_transactions: 0, max_receipts: 100, safety_depth: 12 };

        let result = TransactionCache::new(&config);
        assert!(result.is_err());

        if let Err(e) = result {
            assert!(matches!(e, TransactionCacheError::InvalidConfig(_)));
            assert!(e.to_string().contains("max_transactions"));
        }
    }

    #[tokio::test]
    async fn test_config_zero_receipt_capacity_returns_error() {
        let config =
            TransactionCacheConfig { max_transactions: 100, max_receipts: 0, safety_depth: 12 };

        let result = TransactionCache::new(&config);
        assert!(result.is_err());

        if let Err(e) = result {
            assert!(matches!(e, TransactionCacheError::InvalidConfig(_)));
            assert!(e.to_string().contains("max_receipts"));
        }
    }

    #[tokio::test]
    async fn test_config_default_values_are_sensible() {
        let config = TransactionCacheConfig::default();

        // Verify default values
        assert_eq!(config.max_transactions, 50000);
        assert_eq!(config.max_receipts, 50000);
        assert_eq!(config.safety_depth, 12);

        // Verify cache can be created with defaults
        let result = TransactionCache::new(&config);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_config_valid_capacities_succeed() {
        let config =
            TransactionCacheConfig { max_transactions: 1, max_receipts: 1, safety_depth: 12 };

        let result = TransactionCache::new(&config);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_nonexistent_transaction_returns_none() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let result = cache.get_transaction(&[99u8; 32]);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_nonexistent_receipt_returns_none() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        let result = cache.get_receipt(&[99u8; 32]);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_update_location_nonexistent_transaction_is_noop() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Update location for non-existent transaction
        cache.update_transaction_location([99u8; 32], Some([1u8; 32]), Some(1000), Some(0));

        // Verify no transaction was created
        assert!(cache.get_transaction(&[99u8; 32]).is_none());
    }

    #[tokio::test]
    async fn test_duplicate_insert_overwrites_existing() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transaction with specific data
        let mut tx1 = create_test_transaction([1u8; 32], Some(1000));
        tx1.nonce = 5;
        cache.insert_transaction(tx1).await;

        // Insert same hash with different data
        let mut tx2 = create_test_transaction([1u8; 32], Some(1000));
        tx2.nonce = 10;
        cache.insert_transaction(tx2).await;

        // Verify latest data is cached
        let cached = cache.get_transaction(&[1u8; 32]).unwrap();
        assert_eq!(cached.nonce, 10);
    }

    #[tokio::test]
    async fn test_warm_range_inserts_all_data() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Create test data
        let transactions = vec![
            create_test_transaction([1u8; 32], Some(1000)),
            create_test_transaction([2u8; 32], Some(1001)),
        ];

        let receipts =
            vec![create_test_receipt([1u8; 32], 1000), create_test_receipt([2u8; 32], 1001)];

        // Warm cache
        cache.warm_range(1000, 1001, transactions, receipts).await;

        // Verify all data is cached
        assert!(cache.get_transaction(&[1u8; 32]).is_some());
        assert!(cache.get_transaction(&[2u8; 32]).is_some());
        assert!(cache.get_receipt(&[1u8; 32]).is_some());
        assert!(cache.get_receipt(&[2u8; 32]).is_some());
    }

    #[tokio::test]
    async fn test_stats_reflect_actual_cache_size() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert data
        cache.insert_transaction(create_test_transaction([1u8; 32], Some(1000))).await;
        cache.insert_transaction(create_test_transaction([2u8; 32], Some(1001))).await;
        cache.insert_receipt(create_test_receipt([1u8; 32], 1000)).await;

        // Get stats
        let stats = cache.get_stats().await;
        assert_eq!(stats.transaction_cache_size, 2);
        assert_eq!(stats.receipt_cache_size, 1);
    }

    #[tokio::test]
    async fn test_arc_cloning_efficiency() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Insert transaction
        let tx = create_test_transaction([1u8; 32], Some(1000));
        cache.insert_transaction(tx).await;

        // Get transaction multiple times
        let tx1 = cache.get_transaction(&[1u8; 32]).unwrap();
        let tx2 = cache.get_transaction(&[1u8; 32]).unwrap();

        // Verify Arc is shared (same pointer)
        assert!(Arc::ptr_eq(&tx1, &tx2));
    }

    #[tokio::test]
    async fn test_receipt_with_log_ids() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Create receipt with log references
        let mut receipt = create_test_receipt([1u8; 32], 1000);
        receipt.logs = vec![LogId::new(1000, 0), LogId::new(1000, 1), LogId::new(1000, 2)];

        cache.insert_receipt(receipt).await;

        // Verify receipt is cached with log IDs
        let cached = cache.get_receipt(&[1u8; 32]).unwrap();
        assert_eq!(cached.logs.len(), 3);
        assert_eq!(cached.logs[0].block_number, 1000);
        assert_eq!(cached.logs[0].log_index, 0);
    }

    #[tokio::test]
    async fn test_transaction_hit_miss_tracking_accuracy() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Initial state
        assert_eq!(cache.transaction_hit_count(), 0);
        assert_eq!(cache.transaction_miss_count(), 0);

        // Insert transaction
        let tx = create_test_transaction([1u8; 32], Some(1000));
        cache.insert_transaction(tx).await;

        // Hit: transaction exists
        let result = cache.get_transaction(&[1u8; 32]);
        assert!(result.is_some());
        assert_eq!(cache.transaction_hit_count(), 1);
        assert_eq!(cache.transaction_miss_count(), 0);

        // Miss: transaction doesn't exist
        let result = cache.get_transaction(&[99u8; 32]);
        assert!(result.is_none());
        assert_eq!(cache.transaction_hit_count(), 1);
        assert_eq!(cache.transaction_miss_count(), 1);

        // Multiple hits
        let _ = cache.get_transaction(&[1u8; 32]);
        let _ = cache.get_transaction(&[1u8; 32]);
        assert_eq!(cache.transaction_hit_count(), 3);
        assert_eq!(cache.transaction_miss_count(), 1);

        // Multiple misses
        let _ = cache.get_transaction(&[2u8; 32]);
        let _ = cache.get_transaction(&[3u8; 32]);
        assert_eq!(cache.transaction_hit_count(), 3);
        assert_eq!(cache.transaction_miss_count(), 3);

        // Verify stats are updated correctly
        let stats = cache.get_stats().await;
        assert_eq!(stats.transaction_cache_hits, 3);
        assert_eq!(stats.transaction_cache_misses, 3);
    }

    #[tokio::test]
    async fn test_receipt_hit_miss_tracking_accuracy() {
        let config = TransactionCacheConfig::default();
        let cache = TransactionCache::new(&config).expect("valid config");

        // Initial state
        assert_eq!(cache.receipt_hit_count(), 0);
        assert_eq!(cache.receipt_miss_count(), 0);

        // Insert receipt
        let receipt = create_test_receipt([1u8; 32], 1000);
        cache.insert_receipt(receipt).await;

        // Hit: receipt exists
        let result = cache.get_receipt(&[1u8; 32]);
        assert!(result.is_some());
        assert_eq!(cache.receipt_hit_count(), 1);
        assert_eq!(cache.receipt_miss_count(), 0);

        // Miss: receipt doesn't exist
        let result = cache.get_receipt(&[99u8; 32]);
        assert!(result.is_none());
        assert_eq!(cache.receipt_hit_count(), 1);
        assert_eq!(cache.receipt_miss_count(), 1);

        // Multiple hits
        let _ = cache.get_receipt(&[1u8; 32]);
        let _ = cache.get_receipt(&[1u8; 32]);
        assert_eq!(cache.receipt_hit_count(), 3);
        assert_eq!(cache.receipt_miss_count(), 1);

        // Multiple misses
        let _ = cache.get_receipt(&[2u8; 32]);
        let _ = cache.get_receipt(&[3u8; 32]);
        assert_eq!(cache.receipt_hit_count(), 3);
        assert_eq!(cache.receipt_miss_count(), 3);

        // Verify stats are updated correctly
        let stats = cache.get_stats().await;
        assert_eq!(stats.receipt_cache_hits, 3);
        assert_eq!(stats.receipt_cache_misses, 3);
    }
}
