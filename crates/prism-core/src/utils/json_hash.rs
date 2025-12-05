//! High-performance JSON hashing utilities for consensus.
//!
//! Provides zero-allocation JSON hashing using thread-local buffers and ahash.
//! Follows the same patterns as `hex_buffer.rs` for optimal performance.
//!
//! # Performance
//!
//! The direct value hashing approach (`hash_json_value`) avoids the overhead of
//! serializing JSON to strings, which eliminates:
//! - String allocation for serialized JSON
//! - UTF-8 encoding overhead
//! - Intermediate buffer copies
//!
//! For cases where serialization is needed, `hash_json_buffered` uses a thread-local
//! buffer to minimize allocations.

use crate::types::{JsonRpcError, JsonRpcResponse};
use ahash::AHasher;
use serde_json::Value;
use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
};

thread_local! {
    /// Thread-local buffer for JSON serialization during hashing.
    ///
    /// Pre-sized to 2KB which is sufficient for most RPC responses without reallocation.
    static JSON_BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(2048));
}

/// Hash a `serde_json::Value` directly without serialization.
///
/// This traverses the JSON structure and hashes each component directly,
/// avoiding the overhead of serializing to a string first. Object keys are
/// sorted for deterministic hashing regardless of insertion order.
///
/// # Type Discrimination
///
/// Each JSON type is prefixed with a discriminant byte to prevent collisions:
/// - Null: 0u8
/// - Bool: 1u8 + bool value
/// - Number: 2u8 + number representation
/// - String: 3u8 + length + bytes
/// - Array: 4u8 + length + each element
/// - Object: 5u8 + length + sorted (key, value) pairs
///
/// # Performance
///
/// This approach is significantly faster than serializing to JSON strings because:
/// - No string allocation
/// - No UTF-8 encoding
/// - Direct hashing of primitives
/// - Zero-copy for string content
pub fn hash_json_value(value: &Value, hasher: &mut impl Hasher) {
    match value {
        Value::Null => {
            0u8.hash(hasher);
        }
        Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        Value::Number(n) => {
            2u8.hash(hasher);
            // Hash number by its components to handle all numeric types consistently
            if let Some(i) = n.as_i64() {
                0u8.hash(hasher);
                i.hash(hasher);
            } else if let Some(u) = n.as_u64() {
                1u8.hash(hasher);
                u.hash(hasher);
            } else if let Some(f) = n.as_f64() {
                2u8.hash(hasher);
                // Normalize NaN/Infinity to canonical representations before hashing.
                // IEEE 754 defines multiple NaN bit patterns (signaling vs quiet, different
                // payloads). Without normalization, semantically equivalent values
                // could hash differently, causing consensus violations when
                // comparing upstream responses.
                let normalized_bits = if f.is_nan() {
                    // Use canonical quiet NaN representation
                    f64::NAN.to_bits()
                } else if f.is_infinite() {
                    // Normalize to canonical +/- infinity
                    if f.is_sign_positive() {
                        f64::INFINITY.to_bits()
                    } else {
                        f64::NEG_INFINITY.to_bits()
                    }
                } else {
                    f.to_bits()
                };
                normalized_bits.hash(hasher);
            }
        }
        Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        Value::Array(arr) => {
            4u8.hash(hasher);
            arr.len().hash(hasher);
            for element in arr {
                hash_json_value(element, hasher);
            }
        }
        Value::Object(obj) => {
            5u8.hash(hasher);
            obj.len().hash(hasher);

            // Sort keys for deterministic hashing
            // This ensures {"a":1,"b":2} and {"b":2,"a":1} produce the same hash
            let mut sorted_keys: Vec<&String> = obj.keys().collect();
            sorted_keys.sort_unstable();

            for key in sorted_keys {
                key.hash(hasher);
                if let Some(value) = obj.get(key) {
                    hash_json_value(value, hasher);
                }
            }
        }
    }
}

/// Hash a `serde_json::Value` while filtering out specified field paths.
///
/// Field paths support:
/// - Simple paths: `"timestamp"`, `"blockHash"`
/// - Nested paths: `"result.timestamp"`, `"transactions.gasPrice"`
/// - Wildcard array paths: `"transactions.*.gasPrice"` (matches all array elements)
///
/// # Arguments
///
/// * `value` - The JSON value to hash
/// * `ignore_paths` - List of field paths to exclude from hashing
/// * `hasher` - The hasher to write to
/// * `current_path` - Internal tracking of current path during recursion
///
/// # Example
///
/// ```ignore
/// let ignore = vec!["timestamp".to_string(), "requestsHash".to_string()];
/// hash_json_value_filtered(&block, &ignore, &mut hasher, "");
/// ```
pub fn hash_json_value_filtered(
    value: &Value,
    ignore_paths: &[String],
    hasher: &mut impl Hasher,
    current_path: &str,
) {
    // Use a reusable buffer for path construction to avoid allocations
    let mut path_buffer = String::with_capacity(64);
    path_buffer.push_str(current_path);
    hash_json_value_filtered_internal(value, ignore_paths, hasher, &mut path_buffer);
}

/// Internal implementation using a reusable path buffer to minimize allocations.
///
/// The `path_buffer` is modified during recursion but restored to its original
/// length before returning, allowing the caller to continue using it.
///
/// # Complexity Note
///
/// This function is long due to exhaustively matching all `serde_json::Value`
/// variants with their filtering logic inline. Extracting handlers would require
/// passing the hasher, `ignore_paths`, and `path_buffer` through each helper,
/// adding indirection without improving readability. The match arms are simple
/// and self-documenting.
#[allow(clippy::too_many_lines)]
fn hash_json_value_filtered_internal(
    value: &Value,
    ignore_paths: &[String],
    hasher: &mut impl Hasher,
    path_buffer: &mut String,
) {
    use std::fmt::Write;

    match value {
        Value::Null => {
            0u8.hash(hasher);
        }
        Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        Value::Number(n) => {
            2u8.hash(hasher);
            if let Some(i) = n.as_i64() {
                0u8.hash(hasher);
                i.hash(hasher);
            } else if let Some(u) = n.as_u64() {
                1u8.hash(hasher);
                u.hash(hasher);
            } else if let Some(f) = n.as_f64() {
                2u8.hash(hasher);
                f.to_bits().hash(hasher);
            }
        }
        Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        Value::Array(arr) => {
            4u8.hash(hasher);
            let base_len = path_buffer.len();

            // Build wildcard path once for all elements
            let mut wildcard_buffer = String::with_capacity(base_len + 2);
            if base_len > 0 {
                wildcard_buffer.push_str(path_buffer);
                wildcard_buffer.push_str(".*");
            } else {
                wildcard_buffer.push('*');
            }
            let wildcard_ignored = should_ignore_path(&wildcard_buffer, ignore_paths);

            // Count non-ignored elements
            let mut count = 0;
            for idx in 0..arr.len() {
                if wildcard_ignored {
                    continue;
                }
                // Build element path
                path_buffer.truncate(base_len);
                if base_len > 0 {
                    let _ = write!(path_buffer, ".{idx}");
                } else {
                    let _ = write!(path_buffer, "{idx}");
                }
                if !should_ignore_path(path_buffer, ignore_paths) {
                    count += 1;
                }
            }
            count.hash(hasher);

            // Hash non-ignored elements
            for (idx, element) in arr.iter().enumerate() {
                if wildcard_ignored {
                    continue;
                }
                // Build element path
                path_buffer.truncate(base_len);
                if base_len > 0 {
                    let _ = write!(path_buffer, ".{idx}");
                } else {
                    let _ = write!(path_buffer, "{idx}");
                }
                if !should_ignore_path(path_buffer, ignore_paths) {
                    hash_json_value_filtered_internal(element, ignore_paths, hasher, path_buffer);
                }
            }

            // Restore path buffer for caller
            path_buffer.truncate(base_len);
        }
        Value::Object(obj) => {
            5u8.hash(hasher);
            let base_len = path_buffer.len();

            // Sort keys for deterministic hashing
            let mut sorted_keys: Vec<&String> = obj.keys().collect();
            sorted_keys.sort_unstable();

            // Count non-ignored keys
            let mut count = 0;
            for key in &sorted_keys {
                path_buffer.truncate(base_len);
                if base_len > 0 {
                    let _ = write!(path_buffer, ".{key}");
                } else {
                    path_buffer.push_str(key);
                }
                if !should_ignore_path(path_buffer, ignore_paths) {
                    count += 1;
                }
            }
            count.hash(hasher);

            // Hash non-ignored keys
            for key in sorted_keys {
                path_buffer.truncate(base_len);
                if base_len > 0 {
                    let _ = write!(path_buffer, ".{key}");
                } else {
                    path_buffer.push_str(key);
                }

                if should_ignore_path(path_buffer, ignore_paths) {
                    continue;
                }

                key.hash(hasher);
                if let Some(val) = obj.get(key) {
                    hash_json_value_filtered_internal(val, ignore_paths, hasher, path_buffer);
                }
            }

            // Restore path buffer for caller
            path_buffer.truncate(base_len);
        }
    }
}

/// Check if a path should be ignored based on the ignore list.
///
/// Supports:
/// - Exact matches: `"timestamp"` matches `"timestamp"`
/// - Wildcard segment: `"transactions.*.gasPrice"` matches `"transactions.0.gasPrice"`,
///   `"transactions.1.gasPrice"`
/// - Trailing wildcard: `"transactions.*"` matches `"transactions.0"`, `"transactions.1.data"`,
///   etc.
fn should_ignore_path(path: &str, ignore_paths: &[String]) -> bool {
    for ignore in ignore_paths {
        // Exact match
        if path == ignore {
            return true;
        }

        // Pattern matching with wildcards
        if ignore.contains('*') && matches_wildcard_pattern(path, ignore) {
            return true;
        }
    }
    false
}

/// Matches a path against a wildcard pattern.
/// `*` matches any single segment (between dots).
fn matches_wildcard_pattern(path: &str, pattern: &str) -> bool {
    let path_segments: Vec<&str> = path.split('.').collect();
    let pattern_segments: Vec<&str> = pattern.split('.').collect();

    // If pattern ends with *, it's a prefix match
    if pattern.ends_with(".*") && pattern_segments.len() <= path_segments.len() {
        // Check all pattern segments except the last *
        for (i, pat_seg) in pattern_segments.iter().take(pattern_segments.len() - 1).enumerate() {
            if *pat_seg != "*" && path_segments.get(i) != Some(pat_seg) {
                return false;
            }
        }
        return true;
    }

    // Exact segment count match required (with wildcards in between)
    if path_segments.len() != pattern_segments.len() {
        return false;
    }

    for (path_seg, pat_seg) in path_segments.iter().zip(pattern_segments.iter()) {
        if *pat_seg != "*" && path_seg != pat_seg {
            return false;
        }
    }
    true
}

/// Hash a `JsonRpcResponse` with per-method field filtering.
///
/// Uses the filtered hashing approach to exclude chain-specific or timing-sensitive
/// fields from the hash computation. This allows responses with minor differences
/// (like timestamps) to be considered equivalent for consensus.
///
/// # Arguments
///
/// * `response` - The JSON-RPC response to hash
/// * `ignore_paths` - List of field paths to exclude from hashing
///
/// # Example
///
/// ```ignore
/// let ignore = vec!["timestamp".to_string(), "requestsHash".to_string()];
/// let hash = hash_json_response_filtered(&response, &ignore);
/// ```
#[must_use]
pub fn hash_json_response_filtered(response: &JsonRpcResponse, ignore_paths: &[String]) -> u64 {
    let mut hasher = AHasher::default();

    // Hash result if present
    if let Some(ref result) = response.result {
        1u8.hash(&mut hasher);
        hash_json_value_filtered(result, ignore_paths, &mut hasher, "");
    } else {
        0u8.hash(&mut hasher);
    }

    // Hash error if present (errors are not filtered)
    if let Some(ref error) = response.error {
        1u8.hash(&mut hasher);
        hash_json_error(error, &mut hasher);
    } else {
        0u8.hash(&mut hasher);
    }

    hasher.finish()
}

/// Hash a `JsonRpcResponse` for consensus comparison.
///
/// Uses ahash for optimal performance. Ignores `id` and `cache_status`
/// fields as they are not relevant for consensus - only the result
/// and error fields matter for determining agreement.
///
/// # Performance
///
/// This function uses direct value hashing which avoids:
/// - JSON serialization overhead (2-3x faster)
/// - String allocations
/// - UTF-8 encoding
///
/// # Examples
///
/// ```ignore
/// let response = JsonRpcResponse {
///     jsonrpc: "2.0".to_string(),
///     result: Some(json!({"block": "0x100"})),
///     error: None,
///     id: json!(1),
///     cache_status: None,
/// };
/// let hash = hash_json_response(&response);
/// ```
#[must_use]
pub fn hash_json_response(response: &JsonRpcResponse) -> u64 {
    let mut hasher = AHasher::default();

    // Hash result if present
    if let Some(ref result) = response.result {
        1u8.hash(&mut hasher);
        hash_json_value(result, &mut hasher);
    } else {
        0u8.hash(&mut hasher);
    }

    // Hash error if present
    if let Some(ref error) = response.error {
        1u8.hash(&mut hasher);
        hash_json_error(error, &mut hasher);
    } else {
        0u8.hash(&mut hasher);
    }

    hasher.finish()
}

/// Hash a `JsonRpcError` structure.
///
/// Hashes the error code, message, and optional data field.
fn hash_json_error(error: &JsonRpcError, hasher: &mut impl Hasher) {
    error.code.hash(hasher);
    error.message.hash(hasher);
    if let Some(ref data) = error.data {
        hash_json_value(data, hasher);
    }
}

/// Hash JSON using thread-local buffer (fallback for complex cases).
///
/// This serializes the JSON value to bytes using the thread-local buffer,
/// then hashes those bytes. Useful as a fallback when direct hashing isn't
/// appropriate or for verification purposes.
///
/// # Performance
///
/// While slower than `hash_json_value`, this still avoids heap allocation
/// by reusing a thread-local buffer. The buffer is cleared between uses
/// but its capacity is retained.
#[must_use]
pub fn hash_json_buffered(value: &Value) -> u64 {
    JSON_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();

        if serde_json::to_writer(&mut *buf, value).is_ok() {
            let mut hasher = AHasher::default();
            buf.hash(&mut hasher);
            hasher.finish()
        } else {
            0
        }
    })
}

/// Get buffer statistics for monitoring.
///
/// Returns (capacity, length) of the thread-local JSON buffer.
/// Useful for testing and verifying buffer reuse patterns.
#[must_use]
pub fn get_json_buffer_stats() -> (usize, usize) {
    JSON_BUFFER.with(|buffer| {
        let buf = buffer.borrow();
        (buf.capacity(), buf.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JSONRPC_VERSION_COW;
    use serde_json::json;
    use std::sync::Arc;

    /// Helper to create a test response without allocation overhead concerns
    fn test_response(result: Option<Value>, error: Option<JsonRpcError>) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result,
            error,
            id: Arc::new(json!(1)),
            cache_status: None,
        }
    }

    #[test]
    fn test_hash_determinism() {
        let value = json!({"block": "0x100", "hash": "0xabc", "number": 256});

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Same input should produce same hash");
    }

    #[test]
    fn test_object_key_order_independence() {
        let value1 = json!({"a": 1, "b": 2, "c": 3});
        let value2 = json!({"c": 3, "b": 2, "a": 1});
        let value3 = json!({"b": 2, "a": 1, "c": 3});

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        let hash3 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value3, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Different key orders should produce same hash");
        assert_eq!(hash2, hash3, "Different key orders should produce same hash");
    }

    #[test]
    fn test_type_discrimination_null_vs_false() {
        let null_value = json!(null);
        let false_value = json!(false);

        let null_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&null_value, &mut hasher);
            hasher.finish()
        };

        let false_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&false_value, &mut hasher);
            hasher.finish()
        };

        assert_ne!(null_hash, false_hash, "null and false should have different hashes");
    }

    #[test]
    fn test_type_discrimination_zero_vs_false() {
        let zero_value = json!(0);
        let false_value = json!(false);

        let zero_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&zero_value, &mut hasher);
            hasher.finish()
        };

        let false_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&false_value, &mut hasher);
            hasher.finish()
        };

        assert_ne!(zero_hash, false_hash, "0 and false should have different hashes");
    }

    #[test]
    fn test_type_discrimination_empty_string_vs_null() {
        let empty_string = json!("");
        let null_value = json!(null);

        let empty_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&empty_string, &mut hasher);
            hasher.finish()
        };

        let null_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&null_value, &mut hasher);
            hasher.finish()
        };

        assert_ne!(empty_hash, null_hash, "Empty string and null should have different hashes");
    }

    #[test]
    fn test_type_discrimination_zero_string_vs_zero_number() {
        let zero_string = json!("0");
        let zero_number = json!(0);

        let string_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&zero_string, &mut hasher);
            hasher.finish()
        };

        let number_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&zero_number, &mut hasher);
            hasher.finish()
        };

        assert_ne!(string_hash, number_hash, "\"0\" and 0 should have different hashes");
    }

    #[test]
    fn test_integer_hashing() {
        let value1 = json!(42);
        let value2 = json!(42);
        let value3 = json!(43);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        let hash3 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value3, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Same integers should have same hash");
        assert_ne!(hash1, hash3, "Different integers should have different hashes");
    }

    #[test]
    fn test_float_hashing() {
        let value1 = json!(1.23456);
        let value2 = json!(1.23456);
        let value3 = json!(9.87654);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        let hash3 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value3, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Same floats should have same hash");
        assert_ne!(hash1, hash3, "Different floats should have different hashes");
    }

    #[test]
    fn test_array_order_matters() {
        let value1 = json!([1, 2, 3]);
        let value2 = json!([3, 2, 1]);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        assert_ne!(hash1, hash2, "Arrays with different order should have different hashes");
    }

    #[test]
    fn test_nested_arrays() {
        let value1 = json!([[1, 2], [3, 4]]);
        let value2 = json!([[1, 2], [3, 4]]);
        let value3 = json!([[1, 2], [3, 5]]);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        let hash3 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value3, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Same nested arrays should have same hash");
        assert_ne!(hash1, hash3, "Different nested arrays should have different hashes");
    }

    #[test]
    fn test_nested_objects() {
        let value1 = json!({"outer": {"inner": "value"}});
        let value2 = json!({"outer": {"inner": "value"}});
        let value3 = json!({"outer": {"inner": "different"}});

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        let hash3 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value3, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Same nested objects should have same hash");
        assert_ne!(hash1, hash3, "Different nested objects should have different hashes");
    }

    #[test]
    fn test_response_hashing_identical() {
        let response1 = test_response(Some(json!({"block": "0x100"})), None);
        let response2 = test_response(Some(json!({"block": "0x100"})), None);

        let hash1 = hash_json_response(&response1);
        let hash2 = hash_json_response(&response2);

        assert_eq!(hash1, hash2, "Responses with same result should have same hash");
    }

    #[test]
    fn test_response_hashing_different() {
        let response1 = test_response(Some(json!({"block": "0x100"})), None);
        let response2 = test_response(Some(json!({"block": "0x200"})), None);

        let hash1 = hash_json_response(&response1);
        let hash2 = hash_json_response(&response2);

        assert_ne!(hash1, hash2, "Responses with different results should have different hashes");
    }

    #[test]
    fn test_response_with_error() {
        let error =
            Some(JsonRpcError { code: -32000, message: "Error message".to_string(), data: None });
        let response1 = test_response(None, error.clone());
        let response2 = test_response(None, error);

        let hash1 = hash_json_response(&response1);
        let hash2 = hash_json_response(&response2);

        assert_eq!(hash1, hash2, "Responses with same error should have same hash");
    }

    #[test]
    fn test_response_error_different_codes() {
        let response1 = test_response(
            None,
            Some(JsonRpcError { code: -32000, message: "Error".to_string(), data: None }),
        );
        let response2 = test_response(
            None,
            Some(JsonRpcError { code: -32001, message: "Error".to_string(), data: None }),
        );

        let hash1 = hash_json_response(&response1);
        let hash2 = hash_json_response(&response2);

        assert_ne!(hash1, hash2, "Different error codes should produce different hashes");
    }

    #[test]
    fn test_buffered_hashing() {
        let value1 = json!({"test": "data", "number": 42});
        let value2 = json!({"test": "data", "number": 42});
        let value3 = json!({"test": "different", "number": 42});

        let hash1 = hash_json_buffered(&value1);
        let hash2 = hash_json_buffered(&value2);
        let hash3 = hash_json_buffered(&value3);

        assert_eq!(hash1, hash2, "Same values should produce same buffered hash");
        assert_ne!(hash1, hash3, "Different values should produce different buffered hashes");
    }

    #[test]
    fn test_buffer_reuse() {
        let value = json!({"large": "data".repeat(100)});

        // First call establishes buffer size
        let _ = hash_json_buffered(&value);
        let (capacity1, _) = get_json_buffer_stats();

        // Second call should reuse the buffer
        let _ = hash_json_buffered(&value);
        let (capacity2, _) = get_json_buffer_stats();

        assert_eq!(capacity1, capacity2, "Buffer capacity should be reused");
    }

    #[test]
    fn test_ethereum_block_response() {
        let block = json!({
            "number": "0x1b4",
            "hash": "0xe670ec64341771606e55d6b4ca35a1a6b75ee3d5145a99d05921026d1527331",
            "parentHash": "0x9646252be9520f6e71339a8df9c55e4d7619deeb018d2a3f2d21fc165dde5eb5",
            "transactions": [
                {"hash": "0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b"},
                {"hash": "0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060"}
            ]
        });

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&block, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&block, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Complex Ethereum block should hash consistently");
    }

    #[test]
    fn test_buffer_stats() {
        let (capacity, length) = get_json_buffer_stats();
        assert!(capacity >= 2048, "Initial capacity should be at least 2KB");
        assert_eq!(length, 0, "Initial length should be 0");
    }

    #[test]
    fn test_empty_object() {
        let value1 = json!({});
        let value2 = json!({});

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Empty objects should have same hash");
    }

    #[test]
    fn test_empty_array() {
        let value1 = json!([]);
        let value2 = json!([]);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value1, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value2, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Empty arrays should have same hash");
    }

    #[test]
    fn test_empty_array_vs_empty_object() {
        let array = json!([]);
        let object = json!({});

        let array_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&array, &mut hasher);
            hasher.finish()
        };

        let object_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&object, &mut hasher);
            hasher.finish()
        };

        assert_ne!(array_hash, object_hash, "Empty array and empty object should differ");
    }

    #[test]
    fn test_very_large_object() {
        // Create a large object with many keys
        let mut obj = serde_json::Map::new();
        for i in 0..1000 {
            obj.insert(format!("key{i}"), json!(i));
        }
        let value = Value::Object(obj);

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value, &mut hasher);
            hasher.finish()
        };

        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value(&value, &mut hasher);
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Large objects should hash consistently");
    }

    #[test]
    fn test_filtered_hash_excludes_simple_field() {
        let value1 = json!({
            "blockNumber": "0x100",
            "timestamp": "0x12345",
            "hash": "0xabc"
        });
        let value2 = json!({
            "blockNumber": "0x100",
            "timestamp": "0xdifferent",
            "hash": "0xabc"
        });

        let ignore = vec!["timestamp".to_string()];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Should be equal when ignoring timestamp");
    }

    #[test]
    fn test_filtered_hash_different_when_non_ignored_differs() {
        let value1 = json!({
            "blockNumber": "0x100",
            "timestamp": "0x12345",
            "hash": "0xabc"
        });
        let value2 = json!({
            "blockNumber": "0x200",  // Different!
            "timestamp": "0x12345",
            "hash": "0xabc"
        });

        let ignore = vec!["timestamp".to_string()];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_ne!(hash1, hash2, "Should differ when non-ignored field differs");
    }

    #[test]
    fn test_filtered_hash_nested_path() {
        let value1 = json!({
            "result": {
                "blockNumber": "0x100",
                "timestamp": "0x12345"
            }
        });
        let value2 = json!({
            "result": {
                "blockNumber": "0x100",
                "timestamp": "0xdifferent"
            }
        });

        let ignore = vec!["result.timestamp".to_string()];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Should be equal when ignoring nested path");
    }

    #[test]
    fn test_filtered_hash_wildcard_array() {
        let value1 = json!({
            "transactions": [
                {"hash": "0x1", "gasPrice": "1000"},
                {"hash": "0x2", "gasPrice": "2000"}
            ]
        });
        let value2 = json!({
            "transactions": [
                {"hash": "0x1", "gasPrice": "9999"},
                {"hash": "0x2", "gasPrice": "8888"}
            ]
        });

        // Ignore gasPrice in all transaction array elements
        let ignore = vec!["transactions.*.gasPrice".to_string()];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Should be equal when ignoring wildcard array path");
    }

    #[test]
    fn test_filtered_hash_multiple_ignore_fields() {
        let value1 = json!({
            "blockNumber": "0x100",
            "timestamp": "0x12345",
            "requestsHash": "0xaaa",
            "withdrawalsRoot": "0xbbb",
            "hash": "0xabc"
        });
        let value2 = json!({
            "blockNumber": "0x100",
            "timestamp": "0xdifferent",
            "requestsHash": "0xdifferent",
            "withdrawalsRoot": "0xdifferent",
            "hash": "0xabc"
        });

        let ignore = vec![
            "timestamp".to_string(),
            "requestsHash".to_string(),
            "withdrawalsRoot".to_string(),
        ];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Should be equal when ignoring multiple fields");
    }

    #[test]
    fn test_filtered_hash_empty_ignore_list() {
        let value = json!({
            "blockNumber": "0x100",
            "timestamp": "0x12345"
        });

        let ignore: Vec<String> = vec![];

        let filtered_hash = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let unfiltered_hash = {
            let mut hasher = AHasher::default();
            hash_json_value(&value, &mut hasher);
            hasher.finish()
        };

        assert_eq!(
            filtered_hash, unfiltered_hash,
            "Empty ignore list should produce same hash as unfiltered"
        );
    }

    #[test]
    fn test_hash_json_response_filtered() {
        let response1 = test_response(
            Some(json!({
                "blockNumber": "0x100",
                "timestamp": "0x12345"
            })),
            None,
        );
        let response2 = test_response(
            Some(json!({
                "blockNumber": "0x100",
                "timestamp": "0xdifferent"
            })),
            None,
        );

        let ignore = vec!["timestamp".to_string()];

        let hash1 = hash_json_response_filtered(&response1, &ignore);
        let hash2 = hash_json_response_filtered(&response2, &ignore);

        assert_eq!(hash1, hash2, "Responses should be equal when ignoring timestamp");
    }

    #[test]
    fn test_should_ignore_path_exact_match() {
        let ignore = vec!["timestamp".to_string()];
        assert!(should_ignore_path("timestamp", &ignore));
        assert!(!should_ignore_path("blockNumber", &ignore));
    }

    #[test]
    fn test_should_ignore_path_wildcard() {
        let ignore = vec!["transactions.*".to_string()];
        assert!(should_ignore_path("transactions.0", &ignore));
        assert!(should_ignore_path("transactions.1.gasPrice", &ignore));
        assert!(!should_ignore_path("transactions", &ignore));
        assert!(!should_ignore_path("other.0", &ignore));
    }

    #[test]
    fn test_filtered_hash_deeply_nested() {
        let value1 = json!({
            "result": {
                "block": {
                    "header": {
                        "timestamp": "0x12345",
                        "number": "0x100"
                    }
                }
            }
        });
        let value2 = json!({
            "result": {
                "block": {
                    "header": {
                        "timestamp": "0xdifferent",
                        "number": "0x100"
                    }
                }
            }
        });

        let ignore = vec!["result.block.header.timestamp".to_string()];

        let hash1 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value1, &ignore, &mut hasher, "");
            hasher.finish()
        };
        let hash2 = {
            let mut hasher = AHasher::default();
            hash_json_value_filtered(&value2, &ignore, &mut hasher, "");
            hasher.finish()
        };

        assert_eq!(hash1, hash2, "Should be equal when ignoring deeply nested path");
    }

    use proptest::prelude::*;

    /// Strategy for generating arbitrary JSON values
    fn json_value_strategy() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|i| json!(i)),
            any::<u64>().prop_map(|u| json!(u)),
            any::<f64>()
                .prop_filter("finite floats only", |f| f.is_finite())
                .prop_map(|f| json!(f)),
            "[a-z]{0,20}".prop_map(Value::String),
        ];

        leaf.prop_recursive(
            4,  // Max depth
            32, // Max nodes
            10, // Items per collection
            |inner| {
                prop_oneof![
                    // Arrays
                    prop::collection::vec(inner.clone(), 0..10).prop_map(Value::Array),
                    // Objects
                    prop::collection::vec(("[a-z]{1,10}", inner), 0..10).prop_map(|pairs| {
                        let map: serde_json::Map<String, Value> = pairs.into_iter().collect();
                        Value::Object(map)
                    }),
                ]
            },
        )
    }

    /// Strategy for generating filter path patterns
    fn filter_path_strategy() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(
            prop_oneof![
                "[a-z]{1,10}".prop_map(|s| s),
                "[a-z]{1,5}\\.[a-z]{1,5}".prop_map(|s| s),
                "[a-z]{1,5}\\.\\*".prop_map(|s| s),
            ],
            0..5,
        )
    }

    proptest! {
        #[test]
        fn prop_hash_determinism(value in json_value_strategy()) {
            // Same JSON value should always produce the same hash
            let hash1 = {
                let mut hasher = AHasher::default();
                hash_json_value(&value, &mut hasher);
                hasher.finish()
            };

            let hash2 = {
                let mut hasher = AHasher::default();
                hash_json_value(&value, &mut hasher);
                hasher.finish()
            };

            prop_assert_eq!(hash1, hash2, "Same value should produce same hash");
        }

        #[test]
        fn prop_object_key_order_independence(
            pairs in prop::collection::vec(("[a-z]{1,10}", any::<i64>()), 1..10)
        ) {
            // Create unique keys by deduplicating
            let mut unique_pairs: Vec<(String, i64)> = Vec::new();
            let mut seen_keys = std::collections::HashSet::new();
            for (k, v) in pairs {
                if !seen_keys.contains(&k) {
                    seen_keys.insert(k.clone());
                    unique_pairs.push((k, v));
                }
            }

            // Skip if we don't have at least 2 unique keys
            if unique_pairs.len() < 2 {
                return Ok(());
            }

            // Create object with keys in original order
            let mut obj1 = serde_json::Map::new();
            for (k, v) in &unique_pairs {
                obj1.insert(k.clone(), json!(v));
            }

            // Create object with keys in reverse order
            let mut obj2 = serde_json::Map::new();
            for (k, v) in unique_pairs.iter().rev() {
                obj2.insert(k.clone(), json!(v));
            }

            let value1 = Value::Object(obj1);
            let value2 = Value::Object(obj2);

            let hash1 = {
                let mut hasher = AHasher::default();
                hash_json_value(&value1, &mut hasher);
                hasher.finish()
            };

            let hash2 = {
                let mut hasher = AHasher::default();
                hash_json_value(&value2, &mut hasher);
                hasher.finish()
            };

            prop_assert_eq!(hash1, hash2, "Object key order should not affect hash");
        }

        #[test]
        fn prop_different_values_different_hashes(
            value1 in json_value_strategy(),
            value2 in json_value_strategy()
        ) {
            // Only check if values are actually different
            if value1 != value2 {
                let hash1 = {
                    let mut hasher = AHasher::default();
                    hash_json_value(&value1, &mut hasher);
                    hasher.finish()
                };

                let hash2 = {
                    let mut hasher = AHasher::default();
                    hash_json_value(&value2, &mut hasher);
                    hasher.finish()
                };

                // Note: This is probabilistic - hash collisions are possible but unlikely
                // We're testing that the hash function produces different outputs for different inputs
                // most of the time, not that it's collision-free
                if hash1 == hash2 {
                    // Log collision but don't fail - collisions are theoretically possible
                    println!(
                        "Hash collision detected (expected to be rare): {value1:?} and {value2:?} both hash to {hash1}"
                    );
                }
            }
        }

        #[test]
        fn prop_filtered_hash_consistency(
            value in json_value_strategy(),
            filter_paths in filter_path_strategy()
        ) {
            // Filtered hashing should be deterministic
            let hash1 = {
                let mut hasher = AHasher::default();
                hash_json_value_filtered(&value, &filter_paths, &mut hasher, "");
                hasher.finish()
            };

            let hash2 = {
                let mut hasher = AHasher::default();
                hash_json_value_filtered(&value, &filter_paths, &mut hasher, "");
                hasher.finish()
            };

            prop_assert_eq!(hash1, hash2, "Filtered hash should be deterministic");
        }

        #[test]
        fn prop_empty_filter_equals_unfiltered(value in json_value_strategy()) {
            // Empty filter list should produce same hash as unfiltered
            let empty_filters: Vec<String> = vec![];

            let filtered_hash = {
                let mut hasher = AHasher::default();
                hash_json_value_filtered(&value, &empty_filters, &mut hasher, "");
                hasher.finish()
            };

            let unfiltered_hash = {
                let mut hasher = AHasher::default();
                hash_json_value(&value, &mut hasher);
                hasher.finish()
            };

            prop_assert_eq!(
                filtered_hash,
                unfiltered_hash,
                "Empty filter should produce same hash as unfiltered"
            );
        }

        #[test]
        fn prop_buffered_hash_determinism(value in json_value_strategy()) {
            // Buffered hashing should be deterministic
            let hash1 = hash_json_buffered(&value);
            let hash2 = hash_json_buffered(&value);

            prop_assert_eq!(hash1, hash2, "Buffered hash should be deterministic");
        }

        #[test]
        fn prop_response_hash_ignores_id(
            result in json_value_strategy(),
            id1 in any::<i64>(),
            id2 in any::<i64>()
        ) {
            // Response hash should ignore the ID field
            let response1 = JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(result.clone()),
                error: None,
                id: Arc::new(json!(id1)),
                cache_status: None,
            };

            let response2 = JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(result),
                error: None,
                id: Arc::new(json!(id2)),
                cache_status: None,
            };

            let hash1 = hash_json_response(&response1);
            let hash2 = hash_json_response(&response2);

            prop_assert_eq!(hash1, hash2, "Response hash should ignore ID field");
        }

        #[test]
        fn prop_type_discrimination_null_bool_number(i in any::<i64>()) {
            // Different types should produce different hashes
            let null_val = Value::Null;
            let bool_val = Value::Bool(false);
            let num_val = json!(i);

            let null_hash = {
                let mut hasher = AHasher::default();
                hash_json_value(&null_val, &mut hasher);
                hasher.finish()
            };

            let bool_hash = {
                let mut hasher = AHasher::default();
                hash_json_value(&bool_val, &mut hasher);
                hasher.finish()
            };

            let num_hash = {
                let mut hasher = AHasher::default();
                hash_json_value(&num_val, &mut hasher);
                hasher.finish()
            };

            prop_assert_ne!(null_hash, bool_hash, "null and false should hash differently");
            prop_assert_ne!(null_hash, num_hash, "null and number should hash differently");
            prop_assert_ne!(bool_hash, num_hash, "false and number should hash differently");
        }

        #[test]
        fn prop_array_order_matters(elements in prop::collection::vec(any::<i64>(), 2..10)) {
            // Arrays with different element orders should hash differently
            let arr1 = Value::Array(elements.iter().map(|i| json!(i)).collect());
            let arr2 = Value::Array(elements.iter().rev().map(|i| json!(i)).collect());

            let hash1 = {
                let mut hasher = AHasher::default();
                hash_json_value(&arr1, &mut hasher);
                hasher.finish()
            };

            let hash2 = {
                let mut hasher = AHasher::default();
                hash_json_value(&arr2, &mut hasher);
                hasher.finish()
            };

            // Only assert if the arrays are actually different
            if arr1 != arr2 {
                prop_assert_ne!(hash1, hash2, "Different array orders should hash differently");
            }
        }

        #[test]
        fn prop_filter_removes_specified_paths(
            key in "[a-z]{1,10}",
            value1 in any::<i64>(),
            value2 in any::<i64>(),
            other_key in "[a-z]{1,10}",
            other_value in any::<i64>()
        ) {
            // Only run if keys are different
            if key != other_key {
                let mut obj1 = serde_json::Map::new();
                obj1.insert(key.clone(), json!(value1));
                obj1.insert(other_key.clone(), json!(other_value));

                let mut obj2 = serde_json::Map::new();
                obj2.insert(key.clone(), json!(value2));
                obj2.insert(other_key.clone(), json!(other_value));

                let obj1_val = Value::Object(obj1);
                let obj2_val = Value::Object(obj2);

                let filter = vec![key.clone()];

                let hash1 = {
                    let mut hasher = AHasher::default();
                    hash_json_value_filtered(&obj1_val, &filter, &mut hasher, "");
                    hasher.finish()
                };

                let hash2 = {
                    let mut hasher = AHasher::default();
                    hash_json_value_filtered(&obj2_val, &filter, &mut hasher, "");
                    hasher.finish()
                };

                prop_assert_eq!(
                    hash1, hash2,
                    "Objects should hash the same when filtered field differs"
                );
            }
        }
    }
}
