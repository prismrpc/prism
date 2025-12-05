//! Configuration types for `LogCache`.

/// Maximum chunk size that fits in `LogId` packed format (10 bits for block offset)
pub const MAX_CHUNK_SIZE: u64 = 1024;

/// Configuration for log cache behavior.
///
/// Chunks divide the block range into fixed-size windows to enable efficient
/// partial cache hits and memory management. Bitmaps have capacity limits to
/// prevent unbounded growth on high-cardinality filters.
///
/// # Chunk Size Constraint
///
/// `chunk_size` must be <= 1024 to fit within the `LogId` packed format.
/// The packed u32 format allocates 10 bits for block offset within chunk (max 1023).
/// Use [`LogCacheConfig::validate`] or [`LogCacheConfig::new`] to ensure validity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogCacheConfig {
    /// Number of blocks per chunk (default: 1000, max: 1024)
    ///
    /// Must be <= 1024 to fit in `LogId` packed format (10 bits for block offset).
    pub chunk_size: u64,
    /// Maximum number of exact filter results to cache (default: 10,000)
    pub max_exact_results: usize,
    /// Maximum entries per bitmap index before new entries are dropped (default: 100,000)
    pub max_bitmap_entries: usize,
    /// Blocks from chain tip considered unsafe due to reorg risk (default: 12)
    pub safety_depth: u64,
}

impl LogCacheConfig {
    /// Creates a new config with validation.
    ///
    /// # Errors
    /// Returns an error if `chunk_size` exceeds `MAX_CHUNK_SIZE` (1024).
    pub fn new(
        chunk_size: u64,
        max_exact_results: usize,
        max_bitmap_entries: usize,
        safety_depth: u64,
    ) -> Result<Self, LogCacheConfigError> {
        let config = Self { chunk_size, max_exact_results, max_bitmap_entries, safety_depth };
        config.validate()?;
        Ok(config)
    }

    /// Validates the configuration.
    ///
    /// # Errors
    /// Returns an error if `chunk_size` exceeds `MAX_CHUNK_SIZE` (1024).
    pub fn validate(&self) -> Result<(), LogCacheConfigError> {
        if self.chunk_size > MAX_CHUNK_SIZE {
            return Err(LogCacheConfigError::ChunkSizeTooLarge {
                chunk_size: self.chunk_size,
                max: MAX_CHUNK_SIZE,
            });
        }
        Ok(())
    }
}

impl Default for LogCacheConfig {
    fn default() -> Self {
        Self {
            chunk_size: 1000,
            max_exact_results: 10_000,
            max_bitmap_entries: 100_000,
            safety_depth: 12,
        }
    }
}

/// Error type for `LogCacheConfig` validation
#[derive(Debug, thiserror::Error)]
pub enum LogCacheConfigError {
    #[error("chunk_size {chunk_size} exceeds maximum {max} (LogId packed format constraint)")]
    ChunkSizeTooLarge { chunk_size: u64, max: u64 },
}
