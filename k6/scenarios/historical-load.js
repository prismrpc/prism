// Prism k6 Load Testing - Historical/Backfilling Pattern
// Based on erpc's evm-historical-randomized.js
//
// This script simulates clients backfilling historical blockchain data:
// - Indexers fetching old blocks
// - Analytics querying historical logs
// - Archive queries for past transactions
//
// Expected behavior: HIGH cache hit rate (historical data is immutable)

import http from 'k6/http';
import { check, randomSeed } from 'k6';
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.1.0/index.js';

import { PRISM_URL, CONFIG, HISTORICAL_PATTERNS, EXECUTORS } from '../lib/config.js';
import {
  ethGetBlockByNumber,
  ethGetBlockByHash,
  ethGetLogs,
  ethGetTransactionByHash,
  ethGetTransactionReceipt,
  getRandomBlock,
  getRandomBlockRange,
  parseBlockResponse,
  parseLogsResponse,
  hasRpcError,
  randomIntBetween,
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
  addBlockHashes,
  addTxHashes,
  getRandomBlockHash,
  getRandomTxHash,
  hasBlockHashes,
  hasTxHashes,
  updateBlockMax,
  seedCaches,
  getPoolSizes,
} from '../lib/cache.js';

// Configuration

// Allow random seed for reproducibility (like erpc)
if (__ENV.RANDOM_SEED) {
  randomSeed(parseInt(__ENV.RANDOM_SEED));
}

// Select executor based on environment variable
const executorName = __ENV.EXECUTOR || 'load';
const executor = EXECUTORS[executorName] || EXECUTORS.load;

export const options = {
  scenarios: {
    historical_load: executor,
  },
  thresholds: {
    'http_req_duration': ['p(95)<500', 'p(99)<1000'],
    'http_req_failed': ['rate<0.01'],
    'cache_hit_rate': ['rate>0.5'], // Expect >50% cache hits for historical
  },
};

// HTTP request parameters
const params = {
  headers: { 'Content-Type': 'application/json' },
  timeout: CONFIG.REQUEST_TIMEOUT,
};

// Setup - Seed Caches

export function setup() {
  console.log('=== Prism Historical Load Test ===');
  console.log(`Target: ${PRISM_URL}`);
  console.log(`Executor: ${executorName}`);
  console.log('');

  // Seed caches with some blocks to avoid cold start issues
  seedCaches(params, 20);

  return { startTime: Date.now() };
}

// Traffic Pattern Implementations

function randomHistoricalBlocks() {
  const blockNum = getRandomBlock();
  const payload = JSON.stringify(ethGetBlockByNumber(blockNum, true));

  const res = http.post(PRISM_URL, payload, params);

  // Cache block hash and transactions for later use (erpc pattern)
  if (res.status === 200) {
    const block = parseBlockResponse(res.body);
    if (block) {
      addBlockHash(block.hash);
      addTxHashes(block.transactions);
      updateBlockMax(block.number);
    }
  }

  return { res, method: 'eth_getBlockByNumber' };
}

function randomLogRanges() {
  const { fromBlock, toBlock } = getRandomBlockRange();
  const payload = JSON.stringify(ethGetLogs(fromBlock, toBlock));

  const res = http.post(PRISM_URL, payload, params);

  // Cache block hashes and tx hashes from logs (erpc pattern)
  if (res.status === 200) {
    const logs = parseLogsResponse(res.body);
    if (logs) {
      addBlockHashes(logs.blockHashes);
      addTxHashes(logs.txHashes);
    }
  }

  return { res, method: 'eth_getLogs' };
}

function randomHistoricalReceipts() {
  // Try to use cached tx hash first (avoids extra call)
  let txHash = getRandomTxHash();

  if (!txHash) {
    // Fallback: fetch a block first to get tx hashes
    const { res: blockRes } = randomHistoricalBlocks();
    if (blockRes.status !== 200) {
      return { res: blockRes, method: 'eth_getBlockByNumber' };
    }
    txHash = getRandomTxHash();
    if (!txHash) {
      // Block had no transactions
      return { res: blockRes, method: 'eth_getBlockByNumber' };
    }
  }

  const payload = JSON.stringify(ethGetTransactionReceipt(txHash));
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getTransactionReceipt' };
}

function randomBlockByHash() {
  // Need cached block hashes
  if (!hasBlockHashes()) {
    // Populate cache first
    return randomHistoricalBlocks();
  }

  const hash = getRandomBlockHash();
  const payload = JSON.stringify(ethGetBlockByHash(hash, false));
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getBlockByHash' };
}

function randomTransactionByHash() {
  // Try to use cached tx hash first
  let txHash = getRandomTxHash();

  if (!txHash) {
    // Fallback: fetch a block first to get tx hashes
    const { res: blockRes } = randomHistoricalBlocks();
    if (blockRes.status !== 200) {
      return { res: blockRes, method: 'eth_getBlockByNumber' };
    }
    txHash = getRandomTxHash();
    if (!txHash) {
      // Block had no transactions
      return { res: blockRes, method: 'eth_getBlockByNumber' };
    }
  }

  const payload = JSON.stringify(ethGetTransactionByHash(txHash));
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getTransactionByHash' };
}

// Main Test Function

export default function () {
  // Weighted random pattern selection (like erpc)
  const rand = Math.random() * 100;
  let cumulativeWeight = 0;
  let result;
  let pattern;

  for (const [p, weight] of Object.entries(HISTORICAL_PATTERNS)) {
    cumulativeWeight += weight;
    if (rand <= cumulativeWeight) {
      pattern = p;
      switch (p) {
        case 'RANDOM_HISTORICAL_BLOCKS':
          result = randomHistoricalBlocks();
          break;
        case 'RANDOM_LOG_RANGES':
          result = randomLogRanges();
          break;
        case 'RANDOM_HISTORICAL_RECEIPTS':
          result = randomHistoricalReceipts();
          break;
        case 'RANDOM_BLOCK_BY_HASH':
          result = randomBlockByHash();
          break;
        case 'RANDOM_TRANSACTION_BY_HASH':
          result = randomTransactionByHash();
          break;
      }
      break;
    }
  }

  if (!result || !result.res) {
    return;
  }

  const { res, method } = result;

  // Record metrics
  const tags = recordResponse(res, pattern, method);

  // Check for JSON-RPC errors
  const rpcError = hasRpcError(res.body);
  if (rpcError && rpcError.code) {
    recordRpcError(res, tags, rpcError);
  }

  // Assertions
  check(res, {
    'status is 2xx': (r) => r.status >= 200 && r.status < 300,
    'no JSON-RPC error': () => !rpcError || !rpcError.code,
    'has result': () => {
      try {
        const body = JSON.parse(res.body);
        return body.result !== undefined;
      } catch (e) {
        recordParsingError(tags);
        return false;
      }
    },
  }, tags);
}

// Teardown and Summary

export function teardown(data) {
  const duration = ((Date.now() - data.startTime) / 1000).toFixed(1);
  console.log(`\nTest duration: ${duration}s`);

  const pools = getPoolSizes();
  console.log(`Final cache pools: ${pools.blockHashes} block hashes, ${pools.txHashes} tx hashes`);
}

export function handleSummary(data) {
  console.log('\n' + '='.repeat(60));
  console.log('PRISM HISTORICAL LOAD TEST RESULTS');
  console.log('='.repeat(60));

  printCacheSummary(data);
  printMethodLatencies(data);
  printRpcErrors(data);

  // Pattern breakdown
  console.log('\n=== Traffic Pattern Weights ===');
  for (const [pattern, weight] of Object.entries(HISTORICAL_PATTERNS)) {
    console.log(`  ${pattern}: ${weight}%`);
  }

  return {
    stdout: textSummary(data, { indent: '  ', enableColors: true }),
  };
}
