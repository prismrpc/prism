//! Coverage tracking operations for `LogCache`.
//!
//! Manages which block ranges have been successfully fetched, enabling
//! distinction between "fetched but empty" and "not yet fetched".

use super::LogCache;
use roaring::RoaringBitmap;
use std::sync::Arc;
use tracing::trace;

impl LogCache {
    /// Marks a block range as successfully covered (fetched with or without logs).
    ///
    /// Call this after successfully fetching logs for a range, even if the result
    /// was empty. This allows the cache to distinguish "no logs exist" from
    /// "not yet fetched", enabling proper partial cache hits.
    ///
    /// # Arguments
    /// * `from_block` - Start block of the covered range (inclusive)
    /// * `to_block` - End block of the covered range (inclusive)
    ///
    /// # Example
    /// ```ignore
    /// // After fetching logs for blocks 100-200 (even if empty):
    /// cache.mark_range_covered(100, 200);
    ///
    /// // Future queries for this range will be cache hits
    /// let (logs, missing) = cache.query_logs(&filter, tip).await;
    /// assert!(missing.is_empty()); // No missing ranges
    /// ```
    pub fn mark_range_covered(&self, from_block: u64, to_block: u64) {
        trace!(from_block = from_block, to_block = to_block, "marking range as covered");

        // Group blocks by chunk for efficient batch updates
        let start_chunk = self.get_chunk_id(from_block);
        let end_chunk = self.get_chunk_id(to_block);

        for chunk_id in start_chunk..=end_chunk {
            let (chunk_start, chunk_end) = self.get_chunk_range(chunk_id);

            // Calculate the range within this chunk
            let range_start = from_block.max(chunk_start);
            let range_end = to_block.min(chunk_end);

            let mut entry = self
                .covered_blocks
                .entry(chunk_id)
                .or_insert_with(|| Arc::new(RoaringBitmap::new()));

            let bitmap = Arc::make_mut(&mut entry);
            for block in range_start..=range_end {
                // Calculate offset within chunk (0 to chunk_size-1)
                #[allow(clippy::cast_possible_truncation)]
                let block_offset = (block % self.config.chunk_size) as u32;
                bitmap.insert(block_offset);
            }
        }
    }

    /// Checks if a specific block has been covered (successfully fetched).
    ///
    /// Returns `true` if the block was previously marked as covered via
    /// `mark_range_covered`, indicating we've fetched data for this block
    /// (whether it contained logs or not).
    ///
    /// # Arguments
    /// * `block_number` - The block number to check
    ///
    /// # Returns
    /// * `true` if the block has been covered
    /// * `false` if the block has not been fetched yet
    #[must_use]
    pub fn is_block_covered(&self, block_number: u64) -> bool {
        let chunk_id = self.get_chunk_id(block_number);
        #[allow(clippy::cast_possible_truncation)]
        let block_offset = (block_number % self.config.chunk_size) as u32;

        self.covered_blocks
            .get(&chunk_id)
            .is_some_and(|bitmap| bitmap.contains(block_offset))
    }

    /// Checks if an entire block range is covered.
    ///
    /// Returns `true` only if ALL blocks in the range have been covered.
    ///
    /// # Arguments
    /// * `from_block` - Start block (inclusive)
    /// * `to_block` - End block (inclusive)
    #[must_use]
    pub fn is_range_covered(&self, from_block: u64, to_block: u64) -> bool {
        let start_chunk = self.get_chunk_id(from_block);
        let end_chunk = self.get_chunk_id(to_block);

        for chunk_id in start_chunk..=end_chunk {
            let Some(bitmap) = self.covered_blocks.get(&chunk_id) else {
                return false;
            };

            let (chunk_start, chunk_end) = self.get_chunk_range(chunk_id);
            let range_start = from_block.max(chunk_start);
            let range_end = to_block.min(chunk_end);

            for block in range_start..=range_end {
                #[allow(clippy::cast_possible_truncation)]
                let block_offset = (block % self.config.chunk_size) as u32;
                if !bitmap.contains(block_offset) {
                    return false;
                }
            }
        }

        true
    }

    /// Clears coverage for a specific block.
    ///
    /// Called during reorg invalidation to remove coverage for blocks that
    /// may have been reorganized. After clearing, the block will appear as
    /// "not fetched" and will be re-fetched on the next query.
    ///
    /// # Arguments
    /// * `block_number` - The block number to clear coverage for
    pub fn clear_block_coverage(&self, block_number: u64) {
        let chunk_id = self.get_chunk_id(block_number);
        #[allow(clippy::cast_possible_truncation)]
        let block_offset = (block_number % self.config.chunk_size) as u32;

        if let Some(mut bitmap) = self.covered_blocks.get_mut(&chunk_id) {
            Arc::make_mut(&mut bitmap).remove(block_offset);

            // Clean up empty bitmaps to prevent memory leaks
            if bitmap.is_empty() {
                drop(bitmap);
                self.covered_blocks.remove(&chunk_id);
            }
        }
    }

    /// Clears coverage for a block range.
    ///
    /// Useful for bulk invalidation during reorgs that affect multiple blocks.
    ///
    /// # Arguments
    /// * `from_block` - Start block (inclusive)
    /// * `to_block` - End block (inclusive)
    pub fn clear_range_coverage(&self, from_block: u64, to_block: u64) {
        trace!(from_block = from_block, to_block = to_block, "clearing range coverage");

        let start_chunk = self.get_chunk_id(from_block);
        let end_chunk = self.get_chunk_id(to_block);

        for chunk_id in start_chunk..=end_chunk {
            if let Some(mut bitmap) = self.covered_blocks.get_mut(&chunk_id) {
                let (chunk_start, chunk_end) = self.get_chunk_range(chunk_id);
                let range_start = from_block.max(chunk_start);
                let range_end = to_block.min(chunk_end);

                let bm = Arc::make_mut(&mut bitmap);
                for block in range_start..=range_end {
                    #[allow(clippy::cast_possible_truncation)]
                    let block_offset = (block % self.config.chunk_size) as u32;
                    bm.remove(block_offset);
                }

                // Clean up empty bitmaps
                if bitmap.is_empty() {
                    drop(bitmap);
                    self.covered_blocks.remove(&chunk_id);
                }
            }
        }
    }

    /// Returns the number of covered blocks (for stats/debugging).
    #[must_use]
    pub fn covered_block_count(&self) -> u64 {
        self.covered_blocks.iter().map(|entry| entry.value().len()).sum()
    }

    /// Marks a single block as covered (internal helper).
    pub(crate) fn mark_block_covered(&self, block_number: u64) {
        let chunk_id = self.get_chunk_id(block_number);
        #[allow(clippy::cast_possible_truncation)]
        let block_offset = (block_number % self.config.chunk_size) as u32;

        let mut entry = self
            .covered_blocks
            .entry(chunk_id)
            .or_insert_with(|| Arc::new(RoaringBitmap::new()));
        Arc::make_mut(&mut entry).insert(block_offset);
    }
}
