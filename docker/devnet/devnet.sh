#!/bin/bash
# Prism RPC Aggregator - Devnet Management Script
#
# This script provides utilities for managing the local Ethereum devnet
# used for testing the RPC aggregator.
#
# Usage:
#   ./docker/devnet/devnet.sh <command> [options]
#
# Commands:
#   start       Start the devnet (default: core nodes only)
#   stop        Stop all devnet containers
#   restart     Restart the devnet
#   status      Show status of all nodes
#   logs        Show logs (follow mode)
#   health      Check health of all nodes
#   reset       Stop, clean volumes, and restart
#   fund        Fund test accounts on all nodes
#   mine        Mine blocks on all nodes
#   snapshot    Create a snapshot of current state
#   revert      Revert to a snapshot
#   reorg       Trigger a chain reorganization
#   fork        Start with mainnet fork (requires RPC_FORK_URL)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.yml"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Configuration
DEFAULT_TIMEOUT=30
TEST_MNEMONIC="test test test test test test test test test test test junk"

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if docker compose is available
check_docker() {
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        exit 1
    fi

    if ! docker compose version &> /dev/null; then
        log_error "Docker Compose V2 is not available"
        exit 1
    fi
}

# Start devnet
cmd_start() {
    local profile="${1:-}"

    log_info "Starting Prism devnet..."

    if [ "$profile" = "fork" ]; then
        if [ -z "$RPC_FORK_URL" ]; then
            log_warn "RPC_FORK_URL not set, using public endpoint"
        fi
        docker compose -f "$COMPOSE_FILE" --profile fork up -d
    elif [ "$profile" = "all" ]; then
        docker compose -f "$COMPOSE_FILE" --profile fork up -d
    else
        # Start core nodes only (no fork)
        docker compose -f "$COMPOSE_FILE" up -d
    fi

    log_info "Waiting for nodes to be ready..."
    wait_for_nodes

    log_success "Devnet is running"
    cmd_status
}

# Stop devnet
cmd_stop() {
    log_info "Stopping Prism devnet..."
    docker compose -f "$COMPOSE_FILE" --profile fork down
    log_success "Devnet stopped"
}

# Restart devnet
cmd_restart() {
    cmd_stop
    sleep 2
    cmd_start "$1"
}

# Show status
cmd_status() {
    echo ""
    echo -e "${CYAN}=== Prism Devnet Status ===${NC}"
    echo ""
    docker compose -f "$COMPOSE_FILE" --profile fork ps --format "table {{.Name}}\t{{.Status}}\t{{.Ports}}"
    echo ""

    # Show block numbers
    echo -e "${CYAN}=== Node Block Numbers ===${NC}"
    echo ""

    local nodes=(
        "geth-sealer:8545"
        "geth-rpc-1:8547"
        "geth-rpc-2:8549"
        "geth-rpc-3:8551"
    )

    for node in "${nodes[@]}"; do
        local name="${node%%:*}"
        local port="${node##*:}"
        local block_num=$(get_block_number "$port" 2>/dev/null || echo "N/A")
        printf "  %-20s Block: %s\n" "$name" "$block_num"
    done
    echo ""
}

# Show logs
cmd_logs() {
    local service="${1:-}"

    if [ -n "$service" ]; then
        docker compose -f "$COMPOSE_FILE" logs -f "$service"
    else
        docker compose -f "$COMPOSE_FILE" logs -f
    fi
}

# Health check all nodes
cmd_health() {
    echo ""
    echo -e "${CYAN}=== Node Health Check ===${NC}"
    echo ""

    local nodes=(
        "geth-sealer:8545"
        "geth-rpc-1:8547"
        "geth-rpc-2:8549"
        "geth-rpc-3:8551"
    )

    for node in "${nodes[@]}"; do
        local name="${node%%:*}"
        local port="${node##*:}"
        check_node_health "$name" "$port"
    done
    echo ""
}

# Reset devnet (clean state)
cmd_reset() {
    log_info "Resetting Prism devnet..."

    docker compose -f "$COMPOSE_FILE" --profile fork down -v

    log_info "Removed volumes, starting fresh..."
    cmd_start "$1"
}

# Fund accounts (note: accounts are pre-funded in genesis)
cmd_fund() {
    local address="${1:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"
    local amount="${2:-1000}"

    log_warn "Fund command not available for Geth PoA network"
    log_info "Accounts are pre-funded in genesis.json (10,000 ETH each)"
    log_info "To send ETH, use cast or another tool with a pre-funded account"
    echo ""
    echo "Pre-funded accounts (from test mnemonic):"
    echo "  0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 (sealer)"
    echo "  0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
    echo "  0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
    echo "  ..."
}

# Mine blocks (Geth PoA mines automatically every 12 seconds)
cmd_mine() {
    local blocks="${1:-1}"

    log_warn "Manual mining not available for Geth PoA network"
    log_info "The sealer node automatically produces blocks every 12 seconds"
    log_info "Current block numbers:"

    local nodes=(
        "geth-sealer:8545"
        "geth-rpc-1:8547"
        "geth-rpc-2:8549"
        "geth-rpc-3:8551"
    )

    for node in "${nodes[@]}"; do
        local name="${node%%:*}"
        local port="${node##*:}"
        local block_num=$(get_block_number "$port" 2>/dev/null || echo "N/A")
        printf "  %-20s Block: %s\n" "$name" "$block_num"
    done
}

# Create snapshot (not available for Geth PoA)
cmd_snapshot() {
    log_warn "Snapshot not available for Geth PoA network"
    log_info "To reset the network, use: ./devnet.sh reset"
}

# Revert to snapshot (not available for Geth PoA)
cmd_revert() {
    log_warn "Revert not available for Geth PoA network"
    log_info "To reset the network, use: ./devnet.sh reset"
}

# Trigger a chain reorganization using debug_setHead
# Rolls back ALL nodes (sealer + RPC nodes) to ensure consistent reorg
cmd_reorg() {
    local depth="${1:-5}"
    
    if ! [[ "$depth" =~ ^[0-9]+$ ]] || [ "$depth" -lt 1 ]; then
        log_error "Invalid depth: $depth. Must be a positive integer."
        exit 1
    fi
    
    log_info "Triggering reorg with depth: $depth blocks"
    
    # Get current block number from sealer
    local current_block=$(get_block_number "8545")
    
    if [ "$current_block" = "N/A" ]; then
        log_error "Failed to get current block number from sealer"
        exit 1
    fi
    
    log_info "Current block number: $current_block"
    
    # Calculate target block (current - depth)
    local target_block=$((current_block - depth))
    
    if [ "$target_block" -lt 0 ]; then
        log_error "Cannot reorg: target block ($target_block) would be negative"
        exit 1
    fi
    
    log_info "Rolling back ALL nodes to block: $target_block"
    
    # Convert to hex
    local target_hex=$(printf "0x%x" "$target_block")
    
    # All node ports: sealer (8545), rpc-1 (8547), rpc-2 (8549), rpc-3 (8551)
    local nodes=(
        "geth-sealer:8545"
        "geth-rpc-1:8547"
        "geth-rpc-2:8549"
        "geth-rpc-3:8551"
    )
    
    local failed=0
    
    for node in "${nodes[@]}"; do
        local name="${node%%:*}"
        local port="${node##*:}"
        
        log_info "Rolling back $name to block $target_block..."
        
        local result=$(curl -s -X POST "http://localhost:$port" \
            -H "Content-Type: application/json" \
            -d "{\"jsonrpc\":\"2.0\",\"method\":\"debug_setHead\",\"params\":[\"$target_hex\"],\"id\":1}" \
            --connect-timeout 5 \
            2>/dev/null)
        
        if echo "$result" | grep -q '"error"'; then
            log_warn "Failed to rollback $name: $result"
            failed=$((failed + 1))
        else
            log_success "$name rolled back successfully"
        fi
    done
    
    if [ $failed -gt 0 ]; then
        log_warn "$failed node(s) failed to rollback"
    fi
    
    echo ""
    log_success "Reorg triggered: all nodes rolled back from block $current_block to block $target_block"
    log_info "The sealer will now continue mining from block $target_block, creating a new branch"
    log_info "All nodes will have the new chain with different block hashes"
    
    # Show current state
    echo ""
    log_info "Current node block numbers after reorg:"
    for node in "${nodes[@]}"; do
        local name="${node%%:*}"
        local port="${node##*:}"
        local block_num=$(get_block_number "$port" 2>/dev/null || echo "N/A")
        printf "  %-20s Block: %s\n" "$name" "$block_num"
    done
}

# Helper: Get block number
get_block_number() {
    local port="$1"
    local result=$(curl -s -X POST "http://localhost:$port" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        --connect-timeout 2 \
        2>/dev/null)

    local hex=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
    if [ -n "$hex" ]; then
        printf "%d" "$hex"
    else
        echo "N/A"
    fi
}

# Helper: Check node health
check_node_health() {
    local name="$1"
    local port="$2"

    local start_time=$(date +%s%3N)
    local result=$(curl -s -X POST "http://localhost:$port" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        --connect-timeout 3 \
        2>/dev/null)
    local end_time=$(date +%s%3N)
    local latency=$((end_time - start_time))

    if echo "$result" | grep -q '"result"'; then
        local block_num=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
        printf "  ${GREEN}[OK]${NC} %-20s Block: %-10s Latency: %dms\n" "$name" "$block_num" "$latency"
    else
        printf "  ${RED}[FAIL]${NC} %-20s (not responding)\n" "$name"
    fi
}

# Helper: Wait for nodes to be ready
wait_for_nodes() {
    local timeout="${1:-$DEFAULT_TIMEOUT}"
    local ports=(8545 8547 8549 8551)

    for port in "${ports[@]}"; do
        local elapsed=0
        while [ $elapsed -lt $timeout ]; do
            if curl -s -X POST "http://localhost:$port" \
                -H "Content-Type: application/json" \
                -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
                --connect-timeout 2 2>/dev/null | grep -q '"result"'; then
                break
            fi
            sleep 1
            elapsed=$((elapsed + 1))
        done

        if [ $elapsed -ge $timeout ]; then
            log_warn "Node on port $port did not become ready in time"
        fi
    done
}

# Show help
cmd_help() {
    echo ""
    echo -e "${CYAN}Prism RPC Aggregator - Devnet Management${NC}"
    echo ""
    echo "Usage: $0 <command> [options]"
    echo ""
    echo "Commands:"
    echo "  start [profile]   Start the devnet"
    echo "                    Profiles: default, fork, all"
    echo "  stop              Stop all devnet containers"
    echo "  restart [profile] Restart the devnet"
    echo "  status            Show status of all nodes"
    echo "  logs [service]    Show logs (follow mode)"
    echo "  health            Check health of all nodes"
    echo "  reset [profile]   Stop, clean volumes, and restart"
    echo "  fund [addr] [amt] Fund test accounts (default: first Anvil account, 1000 ETH)"
    echo "  mine [blocks]     Mine blocks on all nodes (default: 1)"
    echo "  snapshot          Create a snapshot of current state"
    echo "  revert [id]       Revert to a snapshot (default: 0x1)"
    echo "  reorg [depth]     Trigger a chain reorganization (default: 5 blocks)"
    echo "  help              Show this help message"
    echo ""
    echo "Environment Variables:"
    echo "  RPC_FORK_URL        URL for mainnet fork (required for fork profile)"
    echo "  FORK_BLOCK_NUMBER   Specific block to fork from (optional)"
    echo ""
    echo "Examples:"
    echo "  $0 start                    # Start core nodes"
    echo "  $0 start fork               # Start with mainnet fork"
    echo "  $0 mine 10                  # Mine 10 blocks"
    echo "  $0 fund 0x123... 100        # Fund address with 100 ETH"
    echo ""
    echo "Node Ports (Geth PoA):"
    echo "  8545 - geth-sealer      (block producer, weight: 3)"
    echo "  8547 - geth-rpc-1       (RPC node, weight: 2)"
    echo "  8549 - geth-rpc-2       (RPC node, weight: 2)"
    echo "  8551 - geth-rpc-3       (RPC node, weight: 2)"
    echo ""
    echo "WebSocket Ports:"
    echo "  8546 - geth-sealer WS"
    echo "  8548 - geth-rpc-1 WS"
    echo "  8550 - geth-rpc-2 WS"
    echo "  8552 - geth-rpc-3 WS"
    echo ""
}

# Main
main() {
    check_docker

    local command="${1:-help}"
    shift || true

    case "$command" in
        start)
            cmd_start "$@"
            ;;
        stop)
            cmd_stop
            ;;
        restart)
            cmd_restart "$@"
            ;;
        status)
            cmd_status
            ;;
        logs)
            cmd_logs "$@"
            ;;
        health)
            cmd_health
            ;;
        reset)
            cmd_reset "$@"
            ;;
        fund)
            cmd_fund "$@"
            ;;
        mine)
            cmd_mine "$@"
            ;;
        snapshot)
            cmd_snapshot
            ;;
        revert)
            cmd_revert "$@"
            ;;
        reorg)
            cmd_reorg "$@"
            ;;
        help|--help|-h)
            cmd_help
            ;;
        *)
            log_error "Unknown command: $command"
            cmd_help
            exit 1
            ;;
    esac
}

main "$@"
