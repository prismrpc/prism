//! Example: Complete HTTP server implementation using PrismRuntime
//!
//! This example shows how server/main.rs would look after migrating to the PrismRuntime API.
//! It demonstrates a clean, maintainable server implementation with automatic lifecycle management.
//!
//! Run with: cargo run --example server_with_runtime

#![feature(stmt_expr_attributes)]

use anyhow::Result;
use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    serve, Router,
};
use prism_core::{
    auth::repository::{CacheManagerCallback, SqliteRepository},
    cache::CacheManager,
    config::AppConfig,
    middleware::ApiKeyAuth,
    proxy::ProxyEngine,
    runtime::PrismRuntimeBuilder,
};
use rustls::crypto::{ring::default_provider, CryptoProvider};
use std::{net::SocketAddr, sync::Arc};
use tokio::signal;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::compression::CompressionLayer;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Install TLS crypto provider
    CryptoProvider::install_default(default_provider())
        .map_err(|e| anyhow::anyhow!("Failed to install crypto provider: {:?}", e))?;

    // Load configuration
    let config = AppConfig::load()
        .map_err(|e| anyhow::anyhow!("Configuration validation failed: {}", e))?;

    // Setup tracing (same as before)
    setup_tracing(&config);
    info!("Starting RPC Aggregator with PrismRuntime");

    let runtime = PrismRuntimeBuilder::new()
        .with_config(config.clone())
        .enable_health_checker()
        .enable_websocket_subscriptions()
        .build()
        .await?;

    info!("Prism runtime initialized successfully");

    let proxy_engine = runtime.proxy_engine().clone();
    let cache_manager = runtime.cache_manager().clone();

    // Create HTTP application with routes and middleware
    let app = create_app_with_security(proxy_engine, &config, cache_manager).await?;

    // Start HTTP server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.bind_port));
    info!(address = %addr, "Server listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = serve(listener, app.into_make_service_with_connect_info::<SocketAddr>());

    // Graceful shutdown on signals
    let graceful = server.with_graceful_shutdown(shutdown_signal());

    // Run server until shutdown signal
    if let Err(e) = graceful.await {
        error!(error = %e, "Server error occurred");
    }

    runtime.shutdown().await;
    info!("Server shutdown complete");

    Ok(())
}

/// Setup tracing/logging configuration
fn setup_tracing(config: &AppConfig) {
    let filter = if let Ok(env_filter) = std::env::var("RUST_LOG") {
        if env_filter == "debug" {
            EnvFilter::new("warn,prism_core=debug,server=debug,cli=debug,tests=debug")
        } else if env_filter == "trace" {
            EnvFilter::new("warn,prism_core=trace,server=trace,cli=trace,tests=trace")
        } else {
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
                EnvFilter::new("warn,prism_core=debug,server=debug,cli=debug,tests=debug")
            })
        }
    } else {
        EnvFilter::new("warn,prism_core=info,server=info,cli=info,tests=info")
    };

    let is_development = config.environment == "development";

    if is_development {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .pretty()
            .with_file(true)
            .with_line_number(true)
            .with_target(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    };
}

/// Create HTTP application with security middleware
async fn create_app_with_security(
    proxy_engine: Arc<ProxyEngine>,
    config: &AppConfig,
    cache_manager: Arc<CacheManager>,
) -> Result<Router> {
    // Public routes (no auth required)
    let public = Router::new()
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .with_state(proxy_engine.clone());

    // RPC routes
    let mut rpc = Router::new()
        .route("/", post(handle_rpc))
        .route("/batch", post(handle_batched_rpc))
        .with_state(proxy_engine.clone());

    // Add authentication if enabled
    if config.auth.enabled {
        let mut repo = SqliteRepository::new(&config.auth.database_url)
            .await
            .map_err(|e| anyhow::anyhow!("Auth repo init failed: {e}"))?;

        // Set up cache invalidation callback
        let cache_callback = Box::new(CacheManagerCallback::new(cache_manager.clone()));
        repo.set_cache_callback(cache_callback);

        let repo = Arc::new(repo);
        let api_auth = Arc::new(ApiKeyAuth::new(repo));
        rpc = rpc.layer(axum_middleware::from_fn_with_state(
            api_auth,
            api_key_middleware,
        ));
    }

    // Apply concurrency limiting
    let max_concurrent = config.server.max_concurrent_requests * 2;
    info!(
        configured_limit = config.server.max_concurrent_requests,
        applied_limit = max_concurrent,
        "Applying enhanced concurrency limit"
    );
    rpc = rpc.layer(ConcurrencyLimitLayer::new(max_concurrent));

    // Add compression
    rpc = rpc.layer(CompressionLayer::new());

    // Merge public and protected routes
    let app = public.merge(rpc);
    Ok(app)
}

/// Wait for shutdown signal (Ctrl+C or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!(error = %e, "Failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                error!(error = %e, "Failed to install signal handler");
                let _ = std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received");
}

// HTTP Handlers (placeholder implementations)

async fn handle_health() -> &'static str {
    "OK"
}

async fn handle_metrics() -> &'static str {
    "# Metrics endpoint\n"
}

async fn handle_rpc() -> &'static str {
    "RPC endpoint"
}

async fn handle_batched_rpc() -> &'static str {
    "Batch RPC endpoint"
}

async fn api_key_middleware() -> &'static str {
    "API key middleware"
}
