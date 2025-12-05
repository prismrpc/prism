#!/bin/bash
# Prism RPC Aggregator - Full Devnet E2E Test Suite Runner
#
# This script:
# 1. Checks if devnet is running, restarts if yes, starts if no
# 2. Checks current block number - waits if too low (0 or near 0)
# 3. Starts devnet server (cargo make run-server-devnet)
# 4. Waits for server to be fully operational
# 5. Runs e2e tests (cargo make e2e-rust)
# 6. Ensures all tests pass
# 7. Stops devnet server
# 8. Stops devnet
# 9. Reports final status

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEVNET_SCRIPT="${PROJECT_ROOT}/docker/devnet/devnet.sh"
COMPOSE_FILE="${PROJECT_ROOT}/docker/devnet/docker-compose.yml"
MIN_BLOCK_NUMBER=10  # Minimum blocks needed for reorg testing
BLOCK_WAIT_TIME=90   # Wait time in seconds if blocks are too low (60-120s range)
SERVER_PORT=3030
SEALER_PORT=8545
HEALTH_CHECK_RETRIES=30
HEALTH_CHECK_INTERVAL=2

# Track status
DEVNET_STARTED=false
SERVER_STARTED=false
SERVER_PID=""
TEST_EXIT_CODE=0

# Logging functions
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

log_section() {
    echo ""
    echo -e "${CYAN}=== $1 ===${NC}"
}

# Cleanup function
cleanup() {
    log_section "Cleanup"
    
    if [ "$SERVER_STARTED" = true ] && [ -n "$SERVER_PID" ]; then
        log_info "Stopping devnet server (PID: $SERVER_PID)..."
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        log_success "Server stopped"
    fi
    
    if [ "$DEVNET_STARTED" = true ]; then
        log_info "Stopping devnet..."
        "$DEVNET_SCRIPT" stop || true
        log_success "Devnet stopped"
    fi
}

# Set trap for cleanup on exit
trap cleanup EXIT

# Check if devnet is running
check_devnet_running() {
    if docker compose -f "$COMPOSE_FILE" ps --format json 2>/dev/null | grep -q '"State":"running"'; then
        return 0
    else
        return 1
    fi
}

# Get current block number from sealer
get_block_number() {
    local result=$(curl -s -X POST "http://localhost:${SEALER_PORT}" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        --connect-timeout 2 \
        2>/dev/null || echo "")
    
    if [ -z "$result" ]; then
        echo "N/A"
        return
    fi
    
    local hex=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4 || echo "")
    if [ -z "$hex" ]; then
        echo "N/A"
        return
    fi
    
    # Convert hex to decimal
    printf "%d" "$hex" 2>/dev/null || echo "N/A"
}

# Wait for blocks if needed
wait_for_blocks() {
    log_section "Checking Block Number"
    
    local current_block=$(get_block_number)
    log_info "Current block number: $current_block"
    
    if [ "$current_block" = "N/A" ]; then
        log_warn "Could not get block number, waiting for devnet to be ready..."
        sleep 10
        current_block=$(get_block_number)
        log_info "Block number after wait: $current_block"
    fi
    
    if [ "$current_block" = "N/A" ]; then
        log_error "Failed to get block number from devnet"
        return 1
    fi
    
    if [ "$current_block" -lt "$MIN_BLOCK_NUMBER" ]; then
        log_warn "Block number ($current_block) is below safe distance ($MIN_BLOCK_NUMBER)"
        log_info "Waiting ${BLOCK_WAIT_TIME}s for blocks to be mined..."
        
        local waited=0
        while [ $waited -lt $BLOCK_WAIT_TIME ]; do
            sleep 5
            waited=$((waited + 5))
            current_block=$(get_block_number)
            if [ "$current_block" != "N/A" ] && [ "$current_block" -ge "$MIN_BLOCK_NUMBER" ]; then
                log_success "Block number reached $current_block (safe distance achieved)"
                return 0
            fi
            if [ $((waited % 15)) -eq 0 ]; then
                log_info "Still waiting... current block: $current_block (waited ${waited}s)"
            fi
        done
        
        current_block=$(get_block_number)
        log_info "Final block number after wait: $current_block"
        
        if [ "$current_block" != "N/A" ] && [ "$current_block" -ge "$MIN_BLOCK_NUMBER" ]; then
            log_success "Block number reached safe distance: $current_block"
        else
            log_warn "Block number ($current_block) still below safe distance, but proceeding..."
        fi
    else
        log_success "Block number ($current_block) is above safe distance, proceeding immediately"
    fi
}

# Check if server is healthy
check_server_health() {
    local response=$(curl -s -w "\n%{http_code}" "http://localhost:${SERVER_PORT}/health" \
        --connect-timeout 2 \
        2>/dev/null || echo "")
    
    if [ -z "$response" ]; then
        return 1
    fi
    
    local http_code=$(echo "$response" | tail -n1)
    if [ "$http_code" = "200" ]; then
        return 0
    else
        return 1
    fi
}

# Wait for server to be operational
wait_for_server() {
    log_section "Waiting for Server to be Operational"
    
    local retries=0
    while [ $retries -lt $HEALTH_CHECK_RETRIES ]; do
        if check_server_health; then
            log_success "Server is healthy and operational"
            return 0
        fi
        
        retries=$((retries + 1))
        if [ $retries -lt $HEALTH_CHECK_RETRIES ]; then
            sleep $HEALTH_CHECK_INTERVAL
            if [ $((retries % 5)) -eq 0 ]; then
                log_info "Still waiting for server... (${retries}/${HEALTH_CHECK_RETRIES})"
            fi
        fi
    done
    
    log_error "Server failed to become healthy after $((HEALTH_CHECK_RETRIES * HEALTH_CHECK_INTERVAL))s"
    return 1
}

# Main execution
main() {
    log_section "Prism Devnet E2E Test Suite"
    log_info "Starting full test suite execution..."
    
    # Step 1: Check and manage devnet
    log_section "Step 1: Managing Devnet"
    if check_devnet_running; then
        log_info "Devnet is already running, restarting..."
        "$DEVNET_SCRIPT" restart
    else
        log_info "Devnet is not running, starting..."
        "$DEVNET_SCRIPT" start
    fi
    DEVNET_STARTED=true
    log_success "Devnet is running"
    
    # Step 2: Wait for blocks if needed
    wait_for_blocks || {
        log_error "Failed to wait for blocks"
        exit 1
    }
    
    # Step 3: Start devnet server
    log_section "Step 3: Starting Devnet Server"
    log_info "Starting server with: cargo make run-server-devnet"
    
    # Start server in background
    cd "$PROJECT_ROOT"
    PRISM_CONFIG=docker/devnet/config.test.toml cargo run --bin server > /tmp/prism-server.log 2>&1 &
    SERVER_PID=$!
    SERVER_STARTED=true
    log_info "Server started with PID: $SERVER_PID"
    log_info "Server logs: /tmp/prism-server.log"
    
    # Step 4: Wait for server to be operational
    wait_for_server || {
        log_error "Server failed to start properly"
        log_info "Last 20 lines of server log:"
        tail -n 20 /tmp/prism-server.log || true
        exit 1
    }
    
    # Step 5: Run e2e tests
    log_section "Step 5: Running E2E Tests"
    log_info "Running: cargo make e2e-rust"
    
    cd "$PROJECT_ROOT"
    if cargo make e2e-rust; then
        log_success "All E2E tests passed!"
        TEST_EXIT_CODE=0
    else
        log_error "E2E tests failed!"
        TEST_EXIT_CODE=$?
    fi
    
    # Step 6: Stop server (handled by cleanup, but we can do it explicitly)
    log_section "Step 6: Stopping Server"
    if [ -n "$SERVER_PID" ]; then
        log_info "Stopping server (PID: $SERVER_PID)..."
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        log_success "Server stopped"
        SERVER_STARTED=false
    fi
    
    # Step 7: Stop devnet (handled by cleanup, but we can do it explicitly)
    log_section "Step 7: Stopping Devnet"
    log_info "Stopping devnet..."
    "$DEVNET_SCRIPT" stop
    log_success "Devnet stopped"
    DEVNET_STARTED=false
    
    # Step 8: Report final status
    log_section "Final Status Report"
    if [ $TEST_EXIT_CODE -eq 0 ]; then
        log_success " All E2E tests passed successfully!"
        echo ""
        echo -e "${GREEN}Test Suite Status: PASSED${NC}"
        exit 0
    else
        log_error "E2E tests failed with exit code: $TEST_EXIT_CODE"
        echo ""
        echo -e "${RED}Test Suite Status: FAILED${NC}"
        exit $TEST_EXIT_CODE
    fi
}

# Run main function
main "$@"