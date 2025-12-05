//! HTTP middleware components for the RPC server.
//!
//! This module contains Axum middleware adapters that wrap the business logic
//! components from `prism_core::middleware`. The middleware functions here handle
//! HTTP-specific concerns (request/response manipulation, status codes) while
//! delegating business logic to the core library.

pub mod auth;
pub mod rate_limiting;
pub mod validation;

pub use auth::api_key_middleware;
