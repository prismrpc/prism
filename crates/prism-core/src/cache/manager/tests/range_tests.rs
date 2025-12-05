//! Tests for block range operations.

use super::*;

#[test]
fn test_get_missing_ranges_empty_cache() {
    let cache = create_test_cache_manager();

    // Query a range when cache is empty - should return the entire range as missing
    let missing = cache.get_missing_ranges(100, 200);
    assert_eq!(missing, vec![(100, 200)]);
}

#[test]
fn test_get_missing_ranges_single_block() {
    let cache = create_test_cache_manager();

    // Query for single block range when empty
    let missing = cache.get_missing_ranges(100, 100);
    assert_eq!(missing, vec![(100, 100)]);
}

#[test]
fn test_get_missing_ranges_from_equals_to() {
    let cache = create_test_cache_manager();

    // Edge case: from_block equals to_block
    let missing = cache.get_missing_ranges(50, 50);
    assert_eq!(missing, vec![(50, 50)]);
}

#[test]
fn test_is_range_covered_empty_cache() {
    let cache = create_test_cache_manager();

    // Empty cache should not cover any range
    assert!(!cache.is_range_covered(100, 200));
    assert!(!cache.is_range_covered(0, 0));
    assert!(!cache.is_range_covered(50, 50));
}

#[test]
fn test_is_range_covered_boundary() {
    let cache = create_test_cache_manager();

    // Test boundary conditions
    assert!(!cache.is_range_covered(0, 1));
    assert!(!cache.is_range_covered(u64::MAX - 1, u64::MAX));
}

#[test]
fn test_get_missing_ranges_large_range() {
    let cache = create_test_cache_manager();

    // Large range should still work correctly
    let missing = cache.get_missing_ranges(0, 10000);
    assert_eq!(missing, vec![(0, 10000)]);
}

#[test]
fn test_get_missing_ranges_zero_start() {
    let cache = create_test_cache_manager();

    // Range starting at block 0 (genesis)
    let missing = cache.get_missing_ranges(0, 10);
    assert_eq!(missing, vec![(0, 10)]);
}

#[test]
fn test_get_missing_ranges_adjacent_blocks() {
    let cache = create_test_cache_manager();

    // Adjacent blocks: 99, 100, 101
    let missing = cache.get_missing_ranges(99, 101);
    assert_eq!(missing, vec![(99, 101)]);
}

#[test]
fn test_get_block_range_from_logs_hex() {
    let cache = create_test_cache_manager();

    let params = json!({
        "fromBlock": "0x64",
        "toBlock": "0xc8"
    });

    let range = cache.get_block_range_from_logs(&params);
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.from, 100);
    assert_eq!(range.to, 200);
}

#[test]
fn test_get_block_range_from_logs_decimal() {
    let cache = create_test_cache_manager();

    let params = json!({
        "fromBlock": "100",
        "toBlock": "200"
    });

    let range = cache.get_block_range_from_logs(&params);
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.from, 100);
    assert_eq!(range.to, 200);
}

#[test]
fn test_get_block_range_from_logs_from_only() {
    let cache = create_test_cache_manager();

    let params = json!({
        "fromBlock": "0x64"
    });

    let range = cache.get_block_range_from_logs(&params);
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.from, 100);
    assert_eq!(range.to, 100); // Should default to fromBlock
}

#[test]
fn test_get_block_range_from_logs_missing_from() {
    let cache = create_test_cache_manager();

    let params = json!({
        "toBlock": "0xc8"
    });

    let range = cache.get_block_range_from_logs(&params);
    assert!(range.is_none(), "Should return None when fromBlock is missing");
}

#[test]
fn test_get_block_range_from_logs_invalid_params() {
    let cache = create_test_cache_manager();

    // Not an object
    let params = json!([]);
    assert!(cache.get_block_range_from_logs(&params).is_none());

    // Invalid fromBlock
    let params = json!({
        "fromBlock": "invalid",
        "toBlock": "0xc8"
    });
    assert!(cache.get_block_range_from_logs(&params).is_none());
}

#[tokio::test]
async fn test_is_range_covered_true() {
    let cache = create_test_cache_manager();

    // Cache all blocks in range
    for num in 400..=410 {
        cache.insert_header(create_test_header(num)).await;
    }

    assert!(cache.is_range_covered(400, 410), "Range should be covered");
    assert!(cache.is_range_covered(405, 408), "Subset should be covered");
}

#[tokio::test]
async fn test_get_missing_ranges_with_gaps() {
    let cache = create_test_cache_manager();

    // Cache blocks with gaps: 500, 501, 502, skip 503-505, 506, 507
    for num in [500, 501, 502, 506, 507] {
        cache.insert_header(create_test_header(num)).await;
    }

    let missing = cache.get_missing_ranges(500, 510);

    // Should find gaps: 503-505 and 508-510
    assert!(!missing.is_empty(), "Should find gaps");
    assert!(missing.contains(&(503, 505)), "Should find gap 503-505");
}

#[tokio::test]
async fn test_get_missing_ranges_trailing_gap() {
    let cache = create_test_cache_manager();

    // Cache only first few blocks
    for num in 600..=603 {
        cache.insert_header(create_test_header(num)).await;
    }

    let missing = cache.get_missing_ranges(600, 610);

    // Should find trailing gap 604-610
    assert!(missing.iter().any(|&(_from, to)| to == 610), "Should find trailing gap");
}
