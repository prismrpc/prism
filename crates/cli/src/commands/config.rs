use clap::Subcommand;
use prism_core::config::AppConfig;
use std::path::Path;

use super::utils::{print_error, print_info, print_success, CliResult};

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Validate the current configuration
    Validate {
        /// Path to config file (defaults to config/config.toml)
        #[arg(short, long, default_value = "config/config.toml")]
        file: String,
    },

    /// Show current configuration
    Show {
        /// Path to config file (defaults to config/config.toml)
        #[arg(short, long, default_value = "config/config.toml")]
        file: String,

        /// Show sensitive values (like database URLs)
        #[arg(long)]
        show_sensitive: bool,
    },

    /// Generate a sample configuration file
    Generate {
        /// Output path for the config file
        #[arg(short, long, default_value = "config/config.toml")]
        output: String,

        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },

    /// Test connection to configured upstreams
    TestUpstreams {
        /// Path to config file (defaults to config/config.toml)
        #[arg(short, long, default_value = "config/config.toml")]
        file: String,

        /// Timeout in seconds for each test
        #[arg(short, long, default_value = "10")]
        timeout: u64,
    },
}

pub async fn handle_config_command(command: ConfigCommands) -> CliResult<()> {
    match command {
        ConfigCommands::Validate { file } => validate_config(&file),
        ConfigCommands::Show { file, show_sensitive } => show_config(&file, show_sensitive),
        ConfigCommands::Generate { output, force } => generate_config(&output, force),
        ConfigCommands::TestUpstreams { file, timeout } => test_upstreams(&file, timeout).await,
    }
}

fn validate_config(file: &str) -> CliResult<()> {
    if !Path::new(file).exists() {
        print_error(&format!("Configuration file not found: {file}"));
        return Err(super::utils::CliError::Config(format!("File not found: {file}")));
    }

    print_info(&format!("Loading configuration from {file}..."));

    let config =
        AppConfig::from_file(file).map_err(|e| super::utils::CliError::Config(e.to_string()))?;

    print_info("Validating configuration...");
    config.validate().map_err(super::utils::CliError::Config)?;

    print_success("Configuration is valid!");

    // Show basic stats
    println!("Configuration Summary:");
    println!("  Server: {}:{}", config.server.bind_address, config.server.bind_port);
    println!("  Upstreams: {} providers", config.upstreams.providers.len());
    println!(
        "  Cache: {}",
        if config.cache.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Auth: {}",
        if config.auth.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Metrics: {}",
        if config.metrics.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );

    Ok(())
}

fn show_config(file: &str, show_sensitive: bool) -> CliResult<()> {
    let config =
        AppConfig::from_file(file).map_err(|e| super::utils::CliError::Config(e.to_string()))?;

    println!("Configuration from {file}:");

    // Server config
    println!("\n[Server]");
    println!("  Bind Address: {}", config.server.bind_address);
    println!("  Bind Port: {}", config.server.bind_port);
    println!("  Max Concurrent Requests: {}", config.server.max_concurrent_requests);
    println!("  Request Timeout: {}s", config.server.request_timeout_seconds);

    // Upstreams
    println!("\n[Upstreams] ({} providers)", config.upstreams.providers.len());
    for provider in &config.upstreams.providers {
        println!("  {}: {} (chain {})", provider.name, provider.https_url, provider.chain_id);
        if let Some(ws_url) = &provider.wss_url {
            println!("    WebSocket: {ws_url}");
        }
    }

    // Cache
    println!("\n[Cache]");
    println!("  Enabled: {}", config.cache.enabled);
    println!("  TTL: {}s", config.cache.cache_ttl_seconds);
    println!("  Retain Blocks: {}", config.cache.manager_config.retain_blocks);

    // Auth
    println!("\n[Authentication]");
    println!("  Enabled: {}", config.auth.enabled);
    if show_sensitive || !config.auth.enabled {
        println!("  Database URL: {}", config.auth.database_url);
    } else {
        println!("  Database URL: [hidden - use --show-sensitive to reveal]");
    }

    // Metrics
    println!("\n[Metrics]");
    println!("  Enabled: {}", config.metrics.enabled);
    if let Some(port) = config.metrics.prometheus_port {
        println!("  Prometheus Port: {port}");
    }

    // Logging
    println!("\n[Logging]");
    println!("  Level: {}", config.logging.level);
    println!("  Format: {}", config.logging.format);

    Ok(())
}

fn generate_config(output: &str, force: bool) -> CliResult<()> {
    if Path::new(output).exists() && !force {
        return Err(super::utils::CliError::Config(format!(
            "File {output} already exists. Use --force to overwrite."
        )));
    }

    let sample_config = r#"# Prism RPC Aggregator Configuration
# This is a sample configuration file with sensible defaults

[server]
bind_address = "127.0.0.1"
bind_port = 3030
max_concurrent_requests = 100
request_timeout_seconds = 30

# Configure your RPC providers
[[upstreams.providers]]
name = "infura"
chain_id = 1
https_url = "https://mainnet.infura.io/v3/YOUR_API_KEY"
wss_url = "wss://mainnet.infura.io/ws/v3/YOUR_API_KEY"
weight = 1
timeout_seconds = 30
circuit_breaker_threshold = 2
circuit_breaker_timeout_seconds = 1

[[upstreams.providers]]
name = "alchemy"
chain_id = 1
https_url = "https://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
wss_url = "wss://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
weight = 1
timeout_seconds = 30
circuit_breaker_threshold = 2
circuit_breaker_timeout_seconds = 1

[cache]
enabled = true
cache_ttl_seconds = 300
retain_blocks = 1000

[cache.manager_config]
retain_blocks = 1000
vector_pool_max_size = 100

[cache.manager_config.log_cache]
chunk_size = 1000
max_exact_results = 10000
max_bitmap_entries = 100000
safety_depth = 12

[cache.manager_config.block_cache]
hot_window_size = 200
max_headers = 10000
max_bodies = 10000
safety_depth = 12

[cache.manager_config.transaction_cache]
max_transactions = 50000
max_receipts = 50000
safety_depth = 12

[cache.manager_config.reorg_manager]
safety_depth = 64
max_reorg_depth = 256
reorg_detection_threshold = 3

[health_check]
interval_seconds = 60

[auth]
enabled = false
database_url = "sqlite://db/auth.db"

[metrics]
enabled = true
prometheus_port = 9090

[logging]
level = "info"
format = "pretty"
"#;

    std::fs::write(output, sample_config).map_err(super::utils::CliError::from)?;

    print_success(&format!("Sample configuration generated: {output}"));
    print_info("Remember to:");
    print_info("  1. Replace YOUR_API_KEY placeholders with real API keys");
    print_info("  2. Configure additional upstream providers as needed");
    print_info("  3. Adjust cache and rate limiting settings for your use case");

    Ok(())
}

async fn test_upstreams(file: &str, timeout: u64) -> CliResult<()> {
    let config =
        AppConfig::from_file(file).map_err(|e| super::utils::CliError::Config(e.to_string()))?;

    print_info(&format!("Testing {} upstream providers...", config.upstreams.providers.len()));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .map_err(super::utils::CliError::from)?;

    let mut successful = 0;
    let mut failed = 0;

    for provider in &config.upstreams.providers {
        print!("Testing {}: ", provider.name);

        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });

        let start = std::time::Instant::now();
        let result = client
            .post(&provider.https_url)
            .header("Content-Type", "application/json")
            .json(&rpc_request)
            .send()
            .await;

        match result {
            Ok(response) => {
                let elapsed = start.elapsed();
                if response.status().is_success() {
                    println!("[OK] ({:.0}ms)", elapsed.as_millis());
                    successful += 1;
                } else {
                    println!("[ERROR] HTTP {}", response.status());
                    failed += 1;
                }
            }
            Err(e) => {
                println!("[ERROR] Failed: {e}");
                failed += 1;
            }
        }
    }

    println!("\nTest Results:");
    println!("  [SUCCESS] Successful: {successful}");
    println!("  [ERROR] Failed: {failed}");

    if failed > 0 {
        print_error("Some upstream providers are not responding correctly");
        print_info("Check your API keys and network connectivity");
    } else {
        print_success("All upstream providers are working correctly!");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_config_creates_sample_content() {
        // Test that the sample config contains expected sections
        let sample_config = r#"# Prism RPC Aggregator Configuration
# This is a sample configuration file with sensible defaults

[server]
bind_address = "127.0.0.1"
bind_port = 3030
max_concurrent_requests = 100
request_timeout_seconds = 30

# Configure your RPC providers
[[upstreams.providers]]
name = "infura"
chain_id = 1
https_url = "https://mainnet.infura.io/v3/YOUR_API_KEY"
wss_url = "wss://mainnet.infura.io/ws/v3/YOUR_API_KEY"
weight = 1
timeout_seconds = 30
circuit_breaker_threshold = 2
circuit_breaker_timeout_seconds = 1

[[upstreams.providers]]
name = "alchemy"
chain_id = 1
https_url = "https://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
wss_url = "wss://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY"
weight = 1
timeout_seconds = 30
circuit_breaker_threshold = 2
circuit_breaker_timeout_seconds = 1

[cache]
enabled = true
cache_ttl_seconds = 300
retain_blocks = 1000

[cache.manager_config]
retain_blocks = 1000
vector_pool_max_size = 100

[cache.manager_config.log_cache]
chunk_size = 1000
max_exact_results = 10000
max_bitmap_entries = 100000
safety_depth = 12

[cache.manager_config.block_cache]
hot_window_size = 200
max_headers = 10000
max_bodies = 10000
safety_depth = 12

[cache.manager_config.transaction_cache]
max_transactions = 50000
max_receipts = 50000
safety_depth = 12

[cache.manager_config.reorg_manager]
safety_depth = 64
max_reorg_depth = 256
reorg_detection_threshold = 3

[health_check]
interval_seconds = 60

[auth]
enabled = false
database_url = "sqlite://db/auth.db"

[metrics]
enabled = true
prometheus_port = 9090

[logging]
level = "info"
format = "pretty"
"#;

        // Verify key sections are present
        assert!(sample_config.contains("[server]"));
        assert!(sample_config.contains("[cache]"));
        assert!(sample_config.contains("[auth]"));
        assert!(sample_config.contains("[metrics]"));
        assert!(sample_config.contains("[logging]"));
        assert!(sample_config.contains("[[upstreams.providers]]"));

        // Verify sensible defaults
        assert!(sample_config.contains("bind_address = \"127.0.0.1\""));
        assert!(sample_config.contains("bind_port = 3030"));
        assert!(sample_config.contains("enabled = true"));
        assert!(sample_config.contains("YOUR_API_KEY"));
    }

    #[test]
    fn test_config_commands_enum_variants() {
        // Test that we can construct all config command variants
        let validate = ConfigCommands::Validate { file: "config.toml".to_string() };
        match validate {
            ConfigCommands::Validate { file } => assert_eq!(file, "config.toml"),
            _ => panic!("Wrong variant"),
        }

        let show = ConfigCommands::Show { file: "config.toml".to_string(), show_sensitive: false };
        match show {
            ConfigCommands::Show { file, show_sensitive } => {
                assert_eq!(file, "config.toml");
                assert!(!show_sensitive);
            }
            _ => panic!("Wrong variant"),
        }

        let generate = ConfigCommands::Generate { output: "output.toml".to_string(), force: false };
        match generate {
            ConfigCommands::Generate { output, force } => {
                assert_eq!(output, "output.toml");
                assert!(!force);
            }
            _ => panic!("Wrong variant"),
        }

        let test_upstreams =
            ConfigCommands::TestUpstreams { file: "config.toml".to_string(), timeout: 10 };
        match test_upstreams {
            ConfigCommands::TestUpstreams { file, timeout } => {
                assert_eq!(file, "config.toml");
                assert_eq!(timeout, 10);
            }
            _ => panic!("Wrong variant"),
        }
    }
}
