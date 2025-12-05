//! Application configuration with layered loading.
//!
//! # Configuration Hierarchy
//!
//! Configuration is loaded in this order (later overrides earlier):
//!
//! 1. **Compiled defaults**: Hardcoded in struct `Default` implementations
//! 2. **Config file**: TOML file specified by `PRISM_CONFIG` env var
//! 3. **Environment variables**: `PRISM_*` env vars override specific fields
//!
//! # Configuration Sections
//!
//! - [`ServerConfig`]: HTTP server settings (bind address, concurrency)
//! - [`UpstreamProvider`]: RPC endpoint definitions with weights and timeouts
//! - [`CacheConfig`]: Cache sizing and TTL settings
//! - [`AuthConfig`]: API key authentication settings
//! - [`MetricsConfig`]: Prometheus metrics endpoint
//! - [`LoggingConfig`]: Log level and format
//!
//! # Validation
//!
//! Configuration is validated at load time. Invalid configurations (e.g.,
//! zero cache sizes, invalid URLs) return errors rather than failing silently.
//!
//! # Example
//!
//! ```toml
//! [server]
//! bind_address = "0.0.0.0:3030"
//! max_concurrent_requests = 1000
//!
//! [[upstreams.providers]]
//! name = "infura"
//! chain_id = 1
//! https_url = "https://eth-mainnet.example.com"
//! weight = 100
//! timeout_seconds = 5
//! ```

use crate::{
    cache::CacheManagerConfig,
    types::UpstreamConfig,
    upstream::{ConsensusConfig, HedgeConfig, ScoringConfig},
};
use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc, time::Duration};

/// HTTP server configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// IP address to bind the server to. Defaults to `127.0.0.1`.
    #[serde(default = "default_bind_address")]
    pub bind_address: String,

    /// Port number to listen on. Must be greater than 0. Defaults to `3030`.
    pub bind_port: u16,

    /// Maximum number of concurrent RPC requests the server can handle. Defaults to `100`.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: usize,

    /// Request timeout in seconds. Defaults to `30`.
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}

fn default_max_concurrent_requests() -> usize {
    100
}

fn default_request_timeout_seconds() -> u64 {
    30
}

/// Configuration for a single upstream RPC provider.
///
/// Defines connection details, load balancing weight, timeouts, and circuit breaker settings
/// for an individual Ethereum RPC endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamProvider {
    /// Human-readable identifier for this provider (e.g., "infura", "alchemy").
    pub name: String,

    /// Ethereum chain ID this provider serves (e.g., `1` for mainnet, `11155111` for Sepolia).
    pub chain_id: u64,

    /// HTTPS endpoint URL. Must start with `http` or `https`.
    pub https_url: String,

    /// Optional WebSocket endpoint URL for subscriptions. Must start with `ws` or `wss` if
    /// provided.
    #[serde(default)]
    pub wss_url: Option<String>,

    /// Load balancing weight for this provider. Higher weights receive more traffic. Defaults to
    /// `1`.
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// Request timeout in seconds for this provider. Defaults to `30`.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// Number of consecutive failures before opening the circuit breaker. Defaults to `2`.
    #[serde(default = "default_circuit_breaker_threshold")]
    pub circuit_breaker_threshold: u32,

    /// Seconds to wait before attempting to close an open circuit breaker. Defaults to `1`.
    #[serde(default = "default_circuit_breaker_timeout_seconds")]
    pub circuit_breaker_timeout_seconds: u64,
}

fn default_weight() -> u32 {
    1
}

fn default_timeout_seconds() -> u64 {
    30
}

fn default_circuit_breaker_threshold() -> u32 {
    2
}

fn default_circuit_breaker_timeout_seconds() -> u64 {
    1
}

/// Container for all upstream RPC provider configurations.
///
/// Must contain at least one provider for the application to function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamsConfig {
    /// List of configured upstream RPC providers. Cannot be empty.
    pub providers: Vec<UpstreamProvider>,
}

/// Health check configuration for monitoring upstream provider availability.
///
/// Health checks use `eth_blockNumber` calls to verify upstream endpoints are responsive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// Interval between health checks in seconds. Must be greater than 0. Defaults to `60`.
    pub interval_seconds: u64,
}

/// API key authentication configuration.
///
/// When enabled, requires valid API keys stored in the configured database
/// for all incoming RPC requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Whether authentication is required. Defaults to `false`.
    pub enabled: bool,

    /// `SQLite` database URL for storing API keys. Defaults to `sqlite://./db/auth.db`.
    pub database_url: String,
}

/// Prometheus metrics collection and export configuration.
///
/// When enabled, metrics are exposed at `/metrics` on the configured port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Whether metrics collection is enabled. Defaults to `true`.
    pub enabled: bool,

    /// Port to expose Prometheus metrics endpoint. Defaults to `9090`.
    pub prometheus_port: Option<u16>,
}

/// Application logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (e.g., "trace", "debug", "info", "warn", "error"). Defaults to `"info"`.
    pub level: String,

    /// Output format: `"json"` or `"pretty"`. Defaults to `"pretty"`.
    pub format: String,
}

/// Caching system configuration.
///
/// Includes basic TTL settings and advanced caching options via [`CacheManagerConfig`].
/// Supports block caching, transaction caching, and partial-range fulfillment for `eth_getLogs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Whether caching is enabled. Defaults to `true`.
    pub enabled: bool,

    /// Default cache entry time-to-live in seconds. Must be greater than 0. Defaults to `300`.
    pub cache_ttl_seconds: u64,

    /// Advanced cache manager settings for fine-grained control.
    #[serde(default)]
    pub manager_config: CacheManagerConfig,
}

/// Root application configuration containing all subsystem settings.
///
/// This is the primary configuration structure loaded from TOML files and environment variables.
/// Configuration is loaded with the `PRISM_` prefix for environment overrides using `__` as a
/// separator.
///
/// # Example
///
/// ```toml
/// environment = "production"
///
/// [server]
/// bind_port = 8080
/// max_concurrent_requests = 200
///
/// [[upstreams.providers]]
/// name = "infura"
/// chain_id = 1
/// https_url = "https://mainnet.infura.io/v3/YOUR_KEY"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Deployment environment (e.g., "development", "production"). Defaults to `"development"`.
    #[serde(default = "default_environment")]
    pub environment: String,

    /// HTTP server configuration.
    #[serde(default)]
    pub server: ServerConfig,

    /// Upstream RPC provider configuration.
    #[serde(default)]
    pub upstreams: UpstreamsConfig,

    /// Cache system configuration.
    #[serde(default)]
    pub cache: CacheConfig,

    /// Health check configuration.
    #[serde(default)]
    pub health_check: HealthCheckConfig,

    /// API key authentication configuration.
    #[serde(default)]
    pub auth: AuthConfig,

    /// Prometheus metrics configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,

    /// Logging configuration.
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Request hedging configuration for improving tail latency.
    #[serde(default)]
    pub hedging: HedgeConfig,

    /// Multi-factor scoring configuration for upstream selection.
    #[serde(default)]
    pub scoring: ScoringConfig,

    /// Consensus and data validation configuration.
    #[serde(default)]
    pub consensus: ConsensusConfig,
}

fn default_environment() -> String {
    "development".to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_string(),
            bind_port: 3030,
            max_concurrent_requests: 100,
            request_timeout_seconds: 30,
        }
    }
}

impl Default for UpstreamsConfig {
    fn default() -> Self {
        Self {
            providers: vec![
                UpstreamProvider {
                    name: "infura".to_string(),
                    chain_id: 1,
                    https_url: "https://mainnet.infura.io/v3/YOUR_API_KEY".to_string(),
                    wss_url: None,
                    weight: 1,
                    timeout_seconds: 30,
                    circuit_breaker_threshold: 2,
                    circuit_breaker_timeout_seconds: 1,
                },
                UpstreamProvider {
                    name: "alchemy".to_string(),
                    chain_id: 1,
                    https_url: "https://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY".to_string(),
                    wss_url: None,
                    weight: 1,
                    timeout_seconds: 30,
                    circuit_breaker_threshold: 2,
                    circuit_breaker_timeout_seconds: 1,
                },
            ],
        }
    }
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self { interval_seconds: 60 }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        let project_root = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("db")
            .join("auth.db");

        Self { enabled: false, database_url: format!("sqlite://{}", project_root.display()) }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self { enabled: true, prometheus_port: Some(9090) }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".to_string(), format: "pretty".to_string() }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_ttl_seconds: 300,
            manager_config: CacheManagerConfig::default(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            environment: "development".to_string(),
            server: ServerConfig::default(),
            upstreams: UpstreamsConfig::default(),
            cache: CacheConfig::default(),
            health_check: HealthCheckConfig::default(),
            auth: AuthConfig::default(),
            metrics: MetricsConfig::default(),
            logging: LoggingConfig::default(),
            hedging: HedgeConfig::default(),
            scoring: ScoringConfig::default(),
            consensus: ConsensusConfig::default(),
        }
    }
}

impl AppConfig {
    /// Loads configuration from a TOML file with environment variable overrides.
    ///
    /// Environment variables with the `PRISM__` prefix can override any configuration value.
    /// Use `__` as a separator for nested fields (e.g., `PRISM__SERVER__BIND_PORT=8080`).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if the file cannot be read, parsed, or deserialized.
    pub fn from_file<P: AsRef<Path>>(config_path: P) -> Result<Self, ConfigError> {
        let config_builder = Config::builder()
            .set_default("environment", "development")?
            .set_default("server.bind_address", "127.0.0.1")?
            .set_default("server.bind_port", 3030)?
            .set_default("server.max_concurrent_requests", 100)?
            .set_default("server.request_timeout_seconds", 30)?
            .set_default("health_check.interval_seconds", 60)?
            .set_default("cache.enabled", true)?
            .set_default("cache.cache_ttl_seconds", 300)?
            .set_default("auth.enabled", false)?
            .set_default("metrics.enabled", true)?
            .set_default("metrics.prometheus_port", 9090)?
            .set_default("logging.level", "info")?
            .set_default("logging.format", "pretty")?
            .add_source(File::with_name(&config_path.as_ref().to_string_lossy()).required(false))
            .add_source(Environment::with_prefix("PRISM").separator("__"))
            .build()?;

        config_builder.try_deserialize()
    }

    /// Loads configuration from `config/config.toml` with fallback to defaults.
    ///
    /// The config file path can be overridden using the `PRISM_CONFIG` environment variable.
    /// Environment variable overrides are supported via the `PRISM_` prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if the configuration cannot be loaded or parsed.
    pub fn load() -> Result<Self, ConfigError> {
        let config_path =
            std::env::var("PRISM_CONFIG").unwrap_or_else(|_| "config/config.toml".to_string());
        Self::from_file(&config_path)
    }

    /// Converts upstream providers to the legacy [`UpstreamConfig`] format.
    ///
    /// This transformation is used internally to bridge between the new configuration
    /// structure and the existing upstream management code.
    #[must_use]
    pub fn to_legacy_upstreams(&self) -> Vec<UpstreamConfig> {
        self.upstreams
            .providers
            .iter()
            .map(|p| UpstreamConfig {
                url: p.https_url.clone(),
                name: Arc::from(p.name.as_str()),
                weight: p.weight,
                timeout_seconds: p.timeout_seconds,
                ws_url: p.wss_url.clone(),
                supports_websocket: p.wss_url.is_some(),
                circuit_breaker_threshold: p.circuit_breaker_threshold,
                circuit_breaker_timeout_seconds: p.circuit_breaker_timeout_seconds,
                chain_id: p.chain_id,
            })
            .collect()
    }

    /// Returns the parsed socket address for the HTTP server.
    ///
    /// Combines `server.bind_address` and `server.bind_port` into a [`SocketAddr`].
    ///
    /// # Errors
    ///
    /// Returns an error string if the address cannot be parsed into a valid [`SocketAddr`].
    ///
    /// [`SocketAddr`]: std::net::SocketAddr
    pub fn socket_addr(&self) -> Result<std::net::SocketAddr, String> {
        format!("{}:{}", self.server.bind_address, self.server.bind_port)
            .parse()
            .map_err(|_| {
                format!(
                    "Invalid socket address: {}:{}",
                    self.server.bind_address, self.server.bind_port
                )
            })
    }

    /// Returns the request timeout as a [`Duration`].
    ///
    /// Converts `server.request_timeout_seconds` to a [`Duration`] for use in timeout operations.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.server.request_timeout_seconds)
    }

    /// Returns the health check interval as a [`Duration`].
    ///
    /// Converts `health_check.interval_seconds` to a [`Duration`] for scheduling health checks.
    #[must_use]
    pub fn health_check_interval(&self) -> Duration {
        Duration::from_secs(self.health_check.interval_seconds)
    }

    /// Validates the configuration for correctness and consistency.
    ///
    /// Checks include:
    /// - At least one upstream provider is configured
    /// - All URLs are properly formatted
    /// - All numeric values are greater than zero where required
    /// - Logging format is either `"json"` or `"pretty"`
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if validation fails.
    pub fn validate(&self) -> Result<(), String> {
        if self.upstreams.providers.is_empty() {
            return Err("No upstream RPC endpoints configured".to_string());
        }

        for provider in &self.upstreams.providers {
            if provider.https_url.is_empty() {
                return Err(format!("Empty HTTPS URL for upstream: {}", provider.name));
            }
            if !provider.https_url.starts_with("http") {
                return Err(format!(
                    "Invalid HTTPS URL for upstream {}: {}",
                    provider.name, provider.https_url
                ));
            }
            if let Some(ref ws_url) = provider.wss_url {
                if !ws_url.starts_with("ws") {
                    return Err(format!(
                        "Invalid WebSocket URL for upstream {}: {}",
                        provider.name, ws_url
                    ));
                }
            }
        }

        if self.cache.cache_ttl_seconds == 0 {
            return Err("Cache TTL must be greater than 0".to_string());
        }

        if self.health_check.interval_seconds == 0 {
            return Err("Health check interval must be greater than 0".to_string());
        }

        if self.server.max_concurrent_requests == 0 {
            return Err("Max concurrent requests must be greater than 0".to_string());
        }

        if self.server.bind_port == 0 {
            return Err("Bind port must be greater than 0".to_string());
        }

        if !["json", "pretty"].contains(&self.logging.format.as_str()) {
            return Err("Logging format must be 'json' or 'pretty'".to_string());
        }

        Ok(())
    }

    /// Returns the upstream providers in legacy [`UpstreamConfig`] format.
    ///
    /// Convenience method that calls [`to_legacy_upstreams`](Self::to_legacy_upstreams).
    #[must_use]
    pub fn upstreams(&self) -> Vec<UpstreamConfig> {
        self.to_legacy_upstreams()
    }

    /// Returns the cache TTL in seconds.
    #[must_use]
    pub fn cache_ttl_seconds(&self) -> u64 {
        self.cache.cache_ttl_seconds
    }

    /// Returns the health check interval in seconds.
    #[must_use]
    pub fn health_check_interval_seconds(&self) -> u64 {
        self.health_check.interval_seconds
    }

    /// Returns the maximum number of concurrent requests allowed.
    #[must_use]
    pub fn max_concurrent_requests(&self) -> usize {
        self.server.max_concurrent_requests
    }

    /// Returns the request timeout in seconds.
    #[must_use]
    pub fn request_timeout_seconds(&self) -> u64 {
        self.server.request_timeout_seconds
    }

    /// Returns the server bind address.
    #[must_use]
    pub fn bind_address(&self) -> &str {
        &self.server.bind_address
    }

    /// Returns the server bind port.
    #[must_use]
    pub fn bind_port(&self) -> u16 {
        self.server.bind_port
    }

    /// Returns whether advanced caching is enabled.
    #[must_use]
    pub fn advanced_cache_enabled(&self) -> bool {
        self.cache.enabled
    }

    /// Returns a reference to the advanced cache configuration.
    #[must_use]
    pub fn advanced_cache_config(&self) -> &CacheManagerConfig {
        &self.cache.manager_config
    }

    /// Returns whether API key authentication is enabled.
    #[must_use]
    pub fn auth_enabled(&self) -> bool {
        self.auth.enabled
    }

    /// Returns the authentication database URL.
    #[must_use]
    pub fn auth_database_url(&self) -> &str {
        &self.auth.database_url
    }
}

// Re-export for convenience
pub use UpstreamProvider as TomlUpstreamConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = AppConfig::default();
        assert_eq!(config.environment, "development");
        assert_eq!(config.server.bind_address, "127.0.0.1");
        assert_eq!(config.server.bind_port, 3030);
        assert!(config.cache.enabled);
        assert!(config.metrics.enabled);
        assert!(!config.auth.enabled);
    }

    #[test]
    fn test_config_validation() {
        let mut config = AppConfig::default();
        assert!(config.validate().is_ok());

        // Test empty providers
        config.upstreams.providers.clear();
        assert!(config.validate().is_err());

        // Test invalid URL
        config.upstreams.providers = vec![UpstreamProvider {
            name: "test".to_string(),
            chain_id: 1,
            https_url: "invalid-url".to_string(),
            wss_url: None,
            weight: 1,
            timeout_seconds: 30,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
        }];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_legacy_conversion() {
        let config = AppConfig::default();
        let legacy_upstreams = config.to_legacy_upstreams();

        assert_eq!(legacy_upstreams.len(), 2);
        assert_eq!(legacy_upstreams[0].name.as_ref(), "infura");
        assert_eq!(legacy_upstreams[1].name.as_ref(), "alchemy");
    }

    #[test]
    fn test_toml_deserialization() {
        let toml_content = r#"
[server]
bind_port = 8080

[[upstreams.providers]]
name = "test"
chain_id = 1
https_url = "https://test.com"

[cache]
enabled = true
cache_ttl_seconds = 600
"#;

        let config: AppConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(config.server.bind_port, 8080);
        assert_eq!(config.upstreams.providers[0].name, "test");
        assert_eq!(config.cache.cache_ttl_seconds, 600);
    }
}
