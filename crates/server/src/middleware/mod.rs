//! HTTP middleware components for the RPC server.
//!
//! This module contains Axum middleware adapters that wrap the business logic
//! components from `prism_core::middleware`. The middleware functions here handle
//! HTTP-specific concerns (request/response manipulation, status codes) while
//! delegating business logic to the core library.

pub mod auth;
pub mod correlation_id;
pub mod rate_limiting;
pub mod validation;

pub use auth::{admin_auth_middleware, api_key_middleware, AdminAuthState};
pub use correlation_id::{
    create_request_id_layers, CorrelationId, UuidRequestIdGenerator, X_REQUEST_ID,
};
