// Prism k6 Load Testing - RPC Payload Builders
// Based on erpc patterns for Ethereum JSON-RPC

import { CHAIN, CONFIG } from './config.js';

// Generate unique request ID
export function generateId() {
  return Math.floor(Math.random() * 100000000);
}

// Random integer between min and max (inclusive)
export function randomIntBetween(min, max) {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

// Get random block number as hex string
export function getRandomBlock(min, max) {
  const blockNum = randomIntBetween(min || CHAIN.blockMin, max || CHAIN.blockMax);
  return `0x${blockNum.toString(16)}`;
}

// Get random block range for eth_getLogs
export function getRandomBlockRange(maxRange) {
  const rangeSize = randomIntBetween(
    CONFIG.LOG_RANGE_MIN_BLOCKS,
    maxRange || CONFIG.LOG_RANGE_MAX_BLOCKS
  );
  const maxStart = Math.max(CHAIN.blockMin, CHAIN.blockMax - rangeSize);
  const from = randomIntBetween(CHAIN.blockMin, maxStart);
  return {
    fromBlock: `0x${from.toString(16)}`,
    toBlock: `0x${(from + rangeSize).toString(16)}`,
  };
}

// Generate random Ethereum address
export function getRandomAddress() {
  const chars = '0123456789abcdef';
  let addr = '0x';
  for (let i = 0; i < 40; i++) {
    addr += chars[randomIntBetween(0, 15)];
  }
  return addr;
}

// JSON-RPC Payload Builders

export function ethBlockNumber() {
  return {
    jsonrpc: '2.0',
    method: 'eth_blockNumber',
    params: [],
    id: generateId(),
  };
}

export function ethChainId() {
  return {
    jsonrpc: '2.0',
    method: 'eth_chainId',
    params: [],
    id: generateId(),
  };
}

export function ethGasPrice() {
  return {
    jsonrpc: '2.0',
    method: 'eth_gasPrice',
    params: [],
    id: generateId(),
  };
}

export function ethGetBalance(address, blockTag = 'latest') {
  return {
    jsonrpc: '2.0',
    method: 'eth_getBalance',
    params: [address || getRandomAddress(), blockTag],
    id: generateId(),
  };
}

export function ethGetBlockByNumber(blockNumber, fullTx = false) {
  return {
    jsonrpc: '2.0',
    method: 'eth_getBlockByNumber',
    params: [blockNumber || getRandomBlock(), fullTx],
    id: generateId(),
  };
}

export function ethGetBlockByHash(blockHash, fullTx = false) {
  return {
    jsonrpc: '2.0',
    method: 'eth_getBlockByHash',
    params: [blockHash, fullTx],
    id: generateId(),
  };
}

export function ethGetLogs(fromBlock, toBlock, address = null, topics = null) {
  const filter = { fromBlock, toBlock };
  if (address) filter.address = address;
  if (topics) filter.topics = topics;

  return {
    jsonrpc: '2.0',
    method: 'eth_getLogs',
    params: [filter],
    id: generateId(),
  };
}

export function ethGetTransactionByHash(txHash) {
  return {
    jsonrpc: '2.0',
    method: 'eth_getTransactionByHash',
    params: [txHash],
    id: generateId(),
  };
}

export function ethGetTransactionReceipt(txHash) {
  return {
    jsonrpc: '2.0',
    method: 'eth_getTransactionReceipt',
    params: [txHash],
    id: generateId(),
  };
}

export function ethGetCode(address, blockTag = 'latest') {
  return {
    jsonrpc: '2.0',
    method: 'eth_getCode',
    params: [address, blockTag],
    id: generateId(),
  };
}

// Response Parsers

// Parse block response and extract useful data
export function parseBlockResponse(body) {
  try {
    const parsed = JSON.parse(body);
    if (!parsed.result) return null;

    const block = parsed.result;
    return {
      number: block.number,
      hash: block.hash,
      transactions: Array.isArray(block.transactions)
        ? block.transactions.map(tx => typeof tx === 'string' ? tx : tx.hash)
        : [],
    };
  } catch (e) {
    return null;
  }
}

// Parse logs response and extract block hashes
export function parseLogsResponse(body) {
  try {
    const parsed = JSON.parse(body);
    if (!parsed.result || !Array.isArray(parsed.result)) return null;

    const blockHashes = new Set();
    const txHashes = new Set();

    for (const log of parsed.result) {
      if (log.blockHash) blockHashes.add(log.blockHash);
      if (log.transactionHash) txHashes.add(log.transactionHash);
    }

    return {
      logCount: parsed.result.length,
      blockHashes: Array.from(blockHashes),
      txHashes: Array.from(txHashes),
    };
  } catch (e) {
    return null;
  }
}

// Check if response has JSON-RPC error
export function hasRpcError(body) {
  try {
    const parsed = JSON.parse(body);
    return parsed.error ? parsed.error : null;
  } catch (e) {
    return { code: -32700, message: 'Parse error' };
  }
}
