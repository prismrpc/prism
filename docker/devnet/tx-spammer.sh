#!/bin/bash
# Transaction Spammer for Prism Devnet
# Sends a random mix of ETH and ERC20 transactions, ensuring at least one per block

set -e

RPC_URL="${RPC_URL:-http://geth-sealer:8545}"
PRIVATE_KEY="${PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
BLOCK_PERIOD="${BLOCK_PERIOD:-12}"

# Receiver addresses (from test mnemonic)
R1="0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
R2="0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
R3="0x90F79bf6EB2c4f870365E785982E1f101E93b906"
R4="0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65"
R5="0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc"

# Transaction counts per block (min 1, max 5)
MIN_TX_PER_BLOCK="${MIN_TX_PER_BLOCK:-1}"
MAX_TX_PER_BLOCK="${MAX_TX_PER_BLOCK:-5}"

log() {
    echo "[$(date '+%H:%M:%S')] $1"
}

# Random number generator
rand() {
    echo $(($1 + $(od -A n -t u4 -N 4 /dev/urandom | tr -d ' ') % ($2 - $1 + 1)))
}

# Get current block number
get_block_number() {
    cast block-number --rpc-url "$RPC_URL" 2>/dev/null || echo "0"
}

# Wait for RPC to be ready
wait_for_rpc() {
    log "Waiting for RPC endpoint..."
    local max_attempts=60
    local attempt=0
    
    while [ $attempt -lt $max_attempts ]; do
        if cast block-number --rpc-url "$RPC_URL" >/dev/null 2>&1; then
            log "RPC endpoint ready"
            return 0
        fi
        sleep 2
        attempt=$((attempt + 1))
    done
    
    log "ERROR: RPC endpoint not available after $max_attempts attempts"
    exit 1
}

# Deploy ERC20 contract
deploy_token() {
    log "Deploying TestToken contract..."
    
    # Copy contract files to working directory
    mkdir -p /tmp/contracts
    cp /contracts/TestToken.sol /tmp/contracts/
    cp /contracts/foundry.toml /tmp/contracts/ 2>/dev/null || {
        # Create foundry.toml if not provided
        cat > /tmp/contracts/foundry.toml << 'TOML'
[profile.default]
src = "."
out = "out"
libs = []
solc_version = "0.8.20"
evm_version = "paris"
optimizer = true
TOML
    }
    
    cd /tmp/contracts
    
    rm -rf out cache
    forge build --root . >/dev/null 2>&1
    
    log "Deploying contract..."
    DEPLOY_OUT=$(forge create --rpc-url "$RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --root . TestToken.sol:TestToken --constructor-args "1000000000000000000000000000" 2>&1) || true
    
    TOKEN=$(echo "$DEPLOY_OUT" | grep "Deployed to:" | awk '{print $3}')

    if [ -z "$TOKEN" ]; then
        log "WARNING: Failed to deploy contract, continuing with ETH only"
        TOKEN=""
    else
        log "TestToken deployed at: $TOKEN"
        echo "$TOKEN" > /tmp/token_address
    fi
    
    cd - >/dev/null
    echo "$TOKEN"
}

# Send ETH transfer
send_eth() {
    local receiver="$1"
    local amount=$(rand 1 100)
    local value=$((amount * 10000000000))  # 0.0001 ETH to 0.01 ETH
    
    if cast send --private-key "$PRIVATE_KEY" --rpc-url "$RPC_URL" "$receiver" --value "$value" --gas-limit 21000 >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Send ERC20 transfer
send_erc20() {
    local token="$1"
    local receiver="$2"
    local amount=$(rand 1 1000)
    local value="${amount}000000000000000000"  # 1 to 1000 tokens
    
    if cast send --private-key "$PRIVATE_KEY" --rpc-url "$RPC_URL" "$token" "transfer(address,uint256)" "$receiver" "$value" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Send ERC20 mint
send_mint() {
    local token="$1"
    local receiver="$2"
    local amount=$(rand 100 10000)
    local value="${amount}000000000000000000"  # 100 to 10000 tokens
    
    if cast send --private-key "$PRIVATE_KEY" --rpc-url "$RPC_URL" "$token" "mint(address,uint256)" "$receiver" "$value" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Main spam loop - tracks blocks and sends multiple txs per block
main() {
    wait_for_rpc
    
    TOKEN=$(deploy_token)
    
    log "Starting transaction spammer (block period: ${BLOCK_PERIOD}s, ${MIN_TX_PER_BLOCK}-${MAX_TX_PER_BLOCK} txs per block)..."
    
    TOTAL_ETH=0
    TOTAL_ERC20=0
    TOTAL_MINT=0
    TOTAL_BLOCKS=0
    LAST_BLOCK=$(get_block_number)
    
    while true; do
        CURRENT_BLOCK=$(get_block_number)
        
        # Wait for a new block
        if [ "$CURRENT_BLOCK" -le "$LAST_BLOCK" ]; then
            sleep 1
            continue
        fi
        
        # New block detected
        TOTAL_BLOCKS=$((TOTAL_BLOCKS + 1))
        LAST_BLOCK=$CURRENT_BLOCK
        
        # Determine how many transactions to send this block (at least 1)
        TX_COUNT=$(rand $MIN_TX_PER_BLOCK $MAX_TX_PER_BLOCK)
        
        log "Block $CURRENT_BLOCK: Sending $TX_COUNT transaction(s)..."
        
        # Send transactions for this block
        for i in $(seq 1 $TX_COUNT); do
            # Pick random receiver
            case $(rand 1 5) in
                1) RECV="$R1" ;;
                2) RECV="$R2" ;;
                3) RECV="$R3" ;;
                4) RECV="$R4" ;;
                *) RECV="$R5" ;;
            esac
            
            # Random tx type: 40% ETH, 50% ERC20 transfer, 10% mint
            TX_TYPE=$(rand 1 100)
            
            if [ $TX_TYPE -le 40 ]; then
                if send_eth "$RECV"; then
                    TOTAL_ETH=$((TOTAL_ETH + 1))
                fi
            elif [ $TX_TYPE -le 90 ] && [ -n "$TOKEN" ]; then
                if send_erc20 "$TOKEN" "$RECV"; then
                    TOTAL_ERC20=$((TOTAL_ERC20 + 1))
                fi
            elif [ -n "$TOKEN" ]; then
                if send_mint "$TOKEN" "$RECV"; then
                    TOTAL_MINT=$((TOTAL_MINT + 1))
                fi
            else
                # Fallback to ETH if token not available
                if send_eth "$RECV"; then
                    TOTAL_ETH=$((TOTAL_ETH + 1))
                fi
            fi
            
            # Small delay between transactions in the same block to avoid nonce issues
            if [ $i -lt $TX_COUNT ]; then
                sleep 0.5
            fi
        done
        
        # Log statistics every 10 blocks
        if [ $((TOTAL_BLOCKS % 10)) -eq 0 ]; then
            log "Stats after $TOTAL_BLOCKS blocks: ETH=$TOTAL_ETH ERC20=$TOTAL_ERC20 MINT=$TOTAL_MINT"
        fi
    done
}

# Handle signals for graceful shutdown
cleanup() {
    log "Shutting down..."
    exit 0
}

trap cleanup TERM INT

main "$@"