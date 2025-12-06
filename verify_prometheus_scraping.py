#!/usr/bin/env python3
"""
Prometheus Scraping Verification Script
This script verifies whether Prometheus is actually scraping Prism metrics
"""

import requests
import json
import sys
from datetime import datetime

PROMETHEUS_URL = "http://localhost:9090"
PRISM_METRICS_URL = "http://localhost:3030/metrics"

class Colors:
    RED = '\033[0;31m'
    GREEN = '\033[0;32m'
    YELLOW = '\033[1;33m'
    BLUE = '\033[0;34m'
    NC = '\033[0m'  # No Color

def print_header(text):
    print(f"\n{'='*60}")
    print(f"{text}")
    print(f"{'='*60}\n")

def print_success(text):
    print(f"{Colors.GREEN}✓ {text}{Colors.NC}")

def print_error(text):
    print(f"{Colors.RED}✗ {text}{Colors.NC}")

def print_warning(text):
    print(f"{Colors.YELLOW}⚠ {text}{Colors.NC}")

def print_info(text):
    print(f"{Colors.BLUE}ℹ {text}{Colors.NC}")

def check_prism_metrics():
    """Check if Prism metrics endpoint is accessible"""
    print_header("[1/5] Checking Prism metrics endpoint")

    try:
        response = requests.get(PRISM_METRICS_URL, timeout=5)
        if response.status_code == 200:
            print_success(f"Prism metrics endpoint is accessible at {PRISM_METRICS_URL}")

            # Show sample metrics
            lines = response.text.split('\n')
            print("\nSample metrics from Prism (first 20 lines):")
            for line in lines[:20]:
                if line and not line.startswith('#'):
                    print(f"  {line}")

            return True
        else:
            print_error(f"Prism metrics endpoint returned status {response.status_code}")
            return False
    except requests.exceptions.RequestException as e:
        print_error(f"Prism metrics endpoint is NOT accessible at {PRISM_METRICS_URL}")
        print(f"Error: {e}")
        print("Make sure Prism server is running on port 3030")
        return False

def check_prometheus_health():
    """Check if Prometheus is running"""
    print_header("[2/5] Checking Prometheus availability")

    try:
        response = requests.get(f"{PROMETHEUS_URL}/-/healthy", timeout=5)
        if response.status_code == 200:
            print_success(f"Prometheus is running at {PROMETHEUS_URL}")
            return True
        else:
            print_error(f"Prometheus health check returned status {response.status_code}")
            return False
    except requests.exceptions.RequestException as e:
        print_error(f"Prometheus is NOT running at {PROMETHEUS_URL}")
        print(f"Error: {e}")
        print("Start Prometheus with: docker compose -f docker/docker-compose.yml up -d prometheus")
        return False

def check_prometheus_targets():
    """Check Prometheus scrape targets"""
    print_header("[3/5] Checking Prometheus scrape targets")

    try:
        response = requests.get(f"{PROMETHEUS_URL}/api/v1/targets", timeout=5)
        if response.status_code != 200:
            print_error(f"Failed to query Prometheus targets API (status {response.status_code})")
            return None

        print_success("Successfully queried Prometheus targets API")

        data = response.json()
        active_targets = data.get('data', {}).get('activeTargets', [])

        # Find rpc-aggregator target
        rpc_agg_target = None
        for target in active_targets:
            if target.get('labels', {}).get('job') == 'rpc-aggregator':
                rpc_agg_target = target
                break

        if rpc_agg_target:
            health = rpc_agg_target.get('health', 'unknown')
            last_scrape = rpc_agg_target.get('lastScrape', 'unknown')
            scrape_url = rpc_agg_target.get('scrapeUrl', 'unknown')
            last_error = rpc_agg_target.get('lastError', '')

            print_success("Found 'rpc-aggregator' target in Prometheus")
            print(f"  - Scrape URL: {scrape_url}")
            print(f"  - Health: {health}")
            print(f"  - Last Scrape: {last_scrape}")

            if last_error:
                print(f"  - Last Error: {last_error}")

            if health == 'up':
                print_success("  Target is UP and being scraped")
                return True
            else:
                print_error("  Target is DOWN - Prometheus cannot scrape it")
                return False
        else:
            print_error("'rpc-aggregator' target NOT found in Prometheus")
            print("This means Prometheus is not configured to scrape Prism metrics")
            print("\nAvailable targets:")
            for target in active_targets:
                job = target.get('labels', {}).get('job', 'unknown')
                health = target.get('health', 'unknown')
                print(f"  - {job}: {health}")
            return False

    except requests.exceptions.RequestException as e:
        print_error("Failed to query Prometheus targets API")
        print(f"Error: {e}")
        return None
    except json.JSONDecodeError as e:
        print_error("Failed to parse Prometheus response")
        print(f"Error: {e}")
        return None

def check_prometheus_metrics():
    """Query for Prism metrics in Prometheus"""
    print_header("[4/5] Checking if Prism metrics exist in Prometheus")

    metric_query = "rpc_requests_total"

    try:
        response = requests.get(
            f"{PROMETHEUS_URL}/api/v1/query",
            params={"query": metric_query},
            timeout=5
        )

        if response.status_code != 200:
            print_error(f"Failed to query Prometheus (status {response.status_code})")
            return False

        print_success("Successfully queried Prometheus for metrics")

        data = response.json()
        results = data.get('data', {}).get('result', [])

        if len(results) > 0:
            print_success(f"CONFIRMED: Prism metrics ARE being scraped by Prometheus")
            print(f"\nFound {len(results)} series for metric '{metric_query}':")

            for i, result in enumerate(results[:10], 1):
                metric = result.get('metric', {})
                value = result.get('value', ['', '0'])
                method = metric.get('method', metric.get('__name__', 'unknown'))
                print(f"  {i}. {method}: {value[1]}")

            if len(results) > 10:
                print(f"  ... and {len(results) - 10} more")

            return True
        else:
            print_error(f"NO data found for metric '{metric_query}'")
            print("This means Prometheus is either:")
            print("  1. Not scraping Prism yet (wait a few seconds)")
            print("  2. Not configured to scrape Prism")
            print("  3. Unable to reach Prism metrics endpoint")
            return False

    except requests.exceptions.RequestException as e:
        print_error("Failed to query Prometheus")
        print(f"Error: {e}")
        return False
    except json.JSONDecodeError as e:
        print_error("Failed to parse Prometheus response")
        print(f"Error: {e}")
        return False

def check_cache_metrics():
    """Check for cache metrics"""
    print_header("[5/5] Checking for cache metrics")

    cache_metric_query = "rpc_cache_hits_total"

    try:
        response = requests.get(
            f"{PROMETHEUS_URL}/api/v1/query",
            params={"query": cache_metric_query},
            timeout=5
        )

        if response.status_code != 200:
            print_warning(f"Failed to query cache metrics (status {response.status_code})")
            return False

        data = response.json()
        results = data.get('data', {}).get('result', [])

        if len(results) > 0:
            print_success("Cache metrics found in Prometheus")
            print("\nSample cache hit metrics:")

            for i, result in enumerate(results[:5], 1):
                metric = result.get('metric', {})
                value = result.get('value', ['', '0'])
                method = metric.get('method', 'unknown')
                print(f"  {i}. {method}: {value[1]} hits")

            return True
        else:
            print_warning("No cache metrics found (this might be normal if cache hasn't been hit yet)")
            return False

    except requests.exceptions.RequestException as e:
        print_warning("Could not check cache metrics")
        return False

def print_summary(prism_ok, prometheus_ok, target_ok, metrics_ok):
    """Print final summary"""
    print_header("SUMMARY")

    if prism_ok and prometheus_ok and target_ok and metrics_ok:
        print_success("✓✓✓ SUCCESS: Prometheus IS scraping Prism metrics\n")
        print("Details:")
        print("  - Prism metrics endpoint: Working")
        print("  - Prometheus server: Running")
        print("  - Scrape target: Configured and UP")
        print("  - Metrics data: Present in Prometheus")
        print(f"\nYou can view metrics at:")
        print(f"  - Prometheus UI: {PROMETHEUS_URL}")
        print(f"  - Prism metrics: {PRISM_METRICS_URL}")
        return 0
    else:
        print_error("✗✗✗ FAILURE: Prometheus is NOT properly scraping Prism metrics\n")
        print("Troubleshooting steps:")
        if not prism_ok:
            print(f"  1. Verify Prism is running: curl {PRISM_METRICS_URL}")
        if not prometheus_ok:
            print(f"  2. Check Prometheus is running: curl {PROMETHEUS_URL}/-/healthy")
        if not target_ok:
            print("  3. Verify Prometheus config: cat deploy/prometheus/prometheus.yml")
            print("  4. Check network connectivity between Prometheus and Prism")
        if not metrics_ok:
            print("  5. Review Prometheus logs for scrape errors")
            print("  6. Wait a few seconds and try again (Prometheus scrapes every 5s)")
        return 1

def main():
    print_header("Prometheus Scraping Verification")
    print(f"Time: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n")

    # Run all checks
    prism_ok = check_prism_metrics()
    if not prism_ok:
        sys.exit(1)

    prometheus_ok = check_prometheus_health()
    if not prometheus_ok:
        sys.exit(1)

    target_ok = check_prometheus_targets()
    metrics_ok = check_prometheus_metrics()
    check_cache_metrics()

    # Print summary
    exit_code = print_summary(prism_ok, prometheus_ok, target_ok, metrics_ok)
    sys.exit(exit_code)

if __name__ == "__main__":
    main()
