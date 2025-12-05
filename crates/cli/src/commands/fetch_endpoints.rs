use futures::stream::{self, StreamExt};
use prettytable::{row, Table};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fmt::Write,
    fs,
    time::{Duration as StdDuration, Instant},
};
use tokio::time::timeout;
use url::Url;

#[derive(Debug, Deserialize)]
pub struct ChainlistChain {
    #[serde(rename = "chainId")]
    pub chain_id: u64,
    pub name: String,
    pub rpc: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct RpcEndpoint {
    pub url: String,
    pub tracking: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedEndpoint {
    pub chain_id: u64,
    pub chain_name: String,
    pub domain_name: String,
    pub http_url: String,
    pub ws_url: String,
    pub response_time_ms: Option<u64>,
    pub is_working: bool,
}

#[derive(Debug)]
pub struct FetchOptions {
    pub output_file: String,
    pub chain_ids: Vec<u64>,
    pub chain_name_filter: Option<String>,
    pub protocol_filter: ProtocolFilter,
    pub tracking_filter: Option<String>,
    pub run_mode: RunMode,
    pub speed_test: SpeedTest,
    pub limit: usize,
    pub timeout_secs: u64,
    pub concurrency: usize,
    pub list_chains: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ProtocolFilter {
    All,
    HttpsOnly,
    WssOnly,
}

#[derive(Debug, Clone, Copy)]
pub enum RunMode {
    Normal,
    DryRun,
}

#[derive(Debug, Clone, Copy)]
pub enum SpeedTest {
    Disabled,
    Enabled,
}

// This function is long (173 lines) but represents a single sequential CLI workflow.
// Splitting it would reduce readability by separating related steps.
#[allow(clippy::too_many_lines)]
pub async fn fetch_endpoints(options: FetchOptions) -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching RPC endpoints from Chainlist...");

    let client = reqwest::Client::new();
    let response = client.get("https://chainlist.org/rpcs.json").send().await?;

    if !response.status().is_success() {
        return Err(format!("Failed to fetch data: {}", response.status()).into());
    }

    let chains: Vec<ChainlistChain> = response.json().await?;
    println!("Found {} chains", chains.len());

    if options.list_chains {
        list_available_chains(&chains, options.chain_name_filter.as_ref());
        return Ok(());
    }

    let mut processed_endpoints = Vec::new();

    for chain in chains {
        if !options.chain_ids.is_empty() && !options.chain_ids.contains(&chain.chain_id) {
            continue;
        }

        if options.chain_ids.is_empty() {
            if let Some(ref filter) = options.chain_name_filter {
                if !chain.name.to_lowercase().contains(&filter.to_lowercase()) {
                    continue;
                }
            }
        }

        for rpc_value in chain.rpc.clone() {
            if let Some(endpoint) = extract_rpc_endpoint(&rpc_value) {
                if matches!(options.protocol_filter, ProtocolFilter::HttpsOnly) &&
                    !endpoint.url.starts_with("https://")
                {
                    continue;
                }
                if matches!(options.protocol_filter, ProtocolFilter::WssOnly) &&
                    !endpoint.url.starts_with("wss://")
                {
                    continue;
                }
                if let Some(ref tracking) = options.tracking_filter {
                    let endpoint_tracking = endpoint.tracking.as_deref().unwrap_or("none");
                    if endpoint_tracking != tracking {
                        continue;
                    }
                }

                if endpoint.url.starts_with("https://") {
                    if let Some(processed) = process_endpoint(&chain, &endpoint) {
                        processed_endpoints.push(processed);
                    }
                }
            }
        }
    }

    println!("[SUCCESS] Processed {} endpoints", processed_endpoints.len());

    if matches!(options.speed_test, SpeedTest::Enabled) {
        println!("[INFO] Testing response times...");
        processed_endpoints =
            test_endpoint_speeds(processed_endpoints, options.timeout_secs, options.concurrency)
                .await;

        processed_endpoints.sort_by(|a, b| match (a.is_working, b.is_working) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => a
                .response_time_ms
                .unwrap_or(u64::MAX)
                .cmp(&b.response_time_ms.unwrap_or(u64::MAX)),
            (false, false) => std::cmp::Ordering::Equal,
        });

        print_speed_results(&processed_endpoints);
    }

    let mut final_endpoints = handle_duplicates(processed_endpoints);

    if options.limit > 0 {
        final_endpoints = apply_chain_limits(final_endpoints, options.limit);
        println!("[INFO] Limited to {} endpoints per chain", options.limit);
    }

    let env_content = generate_env_content(&final_endpoints, false);

    if matches!(options.run_mode, RunMode::DryRun) {
        println!("{env_content}");
    } else {
        fs::write(&options.output_file, env_content)?;
        println!("[INFO] Endpoints written to: {}", options.output_file);
    }

    print_summary(&options, &final_endpoints);

    Ok(())
}

fn print_summary(options: &FetchOptions, final_endpoints: &[ProcessedEndpoint]) {
    println!("\nSummary:");
    println!("  Output file: {}", options.output_file);
    println!(
        "  Chain IDs: {}",
        if options.chain_ids.is_empty() {
            "all".to_string()
        } else {
            format!("{:?}", options.chain_ids)
        }
    );
    println!(
        "  Chain name filter: {}",
        options.chain_name_filter.as_ref().unwrap_or(&"none".to_string())
    );
    println!(
        "  Protocol filter: {}",
        match options.protocol_filter {
            ProtocolFilter::HttpsOnly => "https",
            ProtocolFilter::WssOnly => "wss",
            ProtocolFilter::All => "all",
        }
    );
    println!(
        "  Tracking filter: {}",
        options.tracking_filter.as_ref().unwrap_or(&"all".to_string())
    );
    println!(
        "  Speed testing: {}",
        if matches!(options.speed_test, SpeedTest::Enabled) {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Limit per chain: {}",
        if options.limit > 0 {
            options.limit.to_string()
        } else {
            "none".to_string()
        }
    );
    println!("  Total endpoints: {}", final_endpoints.len());

    if matches!(options.speed_test, SpeedTest::Enabled) {
        let working_count = final_endpoints.iter().filter(|ep| ep.is_working).count();
        println!("  Working endpoints: {working_count}");
    }
}

fn list_available_chains(chains: &[ChainlistChain], name_filter: Option<&String>) {
    println!("\nAvailable Chains:");
    let mut table = Table::new();
    table.add_row(row!["Chain ID", "Name", "RPC Count"]);

    let mut filtered_chains: Vec<_> = chains.iter().collect();

    if let Some(filter) = name_filter {
        filtered_chains.retain(|chain| chain.name.to_lowercase().contains(&filter.to_lowercase()));
    }

    filtered_chains.sort_by_key(|chain| chain.chain_id);

    for chain in filtered_chains.iter().take(50) {
        let rpc_count = chain.rpc.len();
        table.add_row(row![chain.chain_id, chain.name, rpc_count]);
    }

    table.printstd();

    if filtered_chains.len() > 50 {
        println!("... and {} more chains (showing first 50)", filtered_chains.len() - 50);
    }

    println!("\nCommon Chain IDs:");
    println!("  1   - Ethereum Mainnet");
    println!("  137 - Polygon");
    println!("  56  - BNB Smart Chain");
    println!("  42161 - Arbitrum One");
    println!("  10  - Optimism");
    println!("  43114 - Avalanche C-Chain");
    println!("  250 - Fantom");
    println!("  100 - Gnosis Chain");
    println!("\nUse --chain-name to filter by name, then use specific chain IDs");
}

async fn test_endpoint_speeds(
    endpoints: Vec<ProcessedEndpoint>,
    timeout_secs: u64,
    concurrency: usize,
) -> Vec<ProcessedEndpoint> {
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(timeout_secs))
        .build()
        .expect("Failed to create HTTP client");

    let total = endpoints.len();

    let results: Vec<ProcessedEndpoint> = stream::iter(endpoints)
        .map(|mut endpoint| {
            let client = client.clone();
            async move {
                let start = Instant::now();

                let rpc_request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_blockNumber",
                    "params": [],
                    "id": 1
                });

                let result = timeout(
                    StdDuration::from_secs(timeout_secs),
                    client
                        .post(&endpoint.http_url)
                        .header("Content-Type", "application/json")
                        .json(&rpc_request)
                        .send(),
                )
                .await;

                let elapsed = start.elapsed();

                match result {
                    Ok(Ok(response)) => {
                        if response.status().is_success() {
                            endpoint.response_time_ms =
                                Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
                            endpoint.is_working = true;
                        } else {
                            endpoint.is_working = false;
                        }
                    }
                    _ => {
                        endpoint.is_working = false;
                    }
                }

                endpoint
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let working_count = results.iter().filter(|ep| ep.is_working).count();
    println!("[SUCCESS] Speed testing complete: {working_count}/{total} endpoints are working");

    results
}

fn print_speed_results(endpoints: &[ProcessedEndpoint]) {
    println!("\nTop 10 Fastest Working Endpoints:");
    let mut table = Table::new();
    table.add_row(row!["Chain ID", "Chain Name", "Domain", "Response Time", "Status", "URL"]);

    let working_endpoints: Vec<_> = endpoints.iter().filter(|ep| ep.is_working).take(10).collect();

    for endpoint in working_endpoints {
        table.add_row(row![
            endpoint.chain_id,
            endpoint.chain_name,
            endpoint.domain_name,
            format!("{}ms", endpoint.response_time_ms.unwrap_or(0)),
            "[SUCCESS] Working",
            endpoint.http_url
        ]);
    }

    table.printstd();
}

fn extract_rpc_endpoint(rpc_value: &serde_json::Value) -> Option<RpcEndpoint> {
    match rpc_value {
        serde_json::Value::Object(obj) => {
            obj.get("url").and_then(|v| v.as_str()).map(|url| RpcEndpoint {
                url: url.to_string(),
                tracking: obj
                    .get("tracking")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string),
            })
        }
        serde_json::Value::String(url) => Some(RpcEndpoint { url: url.clone(), tracking: None }),
        _ => None,
    }
}

fn process_endpoint(chain: &ChainlistChain, endpoint: &RpcEndpoint) -> Option<ProcessedEndpoint> {
    let domain_name = extract_domain_name(&endpoint.url)?;
    let ws_url = endpoint.url.replace("https://", "wss://");

    Some(ProcessedEndpoint {
        chain_id: chain.chain_id,
        chain_name: chain.name.clone(),
        domain_name,
        http_url: endpoint.url.clone(),
        ws_url,
        response_time_ms: None,
        is_working: true,
    })
}

fn extract_domain_name(url: &str) -> Option<String> {
    let parsed_url = Url::parse(url).ok()?;
    let host = parsed_url.host_str()?;

    let parts: Vec<&str> = host.split('.').collect();

    if parts.len() >= 3 {
        let second_last = parts[parts.len() - 2];
        if second_last.len() <= 3 {
            parts.get(parts.len() - 3).map(std::string::ToString::to_string)
        } else {
            Some(second_last.to_string())
        }
    } else if parts.len() == 2 {
        Some(parts[0].to_string())
    } else {
        Some(host.to_string())
    }
}

fn handle_duplicates(endpoints: Vec<ProcessedEndpoint>) -> Vec<ProcessedEndpoint> {
    let mut domain_counts: HashMap<String, usize> = HashMap::new();
    let mut final_endpoints = Vec::new();

    for mut endpoint in endpoints {
        let count = domain_counts.entry(endpoint.domain_name.clone()).or_insert(0);

        if *count == 0 {
            final_endpoints.push(endpoint);
        } else {
            endpoint.domain_name = format!("{}_{}", endpoint.domain_name, count);
            final_endpoints.push(endpoint);
        }

        *count += 1;
    }

    final_endpoints
}

fn apply_chain_limits(endpoints: Vec<ProcessedEndpoint>, limit: usize) -> Vec<ProcessedEndpoint> {
    let mut chain_counts: HashMap<u64, usize> = HashMap::new();
    let mut limited_endpoints = Vec::new();

    for endpoint in endpoints {
        let count = chain_counts.entry(endpoint.chain_id).or_insert(0);

        if *count < limit {
            limited_endpoints.push(endpoint);
            *count += 1;
        }
    }

    limited_endpoints
}

fn generate_env_content(endpoints: &[ProcessedEndpoint], include_speed_info: bool) -> String {
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

    let mut content = format!(
        "# RPC Endpoints from Chainlist\n\
         # Generated on: {timestamp}\n\
         # Format: CHAIN_ID|NAME|HTTPS|WSS,CHAIN_ID|NAME2|HTTPS2|WSS2,...\n"
    );

    if include_speed_info {
        content.push_str("# Endpoints are sorted by response time (fastest first)\n");
        content.push_str("# Only working endpoints are included\n");
    }

    content.push('\n');

    if endpoints.is_empty() {
        content.push_str("RPC_UPSTREAMS=\"\"\n");
    } else {
        let mut writer = String::new();
        for (i, ep) in endpoints.iter().enumerate() {
            let entry = format!("{}|{}|{}|{}", ep.chain_id, ep.domain_name, ep.http_url, ep.ws_url);
            if include_speed_info && ep.is_working {
                if let Some(response_time) = ep.response_time_ms {
                    writeln!(&mut writer, "{entry} # {response_time}ms").unwrap();
                }
            }
            if i < endpoints.len() - 1 {
                writeln!(&mut writer).unwrap();
            }
        }
        writeln!(content, "RPC_UPSTREAMS=\"{writer}\"").unwrap();
    }

    content.push_str("\n# Example usage in your application:\n");
    content
        .push_str("# The RPC_UPSTREAMS variable contains comma-separated entries in the format:\n");
    content.push_str("# CHAIN_ID|NAME|HTTPS_URL|WSS_URL,CHAIN_ID|NAME2|HTTPS_URL2|WSS_URL2,...\n");
    content.push_str("#\n");
    content.push_str("# Each entry provides:\n");
    content.push_str("# - CHAIN_ID: The blockchain network ID\n");
    content.push_str("# - NAME: Extracted domain name (with suffixes for duplicates)\n");
    content.push_str("# - HTTPS_URL: The HTTP RPC endpoint\n");
    content.push_str("# - WSS_URL: The WebSocket RPC endpoint (auto-generated from HTTPS)\n");

    if include_speed_info {
        content.push_str("#\n");
        content.push_str("# Endpoints are ordered by response time (fastest first)\n");
        content.push_str("# Response times are included as comments for reference\n");
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for extract_domain_name
    #[test]
    fn test_extract_domain_name_simple_domain() {
        let result = extract_domain_name("https://example.com");
        assert_eq!(result, Some("example".to_string()));
    }

    #[test]
    fn test_extract_domain_name_with_subdomain() {
        let result = extract_domain_name("https://api.example.com");
        assert_eq!(result, Some("example".to_string()));
    }

    #[test]
    fn test_extract_domain_name_with_path() {
        let result = extract_domain_name("https://example.com/path/to/resource");
        assert_eq!(result, Some("example".to_string()));
    }

    #[test]
    fn test_extract_domain_name_with_port() {
        let result = extract_domain_name("https://example.com:8545");
        assert_eq!(result, Some("example".to_string()));
    }

    #[test]
    fn test_extract_domain_name_long_subdomain() {
        let result = extract_domain_name("https://mainnet.infura.io");
        assert_eq!(result, Some("infura".to_string()));
    }

    #[test]
    fn test_extract_domain_name_tld_with_country_code() {
        let result = extract_domain_name("https://example.co.uk");
        assert_eq!(result, Some("example".to_string()));
    }

    #[test]
    fn test_extract_domain_name_short_tld() {
        let result = extract_domain_name("https://api.test.io");
        assert_eq!(result, Some("test".to_string()));
    }

    #[test]
    fn test_extract_domain_name_localhost() {
        let result = extract_domain_name("https://localhost:8545");
        assert_eq!(result, Some("localhost".to_string()));
    }

    #[test]
    fn test_extract_domain_name_ip_address() {
        let result = extract_domain_name("https://192.168.1.1");
        // IP addresses are returned as-is
        assert!(result.is_some());
    }

    #[test]
    fn test_extract_domain_name_invalid_url() {
        let result = extract_domain_name("not a url");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_domain_name_wss_protocol() {
        let result = extract_domain_name("wss://example.com");
        assert_eq!(result, Some("example".to_string()));
    }

    // Tests for extract_rpc_endpoint
    #[test]
    fn test_extract_rpc_endpoint_from_object() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "tracking": "yes"
        });

        let result = extract_rpc_endpoint(&json);
        assert!(result.is_some());

        let endpoint = result.unwrap();
        assert_eq!(endpoint.url, "https://example.com");
        assert_eq!(endpoint.tracking, Some("yes".to_string()));
    }

    #[test]
    fn test_extract_rpc_endpoint_from_object_no_tracking() {
        let json = serde_json::json!({
            "url": "https://example.com"
        });

        let result = extract_rpc_endpoint(&json);
        assert!(result.is_some());

        let endpoint = result.unwrap();
        assert_eq!(endpoint.url, "https://example.com");
        assert_eq!(endpoint.tracking, None);
    }

    #[test]
    fn test_extract_rpc_endpoint_from_string() {
        let json = serde_json::json!("https://example.com");

        let result = extract_rpc_endpoint(&json);
        assert!(result.is_some());

        let endpoint = result.unwrap();
        assert_eq!(endpoint.url, "https://example.com");
        assert_eq!(endpoint.tracking, None);
    }

    #[test]
    fn test_extract_rpc_endpoint_from_invalid_type() {
        let json = serde_json::json!(123);
        let result = extract_rpc_endpoint(&json);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_rpc_endpoint_from_object_missing_url() {
        let json = serde_json::json!({
            "tracking": "yes"
        });
        let result = extract_rpc_endpoint(&json);
        assert_eq!(result, None);
    }

    // Tests for process_endpoint
    #[test]
    fn test_process_endpoint_valid_https_url() {
        let chain =
            ChainlistChain { chain_id: 1, name: "Ethereum Mainnet".to_string(), rpc: vec![] };

        let endpoint = RpcEndpoint {
            url: "https://mainnet.infura.io/v3/key".to_string(),
            tracking: Some("yes".to_string()),
        };

        let result = process_endpoint(&chain, &endpoint);
        assert!(result.is_some());

        let processed = result.unwrap();
        assert_eq!(processed.chain_id, 1);
        assert_eq!(processed.chain_name, "Ethereum Mainnet");
        assert_eq!(processed.domain_name, "infura");
        assert_eq!(processed.http_url, "https://mainnet.infura.io/v3/key");
        assert_eq!(processed.ws_url, "wss://mainnet.infura.io/v3/key");
        assert_eq!(processed.response_time_ms, None);
        assert!(processed.is_working);
    }

    #[test]
    fn test_process_endpoint_invalid_url() {
        let chain =
            ChainlistChain { chain_id: 1, name: "Ethereum Mainnet".to_string(), rpc: vec![] };

        let endpoint = RpcEndpoint { url: "not a valid url".to_string(), tracking: None };

        let result = process_endpoint(&chain, &endpoint);
        assert_eq!(result, None);
    }

    // Tests for handle_duplicates
    #[test]
    fn test_handle_duplicates_no_duplicates() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io".to_string(),
                ws_url: "wss://infura.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "alchemy".to_string(),
                http_url: "https://alchemy.com".to_string(),
                ws_url: "wss://alchemy.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = handle_duplicates(endpoints);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].domain_name, "infura");
        assert_eq!(result[1].domain_name, "alchemy");
    }

    #[test]
    fn test_handle_duplicates_with_duplicates() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io/1".to_string(),
                ws_url: "wss://infura.io/1".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io/2".to_string(),
                ws_url: "wss://infura.io/2".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = handle_duplicates(endpoints);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].domain_name, "infura");
        assert_eq!(result[1].domain_name, "infura_1");
    }

    #[test]
    fn test_handle_duplicates_multiple_duplicates() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io/1".to_string(),
                ws_url: "wss://infura.io/1".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io/2".to_string(),
                ws_url: "wss://infura.io/2".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io/3".to_string(),
                ws_url: "wss://infura.io/3".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = handle_duplicates(endpoints);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].domain_name, "infura");
        assert_eq!(result[1].domain_name, "infura_1");
        assert_eq!(result[2].domain_name, "infura_2");
    }

    #[test]
    fn test_handle_duplicates_empty() {
        let endpoints = vec![];
        let result = handle_duplicates(endpoints);
        assert_eq!(result.len(), 0);
    }

    // Tests for apply_chain_limits
    #[test]
    fn test_apply_chain_limits_no_limit() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io".to_string(),
                ws_url: "wss://infura.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "alchemy".to_string(),
                http_url: "https://alchemy.com".to_string(),
                ws_url: "wss://alchemy.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = apply_chain_limits(endpoints, 0);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_apply_chain_limits_within_limit() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io".to_string(),
                ws_url: "wss://infura.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "alchemy".to_string(),
                http_url: "https://alchemy.com".to_string(),
                ws_url: "wss://alchemy.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = apply_chain_limits(endpoints, 3);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_apply_chain_limits_exceeds_limit() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io".to_string(),
                ws_url: "wss://infura.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "alchemy".to_string(),
                http_url: "https://alchemy.com".to_string(),
                ws_url: "wss://alchemy.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "quicknode".to_string(),
                http_url: "https://quicknode.com".to_string(),
                ws_url: "wss://quicknode.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = apply_chain_limits(endpoints, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].domain_name, "infura");
        assert_eq!(result[1].domain_name, "alchemy");
    }

    #[test]
    fn test_apply_chain_limits_multiple_chains() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura1".to_string(),
                http_url: "https://infura1.io".to_string(),
                ws_url: "wss://infura1.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura2".to_string(),
                http_url: "https://infura2.io".to_string(),
                ws_url: "wss://infura2.io".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 137,
                chain_name: "Polygon".to_string(),
                domain_name: "alchemy1".to_string(),
                http_url: "https://alchemy1.com".to_string(),
                ws_url: "wss://alchemy1.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 137,
                chain_name: "Polygon".to_string(),
                domain_name: "alchemy2".to_string(),
                http_url: "https://alchemy2.com".to_string(),
                ws_url: "wss://alchemy2.com".to_string(),
                response_time_ms: None,
                is_working: true,
            },
        ];

        let result = apply_chain_limits(endpoints, 1);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].chain_id, 1);
        assert_eq!(result[1].chain_id, 137);
    }

    #[test]
    fn test_apply_chain_limits_empty() {
        let endpoints = vec![];
        let result = apply_chain_limits(endpoints, 5);
        assert_eq!(result.len(), 0);
    }

    // Tests for generate_env_content
    #[test]
    fn test_generate_env_content_empty() {
        let endpoints = vec![];
        let result = generate_env_content(&endpoints, false);

        assert!(result.contains("# RPC Endpoints from Chainlist"));
        assert!(result.contains("RPC_UPSTREAMS=\"\""));
        assert!(!result.contains("sorted by response time"));
    }

    #[test]
    fn test_generate_env_content_single_endpoint() {
        let endpoints = vec![ProcessedEndpoint {
            chain_id: 1,
            chain_name: "Ethereum".to_string(),
            domain_name: "infura".to_string(),
            http_url: "https://infura.io".to_string(),
            ws_url: "wss://infura.io".to_string(),
            response_time_ms: Some(100),
            is_working: true,
        }];

        let result = generate_env_content(&endpoints, true);

        assert!(result.contains("# RPC Endpoints from Chainlist"));
        assert!(result.contains("RPC_UPSTREAMS="));
        assert!(result.contains("1|infura|https://infura.io|wss://infura.io"));
    }

    #[test]
    fn test_generate_env_content_multiple_endpoints() {
        let endpoints = vec![
            ProcessedEndpoint {
                chain_id: 1,
                chain_name: "Ethereum".to_string(),
                domain_name: "infura".to_string(),
                http_url: "https://infura.io".to_string(),
                ws_url: "wss://infura.io".to_string(),
                response_time_ms: Some(100),
                is_working: true,
            },
            ProcessedEndpoint {
                chain_id: 137,
                chain_name: "Polygon".to_string(),
                domain_name: "alchemy".to_string(),
                http_url: "https://alchemy.com".to_string(),
                ws_url: "wss://alchemy.com".to_string(),
                response_time_ms: Some(150),
                is_working: true,
            },
        ];

        let result = generate_env_content(&endpoints, true);

        assert!(result.contains("1|infura|https://infura.io|wss://infura.io"));
        assert!(result.contains("137|alchemy|https://alchemy.com|wss://alchemy.com"));
    }

    #[test]
    fn test_generate_env_content_with_speed_info() {
        let endpoints = vec![ProcessedEndpoint {
            chain_id: 1,
            chain_name: "Ethereum".to_string(),
            domain_name: "infura".to_string(),
            http_url: "https://infura.io".to_string(),
            ws_url: "wss://infura.io".to_string(),
            response_time_ms: Some(250),
            is_working: true,
        }];

        let result = generate_env_content(&endpoints, true);

        assert!(result.contains("# Endpoints are sorted by response time"));
        assert!(result.contains("# Only working endpoints are included"));
        assert!(result.contains("# 250ms"));
    }

    #[test]
    fn test_generate_env_content_without_speed_info() {
        let endpoints = vec![ProcessedEndpoint {
            chain_id: 1,
            chain_name: "Ethereum".to_string(),
            domain_name: "infura".to_string(),
            http_url: "https://infura.io".to_string(),
            ws_url: "wss://infura.io".to_string(),
            response_time_ms: Some(250),
            is_working: true,
        }];

        let result = generate_env_content(&endpoints, false);

        assert!(!result.contains("sorted by response time"));
        assert!(!result.contains("250ms"));
    }

    #[test]
    fn test_generate_env_content_contains_usage_examples() {
        let endpoints = vec![];
        let result = generate_env_content(&endpoints, false);

        assert!(result.contains("# Example usage in your application"));
        assert!(result.contains("# - CHAIN_ID: The blockchain network ID"));
        assert!(result.contains("# - NAME: Extracted domain name"));
        assert!(result.contains("# - HTTPS_URL: The HTTP RPC endpoint"));
        assert!(result.contains("# - WSS_URL: The WebSocket RPC endpoint"));
    }
}
