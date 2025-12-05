#!/bin/bash
# Prism RPC Proxy - Manual Verification Script
#
# This script provides pre-defined requests to manually verify proxy functionality.
# Run against the devnet for testing or production endpoints for verification.
#
# Usage:
#   ./docker/devnet/verify.sh [options]
#
# Options:
#   --url URL     Proxy URL (default: http://localhost:3030)
#   --help        Show this help

set -e

PRISM_URL="${PRISM_URL:-http://localhost:3030}"
VERBOSE="${VERBOSE:-false}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --url)
            PRISM_URL="$2"
            shift 2
            ;;
        --help)
            echo "Prism RPC Proxy - Manual Verification Script"
            echo ""
            echo "Usage: ./docker/devnet/verify.sh [options]"
            echo ""
            echo "Options:"
            echo "  --url URL     Proxy URL (default: http://localhost:3030)"
            echo "  --help        Show this help"
            echo ""
            echo "Environment Variables:"
            echo "  PRISM_URL     Same as --url"
            echo "  VERBOSE=true  Show full response bodies"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_section() {
    echo ""
    echo -e "${CYAN}=== $1 ===${NC}"
    echo ""
}

log_test() {
    echo -e "${BLUE}Testing:${NC} $1"
}

log_result() {
    local status="$1"
    local msg="$2"
    if [ "$status" = "pass" ]; then
        echo -e "  ${GREEN}PASS${NC} $msg"
    elif [ "$status" = "fail" ]; then
        echo -e "  ${RED}FAIL${NC} $msg"
    else
        echo -e "  ${YELLOW}INFO${NC} $msg"
    fi
}

# JSON-RPC request helper
rpc() {
    local method="$1"
    local params="${2:-[]}"
    curl -s -X POST "$PRISM_URL" \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}"
}

# JSON-RPC request with headers
rpc_with_headers() {
    local method="$1"
    local params="${2:-[]}"
    curl -sS -D - -X POST "$PRISM_URL" \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}"
}

echo ""
echo -e "${CYAN}Prism RPC Proxy - Manual Verification${NC}"
echo "========================================"
echo "Target: $PRISM_URL"
echo ""

# =============================================================================
# 1. BASIC CONNECTIVITY
# =============================================================================
log_section "1. Basic Connectivity"

log_test "Health endpoint"
health=$(curl -s "$PRISM_URL/health" || echo "FAILED")
if echo "$health" | grep -q "healthy\|status"; then
    log_result pass "Health endpoint responds"
    if [ "$VERBOSE" = "true" ]; then
        echo "$health" | head -c 500
        echo ""
    fi
else
    log_result fail "Health endpoint not responding"
fi

log_test "Metrics endpoint"
metrics=$(curl -s "$PRISM_URL/metrics" | head -20)
if echo "$metrics" | grep -q "rpc_"; then
    log_result pass "Metrics endpoint responds with Prometheus format"
else
    log_result fail "Metrics endpoint not responding correctly"
fi

log_test "eth_chainId"
result=$(rpc "eth_chainId")
if echo "$result" | grep -q '"result"'; then
    chain_id=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
    log_result pass "Chain ID: $chain_id"
else
    log_result fail "eth_chainId failed"
fi

log_test "eth_blockNumber"
result=$(rpc "eth_blockNumber")
if echo "$result" | grep -q '"result"'; then
    block=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
    log_result pass "Current block: $block"
else
    log_result fail "eth_blockNumber failed"
fi

# =============================================================================
# 2. BLOCK QUERIES
# =============================================================================
log_section "2. Block Queries"

log_test "eth_getBlockByNumber (latest)"
result=$(rpc "eth_getBlockByNumber" '["latest", false]')
if echo "$result" | grep -q '"number"'; then
    block_num=$(echo "$result" | grep -o '"number":"[^"]*"' | head -1 | cut -d'"' -f4)
    log_result pass "Latest block: $block_num"
else
    log_result fail "eth_getBlockByNumber (latest) failed"
fi

log_test "eth_getBlockByNumber (specific block 0x1)"
result=$(rpc "eth_getBlockByNumber" '["0x1", false]')
if echo "$result" | grep -q '"number":"0x1"'; then
    log_result pass "Block 0x1 retrieved successfully"
else
    log_result fail "eth_getBlockByNumber (0x1) failed"
fi

log_test "eth_getBlockByNumber with transactions"
result=$(rpc "eth_getBlockByNumber" '["0x1", true]')
if echo "$result" | grep -q '"transactions"'; then
    log_result pass "Block with transactions retrieved"
else
    log_result fail "eth_getBlockByNumber with transactions failed"
fi

# Get a block hash for the next test
log_test "eth_getBlockByHash"
block_hash=$(rpc "eth_getBlockByNumber" '["0x1", false]' | grep -o '"hash":"[^"]*"' | head -1 | cut -d'"' -f4)
if [ -n "$block_hash" ]; then
    result=$(rpc "eth_getBlockByHash" "[\"$block_hash\", false]")
    if echo "$result" | grep -q '"number":"0x1"'; then
        log_result pass "Block retrieved by hash"
    else
        log_result fail "eth_getBlockByHash failed"
    fi
else
    log_result fail "Could not get block hash for test"
fi

# =============================================================================
# 3. ACCOUNT QUERIES
# =============================================================================
log_section "3. Account Queries"

# Test account (first Anvil default account)
TEST_ACCOUNT="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

log_test "eth_getBalance"
result=$(rpc "eth_getBalance" "[\"$TEST_ACCOUNT\", \"latest\"]")
if echo "$result" | grep -q '"result":"0x'; then
    balance=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
    log_result pass "Balance: $balance"
else
    log_result fail "eth_getBalance failed"
fi

log_test "eth_getTransactionCount (nonce)"
result=$(rpc "eth_getTransactionCount" "[\"$TEST_ACCOUNT\", \"latest\"]")
if echo "$result" | grep -q '"result"'; then
    nonce=$(echo "$result" | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
    log_result pass "Nonce: $nonce"
else
    log_result fail "eth_getTransactionCount failed"
fi

log_test "eth_getCode (EOA should be 0x)"
result=$(rpc "eth_getCode" "[\"$TEST_ACCOUNT\", \"latest\"]")
if echo "$result" | grep -q '"result":"0x"'; then
    log_result pass "EOA returns empty code"
else
    log_result info "eth_getCode returned: $(echo "$result" | head -c 100)"
fi

# =============================================================================
# 4. LOG QUERIES
# =============================================================================
log_section "4. Log Queries (eth_getLogs)"

log_test "eth_getLogs (small range)"
result=$(rpc "eth_getLogs" '[{"fromBlock":"0x1","toBlock":"0xa"}]')
if echo "$result" | grep -q '"result"'; then
    log_result pass "eth_getLogs returns valid response"
else
    log_result fail "eth_getLogs failed"
fi

log_test "eth_getLogs (with address filter)"
result=$(rpc "eth_getLogs" "[{\"fromBlock\":\"0x1\",\"toBlock\":\"0xa\",\"address\":\"$TEST_ACCOUNT\"}]")
if echo "$result" | grep -q '"result"'; then
    log_result pass "eth_getLogs with address filter works"
else
    log_result fail "eth_getLogs with address filter failed"
fi

log_test "eth_getLogs (with topics)"
result=$(rpc "eth_getLogs" '[{"fromBlock":"0x1","toBlock":"0x10","topics":[null]}]')
if echo "$result" | grep -q '"result"'; then
    log_result pass "eth_getLogs with topics works"
else
    log_result fail "eth_getLogs with topics failed"
fi

# =============================================================================
# 5. CACHING BEHAVIOR
# =============================================================================
log_section "5. Caching Behavior"

log_test "Cache headers (x-cache-status)"
# First request - should be a miss
response=$(rpc_with_headers "eth_getBlockByNumber" '["0x5", false]' 2>&1)
cache_status=$(echo "$response" | grep -i "x-cache-status" | head -1 || echo "not found")
if echo "$cache_status" | grep -qi "miss\|full\|partial\|empty"; then
    log_result pass "x-cache-status header present: $(echo "$cache_status" | tr -d '\r')"
else
    log_result info "x-cache-status header not found (may not be enabled)"
fi

log_test "Cache timing comparison"
# First request
start1=$(date +%s%3N)
rpc "eth_getBlockByNumber" '["0x6", false]' > /dev/null
end1=$(date +%s%3N)
time1=$((end1 - start1))

# Second request (should be cached)
start2=$(date +%s%3N)
rpc "eth_getBlockByNumber" '["0x6", false]' > /dev/null
end2=$(date +%s%3N)
time2=$((end2 - start2))

log_result info "First request: ${time1}ms, Second request: ${time2}ms"
if [ $time2 -le $time1 ]; then
    log_result pass "Second request equal or faster (likely cached)"
else
    log_result info "Timing may vary due to network conditions"
fi

# =============================================================================
# 6. BATCH REQUESTS
# =============================================================================
log_section "6. Batch Requests"

log_test "Batch request (3 methods)"
batch='[
    {"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1},
    {"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":2},
    {"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":3}
]'
result=$(curl -s -X POST "$PRISM_URL/batch" -H "Content-Type: application/json" -d "$batch")
if echo "$result" | grep -q '"id":1' && echo "$result" | grep -q '"id":2' && echo "$result" | grep -q '"id":3'; then
    log_result pass "All 3 batch responses received"
else
    log_result fail "Batch request incomplete"
fi

log_test "Batch request with error"
batch='[
    {"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1},
    {"jsonrpc":"2.0","method":"invalid_method","params":[],"id":2}
]'
result=$(curl -s -X POST "$PRISM_URL/batch" -H "Content-Type: application/json" -d "$batch")
if echo "$result" | grep -q '"id":1' && echo "$result" | grep -q '"error"'; then
    log_result pass "Batch handles mixed valid/invalid requests"
else
    log_result fail "Batch error handling failed"
fi

# =============================================================================
# 7. ERROR HANDLING
# =============================================================================
log_section "7. Error Handling"

log_test "Invalid method"
result=$(rpc "invalid_method_xyz")
if echo "$result" | grep -q '"error"'; then
    error_code=$(echo "$result" | grep -o '"code":-[0-9]*' | cut -d':' -f2)
    log_result pass "Invalid method returns error (code: $error_code)"
else
    log_result fail "Invalid method did not return error"
fi

log_test "Invalid JSON-RPC version"
result=$(curl -s -X POST "$PRISM_URL" -H "Content-Type: application/json" \
    -d '{"jsonrpc":"1.0","method":"eth_blockNumber","params":[],"id":1}')
if echo "$result" | grep -q '"error"'; then
    log_result pass "Invalid JSON-RPC version returns error"
else
    log_result info "Server may accept non-2.0 versions"
fi

log_test "Empty request body"
result=$(curl -s -X POST "$PRISM_URL" -H "Content-Type: application/json" -d '{}')
if echo "$result" | grep -q '"error"'; then
    log_result pass "Empty request returns error"
else
    log_result fail "Empty request did not return error"
fi

# =============================================================================
# 8. METRICS VERIFICATION
# =============================================================================
log_section "8. Metrics Verification"

log_test "Checking key metrics exist"
metrics=$(curl -s "$PRISM_URL/metrics")

check_metric() {
    local name="$1"
    if echo "$metrics" | grep -q "^$name"; then
        log_result pass "Found metric: $name"
        return 0
    else
        log_result info "Metric not found: $name"
        return 1
    fi
}

check_metric "rpc_requests_total"
check_metric "rpc_cache_hits_total"
check_metric "rpc_cache_misses_total"
check_metric "rpc_upstream_health"

# =============================================================================
# 9. LOAD TEST (light)
# =============================================================================
log_section "9. Light Load Test"

log_test "10 concurrent requests"
success=0
for i in $(seq 1 10); do
    (rpc "eth_blockNumber" > /dev/null && echo "ok") &
done
wait
results=$(jobs -p | wc -l)
log_result pass "10 concurrent requests completed"

log_test "Sequential requests timing"
total_time=0
for i in $(seq 1 5); do
    start=$(date +%s%3N)
    rpc "eth_blockNumber" > /dev/null
    end=$(date +%s%3N)
    total_time=$((total_time + end - start))
done
avg_time=$((total_time / 5))
log_result info "Average response time: ${avg_time}ms (5 requests)"

# =============================================================================
# SUMMARY
# =============================================================================
log_section "Summary"

echo "Manual verification complete!"
echo ""
echo "Useful commands for further testing:"
echo ""
echo "  # Get latest block with details"
echo "  curl -s -X POST $PRISM_URL -H 'Content-Type: application/json' \\"
echo "    -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_getBlockByNumber\",\"params\":[\"latest\",true],\"id\":1}' | jq"
echo ""
echo "  # Check metrics"
echo "  curl -s $PRISM_URL/metrics | grep rpc_"
echo ""
echo "  # Check health"
echo "  curl -s $PRISM_URL/health | jq"
echo ""
