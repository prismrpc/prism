// Prism k6 Load Testing - Smoke Test
// Quick validation that Prism is responding correctly
//
// Use this for:
// - Quick CI checks
// - Pre-deployment validation
// - Development testing
//
// Low load, short duration, basic checks

import http from 'k6/http';
import { check, sleep } from 'k6';
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.1.0/index.js';

import { PRISM_URL, CONFIG, EXECUTORS } from '../lib/config.js';
import {
  ethBlockNumber,
  ethChainId,
  ethGetBlockByNumber,
  ethGetLogs,
  hasRpcError,
} from '../lib/payloads.js';
import {
  statusCodeCounter,
  cacheHitCounter,
  cacheMissCounter,
  jsonRpcErrorCounter,
} from '../lib/metrics.js';

// Configuration

export const options = {
  scenarios: {
    smoke: EXECUTORS.smoke,
  },
  thresholds: {
    'http_req_duration': ['p(95)<1000'],  // Relaxed for smoke
    'http_req_failed': ['rate<0.05'],      // 5% error rate acceptable for smoke
  },
};

const params = {
  headers: { 'Content-Type': 'application/json' },
  timeout: CONFIG.REQUEST_TIMEOUT,
};

// Setup

export function setup() {
  console.log('=== Prism Smoke Test ===');
  console.log(`Target: ${PRISM_URL}`);
  console.log('');

  // Verify Prism is reachable
  const payload = JSON.stringify(ethBlockNumber());
  const res = http.post(PRISM_URL, payload, params);

  if (res.status !== 200) {
    console.error(`FAILED: Cannot reach Prism at ${PRISM_URL}`);
    console.error(`Status: ${res.status}, Body: ${res.body}`);
    return { healthy: false };
  }

  try {
    const body = JSON.parse(res.body);
    if (body.error) {
      console.error(`FAILED: RPC error: ${body.error.message}`);
      return { healthy: false };
    }
    console.log(`Prism is healthy. Current block: ${body.result}`);
  } catch (e) {
    console.error(`FAILED: Invalid response: ${e}`);
    return { healthy: false };
  }

  return { healthy: true, startTime: Date.now() };
}

// Test Methods

const testMethods = [
  {
    name: 'eth_blockNumber',
    payload: () => ethBlockNumber(),
    validate: (body) => typeof body.result === 'string' && body.result.startsWith('0x'),
  },
  {
    name: 'eth_chainId',
    payload: () => ethChainId(),
    validate: (body) => body.result === '0x539', // 1337 in hex
  },
  {
    name: 'eth_getBlockByNumber',
    payload: () => ethGetBlockByNumber('0x1', false),
    validate: (body) => body.result && body.result.number === '0x1',
  },
  {
    name: 'eth_getLogs',
    payload: () => ethGetLogs('0x1', '0x10'),
    validate: (body) => Array.isArray(body.result),
  },
];

// Main Test Function

export default function (data) {
  if (!data.healthy) {
    console.warn('Skipping iteration - Prism not healthy');
    sleep(1);
    return;
  }

  // Cycle through test methods
  const methodIdx = __ITER % testMethods.length;
  const test = testMethods[methodIdx];

  const payload = JSON.stringify(test.payload());
  const res = http.post(PRISM_URL, payload, params);

  // Track status code
  statusCodeCounter.add(1, {
    method: test.name,
    status_code: res.status.toString(),
  });

  // Track cache status
  const cacheStatus = res.headers['X-Cache-Status'];
  if (cacheStatus === 'FULL' || cacheStatus === 'PARTIAL') {
    cacheHitCounter.add(1, { method: test.name });
  } else if (cacheStatus === 'MISS') {
    cacheMissCounter.add(1, { method: test.name });
  }

  // Parse and validate response
  let body;
  try {
    body = JSON.parse(res.body);
  } catch (e) {
    check(res, { 'valid JSON': () => false });
    return;
  }

  // Check for RPC errors
  if (body.error) {
    jsonRpcErrorCounter.add(1, {
      method: test.name,
      code: body.error.code.toString(),
    });
  }

  // Run checks
  check(res, {
    [`${test.name} status 200`]: (r) => r.status === 200,
    [`${test.name} no error`]: () => !body.error,
    [`${test.name} valid result`]: () => test.validate(body),
  });

  // Small delay between requests
  sleep(0.5);
}

// Summary

export function teardown(data) {
  if (!data.healthy) {
    console.error('\n!!! SMOKE TEST FAILED - PRISM NOT HEALTHY !!!');
    return;
  }

  const duration = ((Date.now() - data.startTime) / 1000).toFixed(1);
  console.log(`\nSmoke test completed in ${duration}s`);
}

export function handleSummary(data) {
  console.log('\n' + '='.repeat(50));
  console.log('PRISM SMOKE TEST RESULTS');
  console.log('='.repeat(50));

  // Quick pass/fail summary
  const failed = data.metrics.http_req_failed?.values?.rate || 0;
  const duration = data.metrics.http_req_duration?.values?.['p(95)'] || 0;

  console.log(`\n  Error Rate: ${(failed * 100).toFixed(2)}% ${failed < 0.05 ? 'PASS' : 'FAIL'}`);
  console.log(`  P95 Latency: ${duration.toFixed(0)}ms ${duration < 1000 ? 'PASS' : 'FAIL'}`);

  const hits = data.metrics.cache_hits?.values?.count || 0;
  const misses = data.metrics.cache_misses?.values?.count || 0;
  if (hits + misses > 0) {
    console.log(`  Cache Hit Rate: ${((hits / (hits + misses)) * 100).toFixed(1)}%`);
  }

  const overallPass = failed < 0.05 && duration < 1000;
  console.log(`\n  Overall: ${overallPass ? 'PASS' : 'FAIL'}`);

  return {
    stdout: textSummary(data, { indent: '  ', enableColors: true }),
  };
}
