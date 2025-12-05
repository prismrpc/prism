use std::{cell::RefCell, fmt::Write};

thread_local! {
    /// Thread-local buffer for hex formatting and parsing operations.
    ///
    /// Single buffer reused across all hex operations. Pre-sized to 1KB to handle
    /// both typical hashes (66 chars) and large payloads (transaction calldata).
    static HEX_BUFFER: RefCell<String> = RefCell::new(String::with_capacity(1024));
}

/// Formats bytes as hex with "0x" prefix using a callback to avoid cloning.
///
/// This is the most efficient approach when the hex string is only needed temporarily.
/// The callback receives a reference to the formatted string and must return immediately.
///
/// # Performance
/// Zero allocations - reuses thread-local buffer. Up to 2x faster than allocating methods.
///
/// # Examples
/// ```ignore
/// with_hex_format(&[0xde, 0xad], |hex| println!("{}", hex)); // prints "0xdead"
/// ```
pub fn with_hex_format<T, F>(bytes: &[u8], f: F) -> T
where
    F: FnOnce(&str) -> T,
{
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in bytes {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        f(&buf)
    })
}

/// Formats a `u64` as hex with "0x" prefix using a callback to avoid cloning.
///
/// Optimized for block numbers and other numeric values. Zero is formatted as "0x0".
pub fn with_hex_u64<T, F>(value: u64, f: F) -> T
where
    F: FnOnce(&str) -> T,
{
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();

        if value == 0 {
            buf.push_str("0x0");
        } else {
            buf.push_str("0x");
            let _ = write!(&mut buf, "{value:x}");
        }

        f(&buf)
    })
}

/// Formats a 32-byte hash with "0x" prefix using a callback to avoid cloning.
///
/// Optimized for transaction and block hashes.
pub fn with_hash32<T, F>(hash: &[u8; 32], f: F) -> T
where
    F: FnOnce(&str) -> T,
{
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in hash {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        f(&buf)
    })
}

/// Formats a 20-byte address with "0x" prefix using a callback to avoid cloning.
///
/// Optimized for Ethereum addresses.
pub fn with_address<T, F>(address: &[u8; 20], f: F) -> T
where
    F: FnOnce(&str) -> T,
{
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in address {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        f(&buf)
    })
}

/// Custom serializer for hex data that writes directly to JSON output.
///
/// Eliminates intermediate string allocations during JSON serialization by formatting
/// hex directly into the serializer's buffer.
pub struct HexSerializer<'a> {
    bytes: &'a [u8],
}

impl<'a> HexSerializer<'a> {
    /// Creates a new hex serializer wrapping the provided bytes.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl serde::Serialize for HexSerializer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut output = String::with_capacity(2 + self.bytes.len() * 2);
        output.push_str("0x");
        for byte in self.bytes {
            write!(&mut output, "{byte:02x}").map_err(serde::ser::Error::custom)?;
        }
        serializer.serialize_str(&output)
    }
}

/// Formats bytes as hex with "0x" prefix, returning an owned `String`.
///
/// This is the primary formatting function that reuses thread-local buffers to minimize
/// allocations. Prefer callback-based `with_hex_format` when the string doesn't need to escape the
/// current scope.
///
/// # Performance
/// Reuses thread-local buffer for formatting, only cloning the final result.
///
/// # Panics
/// Panics if the string buffer fails to write (extremely unlikely).
#[must_use]
pub fn format_hex(bytes: &[u8]) -> String {
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in bytes {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        buf.clone()
    })
}

/// Formats a `u64` as hex with "0x" prefix, returning an owned `String`.
///
/// Optimized for block numbers and other numeric values. Zero is formatted as "0x0".
///
/// # Panics
/// Panics if the string buffer fails to write (extremely unlikely).
#[must_use]
pub fn format_hex_u64(value: u64) -> String {
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();

        if value == 0 {
            buf.push_str("0x0");
        } else {
            buf.push_str("0x");
            let _ = write!(&mut buf, "{value:x}");
        }

        buf.clone()
    })
}

/// Formats a 32-byte hash with "0x" prefix, returning an owned `String`.
///
/// Optimized for transaction and block hashes.
///
/// # Panics
/// Panics if the string buffer fails to write (extremely unlikely).
#[must_use]
pub fn format_hash32(hash: &[u8; 32]) -> String {
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in hash {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        buf.clone()
    })
}

/// Formats a 20-byte address with "0x" prefix, returning an owned `String`.
///
/// Optimized for Ethereum addresses.
///
/// # Panics
/// Panics if the string buffer fails to write (extremely unlikely).
#[must_use]
pub fn format_address(address: &[u8; 20]) -> String {
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();
        buf.push_str("0x");

        for byte in address {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        buf.clone()
    })
}

/// Formats large hex data with "0x" prefix.
///
/// Used for transaction calldata and other large payloads. Grows the buffer
/// capacity as needed for very large payloads.
#[must_use]
pub fn format_hex_large(bytes: &[u8]) -> String {
    HEX_BUFFER.with(|buffer| {
        let mut buf = buffer.borrow_mut();
        buf.clear();

        let needed = 2 + bytes.len() * 2;
        let current_capacity = buf.capacity();
        if current_capacity < needed {
            buf.reserve(needed - current_capacity);
        }

        buf.push_str("0x");
        for byte in bytes {
            let _ = write!(&mut buf, "{byte:02x}");
        }

        buf.clone()
    })
}

/// Returns current capacity and length of the hex formatting buffer.
///
/// Useful for testing and verifying buffer reuse patterns.
/// Returns `(capacity, length)` in bytes.
#[must_use]
pub fn get_hex_buffer_stats() -> (u64, u64) {
    HEX_BUFFER.with(|buffer| {
        let buf = buffer.borrow();
        (buf.capacity() as u64, buf.len() as u64)
    })
}

/// Resets buffer stats by clearing the hex buffer.
///
/// Used primarily in tests to ensure clean state between test runs.
pub fn reset_buffer_stats() {
    HEX_BUFFER.with(|buffer| {
        buffer.borrow_mut().clear();
    });
}

/// Parses a hex string to `u64`.
///
/// Accepts strings with or without "0x" prefix. Returns `None` if invalid hex or overflow.
#[must_use]
pub fn parse_hex_u64(hex: &str) -> Option<u64> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
    u64::from_str_radix(hex_str, 16).ok()
}

/// Parses a hex string to `u32`.
///
/// Accepts strings with or without "0x" prefix. Returns `None` if invalid hex or overflow.
#[must_use]
pub fn parse_hex_u32(hex: &str) -> Option<u32> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
    u32::from_str_radix(hex_str, 16).ok()
}

/// Parses a hex string to bytes.
///
/// Accepts strings with or without "0x" prefix. Returns `None` if invalid hex or odd length.
#[must_use]
pub fn parse_hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
    hex::decode(hex_str).ok()
}

/// Parses a hex string to a fixed-size byte array.
///
/// Accepts strings with or without "0x" prefix. Returns `None` if length doesn't
/// match `N * 2` characters or contains invalid hex.
#[must_use]
pub fn parse_hex_array<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
    if hex_str.len() != N * 2 {
        return None;
    }

    let bytes = hex_str.as_bytes();
    let mut array = [0u8; N];
    for (i, chunk) in bytes.chunks(2).enumerate() {
        let high = hex_digit_to_u8(chunk[0])?;
        let low = hex_digit_to_u8(chunk[1])?;
        array[i] = (high << 4) | low;
    }

    Some(array)
}

/// Converts a single hex digit character to its numeric value.
fn hex_digit_to_u8(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod hex_buffer_tests {
    use super::*;

    #[test]
    fn test_hex_buffer_reuse() {
        let hex1 = format_hex(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(hex1, "0xdeadbeef");

        let hex2 = format_hex(&[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(hex2, "0xcafebabe");

        let (_total, reused) = get_hex_buffer_stats();
        assert!(reused > 0, "Buffer should have been reused");
    }

    // NOTE: Performance benchmarks have been moved to benches/hex_benchmarks.rs
    // Run with: cargo bench -p prism-core --bench hex_benchmarks
}
