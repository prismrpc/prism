# Prism Devnet - Private PoA Ethereum Network

This directory contains Docker Compose configuration for running a private Ethereum network using Geth with Clique PoA consensus. All nodes sync from the same chain, ensuring **consistent data across all upstreams**.

## Quick Start

```bash
# Start the devnet
docker compose -f docker/devnet/docker-compose.yml up -d

# Check node status
docker compose -f docker/devnet/docker-compose.yml ps

# Run Prism with devnet config
PRISM_CONFIG=docker/devnet/config.test.toml cargo run --bin server

# Clean reset (removes all data)
docker compose -f docker/devnet/docker-compose.yml down -v
```

## Architecture

```
┌─────────────────────┐
│    geth-sealer      │  ← Block producer (PoA validator)
│    (172.28.0.10)    │     Port 8545 (HTTP), 8546 (WS)
└──────────┬──────────┘
           │ P2P sync (blocks propagate to all nodes)
     ┌─────┴─────┬─────────────┐
     ▼           ▼             ▼
┌─────────┐ ┌─────────┐ ┌─────────┐
│geth-rpc-1│ │geth-rpc-2│ │geth-rpc-3│  ← RPC nodes (sync from sealer)
│  :8547   │ │  :8549   │ │  :8551   │     All have identical blockchain state
└─────────┘ └─────────┘ └─────────┘
```

| Node | HTTP Port | WS Port | Purpose |
|------|-----------|---------|---------|
| geth-sealer | 8545 | 8546 | Block producer (PoA validator) |
| geth-rpc-1 | 8547 | 8548 | RPC node (syncs from sealer) |
| geth-rpc-2 | 8549 | 8550 | RPC node (syncs from sealer) |
| geth-rpc-3 | 8551 | 8552 | RPC node (syncs from sealer) |

## Chain Configuration

- **Chain ID**: 1337
- **Consensus**: Clique PoA (Proof of Authority)
- **Block Time**: 12 seconds (matches mainnet)
- **Gas Limit**: 30,000,000

### Pre-funded Accounts

These accounts are pre-funded with 10,000 ETH each (from Foundry's test mnemonic):

| Index | Address | Private Key |
|-------|---------|-------------|
| 0 (Sealer) | 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 | 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 |
| 1 | 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 | 0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d |
| 2 | 0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC | 0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a |
| 3 | 0x90F79bf6EB2c4f870365E785982E1f101E93b906 | ... |
| ... | ... | ... |

## Why This Setup?

### Problem with Independent Anvil Nodes
Previously, each Anvil node ran as a separate isolated blockchain. This meant:
- Different block numbers on each node
- Different transactions/logs on each node
- Inconsistent cache behavior when load balancing

### Solution: Proper P2P Network
With Geth PoA:
- **One sealer** produces blocks
- **All RPC nodes** sync those blocks via P2P
- **Every node has identical data** at any given block height
- Cache hits/misses are deterministic across upstreams

## Commands

### Basic Operations

```bash
# Start devnet
docker compose -f docker/devnet/docker-compose.yml up -d

# View logs
docker compose -f docker/devnet/docker-compose.yml logs -f

# View specific node logs
docker logs prism-geth-sealer -f
docker logs prism-geth-rpc-1 -f

# Stop devnet (preserves data)
docker compose -f docker/devnet/docker-compose.yml down

# Stop and remove all data (clean reset)
docker compose -f docker/devnet/docker-compose.yml down -v
```

### Transaction Spammer (Optional)

To generate continuous transactions:

```bash
# Start with tx-spammer
docker compose -f docker/devnet/docker-compose.yml --profile spammer up -d
```

### Testing

```bash
# Check block number (should be same on all nodes)
curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

curl -s http://localhost:8547 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Check chain ID (should be 0x539 = 1337)
curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'

# Check peer count on sealer (should have 3 peers)
curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'
```

## Testing Scenarios

### Cache Consistency

Since all nodes have identical data, cache behavior is deterministic:

```bash
# Query block on node 1, then node 2 - should return same data
curl -s http://localhost:8547 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x1",false],"id":1}'

curl -s http://localhost:8549 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x1",false],"id":1}'
```

### Failover

```bash
# Stop an RPC node
docker stop prism-geth-rpc-1

# Requests should continue via other nodes
curl -s http://localhost:3030 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Restart node
docker start prism-geth-rpc-1
```

### Load Balancing

With consistent data across nodes, load balancing returns identical results regardless of which upstream handles the request.

## Configuration Files

- `docker-compose.yml` - Docker Compose service definitions
- `genesis.json` - Chain genesis configuration (Clique PoA)
- `init-geth.sh` - Node initialization script
- `config.test.toml` - Prism configuration for devnet

## Troubleshooting

### Nodes not syncing

Check if sealer is producing blocks:
```bash
docker logs prism-geth-sealer --tail 20
```

Check peer connections:
```bash
curl -s http://localhost:8547 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"admin_peers","params":[],"id":1}'
```

### Clean Reset

If nodes get out of sync or corrupted:
```bash
docker compose -f docker/devnet/docker-compose.yml down -v
docker compose -f docker/devnet/docker-compose.yml up -d
```

### Slow Startup

First startup takes ~30-60 seconds for:
1. Genesis initialization
2. Account import (sealer)
3. P2P peer discovery
4. Initial block sync

Subsequent startups are faster as data is persisted in Docker volumes.
