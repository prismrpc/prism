// Prism k6 Load Testing - Shared Configuration
// Based on erpc patterns adapted for Prism's devnet setup

export const PRISM_URL = __ENV.PRISM_URL || 'http://localhost:3030';

// Prism's supported methods (from crates/prism-core/src/types.rs lines 5-17)
export const ALLOWED_METHODS = [
  'net_version',
  'eth_blockNumber',
  'eth_chainId',
  'eth_gasPrice',
  'eth_getBalance',
  'eth_getBlockByHash',
  'eth_getBlockByNumber',
  'eth_getLogs',
  'eth_getTransactionByHash',
  'eth_getTransactionReceipt',
  'eth_getCode',
];

// Devnet chain configuration (matches docker/devnet/config.test.toml)
export const CHAIN = {
  id: 1337,
  name: 'devnet',
  // Block range grows as tx-spammer runs
  // Start conservative, will be updated dynamically
  blockMin: 1000,
  blockMax: 10000,
  cached: {
    latestBlock: null,
    latestBlockTimestamp: 0,
    blockHashPool: [],
    txHashPool: [],
  },
};

// Test configuration
export const CONFIG = {
  // Log range settings (Prism's strength is range caching)
  // With 9000 blocks, can use larger ranges for realistic eth_getLogs queries
  LOG_RANGE_MIN_BLOCKS: 10,
  LOG_RANGE_MAX_BLOCKS: 500,

  // Cache TTLs
  LATEST_BLOCK_CACHE_TTL: 1, // seconds

  // Pool sizes for cached hashes
  // Scale up to maintain coverage across larger block range
  MAX_BLOCK_HASH_POOL: 500,
  MAX_TX_HASH_POOL: 1000,

  // Request settings
  REQUEST_TIMEOUT: '30s',
};

// Traffic pattern weights for historical load testing
export const HISTORICAL_PATTERNS = {
  RANDOM_HISTORICAL_BLOCKS: 25,   // eth_getBlockByNumber
  RANDOM_LOG_RANGES: 50,          // eth_getLogs (Prism's strength!)
  RANDOM_HISTORICAL_RECEIPTS: 15, // eth_getTransactionReceipt
  RANDOM_BLOCK_BY_HASH: 10,       // eth_getBlockByHash
};

// Traffic pattern weights for tip-of-chain testing
export const TIP_PATTERNS = {
  RECENT_BLOCKS: 25,              // eth_getBlockByNumber (tip - N)
  LATEST_LOGS: 25,                // eth_getLogs (recent range)
  LATEST_RECEIPTS: 25,            // eth_getTransactionReceipt
  BLOCK_NUMBER_POLL: 15,          // eth_blockNumber
  ACCOUNT_BALANCES: 10,           // eth_getBalance
};

// Default thresholds
export const THRESHOLDS = {
  // Latency targets
  http_req_duration_p95: 500,  // ms
  http_req_duration_p99: 1000, // ms

  // Error rate
  http_req_failed_rate: 0.01,  // 1%

  // Cache expectations - lower hit rates expected with larger block range
  // 9000 blocks = more unique queries = more cache misses
  cache_hit_rate_historical: 0.4,  // 40% (down from 70%)
  cache_hit_rate_tip: 0.2,         // 20% (down from 30%)
};

// Executor configurations
export const EXECUTORS = {
  smoke: {
    executor: 'constant-arrival-rate',
    rate: 50,
    timeUnit: '1s',
    duration: '1m',
    preAllocatedVUs: 50,
    maxVUs: 200,
  },
  load: {
    executor: 'constant-arrival-rate',
    rate: 500,  // Realistic normal production load
    timeUnit: '1s',
    duration: '10m',
    preAllocatedVUs: 200,
    maxVUs: 1000,
  },
  stress: {
    executor: 'constant-arrival-rate',
    rate: 1000,  // Realistic peak production load
    timeUnit: '1s',
    duration: '5m',
    preAllocatedVUs: 400,
    maxVUs: 1500,
  },
  spike: {
    executor: 'ramping-arrival-rate',
    startRate: 200,
    timeUnit: '1s',
    stages: [
      { duration: '1m', target: 200 },
      { duration: '30s', target: 1000 },  // Spike to realistic peak
      { duration: '2m', target: 1000 },
      { duration: '30s', target: 200 },
      { duration: '1m', target: 200 },
    ],
    preAllocatedVUs: 400,
    maxVUs: 1500,
  },
};
