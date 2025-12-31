//! Input validation for upstream handlers.

use axum::http::StatusCode;

/// Validates upstream weight.
///
/// # Errors
///
/// Returns error if weight is 0 or greater than 1000.
pub fn validate_weight(weight: u32) -> Result<(), (StatusCode, String)> {
    if weight == 0 || weight > 1000 {
        return Err((StatusCode::BAD_REQUEST, "Weight must be between 1 and 1000".to_string()));
    }
    Ok(())
}

/// Validates upstream URL.
///
/// # Errors
///
/// Returns error if URL is invalid or not using http/https scheme.
pub fn validate_url(url_str: &str) -> Result<(), (StatusCode, String)> {
    let url = url::Url::parse(url_str)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid URL format".to_string()))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err((StatusCode::BAD_REQUEST, "URL must use http or https scheme".to_string()));
    }

    Ok(())
}

/// Validates upstream name.
///
/// # Errors
///
/// Returns error if name is empty, too long, or contains invalid characters.
pub fn validate_name(name: &str) -> Result<(), (StatusCode, String)> {
    if name.is_empty() || name.len() > 128 {
        return Err((StatusCode::BAD_REQUEST, "Name must be 1-128 characters".to_string()));
    }

    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err((
            StatusCode::BAD_REQUEST,
            "Name must contain only alphanumeric characters, hyphens, and underscores".to_string(),
        ));
    }

    Ok(())
}

/// Validates create upstream request.
///
/// # Errors
///
/// Returns error if any validation fails.
pub fn validate_create_upstream_request(
    req: &crate::admin::types::CreateUpstreamRequest,
) -> Result<(), (StatusCode, String)> {
    validate_name(&req.name)?;
    validate_url(&req.url)?;
    validate_weight(req.weight)?;

    // Validate optional WebSocket URL
    if let Some(ws_url) = &req.ws_url {
        let url = url::Url::parse(ws_url)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid WebSocket URL format".to_string()))?;

        if !matches!(url.scheme(), "ws" | "wss") {
            return Err((
                StatusCode::BAD_REQUEST,
                "WebSocket URL must use ws or wss scheme".to_string(),
            ));
        }
    }

    Ok(())
}

/// Validates update upstream request.
///
/// # Errors
///
/// Returns error if any validation fails.
pub fn validate_update_upstream_request(
    req: &crate::admin::types::UpdateUpstreamRequest,
) -> Result<(), (StatusCode, String)> {
    if let Some(name) = &req.name {
        validate_name(name)?;
    }

    if let Some(url) = &req.url {
        validate_url(url)?;
    }

    if let Some(weight) = req.weight {
        validate_weight(weight)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_weight_valid() {
        assert!(validate_weight(1).is_ok());
        assert!(validate_weight(100).is_ok());
        assert!(validate_weight(1000).is_ok());
    }

    #[test]
    fn test_validate_weight_invalid() {
        assert!(validate_weight(0).is_err());
        assert!(validate_weight(1001).is_err());
    }

    #[test]
    fn test_validate_url_valid() {
        assert!(validate_url("http://localhost:8545").is_ok());
        assert!(validate_url("https://mainnet.infura.io").is_ok());
    }

    #[test]
    fn test_validate_url_invalid_scheme() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("ws://example.com").is_err());
    }

    #[test]
    fn test_validate_url_invalid_format() {
        assert!(validate_url("not-a-url").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn test_validate_name_valid() {
        assert!(validate_name("mainnet").is_ok());
        assert!(validate_name("eth-mainnet-1").is_ok());
        assert!(validate_name("rpc_node_123").is_ok());
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn test_validate_name_invalid_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn test_validate_name_invalid_too_long() {
        let long_name = "a".repeat(129);
        assert!(validate_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_name_invalid_characters() {
        assert!(validate_name("test node").is_err()); // space
        assert!(validate_name("test@node").is_err()); // @
        assert!(validate_name("test/node").is_err()); // /
        assert!(validate_name("test.node").is_err()); // .
    }

    #[test]
    fn test_validate_create_upstream_request_valid() {
        let req = crate::admin::types::CreateUpstreamRequest {
            name: "test-upstream".to_string(),
            url: "https://mainnet.infura.io".to_string(),
            ws_url: Some("wss://mainnet.infura.io/ws".to_string()),
            weight: 100,
            chain_id: 1,
            timeout_seconds: 30,
        };
        assert!(validate_create_upstream_request(&req).is_ok());
    }

    #[test]
    fn test_validate_create_upstream_request_invalid_name() {
        let req = crate::admin::types::CreateUpstreamRequest {
            name: "".to_string(),
            url: "https://mainnet.infura.io".to_string(),
            ws_url: None,
            weight: 100,
            chain_id: 1,
            timeout_seconds: 30,
        };
        assert!(validate_create_upstream_request(&req).is_err());
    }

    #[test]
    fn test_validate_create_upstream_request_invalid_url() {
        let req = crate::admin::types::CreateUpstreamRequest {
            name: "test-upstream".to_string(),
            url: "file:///etc/passwd".to_string(),
            ws_url: None,
            weight: 100,
            chain_id: 1,
            timeout_seconds: 30,
        };
        assert!(validate_create_upstream_request(&req).is_err());
    }

    #[test]
    fn test_validate_create_upstream_request_invalid_weight() {
        let req = crate::admin::types::CreateUpstreamRequest {
            name: "test-upstream".to_string(),
            url: "https://mainnet.infura.io".to_string(),
            ws_url: None,
            weight: 0,
            chain_id: 1,
            timeout_seconds: 30,
        };
        assert!(validate_create_upstream_request(&req).is_err());
    }

    #[test]
    fn test_validate_create_upstream_request_invalid_ws_url() {
        let req = crate::admin::types::CreateUpstreamRequest {
            name: "test-upstream".to_string(),
            url: "https://mainnet.infura.io".to_string(),
            ws_url: Some("http://invalid-scheme.com".to_string()),
            weight: 100,
            chain_id: 1,
            timeout_seconds: 30,
        };
        assert!(validate_create_upstream_request(&req).is_err());
    }

    #[test]
    fn test_validate_update_upstream_request_valid() {
        let req = crate::admin::types::UpdateUpstreamRequest {
            name: Some("updated-name".to_string()),
            url: Some("https://newrpc.com".to_string()),
            weight: Some(200),
            enabled: Some(true),
        };
        assert!(validate_update_upstream_request(&req).is_ok());
    }

    #[test]
    fn test_validate_update_upstream_request_partial() {
        let req = crate::admin::types::UpdateUpstreamRequest {
            name: None,
            url: None,
            weight: Some(150),
            enabled: Some(false),
        };
        assert!(validate_update_upstream_request(&req).is_ok());
    }

    #[test]
    fn test_validate_update_upstream_request_invalid_name() {
        let req = crate::admin::types::UpdateUpstreamRequest {
            name: Some("".to_string()),
            url: None,
            weight: None,
            enabled: None,
        };
        assert!(validate_update_upstream_request(&req).is_err());
    }

    #[test]
    fn test_validate_update_upstream_request_invalid_weight() {
        let req = crate::admin::types::UpdateUpstreamRequest {
            name: None,
            url: None,
            weight: Some(1001),
            enabled: None,
        };
        assert!(validate_update_upstream_request(&req).is_err());
    }
}
