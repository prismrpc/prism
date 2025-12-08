# Metrics Data Source Implementation Summary

## Overview
Fixed silent Prometheus failures by adding data source indicators to metrics responses. Clients now receive clear information about whether data comes from Prometheus, in-memory metrics, or fallback/default values, along with warnings when degraded data is returned.

## Files Modified

### 1. `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs`
**Added new types:**
- `DataSource` enum with variants:
  - `Prometheus` - Data from Prometheus time-series database
  - `InMemory` - Data from in-memory metrics collector
  - `Fallback` - Default/fallback data when other sources unavailable

- `MetricsDataResponse<T>` wrapper struct with:
  - `data: T` - The actual metrics data
  - `source: DataSource` - Indicates data origin
  - `warning: Option<String>` - Optional warning message for degraded data

- Helper methods:
  - `from_prometheus(data)` - Create response with Prometheus data
  - `from_in_memory(data)` - Create response with in-memory data
  - `fallback(data, warning)` - Create response with fallback data and warning

### 2. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/metrics.rs`
**Updated 5 metrics handlers:**

#### `get_latency()`
- Returns `MetricsDataResponse<Vec<LatencyDataPoint>>`
- On Prometheus success: Returns data with `source: "prometheus"`
- On Prometheus failure/unavailable: Returns in-memory data with warning

#### `get_request_volume()`
- Returns `MetricsDataResponse<Vec<TimeSeriesPoint>>`
- On Prometheus success: Returns time-series data with `source: "prometheus"`
- On Prometheus failure/unavailable: Returns total count with warning about missing rate calculation

#### `get_request_methods()`
- Returns `MetricsDataResponse<Vec<RequestMethodData>>`
- On Prometheus success: Returns historical method distribution with `source: "prometheus"`
- On Prometheus failure/unavailable: Returns in-memory method counts with warning

#### `get_error_distribution()`
- Returns `MetricsDataResponse<Vec<ErrorDistribution>>`
- On Prometheus success: Returns error breakdown by type with `source: "prometheus"`
- On Prometheus failure/unavailable: Returns errors by upstream with warning

#### `get_hedging_stats()`
- Returns `MetricsDataResponse<HedgingStats>`
- On Prometheus success: Returns hedging metrics with `source: "prometheus"`
- On Prometheus failure/unavailable: Returns default/zero stats with `source: "fallback"` and warning

### 3. `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`
**Updated OpenAPI schemas to include:**
- `types::DataSource`
- `types::MetricsDataResponse<Vec<types::LatencyDataPoint>>`
- `types::MetricsDataResponse<Vec<types::TimeSeriesPoint>>`
- `types::MetricsDataResponse<Vec<types::RequestMethodData>>`
- `types::MetricsDataResponse<Vec<types::ErrorDistribution>>`
- `types::MetricsDataResponse<types::HedgingStats>`

## Example Response Format

### Successful Prometheus Response
```json
{
  "data": [
    {
      "timestamp": "2025-12-07T10:30:00Z",
      "p50": 45.2,
      "p95": 120.5,
      "p99": 250.8
    }
  ],
  "source": "prometheus"
}
```

### Degraded In-Memory Response
```json
{
  "data": [
    {
      "timestamp": "2025-12-07T10:30:00Z",
      "p50": 85.0,
      "p95": 85.0,
      "p99": 85.0
    }
  ],
  "source": "in_memory",
  "warning": "Prometheus query failed, showing in-memory snapshot"
}
```

### Fallback Response
```json
{
  "data": {
    "hedgedRequests": 0.0,
    "hedgeTriggers": 0.0,
    "primaryWins": 0.0,
    "p99Improvement": 0.0,
    "winnerDistribution": {
      "primary": 100.0,
      "hedged": 0.0
    }
  },
  "source": "fallback",
  "warning": "Prometheus not configured, showing default fallback data"
}
```

## Warning Messages

### Prometheus Configured but Failed
- Latency: "Prometheus query failed, showing in-memory snapshot"
- Request Volume: "Prometheus query failed, showing in-memory total count"
- Request Methods: "Prometheus query failed, showing in-memory method counts"
- Error Distribution: "Prometheus query failed, showing in-memory errors by upstream"
- Hedging Stats: "Prometheus query failed, showing default fallback data"

### Prometheus Not Configured
- Latency: "Prometheus not configured, showing in-memory snapshot"
- Request Volume: "Prometheus not configured, showing in-memory total count"
- Request Methods: "Prometheus not configured, showing in-memory method counts"
- Error Distribution: "Prometheus not configured, showing in-memory errors by upstream"
- Hedging Stats: "Prometheus not configured, showing default fallback data"

## Benefits

1. **Transparency**: Clients know exactly where data comes from
2. **Observability**: Warning messages explain why data is degraded
3. **Debugging**: Easier to diagnose Prometheus connectivity issues
4. **User Experience**: Frontend can show appropriate indicators/warnings
5. **API Evolution**: Can add more data sources in the future without breaking changes

## Verification

### Compilation
```bash
cargo check --workspace
```
Result: SUCCESS (only unrelated warnings about unused imports)

### Type Safety
All handlers properly typed with `MetricsDataResponse<T>` where `T` matches the original data type.

### OpenAPI Documentation
All response schemas updated to reflect the new wrapped response format.

## Success Criteria - ACHIEVED

- [x] New wrapper types created (`MetricsDataResponse`, `DataSource`)
- [x] 5 metrics handlers updated (all specified handlers)
- [x] Clients can distinguish Prometheus data from fallback data
- [x] Warnings explain why data is degraded
- [x] `cargo check` passes
- [x] OpenAPI schemas updated

## Next Steps (Optional)

1. Update frontend clients to display data source indicators
2. Add monitoring/alerting on frequent fallback responses
3. Create integration tests for different data source scenarios
4. Consider adding data source metadata to more endpoints (cache stats, upstream metrics)
5. Document migration guide for API consumers
