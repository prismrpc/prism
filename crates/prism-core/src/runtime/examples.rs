//! Example usage patterns for the Prism runtime.
//!
//! This module contains code examples showing how to use the runtime in different scenarios.
//! These are not compiled by default but serve as documentation.

#![allow(dead_code)]
#![allow(unused_variables)]

/// Example: HTTP server using the runtime
///
/// This shows how to refactor `server/main.rs` to use the new runtime API.
#[cfg(doc)]
pub mod http_server_example {
    use prism_core::{
        config::AppConfig,
        runtime::{PrismRuntime, PrismRuntimeBuilder},
    };
    use std::sync::Arc;

    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        // Load configuration
        let config = AppConfig::load()
            .map_err(|e| format!("Configuration validation failed: {}", e))?;

        // Build runtime with all features enabled
        let runtime = PrismRuntimeBuilder::new()
            .with_config(config.clone())
            .enable_health_checker()
            .enable_websocket_subscriptions()
            .build()?;

        // Access components for HTTP server setup
        let proxy_engine = runtime.proxy_engine().clone();
        let cache_manager = runtime.cache_manager().clone();

        // Create HTTP server (using axum, actix-web, etc.)
        let app = create_http_app(proxy_engine, &config, cache_manager).await?;

        // Start server
        let addr = config.socket_addr()?;
        println!("Server listening on {}", addr);

        // Start server in background
        let server_handle = tokio::spawn(async move {
            // Server implementation...
        });

        // Wait for shutdown signal (Ctrl+C, SIGTERM, etc.)
        tokio::select! {
            _ = server_handle => {},
            _ = wait_for_shutdown_signal() => {},
        }

        // Graceful shutdown
        runtime.shutdown().await;

        Ok(())
    }

    async fn create_http_app(
        proxy: Arc<prism_core::proxy::ProxyEngine>,
        config: &AppConfig,
        cache: Arc<prism_core::cache::CacheManager>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // HTTP router setup...
        Ok(())
    }

    async fn wait_for_shutdown_signal() {
        // Signal handling...
    }
}

/// Example: Embedded usage in a trading bot
///
/// This shows how to use the runtime without HTTP server overhead.
#[cfg(doc)]
pub mod trading_bot_example {
    use prism_core::{
        config::AppConfig,
        runtime::PrismRuntimeBuilder,
        types::JsonRpcRequest,
    };

    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        // Load configuration
        let config = AppConfig::load()?;

        // Build minimal runtime (no health checker, no websockets)
        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .disable_health_checker()
            .disable_websocket_subscriptions()
            .build()?;

        // Direct access to components
        let cache = runtime.cache_manager();
        let upstream = runtime.upstream_manager();
        let proxy = runtime.proxy_engine();

        // Trading bot logic using Prism infrastructure
        loop {
            // Example: Query logs for specific events
            let request = JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                method: "eth_getLogs".to_string(),
                params: Some(serde_json::json!([{
                    "fromBlock": "0x1000000",
                    "toBlock": "latest",
                    "address": "0xContractAddress"
                }])),
                id: serde_json::json!(1),
            };

            match proxy.process_request(request).await {
                Ok(response) => {
                    // Process logs and execute trading strategy
                    println!("Received response: {:?}", response);
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                }
            }

            // Wait before next poll
            tokio::time::sleep(std::time::Duration::from_secs(12)).await;

            // Check for exit condition
            if should_exit() {
                break;
            }
        }

        // Clean shutdown
        runtime.shutdown().await;

        Ok(())
    }

    fn should_exit() -> bool {
        // Exit condition logic...
        false
    }
}

/// Example: Custom application with selective features
///
/// Shows fine-grained control over runtime initialization.
#[cfg(doc)]
pub mod custom_app_example {
    use prism_core::{
        config::AppConfig,
        runtime::PrismRuntimeBuilder,
    };

    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        let config = AppConfig::load()?;

        // Custom runtime with specific features
        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .enable_health_checker() // Monitor upstream health
            .disable_websocket_subscriptions() // No WebSocket overhead
            .disable_cache_cleanup() // Manual cache management
            .with_shutdown_channel_capacity(64) // Larger shutdown channel
            .build()?;

        // Get shutdown receiver for custom tasks
        let mut shutdown_rx = runtime.shutdown_receiver();

        // Spawn custom background task
        let cache = runtime.cache_manager().clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                        // Manual cache cleanup every 5 minutes
                        cache.perform_cleanup().await;
                    }
                    _ = shutdown_rx.recv() => {
                        println!("Custom task shutting down");
                        break;
                    }
                }
            }
        });

        // Application logic...

        // Wait for shutdown
        runtime.wait_for_shutdown().await;

        Ok(())
    }
}

/// Example: Multiple runtime instances (multi-chain support)
///
/// Shows how to run multiple Prism instances for different chains.
#[cfg(doc)]
pub mod multi_chain_example {
    use prism_core::{
        config::AppConfig,
        runtime::{PrismRuntime, PrismRuntimeBuilder},
    };
    use std::collections::HashMap;

    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        // Load configurations for different chains
        let mainnet_config = load_chain_config("mainnet")?;
        let sepolia_config = load_chain_config("sepolia")?;

        // Create runtime for each chain
        let mainnet_runtime = PrismRuntimeBuilder::new()
            .with_config(mainnet_config)
            .enable_health_checker()
            .build()?;

        let sepolia_runtime = PrismRuntimeBuilder::new()
            .with_config(sepolia_config)
            .enable_health_checker()
            .build()?;

        // Store runtimes by chain
        let mut runtimes: HashMap<String, PrismRuntime> = HashMap::new();
        runtimes.insert("mainnet".to_string(), mainnet_runtime);
        runtimes.insert("sepolia".to_string(), sepolia_runtime);

        // Route requests to appropriate runtime based on chain
        // ...

        // Shutdown all runtimes
        for (chain, runtime) in runtimes {
            println!("Shutting down {} runtime", chain);
            runtime.shutdown().await;
        }

        Ok(())
    }

    fn load_chain_config(chain: &str) -> Result<AppConfig, Box<dyn std::error::Error>> {
        // Load chain-specific configuration...
        AppConfig::load()
    }
}

/// Example: Testing with runtime
///
/// Shows how to use the runtime in integration tests.
#[cfg(doc)]
pub mod testing_example {
    use prism_core::{
        config::{AppConfig, UpstreamProvider, UpstreamsConfig},
        runtime::PrismRuntimeBuilder,
    };

    #[tokio::test]
    async fn test_with_runtime() {
        // Create test configuration
        let mut config = AppConfig::default();
        config.upstreams = UpstreamsConfig {
            providers: vec![UpstreamProvider {
                name: "test".to_string(),
                chain_id: 1,
                https_url: "https://test.example.com".to_string(),
                wss_url: None,
                weight: 1,
                timeout_seconds: 30,
                circuit_breaker_threshold: 2,
                circuit_breaker_timeout_seconds: 1,
            }],
        };

        // Build runtime for testing
        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .disable_health_checker() // Faster tests
            .disable_websocket_subscriptions() // No external connections
            .build()
            .expect("Failed to build runtime");

        // Test logic using runtime components
        let proxy = runtime.proxy_engine();
        let cache = runtime.cache_manager();

        // Assertions...

        // Cleanup
        runtime.shutdown().await;
    }
}
