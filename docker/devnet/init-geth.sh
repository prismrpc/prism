#!/bin/sh
# Geth Node Initialization Script
# This script initializes geth nodes for the private PoA devnet.

set -e

NODE_TYPE="${1:-rpc}"
DATA_DIR="/data"
GENESIS_FILE="/genesis.json"

# Sealer account (from Foundry's test mnemonic)
# Account 0: 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
SEALER_ADDRESS="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
SEALER_PRIVATE_KEY="ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

echo "=== Geth Node Initialization ==="
echo "Node type: $NODE_TYPE"
echo "Data dir: $DATA_DIR"

# Check if already initialized
if [ ! -d "$DATA_DIR/geth" ]; then
    echo "Initializing geth with genesis..."
    geth init --datadir "$DATA_DIR" "$GENESIS_FILE"
    echo "Genesis initialization complete."
else
    echo "Geth already initialized, skipping genesis init."
fi

# Setup for sealer node
if [ "$NODE_TYPE" = "sealer" ]; then
    echo "Setting up sealer node..."

    # Create password file (empty password for dev)
    echo "" > "$DATA_DIR/password.txt"

    # Import sealer account if not exists
    if [ ! -d "$DATA_DIR/keystore" ] || [ -z "$(ls -A $DATA_DIR/keystore 2>/dev/null)" ]; then
        echo "Importing sealer account..."
        echo "$SEALER_PRIVATE_KEY" > /tmp/sealer.key
        geth account import --datadir "$DATA_DIR" --password "$DATA_DIR/password.txt" /tmp/sealer.key || true
        rm /tmp/sealer.key
        echo "Sealer account imported."
    else
        echo "Sealer account already exists."
    fi

    # Get and save the enode URL for other nodes to connect
    echo "Starting geth to get enode URL..."
    geth --datadir "$DATA_DIR" --networkid 1337 --nodiscover --maxpeers 0 &
    GETH_PID=$!
    sleep 5

    # Get enode from running geth
    ENODE=$(geth attach --datadir "$DATA_DIR" --exec "admin.nodeInfo.enode" 2>/dev/null | tr -d '"' | sed 's/@.*/@172.28.0.10:30303/')

    # Kill temporary geth
    kill $GETH_PID 2>/dev/null || true
    wait $GETH_PID 2>/dev/null || true

    if [ -n "$ENODE" ]; then
        echo "Enode URL: $ENODE"
        # Save enode for other nodes (just the pubkey part)
        ENODE_PUBKEY=$(echo "$ENODE" | sed 's/enode:\/\/\([^@]*\)@.*/\1/')
        echo "$ENODE_PUBKEY" > /sealer-enode/enode
        echo "Enode pubkey saved."
    else
        echo "Warning: Could not get enode URL"
    fi

    echo "Starting sealer node..."
    exec geth \
        --networkid 1337 \
        --datadir "$DATA_DIR" \
        --http \
        --http.addr 0.0.0.0 \
        --http.port 8545 \
        --http.api eth,net,web3,txpool,debug,admin \
        --http.corsdomain '*' \
        --http.vhosts '*' \
        --ws \
        --ws.addr 0.0.0.0 \
        --ws.port 8546 \
        --ws.api eth,net,web3,txpool \
        --ws.origins '*' \
        --authrpc.port 8551 \
        --authrpc.addr 0.0.0.0 \
        --authrpc.vhosts '*' \
        --port 30303 \
        --nat extip:172.28.0.10 \
        --netrestrict 172.28.0.0/16 \
        --nodiscover \
        --syncmode full \
        --gcmode archive \
        --mine \
        --miner.etherbase "$SEALER_ADDRESS" \
        --unlock "$SEALER_ADDRESS" \
        --password "$DATA_DIR/password.txt" \
        --allow-insecure-unlock \
        --verbosity 3

# Setup for RPC nodes
else
    echo "Setting up RPC node..."

    # Wait for sealer enode to be available
    echo "Waiting for sealer enode..."
    RETRY=0
    while [ ! -f /sealer-enode/enode ] && [ $RETRY -lt 60 ]; do
        sleep 1
        RETRY=$((RETRY + 1))
    done

    if [ -f /sealer-enode/enode ]; then
        SEALER_ENODE=$(cat /sealer-enode/enode)
        echo "Sealer enode: $SEALER_ENODE"
        BOOTNODE="enode://${SEALER_ENODE}@172.28.0.10:30303"
    else
        echo "Warning: Sealer enode not found, starting without bootnode"
        BOOTNODE=""
    fi

    # Determine IP based on container
    MY_IP=$(hostname -i | awk '{print $1}')
    echo "My IP: $MY_IP"

    echo "Starting RPC node..."
    if [ -n "$BOOTNODE" ]; then
        exec geth \
            --networkid 1337 \
            --datadir "$DATA_DIR" \
            --http \
            --http.addr 0.0.0.0 \
            --http.port 8545 \
            --http.api eth,net,web3,txpool,debug \
            --http.corsdomain '*' \
            --http.vhosts '*' \
            --ws \
            --ws.addr 0.0.0.0 \
            --ws.port 8546 \
            --ws.api eth,net,web3,txpool \
            --ws.origins '*' \
            --authrpc.port 8551 \
            --authrpc.addr 0.0.0.0 \
            --authrpc.vhosts '*' \
            --port 30303 \
            --nat extip:$MY_IP \
            --netrestrict 172.28.0.0/16 \
            --bootnodes "$BOOTNODE" \
            --syncmode full \
            --gcmode archive \
            --verbosity 3
    else
        exec geth \
            --networkid 1337 \
            --datadir "$DATA_DIR" \
            --http \
            --http.addr 0.0.0.0 \
            --http.port 8545 \
            --http.api eth,net,web3,txpool,debug \
            --http.corsdomain '*' \
            --http.vhosts '*' \
            --ws \
            --ws.addr 0.0.0.0 \
            --ws.port 8546 \
            --ws.api eth,net,web3,txpool \
            --ws.origins '*' \
            --authrpc.port 8551 \
            --authrpc.addr 0.0.0.0 \
            --authrpc.vhosts '*' \
            --port 30303 \
            --nat extip:$MY_IP \
            --netrestrict 172.28.0.0/16 \
            --nodiscover \
            --syncmode full \
            --gcmode archive \
            --verbosity 3
    fi
fi
