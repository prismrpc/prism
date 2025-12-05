//! Query processing logic for `LogCache`.
//!
//! Handles log queries, bitmap filtering, exact result caching,
//! and log retrieval operations.

use super::LogCache;
use crate::cache::types::{LogFilter, LogId, LogRecord};
use roaring::RoaringBitmap;
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc},
};
use tracing::{debug, trace};

impl LogCache {
    /// Query logs matching the filter.
    ///
    /// Returns a tuple of (`matching_log_ids`, `missing_ranges`). An empty `missing_ranges`
    /// indicates a complete cache hit. Non-empty ranges mean the caller should fetch
    /// from upstream to fill gaps.
    ///
    /// Queries are clamped to `safety_depth` blocks from `current_tip` to avoid caching
    /// data in the reorg window. For example, with `safety_depth=12` and `tip=1000`, only
    /// blocks up to 988 will be considered for full cache hits.
    pub async fn query_logs(
        &self,
        filter: &LogFilter,
        current_tip: u64,
    ) -> (Vec<LogId>, Vec<(u64, u64)>) {
        #[cfg(feature = "verbose-logging")]
        trace!(from_block = filter.from_block, to_block = filter.to_block, "querying logs");

        if let Some(exact_ids) = self.get_exact_result(filter).await {
            #[cfg(feature = "verbose-logging")]
            debug!(count = exact_ids.len(), "exact cache hit");
            return (exact_ids, Vec::new());
        }

        let estimated_logs =
            usize::try_from((filter.to_block - filter.from_block + 1) * 10).unwrap_or(0);
        let mut log_ids = Vec::with_capacity(estimated_logs.min(1000));
        let mut covered_blocks = HashSet::new();

        let effective_to_block = self.effective_to_block(filter, current_tip);

        #[cfg(feature = "verbose-logging")]
        debug!(
            from_block = filter.from_block,
            to_block = filter.to_block,
            effective_to = effective_to_block,
            "evaluating cached data"
        );

        let start_chunk = self.get_chunk_id(filter.from_block);
        let end_chunk = self.get_chunk_id(filter.to_block);

        let mut filter_bitmaps = self.prepare_filter_bitmaps(filter);

        for chunk_id in start_chunk..=end_chunk {
            self.add_chunk_coverage(
                chunk_id,
                filter.from_block,
                effective_to_block,
                &mut covered_blocks,
            );

            if let Some(chunk_bitmap) = self.chunk_index.get(&chunk_id) {
                if let Some(ref mut bitmaps) = filter_bitmaps {
                    let filtered = Self::intersect_filters_in_order(&chunk_bitmap, bitmaps);
                    if !filtered.is_empty() {
                        Self::collect_matching_logs_for_chunk(
                            chunk_id,
                            &filtered,
                            filter,
                            &mut log_ids,
                        );
                    }
                }
            }
        }

        let missing_ranges =
            Self::compute_missing_ranges_from_coverage(filter, effective_to_block, &covered_blocks);

        if missing_ranges.is_empty() {
            #[cfg(feature = "verbose-logging")]
            debug!(logs = log_ids.len(), "full cache hit");
            self.cache_exact_result(filter, log_ids.clone()).await;
        } else {
            #[cfg(feature = "verbose-logging")]
            debug!(
                logs = log_ids.len(),
                missing_range_count = missing_ranges.len(),
                "partial cache hit"
            );
        }

        (log_ids, missing_ranges)
    }

    fn effective_to_block(&self, filter: &LogFilter, current_tip: u64) -> u64 {
        let safe_upper = current_tip.saturating_sub(self.config.safety_depth);
        if filter.from_block > safe_upper {
            filter.from_block.saturating_sub(1)
        } else {
            filter.to_block.min(safe_upper)
        }
    }

    fn prepare_filter_bitmaps(
        &self,
        filter: &LogFilter,
    ) -> Option<Vec<(usize, Arc<RoaringBitmap>)>> {
        let mut filters: Vec<(usize, Arc<RoaringBitmap>)> = Vec::new();

        if let Some(addr) = filter.address {
            if let Some(addr_bitmap) = self.address_index.get(&addr) {
                filters.push((
                    usize::try_from(addr_bitmap.len()).unwrap_or(usize::MAX),
                    Arc::clone(&addr_bitmap),
                ));
            } else {
                return None;
            }
        }

        for (i, filter_topic) in filter.topics.iter().enumerate() {
            if let Some(topic) = filter_topic {
                if let Some(topic_bitmap) = self.topic_indexes[i].get(topic) {
                    filters.push((
                        usize::try_from(topic_bitmap.len()).unwrap_or(usize::MAX),
                        Arc::clone(&topic_bitmap),
                    ));
                } else {
                    return None;
                }
            }
        }

        Some(filters)
    }

    /// Adds covered blocks from the explicit coverage bitmap for a chunk.
    ///
    /// This method uses the `covered_blocks` bitmap (explicit coverage tracking)
    /// to determine which blocks have been fetched. This enables distinguishing
    /// "no logs exist" from "not yet fetched".
    fn add_chunk_coverage(
        &self,
        chunk_id: u64,
        from_block: u64,
        effective_to_block: u64,
        covered_blocks_out: &mut HashSet<u64>,
    ) {
        if let Some(coverage_bitmap) = self.covered_blocks.get(&chunk_id) {
            let (chunk_start, chunk_end) = self.get_chunk_range(chunk_id);

            // Only check blocks within the query range and chunk bounds
            let range_start = from_block.max(chunk_start);
            let range_end = effective_to_block.min(chunk_end);

            for block in range_start..=range_end {
                #[allow(clippy::cast_possible_truncation)]
                let block_offset = (block % self.config.chunk_size) as u32;
                if coverage_bitmap.contains(block_offset) {
                    covered_blocks_out.insert(block);
                }
            }
        }
    }

    fn intersect_filters_in_order(
        base: &RoaringBitmap,
        filters: &mut [(usize, Arc<RoaringBitmap>)],
    ) -> RoaringBitmap {
        if filters.is_empty() {
            return base.clone();
        }
        filters.sort_by_key(|(len, _)| *len);

        let mut result = base.clone();
        for (_, bm) in filters.iter() {
            result &= bm.as_ref();
            if result.is_empty() {
                break;
            }
        }
        result
    }

    fn collect_matching_logs_for_chunk(
        chunk_id: u64,
        bitmap: &RoaringBitmap,
        filter: &LogFilter,
        out: &mut Vec<LogId>,
    ) {
        for packed_id in bitmap {
            let log_id = LogId::unpack_with_chunk(packed_id, chunk_id);
            if log_id.block_number >= filter.from_block && log_id.block_number <= filter.to_block {
                out.push(log_id);
            }
        }
    }

    /// Computes missing ranges from coverage.
    ///
    /// This function is `pub(crate)` to allow proptest to call it directly.
    pub(crate) fn compute_missing_ranges_from_coverage(
        filter: &LogFilter,
        effective_to_block: u64,
        covered_blocks: &HashSet<u64>,
    ) -> Vec<(u64, u64)> {
        let mut missing_ranges = Vec::with_capacity(10);
        let mut current = filter.from_block;

        while current <= filter.to_block {
            let is_covered = current <= effective_to_block && covered_blocks.contains(&current);
            if is_covered {
                current += 1;
                continue;
            }

            let start = current;
            while current <= filter.to_block {
                let within_safe = current <= effective_to_block;
                let covered = within_safe && covered_blocks.contains(&current);
                if covered {
                    break;
                }
                current += 1;
            }
            // Clamp end to filter.to_block to prevent overshooting the query range
            // Use saturating_sub to handle the edge case where start == current (shouldn't happen
            // but defensive)
            let end = current.saturating_sub(1).min(filter.to_block);
            debug_assert!(
                start <= end,
                "Missing range invariant violated: start={start} > end={end}"
            );
            missing_ranges.push((start, end));
        }

        missing_ranges
    }

    async fn get_exact_result(&self, filter: &LogFilter) -> Option<Vec<LogId>> {
        self.exact_results.get(filter).await
    }

    pub async fn cache_exact_result(&self, filter: &LogFilter, log_ids: Vec<LogId>) {
        self.exact_results.insert(filter.clone(), log_ids).await;
        // Mark stats as dirty (exact_result_count changes)
        self.stats_dirty.store(true, Ordering::Relaxed);
    }

    // ===== Log Retrieval =====

    #[must_use]
    pub fn get_log_records(&self, log_ids: &[LogId]) -> Vec<Arc<LogRecord>> {
        let mut records = Vec::with_capacity(log_ids.len());

        for log_id in log_ids {
            if let Some(record_arc) = self.log_store.get(log_id) {
                records.push(Arc::clone(&record_arc));
            }
        }

        records
    }

    #[must_use]
    pub fn get_log_records_with_ids(&self, log_ids: &[LogId]) -> Vec<(LogId, Arc<LogRecord>)> {
        let mut records = Vec::with_capacity(log_ids.len());

        for log_id in log_ids {
            if let Some(record_arc) = self.log_store.get(log_id) {
                records.push((*log_id, Arc::clone(&record_arc)));
            }
        }

        records
    }
}
