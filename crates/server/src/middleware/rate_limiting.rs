use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use prism_core::middleware::RateLimiter;
use std::{net::SocketAddr, sync::Arc};

/// Rate limiting middleware that enforces per-IP request limits.
///
/// # Errors
///
/// Returns `StatusCode::TOO_MANY_REQUESTS` when the rate limit is exceeded for the client IP.
pub async fn rate_limit_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(rate_limiter): State<Arc<RateLimiter>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let key = addr.ip().to_string();

    if !rate_limiter.check_rate_limit(&key) {
        tracing::warn!(client = %key, "rate limit exceeded");
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tower::ServiceExt;

    async fn test_handler() -> &'static str {
        "success"
    }

    #[tokio::test]
    async fn test_allows_request_when_under_limit() {
        let rate_limiter = Arc::new(RateLimiter::new(5, 1));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(rate_limiter.clone(), rate_limit_middleware))
            .with_state(rate_limiter);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_blocks_request_when_over_limit() {
        let rate_limiter = Arc::new(RateLimiter::new(2, 1));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(rate_limiter.clone(), rate_limit_middleware))
            .with_state(rate_limiter);

        for _ in 0..2 {
            let request = Request::builder()
                .uri("/test")
                .extension(ConnectInfo(addr))
                .body(Body::empty())
                .unwrap();

            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_uses_ip_as_rate_limit_key() {
        let rate_limiter = Arc::new(RateLimiter::new(1, 1));
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(rate_limiter.clone(), rate_limit_middleware))
            .with_state(rate_limiter.clone());

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr1))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let key1 = addr1.ip().to_string();
        assert!(rate_limiter.get_bucket_info(&key1).is_some());

        let key2 = addr2.ip().to_string();
        assert!(rate_limiter.get_bucket_info(&key2).is_none());
    }

    #[tokio::test]
    async fn test_different_ips_have_separate_limits() {
        let rate_limiter = Arc::new(RateLimiter::new(1, 1));
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(rate_limiter.clone(), rate_limit_middleware))
            .with_state(rate_limiter);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr1))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr2))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr1))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr2))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_ipv6_addresses() {
        let rate_limiter = Arc::new(RateLimiter::new(1, 1));
        let addr = SocketAddr::new(IpAddr::V6("::1".parse().unwrap()), 8080);

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(rate_limiter.clone(), rate_limit_middleware))
            .with_state(rate_limiter.clone());

        let request = Request::builder()
            .uri("/test")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let key = addr.ip().to_string();
        assert!(rate_limiter.get_bucket_info(&key).is_some());
    }
}
