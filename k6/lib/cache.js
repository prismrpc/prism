// Prism k6 Load Testing - Block/Transaction Cache

import http from 'k6/http';
import { PRISM_URL, CHAIN, CONFIG } from './config.js';
import { ethGetBlockByNumber, parseBlockResponse, generateId } from './payloads.js';

const blockHashPool = [];

const txHashPool = [];

let latestBlockCache = {
  block: null,
  timestamp: 0,
};

let dynamicBlockMax = CHAIN.blockMax;

export function addBlockHash(hash) {
  if (!hash) return;
  if (blockHashPool.length >= CONFIG.MAX_BLOCK_HASH_POOL) return;
  if (!blockHashPool.includes(hash)) {
    blockHashPool.push(hash);
  }
}

export function addTxHash(hash) {
  if (!hash) return;
  if (txHashPool.length >= CONFIG.MAX_TX_HASH_POOL) return;
  if (!txHashPool.includes(hash)) {
    txHashPool.push(hash);
  }
}

export function addTxHashes(hashes) {
  if (!hashes || !Array.isArray(hashes)) return;
  for (const hash of hashes) {
    addTxHash(hash);
  }
}

export function addBlockHashes(hashes) {
  if (!hashes || !Array.isArray(hashes)) return;
  for (const hash of hashes) {
    addBlockHash(hash);
  }
}

export function getRandomBlockHash() {
  if (blockHashPool.length === 0) return null;
  const idx = Math.floor(Math.random() * blockHashPool.length);
  return blockHashPool[idx];
}

export function getRandomTxHash() {
  if (txHashPool.length === 0) return null;
  const idx = Math.floor(Math.random() * txHashPool.length);
  return txHashPool[idx];
}

export function hasBlockHashes() {
  return blockHashPool.length > 0;
}

export function hasTxHashes() {
  return txHashPool.length > 0;
}

export function getPoolSizes() {
  return {
    blockHashes: blockHashPool.length,
    txHashes: txHashPool.length,
  };
}

export function getLatestBlock(params) {
  const now = Date.now() / 1000;

  // Return cached if fresh
  if (latestBlockCache.block && (now - latestBlockCache.timestamp) < CONFIG.LATEST_BLOCK_CACHE_TTL) {
    return latestBlockCache.block;
  }

  // Fetch fresh latest block
  //
  // NOTE: This HTTP request is intentionally NOT tracked via recordResponse().
  // It's an auxiliary request used by patterns like recentBlocks() and latestLogs()
  // to determine the current chain tip before making their main request.
  // These requests:
  // - Are counted in http_reqs (k6 built-in)
  // - Are NOT included in cache_hits/cache_misses metrics
  // - Are NOT included in method latency trends
  // This keeps our metrics focused on the actual test traffic patterns rather than
  // infrastructure calls, and avoids skewing cache hit rates with 'latest' requests.
  const payload = JSON.stringify({
    jsonrpc: '2.0',
    method: 'eth_getBlockByNumber',
    params: ['latest', true], // Full transactions for tx hash extraction
    id: generateId(),
  });

  const res = http.post(PRISM_URL, payload, params);
  if (res.status !== 200) return null;

  const block = parseBlockResponse(res.body);
  if (!block) return null;

  latestBlockCache.block = block;
  latestBlockCache.timestamp = now;

  const blockNum = parseInt(block.number, 16);
  if (blockNum > dynamicBlockMax) {
    dynamicBlockMax = blockNum;
  }

  if (block.transactions && block.transactions.length > 0) {
    addTxHashes(block.transactions);
  }

  if (block.hash) {
    addBlockHash(block.hash);
  }

  return block;
}

export function getDynamicBlockMax() {
  return dynamicBlockMax;
}

export function updateBlockMax(blockNumber) {
  if (typeof blockNumber === 'string') {
    blockNumber = parseInt(blockNumber, 16);
  }
  if (blockNumber > dynamicBlockMax) {
    dynamicBlockMax = blockNumber;
  }
}

export function seedCaches(params, numBlocks = 10) {
  console.log(`Seeding caches with ${numBlocks} blocks...`);

  const latest = getLatestBlock(params);
  if (!latest) {
    console.warn('Could not get latest block for seeding');
    return;
  }

  const latestNum = parseInt(latest.number, 16);
  console.log(`Latest block: ${latestNum}`);

  for (let i = 0; i < numBlocks; i++) {
    const blockNum = Math.max(1, latestNum - i * 10); // Every 10th block
    const payload = JSON.stringify(ethGetBlockByNumber(`0x${blockNum.toString(16)}`, true));

    const res = http.post(PRISM_URL, payload, params);
    if (res.status === 200) {
      const block = parseBlockResponse(res.body);
      if (block) {
        addBlockHash(block.hash);
        addTxHashes(block.transactions);
        updateBlockMax(block.number);
      }
    }
  }

  const sizes = getPoolSizes();
  console.log(`Seeding complete: ${sizes.blockHashes} block hashes, ${sizes.txHashes} tx hashes`);
}
