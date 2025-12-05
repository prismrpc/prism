use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

/// Rejects requests without `application/json` content-type header.
///
/// # Errors
///
/// Returns `StatusCode::UNSUPPORTED_MEDIA_TYPE` when the request is missing a content-type header
/// or the content-type is not `application/json`.
pub async fn validate_request_middleware(
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(content_type) = request.headers().get("content-type") {
        let ct = content_type.to_str().unwrap_or("");
        if !ct.starts_with("application/json") {
            tracing::warn!(content_type = ct, "invalid content type rejected");
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
    } else {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
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
        routing::post,
        Router,
    };
    use tower::ServiceExt;

    async fn test_handler() -> &'static str {
        "success"
    }

    #[tokio::test]
    async fn test_accepts_application_json() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_accepts_application_json_with_charset() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "application/json; charset=utf-8")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_accepts_application_json_with_boundary() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "application/json; boundary=something")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_rejects_missing_content_type() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder().method("POST").uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_rejects_text_plain() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "text/plain")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_rejects_text_html() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "text/html")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_rejects_application_xml() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "application/xml")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_rejects_multipart_form_data() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "multipart/form-data")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_rejects_url_encoded() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_case_insensitive_header_value() {
        let app = Router::new()
            .route("/test", post(test_handler))
            .layer(middleware::from_fn(validate_request_middleware));

        // Note: HTTP header values are case-sensitive, but our implementation
        // checks for "application/json" which is lowercase by convention
        let request = Request::builder()
            .method("POST")
            .uri("/test")
            .header("content-type", "Application/Json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // This should fail because we do case-sensitive matching
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
