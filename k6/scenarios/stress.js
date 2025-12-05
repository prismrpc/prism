// Prism k6 Load Testing - Stress Test
// Push Prism to its limits to find breaking points
//
// Based on erpc patterns with high RPS and mixed traffic
//
// Use this for:
// - Finding capacity limits
// - Testing circuit breakers
// - Validating graceful degradation

import http from 'k6/http';
import { check, randomSeed } from 'k6';
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.1.0/index.js';

import { PRISM_URL, CONFIG, HISTORICAL_PATTERNS, TIP_PATTERNS, EXECUTORS } from '../lib/config.js';
import {
  ethBlockNumber,
  ethGetBlockByNumber,
  ethGetBlockByHash,
  ethGetLogs,
  ethGetTransactionReceipt,
  ethGetBalance,
  getRandomBlock,
  getRandomBlockRange,
  getRandomAddress,
  parseBlockResponse,
  hasRpcError,
} from '../lib/payloads.js';
import {
  recordResponse,
  recordRpcError,
  recordParsingError,
  printCacheSummary,
  printMethodLatencies,
  printRpcErrors,
} from '../lib/metrics.js';
import {
  addBlockHash,
  addTxHashes,
  getRandomBlockHash,
  getRandomTxHash,
  getLatestBlock,
  hasBlockHashes,
  seedCaches,
  getPoolSizes,
} from '../lib/cache.js';

// Configuration

if (__ENV.RANDOM_SEED) {
  randomSeed(parseInt(__ENV.RANDOM_SEED));
}

// Use EXECUTOR=extreme for 2000-5000 RPS, EXECUTOR=ultra for 15000 RPS
const executorName = __ENV.EXECUTOR || 'stress';
const useExtreme = executorName === 'extreme';
const useUltra = executorName === 'ultra';

export const options = {
  scenarios: {
    stress_ramp: useUltra
      ? {
          // Ultra mode: constant 15000 req/s for maximum stress testing
          executor: 'constant-arrival-rate',
          rate: 25000,
          timeUnit: '1s',
          duration: '5m',
          preAllocatedVUs: 5000,
          maxVUs: 25000,
        }
      : {
          executor: 'ramping-arrival-rate',
          startRate: useExtreme ? 500 : 200,
          timeUnit: '1s',
          stages: useExtreme
            ? [
                // Extreme load profile (2000-5000 RPS) - for enterprise testing
                { duration: '1m', target: 4500 },    // Warm up
                { duration: '2m', target: 8500 },   // Moderate load (typical peak)
                { duration: '2m', target: 12500 },   // Heavy load (high peak)
                { duration: '2m', target: 9500 },   // Extreme load (maximum realistic)
                { duration: '1m', target: 6500 },   // Cool down
                { duration: '1m', target: 1500 },   // Recovery
              ]
            : [
                // Realistic production peak load (500-2000 RPS)
                { duration: '1m', target: 500 },    // Warm up
                { duration: '2m', target: 1000 },   // Moderate load (typical peak)
                { duration: '2m', target: 1500 },   // Heavy load (high peak)
                { duration: '2m', target: 2000 },   // Extreme load (maximum realistic)
                { duration: '1m', target: 1000 },   // Cool down
                { duration: '1m', target: 500 },    // Recovery
              ],
          preAllocatedVUs: useExtreme ? 1000 : 500,
          maxVUs: useExtreme ? 3000 : 1500,
        },
  },
  thresholds: {
    // Relaxed thresholds - we expect some degradation
    'http_req_duration': useUltra ? ['p(95)<5000', 'p(99)<10000'] : ['p(95)<2000', 'p(99)<5000'],
    'http_req_failed': useUltra ? ['rate<0.2'] : ['rate<0.1'],  // 20% acceptable for ultra
  },
};

const params = {
  headers: { 'Content-Type': 'application/json' },
  timeout: '60s',  // Longer timeout for stress
};

// Combined traffic patterns (mix of historical and tip)
const STRESS_PATTERNS = {
  // Historical patterns (60%)
  RANDOM_BLOCKS: 20,
  RANDOM_LOGS: 25,
  RANDOM_RECEIPTS: 10,
  BLOCK_BY_HASH: 5,
  // Tip patterns (40%)
  BLOCK_NUMBER: 15,
  RECENT_BLOCKS: 10,
  LATEST_LOGS: 10,
  ACCOUNT_BALANCE: 5,
};

// Setup

export function setup() {
  console.log('=== Prism Stress Test ===');
  console.log(`Target: ${PRISM_URL}`);
  console.log(`Executor: ${executorName}${useUltra ? ' (ULTRA - 15000 RPS)' : useExtreme ? ' (EXTREME)' : ''}`);
  if (useUltra) {
    console.log('WARNING: Ultra mode may overwhelm backends');
  } else if (useExtreme) {
    console.log('WARNING: Extreme mode may overwhelm local devnet');
  }
  console.log('');

  // Seed caches aggressively
  seedCaches(params, useUltra ? 100 : 50);

  return { startTime: Date.now() };
}

// Traffic Pattern Implementations

function randomBlocks() {
  const payload = JSON.stringify(ethGetBlockByNumber(getRandomBlock(), true));
  const res = http.post(PRISM_URL, payload, params);

  if (res.status === 200) {
    const block = parseBlockResponse(res.body);
    if (block) {
      addBlockHash(block.hash);
      addTxHashes(block.transactions);
    }
  }

  return { res, method: 'eth_getBlockByNumber' };
}

function randomLogs() {
  const { fromBlock, toBlock } = getRandomBlockRange();
  const payload = JSON.stringify(ethGetLogs(fromBlock, toBlock));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getLogs' };
}

function randomReceipts() {
  const txHash = getRandomTxHash();
  if (!txHash) return randomBlocks();

  const payload = JSON.stringify(ethGetTransactionReceipt(txHash));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getTransactionReceipt' };
}

function blockByHash() {
  if (!hasBlockHashes()) return randomBlocks();

  const hash = getRandomBlockHash();
  const payload = JSON.stringify(ethGetBlockByHash(hash, false));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getBlockByHash' };
}

function blockNumber() {
  const payload = JSON.stringify(ethBlockNumber());
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_blockNumber' };
}

function recentBlocks() {
  const latest = getLatestBlock(params);
  if (!latest) return blockNumber();

  const latestNum = parseInt(latest.number, 16);
  const offset = Math.floor(Math.random() * 50);
  const targetBlock = `0x${Math.max(1, latestNum - offset).toString(16)}`;

  const payload = JSON.stringify(ethGetBlockByNumber(targetBlock, false));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getBlockByNumber' };
}

function latestLogs() {
  const latest = getLatestBlock(params);
  if (!latest) return blockNumber();

  const latestNum = parseInt(latest.number, 16);
  const rangeSize = Math.floor(Math.random() * 50) + 10;
  const fromBlock = Math.max(1, latestNum - rangeSize);

  const payload = JSON.stringify(ethGetLogs(
    `0x${fromBlock.toString(16)}`,
    `0x${latestNum.toString(16)}`
  ));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getLogs' };
}

function accountBalance() {
  const payload = JSON.stringify(ethGetBalance(getRandomAddress(), 'latest'));
  return { res: http.post(PRISM_URL, payload, params), method: 'eth_getBalance' };
}

// Main Test Function

export default function () {
  const rand = Math.random() * 100;
  let cumulativeWeight = 0;
  let result;
  let pattern;

  for (const [p, weight] of Object.entries(STRESS_PATTERNS)) {
    cumulativeWeight += weight;
    if (rand <= cumulativeWeight) {
      pattern = p;
      switch (p) {
        case 'RANDOM_BLOCKS':
          result = randomBlocks();
          break;
        case 'RANDOM_LOGS':
          result = randomLogs();
          break;
        case 'RANDOM_RECEIPTS':
          result = randomReceipts();
          break;
        case 'BLOCK_BY_HASH':
          result = blockByHash();
          break;
        case 'BLOCK_NUMBER':
          result = blockNumber();
          break;
        case 'RECENT_BLOCKS':
          result = recentBlocks();
          break;
        case 'LATEST_LOGS':
          result = latestLogs();
          break;
        case 'ACCOUNT_BALANCE':
          result = accountBalance();
          break;
      }
      break;
    }
  }

  if (!result || !result.res) return;

  const { res, method } = result;
  const tags = recordResponse(res, pattern, method);

  const rpcError = hasRpcError(res.body);
  if (rpcError && rpcError.code) {
    recordRpcError(res, tags, rpcError);
  }

  // Basic checks - we expect some failures under stress
  check(res, {
    'status is 2xx or 503': (r) => (r.status >= 200 && r.status < 300) || r.status === 503,
    'response received': (r) => r.body && r.body.length > 0,
  }, tags);
}

// Summary

export function teardown(data) {
  const duration = ((Date.now() - data.startTime) / 1000).toFixed(1);
  console.log(`\nStress test completed in ${duration}s`);

  const pools = getPoolSizes();
  console.log(`Final cache pools: ${pools.blockHashes} block hashes, ${pools.txHashes} tx hashes`);
}

export function handleSummary(data) {
  console.log('\n' + '='.repeat(60));
  console.log('PRISM STRESS TEST RESULTS');
  console.log('='.repeat(60));

  // Key stress metrics
  const reqs = data.metrics.http_reqs?.values?.count || 0;
  const rate = data.metrics.http_reqs?.values?.rate || 0;
  const failed = data.metrics.http_req_failed?.values?.rate || 0;
  const p95 = data.metrics.http_req_duration?.values?.['p(95)'] || 0;
  const p99 = data.metrics.http_req_duration?.values?.['p(99)'] || 0;

  console.log('\n=== Stress Summary ===');
  console.log(`  Total Requests: ${reqs.toLocaleString()}`);
  console.log(`  Avg Rate:       ${rate.toFixed(1)} req/s`);
  console.log(`  Error Rate:     ${(failed * 100).toFixed(2)}%`);
  console.log(`  P95 Latency:    ${p95.toFixed(0)}ms`);
  console.log(`  P99 Latency:    ${p99.toFixed(0)}ms`);

  printCacheSummary(data);
  printMethodLatencies(data);
  printRpcErrors(data);

  // Stress assessment
  console.log('\n=== Assessment ===');
  if (failed < 0.01) {
    console.log('  Prism handled stress well - minimal errors');
  } else if (failed < 0.05) {
    console.log('  Prism showed some degradation under extreme load');
  } else if (failed < 0.1) {
    console.log('  Prism experienced significant degradation - consider tuning');
  } else {
    console.log('  Prism needs optimization - too many errors under load');
  }

  return {
    stdout: textSummary(data, { indent: '  ', enableColors: true }),
  };
}
