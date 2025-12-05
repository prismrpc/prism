//! Utility functions for common operations with performance optimizations.
//!
//! # Thread-Local Buffer Strategy
//!
//! This module provides allocation-free utilities using thread-local buffers:
//!
//! ## Hex Formatting (`hex_buffer`)
//! - Thread-local buffer for zero-allocation hex encoding
//! - Used for addresses, hashes, and byte array formatting
//! - Falls back to heap allocation only for oversized data
//!
//! ## JSON Hashing (`json_hash`)
//! - Thread-local buffer for deterministic JSON serialization
//! - Used for consensus response comparison
//! - Canonical key ordering ensures consistent hashes
//!
//! ## Block Parameter Parsing (`block_param`)
//! - Zero-allocation parsing of "latest", "finalized", etc.
//! - Hex string parsing without intermediate allocations
//!
//! # Performance Characteristics
//!
//! | Operation | Typical Cost | Notes |
//! |-----------|--------------|-------|
//! | Hex encode (≤256B) | ~50ns | Thread-local buffer |
//! | Hex encode (>256B) | ~200ns+ | Heap allocation fallback |
//! | JSON hash | ~100ns | Deterministic serialization |
//! | Block param parse | ~10ns | Pattern matching + hex parse |

pub mod block_param;
pub mod hex_buffer;
pub mod json_hash;

pub use block_param::{BlockParameter, BlockRef, BlockTag, ParseError as BlockParseError};
pub use hex_buffer::{format_address, format_hash32, format_hex, format_hex_large, format_hex_u64};
pub use json_hash::{
    get_json_buffer_stats as get_json_hash_buffer_stats, hash_json_response, hash_json_value,
};
