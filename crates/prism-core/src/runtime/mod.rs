//! Prism runtime initialization and lifecycle management.
//!
//! This module provides a unified initialization API for all Prism core components,
//! suitable for both HTTP server deployments and embedded use cases (trading bots,
//! custom applications). It manages component lifecycles, background tasks, and
//! graceful shutdown coordination.
//!
//! # Examples
//!
//! ## Basic Server Usage
//!
//! ```no_run
//! use prism_core::{config::AppConfig, runtime::PrismRuntime};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = AppConfig::load()?;
//!
//!     let runtime = PrismRuntime::builder()
//!         .with_config(config)
//!         .enable_health_checker()
//!         .enable_websocket_subscriptions()
//!         .build()?;
//!
//!     // Access components for HTTP server
//!     let proxy = runtime.proxy_engine();
//!
//!     // ... set up HTTP routes ...
//!
//!     // Wait for shutdown signal and cleanup
//!     runtime.wait_for_shutdown().await;
//!     Ok(())
//! }
//! ```
//!
//! ## Embedded Usage (Trading Bot, Standalone Application)
//!
//! ```no_run
//! use prism_core::{config::AppConfig, runtime::PrismRuntime};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = AppConfig::load()?;
//!
//!     // Minimal runtime without health checker or websockets
//!     let runtime = PrismRuntime::builder().with_config(config).build()?;
//!
//!     // Direct access to components for custom logic
//!     let cache = runtime.cache_manager();
//!     let upstream = runtime.upstream_manager();
//!
//!     // Custom trading logic using Prism infrastructure
//!     // ...
//!
//!     // Manual shutdown when done
//!     runtime.shutdown().await;
//!     Ok(())
//! }
//! ```

pub mod builder;
pub mod components;
pub mod lifecycle;

pub use builder::PrismRuntimeBuilder;
pub use components::PrismComponents;
pub use lifecycle::PrismRuntime;
