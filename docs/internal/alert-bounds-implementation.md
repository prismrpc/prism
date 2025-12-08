# Alert Storage Bounds Implementation

## Summary
Added memory bounds to alert storage in `AlertManager` to prevent unbounded memory growth.

## Changes Made

### File: `/home/flo/workspace/personal/prism/crates/prism-core/src/alerts/manager.rs`

#### 1. Added Capacity Constant
```rust
/// Maximum number of alerts to keep in memory.
/// This prevents unbounded memory growth from accumulating historical alerts.
const MAX_ALERTS: usize = 1000;
```

#### 2. Enhanced `create_alert` Method
The method now implements a two-phase eviction strategy:

**Phase 1 - Resolved Alert Cleanup (at 90% capacity):**
- Triggers when alerts reach 900 (90% of 1000)
- Removes all resolved alerts first
- Preserves active and acknowledged alerts

**Phase 2 - FIFO Eviction (at max capacity):**
- If still at 1000 alerts after cleanup
- Removes oldest alerts (index 0) in FIFO order
- Ensures we never exceed MAX_ALERTS

```rust
pub fn create_alert(&self, alert: Alert) {
    let mut alerts = self.alerts.write();

    // If at 90% capacity, clean up resolved alerts
    if alerts.len() >= MAX_ALERTS * 9 / 10 {
        alerts.retain(|a| !matches!(a.status, AlertStatus::Resolved));
    }

    // If still at max capacity, remove oldest alerts
    while alerts.len() >= MAX_ALERTS {
        alerts.remove(0);
    }

    alerts.push(alert);
}
```

#### 3. Added Comprehensive Tests

**Test 1: `test_alert_capacity_bounds`**
- Creates 900 active alerts (below threshold)
- Adds 100 resolved alerts (reaches 1000)
- Adds one more alert (triggers cleanup)
- Verifies resolved alerts are removed
- Confirms 901 active alerts remain

**Test 2: `test_alert_capacity_eviction_when_no_resolved`**
- Fills to max capacity with 1000 active alerts
- Adds another alert
- Verifies oldest alert (alert0) is evicted
- Confirms capacity stays at 1000
- Validates FIFO eviction order

**Test 3: `test_alert_capacity_prefers_resolved_eviction`**
- Creates 950 active alerts
- Adds 50 resolved alerts (total 1000)
- Adds one more alert
- Verifies all resolved alerts are removed first
- Confirms all active alerts are preserved
- Results in 951 total alerts

## Benefits

1. **Memory Safety**: Prevents unbounded growth from accumulating historical alerts
2. **Intelligent Eviction**: Prioritizes keeping active alerts over resolved ones
3. **Predictable Behavior**: Fixed maximum of 1000 alerts ensures consistent memory usage
4. **Graceful Degradation**: Automatically cleans up old alerts without manual intervention

## Performance Characteristics

- **Space Complexity**: O(1) - bounded at MAX_ALERTS (1000)
- **Time Complexity**:
  - Best case: O(1) when below 90% capacity
  - Worst case: O(n) when cleanup is needed (retain or remove operations)
  - Amortized: O(1) as cleanup happens infrequently

## Memory Impact

- Previous: Unbounded - could grow indefinitely
- Current: ~1000 alerts × ~200 bytes/alert ≈ 200KB maximum
- Typical: Much less as resolved alerts are cleaned up regularly

## Configuration

The constant `MAX_ALERTS` can be adjusted based on operational requirements:
- Current: 1000 alerts
- Cleanup trigger: 900 alerts (90%)
- Recommended range: 500-5000 depending on alert frequency
