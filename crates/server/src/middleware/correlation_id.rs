//! Request correlation ID middleware for distributed tracing.
//!
//! This middleware extracts or generates a correlation ID for each request,
//! stores it in request extensions, and adds it to the response headers.
//! This enables end-to-end request tracing across services.

use axum::http::{header::HeaderValue, HeaderName, Request};
use std::sync::Arc;
use tower_http::request_id::{MakeRequestId, RequestId};
use uuid::Uuid;

/// The header name for request correlation IDs.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// A unique identifier for a request, used for distributed tracing.
#[derive(Clone, Debug)]
pub struct CorrelationId(pub Arc<str>);

impl CorrelationId {
    /// Creates a new correlation ID from a string.
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    /// Generates a new random correlation ID using UUID v4.
    #[must_use]
    pub fn generate() -> Self {
        Self(Arc::from(Uuid::new_v4().to_string()))
    }

    /// Returns the correlation ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A UUID v4 generator for request IDs.
/// Used with tower-http's request ID middleware.
#[derive(Clone, Copy, Default)]
pub struct UuidRequestIdGenerator;

impl MakeRequestId for UuidRequestIdGenerator {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::new_v4().to_string();
        Some(RequestId::new(HeaderValue::from_str(&id).ok()?))
    }
}

/// Creates the request ID layer stack.
///
/// This returns a tuple of layers that should be applied to the router:
/// 1. `SetRequestIdLayer` - Sets X-Request-ID header if not present
/// 2. `PropagateRequestIdLayer` - Copies X-Request-ID from request to response
///
/// # Example
///
/// ```ignore
/// let (set_layer, propagate_layer) = create_request_id_layers();
/// let app = Router::new()
///     .route("/", get(handler))
///     .layer(propagate_layer)
///     .layer(set_layer);
/// ```
pub fn create_request_id_layers() -> (
    tower_http::request_id::SetRequestIdLayer<UuidRequestIdGenerator>,
    tower_http::request_id::PropagateRequestIdLayer,
) {
    use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};

    let set_layer = SetRequestIdLayer::new(X_REQUEST_ID.clone(), UuidRequestIdGenerator);
    let propagate_layer = PropagateRequestIdLayer::new(X_REQUEST_ID.clone());

    (set_layer, propagate_layer)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;
    use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};

    async fn simple_handler() -> &'static str {
        "ok"
    }

    fn create_test_app() -> Router {
        let set_layer = SetRequestIdLayer::new(X_REQUEST_ID.clone(), UuidRequestIdGenerator);
        let propagate_layer = PropagateRequestIdLayer::new(X_REQUEST_ID.clone());

        Router::new()
            .route("/test", get(simple_handler))
            .layer(propagate_layer)
            .layer(set_layer)
    }

    #[tokio::test]
    async fn test_generates_correlation_id_when_missing() {
        let app = create_test_app();

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Should have X-Request-ID in response
        let header = response.headers().get(&X_REQUEST_ID).expect("Should have correlation ID");
        let id = header.to_str().unwrap();

        // Should be a valid UUID
        assert!(Uuid::parse_str(id).is_ok(), "Generated ID should be valid UUID, got: {id}");
    }

    #[tokio::test]
    async fn test_preserves_existing_correlation_id() {
        let app = create_test_app();
        let custom_id = "my-custom-correlation-id-123";

        let request = Request::builder()
            .uri("/test")
            .header(X_REQUEST_ID.clone(), custom_id)
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Should preserve the original ID
        let header = response.headers().get(&X_REQUEST_ID).expect("Should have correlation ID");
        assert_eq!(header.to_str().unwrap(), custom_id);
    }

    #[test]
    fn test_correlation_id_display() {
        let id = CorrelationId::new("test-123");
        assert_eq!(format!("{id}"), "test-123");
    }

    #[test]
    fn test_correlation_id_generate() {
        let id1 = CorrelationId::generate();
        let id2 = CorrelationId::generate();

        // Should generate unique IDs
        assert_ne!(id1.as_str(), id2.as_str());

        // Should be valid UUIDs
        assert!(Uuid::parse_str(id1.as_str()).is_ok());
        assert!(Uuid::parse_str(id2.as_str()).is_ok());
    }

    #[test]
    fn test_uuid_generator() {
        let mut generator = UuidRequestIdGenerator;
        let request = Request::builder().body(()).unwrap();

        let id1 = generator.make_request_id(&request).expect("Should generate ID");
        let id2 = generator.make_request_id(&request).expect("Should generate ID");

        // Should generate different IDs
        assert_ne!(id1.header_value(), id2.header_value());

        // Should be valid UUIDs
        let id1_str = id1.header_value().to_str().unwrap();
        assert!(Uuid::parse_str(id1_str).is_ok());
    }
}
