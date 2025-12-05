//! Invalidation, statistics, and maintenance operations for `LogCache`.
//!
//! Handles cache invalidation during reorgs, statistics tracking with
//! the dirty flag pattern, and maintenance operations like pruning.

use super::LogCache;
use crate::cache::types::{CacheStats, LogFilter, LogId, LogRecord};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tracing::{debug, info};

impl LogCache {
    // ===== Invalidation =====

    /// Remove all logs for a block, typically called during reorgs.
    ///
    /// Uses the `block_index` for O(1) lookup of logs to remove instead of
    /// scanning the entire log store. Only updates affected bitmap entries
    /// rather than rebuilding all indexes.
    pub fn invalidate_block(&self, block_number: u64) {
        info!(block = block_number, "invalidating logs");

        // O(1) lookup using block_index instead of O(n) scan
        let Some((_, log_ids)) = self.block_index.remove(&block_number) else {
            // No logs cached for this block - just clear exact results
            self.clear_affected_exact_results(block_number);
            return;
        };

        if log_ids.is_empty() {
            self.clear_affected_exact_results(block_number);
            return;
        }

        debug!(block = block_number, log_count = log_ids.len(), "removing logs from cache");

        let chunk_id = self.get_chunk_id(block_number);

        // Collect packed IDs to remove from bitmaps
        let mut packed_ids_to_remove = Vec::with_capacity(log_ids.len());

        // Remove logs from log_store and collect their packed IDs
        for log_id in &log_ids {
            if let Some((_, log_record)) = self.log_store.remove(log_id) {
                // Pack the log_id for bitmap removal
                if let Ok(packed_id) = log_id.pack() {
                    packed_ids_to_remove.push((packed_id, log_record));
                }
            }
        }

        // Remove from chunk index first (single entry)
        if let Some(mut chunk_bitmap) = self.chunk_index.get_mut(&chunk_id) {
            let bitmap = Arc::make_mut(&mut chunk_bitmap);
            for (packed_id, _) in &packed_ids_to_remove {
                bitmap.remove(*packed_id);
            }
            if chunk_bitmap.is_empty() {
                drop(chunk_bitmap); // Release the mutable reference before removing
                self.chunk_index.remove(&chunk_id);
            }
        }

        // Remove from address and topic indexes - only for affected entries
        for (packed_id, log_record) in &packed_ids_to_remove {
            // Remove from address index
            if let Some(mut addr_bitmap) = self.address_index.get_mut(&log_record.address) {
                Arc::make_mut(&mut addr_bitmap).remove(*packed_id);
                if addr_bitmap.is_empty() {
                    drop(addr_bitmap);
                    self.address_index.remove(&log_record.address);
                }
            }

            // Remove from topic indexes
            for (i, topic) in log_record.topics.iter().enumerate() {
                if let Some(topic) = topic {
                    if let Some(mut topic_bitmap) = self.topic_indexes[i].get_mut(topic) {
                        Arc::make_mut(&mut topic_bitmap).remove(*packed_id);
                        if topic_bitmap.is_empty() {
                            drop(topic_bitmap);
                            self.topic_indexes[i].remove(topic);
                        }
                    }
                }
            }
        }

        self.clear_block_coverage(block_number);

        self.clear_affected_exact_results(block_number);

        // Mark stats as dirty (standardized pattern)
        self.stats_dirty.store(true, Ordering::Relaxed);

        debug!(
            block = block_number,
            removed_logs = packed_ids_to_remove.len(),
            "log invalidation complete"
        );
    }

    /// Clear all cached data. Used when authentication context changes require a full reset.
    pub async fn clear_cache(&self) {
        info!("clearing log cache");

        self.log_store.clear();
        self.address_index.clear();
        for topic_index in &self.topic_indexes {
            topic_index.clear();
        }
        self.chunk_index.clear();
        self.block_index.clear();
        self.covered_blocks.clear();
        self.exact_results.invalidate_all();

        {
            let mut stats = self.stats.write().await;
            *stats = CacheStats::default();
        }

        // Mark stats as dirty (standardized pattern)
        self.stats_dirty.store(true, Ordering::Relaxed);
    }

    fn clear_affected_exact_results(&self, block_number: u64) {
        #[cfg(feature = "verbose-logging")]
        info!(block = block_number, "invalidating exact results cache");
        let _ = block_number; // Silence unused warning when verbose-logging is disabled
        self.exact_results.invalidate_all();
    }

    // ===== Statistics =====

    /// Updates statistics about the cache.
    ///
    /// Stats computation is done outside the write lock to avoid blocking concurrent
    /// queries during iteration over bitmap indexes. Only the final write acquires the lock.
    pub(crate) async fn update_stats(&self) {
        let log_store_size = self.log_store.len();
        let exact_result_count = usize::try_from(self.exact_results.entry_count()).unwrap_or(0);

        // Compute bitmap memory usage while NOT holding any locks
        let mut bitmap_memory = 0;

        bitmap_memory += self
            .address_index
            .iter()
            .map(|entry| entry.value().serialized_size())
            .sum::<usize>();

        for topic_index in &self.topic_indexes {
            bitmap_memory +=
                topic_index.iter().map(|entry| entry.value().serialized_size()).sum::<usize>();
        }

        bitmap_memory += self
            .chunk_index
            .iter()
            .map(|entry| entry.value().serialized_size())
            .sum::<usize>();

        {
            let mut stats = self.stats.write().await;
            stats.log_store_size = log_store_size;
            stats.exact_result_count = exact_result_count;
            stats.bitmap_memory_usage = bitmap_memory;
            stats.total_log_ids_cached = log_store_size;
        }

        self.stats_dirty.store(false, Ordering::Relaxed);
        *self.last_stats_update.write().await = Instant::now();
    }

    /// Check if stats need updating based on time elapsed.
    ///
    /// Updates are triggered if more than 5 minutes have passed since last update.
    pub async fn should_update_stats(&self) -> bool {
        let last_update = *self.last_stats_update.read().await;
        let time_elapsed = last_update.elapsed();

        time_elapsed > Duration::from_secs(300)
    }

    pub async fn force_update_stats(&self) {
        self.update_stats().await;
    }

    #[deprecated(note = "Use get_stats() which handles dirty flag automatically")]
    pub fn get_changes_since_last_stats(&self) -> usize {
        usize::from(self.stats_dirty.load(Ordering::Relaxed))
    }

    pub fn start_background_stats_updater(
        self: Arc<Self>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let last_update = *self.last_stats_update.read().await;
                        if last_update.elapsed() > Duration::from_secs(600) {
                            self.force_update_stats().await;
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("log cache stats updater shutting down");
                        break;
                    }
                }
            }
        });
    }

    /// Returns current cache statistics, recomputing if dirty flag is set.
    pub async fn get_stats(&self) -> CacheStats {
        if self.stats_dirty.swap(false, Ordering::Relaxed) {
            self.update_stats().await;
        }
        self.stats.read().await.clone()
    }

    // ===== Maintenance =====

    /// Preload logs for a block range and cache the exact filter result.
    ///
    /// Marks the entire range as covered.
    pub async fn warm_range(&self, from_block: u64, to_block: u64, logs: Vec<(LogId, LogRecord)>) {
        #[cfg(feature = "verbose-logging")]
        info!(from_block = from_block, to_block = to_block, "warming log cache");

        self.insert_logs_bulk_no_stats(logs).await;

        // Mark the entire range as covered (even blocks without logs)
        self.mark_range_covered(from_block, to_block);

        let wildcard_filter = LogFilter::new(from_block, to_block);

        let log_ids: Vec<LogId> = self
            .log_store
            .iter()
            .filter(|entry| {
                let log_id = entry.key();
                log_id.block_number >= from_block && log_id.block_number <= to_block
            })
            .map(|entry| *entry.key())
            .collect();

        self.cache_exact_result(&wildcard_filter, log_ids).await;
    }

    /// Remove old chunks outside the retention window to bound memory usage.
    pub async fn prune_old_chunks(&self, current_tip: u64, retain_blocks: u64) {
        let keep_from = current_tip.saturating_sub(retain_blocks);
        let keep_chunk = self.get_chunk_id(keep_from);

        let old_blocks: Vec<u64> = self
            .block_index
            .iter()
            .filter(|e| self.get_chunk_id(*e.key()) < keep_chunk)
            .map(|e| *e.key())
            .collect();

        for block_number in old_blocks {
            self.invalidate_block(block_number);
        }

        self.update_stats().await;
    }
}
