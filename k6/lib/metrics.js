// Prism k6 Load Testing - Custom Metrics
// Based on erpc patterns for detailed RPC monitoring

import { Counter, Trend, Rate } from 'k6/metrics';

// HTTP-Level Metrics

// Status code tracking (like erpc)
export const statusCodeCounter = new Counter('status_codes');

// Response size tracking
export const responseSizes = new Trend('response_sizes');

// JSON-RPC Level Metrics

// JSON-RPC error tracking (separate from HTTP errors)
export const jsonRpcErrorCounter = new Counter('jsonrpc_errors');

// Parsing errors (malformed responses)
export const parsingErrorsCounter = new Counter('parsing_errors');

// Prism-Specific Metrics

// Cache hit/miss tracking (uses X-Cache-Status header)
export const cacheHitCounter = new Counter('cache_hits');
export const cacheMissCounter = new Counter('cache_misses');
export const cachePartialCounter = new Counter('cache_partial');

// Cache hit rate as a rate metric
export const cacheHitRate = new Rate('cache_hit_rate');

// Method-Specific Latency Trends

export const methodLatency = {
  eth_blockNumber: new Trend('latency_eth_blockNumber'),
  eth_getBlockByNumber: new Trend('latency_eth_getBlockByNumber'),
  eth_getBlockByHash: new Trend('latency_eth_getBlockByHash'),
  eth_getLogs: new Trend('latency_eth_getLogs'),
  eth_getTransactionReceipt: new Trend('latency_eth_getTransactionReceipt'),
  eth_getBalance: new Trend('latency_eth_getBalance'),
};

// Helper Functions

// Record response metrics
export function recordResponse(res, pattern, method) {
  const tags = {
    pattern: pattern,
    method: method || 'unknown',
    status_code: res.status.toString(),
  };

  // HTTP metrics
  statusCodeCounter.add(1, tags);
  if (res.body) {
    responseSizes.add(res.body.length, tags);
  }

  // Method-specific latency
  if (method && methodLatency[method]) {
    methodLatency[method].add(res.timings.duration);
  }

  // Cache status from Prism's X-Cache-Status header
  const cacheStatus = res.headers['X-Cache-Status'];
  if (cacheStatus) {
    switch (cacheStatus) {
      case 'FULL':
        cacheHitCounter.add(1, tags);
        cacheHitRate.add(true);
        break;
      case 'PARTIAL':
        cachePartialCounter.add(1, tags);
        cacheHitRate.add(true); // Partial is still a hit
        break;
      case 'EMPTY':
        cacheHitCounter.add(1, tags); // Empty range cached
        cacheHitRate.add(true);
        break;
      case 'MISS':
        cacheMissCounter.add(1, tags);
        cacheHitRate.add(false);
        break;
    }
  }

  return tags;
}

// Record JSON-RPC error
export function recordRpcError(res, tags, error) {
  jsonRpcErrorCounter.add(1, {
    ...tags,
    rpc_error_code: error.code.toString(),
    rpc_error_message: error.message || 'unknown',
  });
}

// Record parsing error
export function recordParsingError(tags) {
  parsingErrorsCounter.add(1, tags);
}

// Summary Helpers

export function printCacheSummary(data) {
  const hits = data.metrics.cache_hits?.values?.count || 0;
  const misses = data.metrics.cache_misses?.values?.count || 0;
  const partial = data.metrics.cache_partial?.values?.count || 0;
  const total = hits + misses + partial;

  console.log('\n=== Cache Performance ===');
  console.log(`  Full Hits:    ${hits} (${total > 0 ? ((hits / total) * 100).toFixed(1) : 0}%)`);
  console.log(`  Partial Hits: ${partial} (${total > 0 ? ((partial / total) * 100).toFixed(1) : 0}%)`);
  console.log(`  Misses:       ${misses} (${total > 0 ? ((misses / total) * 100).toFixed(1) : 0}%)`);
  console.log(`  Total:        ${total}`);

  if (total > 0) {
    const hitRate = ((hits + partial) / total * 100).toFixed(1);
    console.log(`  Hit Rate:     ${hitRate}%`);
  }
}

export function printMethodLatencies(data) {
  console.log('\n=== Method Latencies (p95) ===');

  const methods = [
    'eth_blockNumber',
    'eth_getBlockByNumber',
    'eth_getBlockByHash',
    'eth_getLogs',
    'eth_getTransactionReceipt',
    'eth_getBalance',
  ];

  for (const method of methods) {
    const metric = data.metrics[`latency_${method}`];
    if (metric && metric.values) {
      const p95 = metric.values['p(95)'] || 0;
      const avg = metric.values.avg || 0;
      console.log(`  ${method}: avg=${avg.toFixed(1)}ms, p95=${p95.toFixed(1)}ms`);
    }
  }
}

export function printRpcErrors(data) {
  const errors = data.metrics.jsonrpc_errors?.values?.count || 0;
  const parsing = data.metrics.parsing_errors?.values?.count || 0;

  if (errors > 0 || parsing > 0) {
    console.log('\n=== Errors ===');
    console.log(`  JSON-RPC Errors: ${errors}`);
    console.log(`  Parsing Errors:  ${parsing}`);
  }
}
