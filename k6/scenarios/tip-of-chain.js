// Prism k6 Load Testing - Tip-of-Chain Pattern
// Based on erpc's evm-tip-of-chain.js
//
// This script simulates real-time client access patterns:
// - Wallets checking latest balances
// - Explorers tracking new blocks
// - dApps polling for confirmations
//
// Expected behavior: LOWER cache hit rate (tip data changes frequently)
// Tests cache invalidation and freshness guarantees

import http from 'k6/http';
import { check, randomSeed } from 'k6';
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.1.0/index.js';

import { PRISM_URL, CONFIG, TIP_PATTERNS, EXECUTORS } from '../lib/config.js';
import {
  ethBlockNumber,
  ethGetBlockByNumber,
  ethGetLogs,
  ethGetTransactionReceipt,
  ethGetBalance,
  getRandomAddress,
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
  getLatestBlock,
  addTxHashes,
  getRandomTxHash,
  hasTxHashes,
  getPoolSizes,
} from '../lib/cache.js';

// Configuration

// Allow random seed for reproducibility (like erpc)
if (__ENV.RANDOM_SEED) {
  randomSeed(parseInt(__ENV.RANDOM_SEED));
}

// Select executor based on environment variable
// Default to higher RPS for tip-of-chain (like erpc's 500 vs 200)
const executorName = __ENV.EXECUTOR || 'load';
const executor = EXECUTORS[executorName] || EXECUTORS.load;

// Override rate for tip-of-chain (higher throughput, but realistic)
// Tip-of-chain typically sees 1.5-2x the load of historical queries
const tipExecutor = {
  ...executor,
  rate: executor.rate ? Math.min(executor.rate * 1.5, 1000) : 750,
  maxVUs: executor.maxVUs ? Math.max(executor.maxVUs, 1000) : 1000,
};

export const options = {
  scenarios: {
    tip_of_chain: tipExecutor,
  },
  thresholds: {
    'http_req_duration': ['p(95)<300', 'p(99)<500'], // Tighter for real-time
    'http_req_failed': ['rate<0.01'],
    // Lower cache hit expectation for tip data
    'cache_hit_rate': ['rate>0.2'],
  },
};

// HTTP request parameters
const params = {
  headers: {
    'Content-Type': 'application/json',
    'Accept-Encoding': 'gzip, deflate',
    'Connection': 'keep-alive',
  },
  timeout: CONFIG.REQUEST_TIMEOUT,
};

// Setup

export function setup() {
  console.log('=== Prism Tip-of-Chain Load Test ===');
  console.log(`Target: ${PRISM_URL}`);
  console.log(`Executor: ${executorName} (rate: ${tipExecutor.rate} RPS)`);
  console.log('');

  // Get initial latest block to seed the cache
  const latest = getLatestBlock(params);
  if (latest) {
    console.log(`Starting at block: ${parseInt(latest.number, 16)}`);
    console.log(`Transactions in latest: ${latest.transactions?.length || 0}`);
  } else {
    console.warn('Could not get latest block during setup');
  }

  return { startTime: Date.now() };
}

// Traffic Pattern Implementations

function recentBlocks() {
  // Get latest block first (cached for 1s like erpc)
  const latest = getLatestBlock(params);
  if (!latest) {
    return { res: null, method: 'eth_getBlockByNumber' };
  }

  const latestNum = parseInt(latest.number, 16);

  // Request a recent block (within last 50 blocks, like erpc's 500)
  const offset = randomIntBetween(0, Math.min(50, latestNum - 1));
  const targetBlock = `0x${(latestNum - offset).toString(16)}`;

  const payload = JSON.stringify(ethGetBlockByNumber(targetBlock, true));
  const res = http.post(PRISM_URL, payload, params);

  // Cache transactions for receipt tests
  if (res.status === 200) {
    const block = parseBlockResponse(res.body);
    if (block) {
      addTxHashes(block.transactions);
    }
  }

  return { res, method: 'eth_getBlockByNumber' };
}

function latestLogs() {
  // Get latest block
  const latest = getLatestBlock(params);
  if (!latest) {
    return { res: null, method: 'eth_getLogs' };
  }

  const latestNum = parseInt(latest.number, 16);

  // Query logs from recent range (last 100 blocks max)
  const rangeSize = randomIntBetween(10, 100);
  const fromBlock = Math.max(1, latestNum - rangeSize);

  const payload = JSON.stringify(ethGetLogs(
    `0x${fromBlock.toString(16)}`,
    `0x${latestNum.toString(16)}`
  ));

  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getLogs' };
}

function latestReceipts() {
  // Try cached tx hash first
  let txHash = getRandomTxHash();

  if (!txHash) {
    // Get latest block to populate tx cache
    const latest = getLatestBlock(params);
    if (!latest) {
      return { res: null, method: 'eth_getTransactionReceipt' };
    }
    txHash = getRandomTxHash();
    if (!txHash) {
      // No transactions in recent blocks
      return { res: null, method: 'eth_getTransactionReceipt' };
    }
  }

  const payload = JSON.stringify(ethGetTransactionReceipt(txHash));
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getTransactionReceipt' };
}

function blockNumberPoll() {
  // Simple eth_blockNumber call (wallets poll this frequently)
  const payload = JSON.stringify(ethBlockNumber());
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_blockNumber' };
}

function accountBalances() {
  // Random address balance check (like erpc)
  const address = getRandomAddress();
  const payload = JSON.stringify(ethGetBalance(address, 'latest'));
  const res = http.post(PRISM_URL, payload, params);

  return { res, method: 'eth_getBalance' };
}

// Main Test Function

export default function () {
  // Weighted random pattern selection (like erpc)
  const rand = Math.random() * 100;
  let cumulativeWeight = 0;
  let result;
  let pattern;

  for (const [p, weight] of Object.entries(TIP_PATTERNS)) {
    cumulativeWeight += weight;
    if (rand <= cumulativeWeight) {
      pattern = p;
      switch (p) {
        case 'RECENT_BLOCKS':
          result = recentBlocks();
          break;
        case 'LATEST_LOGS':
          result = latestLogs();
          break;
        case 'LATEST_RECEIPTS':
          result = latestReceipts();
          break;
        case 'BLOCK_NUMBER_POLL':
          result = blockNumberPoll();
          break;
        case 'ACCOUNT_BALANCES':
          result = accountBalances();
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

  // Assertions (tighter for real-time patterns)
  check(res, {
    'status is 2xx': (r) => r.status >= 200 && r.status < 300,
    'no JSON-RPC error': () => !rpcError || !rpcError.code,
    'response time < 300ms': (r) => r.timings.duration < 300,
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
  console.log('PRISM TIP-OF-CHAIN LOAD TEST RESULTS');
  console.log('='.repeat(60));

  printCacheSummary(data);
  printMethodLatencies(data);
  printRpcErrors(data);

  // Pattern breakdown
  console.log('\n=== Traffic Pattern Weights ===');
  for (const [pattern, weight] of Object.entries(TIP_PATTERNS)) {
    console.log(`  ${pattern}: ${weight}%`);
  }

  // Tip-specific note
  console.log('\n=== Note ===');
  console.log('  Tip-of-chain tests expect lower cache hit rates.');
  console.log('  This is normal as latest block data changes frequently.');

  return {
    stdout: textSummary(data, { indent: '  ', enableColors: true }),
  };
}
