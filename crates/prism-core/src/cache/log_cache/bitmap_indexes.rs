//! Bitmap index operations for `LogCache`.
//!
//! Handles insert operations, index updates, and overflow tracking.
//! Uses the Arc<RoaringBitmap> copy-on-write pattern for efficient
//! concurrent access.

use super::LogCache;
use crate::cache::types::{LogId, LogRecord};
use roaring::RoaringBitmap;
use std::sync::{atomic::Ordering, Arc};
use tracing::{error, info, trace};

impl LogCache {
    // ===== Insert Operations =====

    pub async fn insert_log(&self, log_id: LogId, log_record: LogRecord) {
        self.insert_log_with_stats_update(log_id, log_record, true).await;
    }

    pub(crate) async fn insert_log_with_stats_update(
        &self,
        log_id: LogId,
        log_record: LogRecord,
        update_stats: bool,
    ) {
        #[cfg(feature = "verbose-logging")]
        trace!(block = log_id.block_number, log_index = log_id.log_index, "inserting log");

        let log_arc = Arc::new(log_record);
        self.log_store.insert(log_id, Arc::clone(&log_arc));

        self.update_address_index(log_id, &log_arc);
        self.update_topic_indexes(log_id, &log_arc);
        self.update_chunk_index(log_id);
        self.update_block_index(log_id);

        // Automatically mark the block as covered when inserting a log
        self.mark_block_covered(log_id.block_number);

        if !update_stats {
            // Mark stats as dirty for lazy computation (standardized pattern)
            self.stats_dirty.store(true, Ordering::Relaxed);
        }

        if update_stats {
            self.update_stats().await;
        }
    }

    /// Bulk insert with a single stats update at the end.
    pub async fn insert_logs_bulk(&self, logs: Vec<(LogId, LogRecord)>) {
        #[cfg(feature = "verbose-logging")]
        trace!(count = logs.len(), "bulk inserting logs");

        for (log_id, log_record) in logs {
            self.insert_log_with_stats_update(log_id, log_record, false).await;
        }

        self.update_stats().await;
    }

    /// Bulk insert without stats update - caller must update stats manually.
    pub async fn insert_logs_bulk_no_stats(&self, logs: Vec<(LogId, LogRecord)>) {
        #[cfg(feature = "verbose-logging")]
        trace!(count = logs.len(), "bulk inserting logs (skip stats)");

        for (log_id, log_record) in logs {
            self.insert_log_with_stats_update(log_id, log_record, false).await;
        }
    }

    pub async fn update_stats_on_demand(&self) {
        self.update_stats().await;
    }

    // ===== Index Update Helpers =====

    fn update_address_index(&self, log_id: LogId, log_record: &LogRecord) {
        match log_id.pack() {
            Ok(packed_id) => {
                let mut entry = self
                    .address_index
                    .entry(log_record.address)
                    .or_insert_with(|| Arc::new(RoaringBitmap::new()));
                let max_u64 = u64::try_from(self.config.max_bitmap_entries).unwrap_or(u64::MAX);
                if entry.len() < max_u64 {
                    Arc::make_mut(&mut entry).insert(packed_id);
                } else {
                    self.overflow_addresses.insert(log_record.address);
                    error!(
                        address = ?log_record.address,
                        cap = self.config.max_bitmap_entries,
                        block = log_id.block_number,
                        log_index = log_id.log_index,
                        "address index bitmap overflow - queries for this address may return incomplete results"
                    );
                }
            }
            Err(e) => {
                error!(error = %e, "failed to pack LogId for address index");
            }
        }
    }

    fn update_topic_indexes(&self, log_id: LogId, log_record: &LogRecord) {
        match log_id.pack() {
            Ok(packed_id) => {
                for (i, topic) in log_record.topics.iter().enumerate() {
                    if let Some(topic) = topic {
                        let mut entry = self.topic_indexes[i]
                            .entry(*topic)
                            .or_insert_with(|| Arc::new(RoaringBitmap::new()));
                        let max_u64 =
                            u64::try_from(self.config.max_bitmap_entries).unwrap_or(u64::MAX);
                        if entry.len() < max_u64 {
                            Arc::make_mut(&mut entry).insert(packed_id);
                        } else {
                            #[allow(clippy::cast_possible_truncation)]
                            self.overflow_topics.insert((i as u8, *topic));
                            error!(
                                topic = ?topic,
                                topic_position = i,
                                cap = self.config.max_bitmap_entries,
                                block = log_id.block_number,
                                log_index = log_id.log_index,
                                "topic index bitmap overflow - queries for this topic may return incomplete results"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to pack LogId for topic indexes");
            }
        }
    }

    fn update_chunk_index(&self, log_id: LogId) {
        let chunk_id = self.get_chunk_id(log_id.block_number);

        match log_id.pack() {
            Ok(packed_id) => {
                let mut entry = self
                    .chunk_index
                    .entry(chunk_id)
                    .or_insert_with(|| Arc::new(RoaringBitmap::new()));
                Arc::make_mut(&mut entry).insert(packed_id);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to pack LogId for chunk index");
            }
        }
    }

    /// Updates the block index with a new log entry for efficient invalidation lookup.
    fn update_block_index(&self, log_id: LogId) {
        self.block_index.entry(log_id.block_number).or_default().push(log_id);
    }

    // ===== Overflow Tracking =====

    /// Checks if an address index has overflowed its bitmap capacity.
    ///
    /// Returns `true` if the address bitmap exceeded `max_bitmap_entries`,
    /// indicating that queries filtering by this address may return incomplete results.
    #[must_use]
    pub fn is_address_overflowed(&self, address: &[u8; 20]) -> bool {
        self.overflow_addresses.contains(address)
    }

    /// Checks if a topic index has overflowed its bitmap capacity.
    ///
    /// Returns `true` if the topic bitmap exceeded `max_bitmap_entries`,
    /// indicating that queries filtering by this topic may return incomplete results.
    ///
    /// # Arguments
    /// * `position` - Topic position (0-3)
    /// * `topic` - Topic value
    #[must_use]
    pub fn is_topic_overflowed(&self, position: u8, topic: &[u8; 32]) -> bool {
        self.overflow_topics.contains(&(position, *topic))
    }

    /// Returns the number of overflowed addresses.
    #[must_use]
    pub fn overflow_address_count(&self) -> usize {
        self.overflow_addresses.len()
    }

    /// Returns the number of overflowed topics.
    #[must_use]
    pub fn overflow_topic_count(&self) -> usize {
        self.overflow_topics.len()
    }

    /// Clears overflow tracking (useful after increasing bitmap capacity or cache reset).
    pub fn clear_overflow_tracking(&self) {
        self.overflow_addresses.clear();
        self.overflow_topics.clear();
        info!("cleared bitmap overflow tracking");
    }
}
