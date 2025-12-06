#!/bin/bash

# Prometheus Scraping Verification Script
# This script verifies whether Prometheus is actually scraping Prism metrics

set -e

PROMETHEUS_URL="http://localhost:9090"
PRISM_METRICS_URL="http://localhost:3030/metrics"

echo "=========================================="
echo "Prometheus Scraping Verification"
echo "=========================================="
echo ""

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test 1: Check if Prism metrics endpoint is accessible
echo "[1/5] Checking Prism metrics endpoint..."
if curl -sf "$PRISM_METRICS_URL" > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Prism metrics endpoint is accessible at $PRISM_METRICS_URL${NC}"
    echo ""
    echo "Sample metrics from Prism:"
    curl -s "$PRISM_METRICS_URL" | head -20
    echo ""
else
    echo -e "${RED}✗ Prism metrics endpoint is NOT accessible at $PRISM_METRICS_URL${NC}"
    echo "Make sure Prism server is running on port 3030"
    exit 1
fi

echo ""
echo "=========================================="

# Test 2: Check if Prometheus is running
echo "[2/5] Checking Prometheus availability..."
if curl -sf "$PROMETHEUS_URL/-/healthy" > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Prometheus is running at $PROMETHEUS_URL${NC}"
else
    echo -e "${RED}✗ Prometheus is NOT running at $PROMETHEUS_URL${NC}"
    echo "Start Prometheus with: docker compose -f docker/docker-compose.yml up -d prometheus"
    exit 1
fi

echo ""
echo "=========================================="

# Test 3: Check Prometheus targets
echo "[3/5] Checking Prometheus scrape targets..."
TARGETS_RESPONSE=$(curl -sf "$PROMETHEUS_URL/api/v1/targets" 2>&1)

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✓ Successfully queried Prometheus targets API${NC}"
    echo ""
    echo "Targets response:"
    echo "$TARGETS_RESPONSE" | jq '.' 2>/dev/null || echo "$TARGETS_RESPONSE"
    echo ""

    # Check if the rpc-aggregator job exists
    RPC_AGG_TARGET=$(echo "$TARGETS_RESPONSE" | jq -r '.data.activeTargets[] | select(.labels.job == "rpc-aggregator")' 2>/dev/null)

    if [ -n "$RPC_AGG_TARGET" ]; then
        HEALTH=$(echo "$RPC_AGG_TARGET" | jq -r '.health' 2>/dev/null)
        LAST_SCRAPE=$(echo "$RPC_AGG_TARGET" | jq -r '.lastScrape' 2>/dev/null)
        SCRAPE_URL=$(echo "$RPC_AGG_TARGET" | jq -r '.scrapeUrl' 2>/dev/null)

        echo -e "${GREEN}✓ Found 'rpc-aggregator' target in Prometheus${NC}"
        echo "  - Scrape URL: $SCRAPE_URL"
        echo "  - Health: $HEALTH"
        echo "  - Last Scrape: $LAST_SCRAPE"

        if [ "$HEALTH" = "up" ]; then
            echo -e "${GREEN}  - Status: Target is UP and being scraped${NC}"
        else
            echo -e "${RED}  - Status: Target is DOWN - Prometheus cannot scrape it${NC}"
        fi
    else
        echo -e "${RED}✗ 'rpc-aggregator' target NOT found in Prometheus${NC}"
        echo "This means Prometheus is not configured to scrape Prism metrics"
    fi
else
    echo -e "${RED}✗ Failed to query Prometheus targets API${NC}"
    echo "$TARGETS_RESPONSE"
fi

echo ""
echo "=========================================="

# Test 4: Query for Prism metrics in Prometheus
echo "[4/5] Checking if Prism metrics exist in Prometheus..."

# Try to query for rpc_requests_total metric
METRIC_QUERY="rpc_requests_total"
QUERY_RESPONSE=$(curl -sf "$PROMETHEUS_URL/api/v1/query?query=$METRIC_QUERY" 2>&1)

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✓ Successfully queried Prometheus for metrics${NC}"
    echo ""

    RESULT_COUNT=$(echo "$QUERY_RESPONSE" | jq -r '.data.result | length' 2>/dev/null)

    if [ "$RESULT_COUNT" -gt 0 ]; then
        echo -e "${GREEN}✓ CONFIRMED: Prism metrics ARE being scraped by Prometheus${NC}"
        echo ""
        echo "Found $RESULT_COUNT series for metric '$METRIC_QUERY':"
        echo "$QUERY_RESPONSE" | jq -r '.data.result[] | "\(.metric.method // .metric.__name__): \(.value[1])"' 2>/dev/null | head -10
    else
        echo -e "${RED}✗ NO data found for metric '$METRIC_QUERY'${NC}"
        echo "This means Prometheus is either:"
        echo "  1. Not scraping Prism yet (wait a few seconds)"
        echo "  2. Not configured to scrape Prism"
        echo "  3. Unable to reach Prism metrics endpoint"
    fi
else
    echo -e "${RED}✗ Failed to query Prometheus${NC}"
    echo "$QUERY_RESPONSE"
fi

echo ""
echo "=========================================="

# Test 5: Check for other Prism metrics
echo "[5/5] Checking for cache metrics..."
CACHE_METRIC_QUERY="rpc_cache_hits_total"
CACHE_QUERY_RESPONSE=$(curl -sf "$PROMETHEUS_URL/api/v1/query?query=$CACHE_METRIC_QUERY" 2>&1)

if [ $? -eq 0 ]; then
    CACHE_RESULT_COUNT=$(echo "$CACHE_QUERY_RESPONSE" | jq -r '.data.result | length' 2>/dev/null)

    if [ "$CACHE_RESULT_COUNT" -gt 0 ]; then
        echo -e "${GREEN}✓ Cache metrics found in Prometheus${NC}"
        echo ""
        echo "Sample cache hit metrics:"
        echo "$CACHE_QUERY_RESPONSE" | jq -r '.data.result[] | "\(.metric.method): \(.value[1]) hits"' 2>/dev/null | head -5
    else
        echo -e "${YELLOW}⚠ No cache metrics found (this might be normal if cache hasn't been hit yet)${NC}"
    fi
fi

echo ""
echo "=========================================="
echo "SUMMARY"
echo "=========================================="
echo ""

# Final verdict
if [ "$HEALTH" = "up" ] && [ "$RESULT_COUNT" -gt 0 ]; then
    echo -e "${GREEN}✓✓✓ SUCCESS: Prometheus IS scraping Prism metrics${NC}"
    echo ""
    echo "Details:"
    echo "  - Prism metrics endpoint: Working"
    echo "  - Prometheus server: Running"
    echo "  - Scrape target: Configured and UP"
    echo "  - Metrics data: Present in Prometheus"
    echo ""
    echo "You can view metrics at:"
    echo "  - Prometheus UI: $PROMETHEUS_URL"
    echo "  - Prism metrics: $PRISM_METRICS_URL"
else
    echo -e "${RED}✗✗✗ FAILURE: Prometheus is NOT properly scraping Prism metrics${NC}"
    echo ""
    echo "Troubleshooting steps:"
    echo "  1. Verify Prism is running: curl $PRISM_METRICS_URL"
    echo "  2. Check Prometheus is running: curl $PROMETHEUS_URL/-/healthy"
    echo "  3. Verify Prometheus config: cat deploy/prometheus/prometheus.yml"
    echo "  4. Check network connectivity between Prometheus and Prism"
    echo "  5. Review Prometheus logs for scrape errors"
fi

echo ""
