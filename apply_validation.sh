#!/bin/bash
# Script to apply input validation to Admin API upstream handlers

set -e

FILE="crates/server/src/admin/handlers/upstreams.rs"

echo "Applying input validation to $FILE..."

# Step 1: Add validation functions after line 158
# Find the line number where we need to insert
LINE_NUM=$(grep -n "^/// GET /admin/upstreams$" "$FILE" | head -1 | cut -d: -f1)

if [ -z "$LINE_NUM" ]; then
    echo "Error: Could not find insertion point"
    exit 1
fi

echo "Found insertion point at line $LINE_NUM"

# Create temp file with validation functions
cat > /tmp/validation_funcs.txt << 'EOF'

/// Validates upstream weight.
///
/// # Errors
///
/// Returns error if weight is 0 or greater than 1000.
fn validate_weight(weight: u32) -> Result<(), (StatusCode, String)> {
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
fn validate_url(url_str: &str) -> Result<(), (StatusCode, String)> {
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
fn validate_name(name: &str) -> Result<(), (StatusCode, String)> {
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
fn validate_create_upstream_request(
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
fn validate_update_upstream_request(
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
EOF

# Insert validation functions
head -n $((LINE_NUM - 1)) "$FILE" > /tmp/upstreams_new.rs
cat /tmp/validation_funcs.txt >> /tmp/upstreams_new.rs
tail -n +$LINE_NUM "$FILE" >> /tmp/upstreams_new.rs

# Step 2: Add validation call in create_upstream
# Find create_upstream function and add validation
CREATE_LINE=$(grep -n "pub async fn create_upstream(" "$FILE" | head -1 | cut -d: -f1)
echo "Found create_upstream at line $CREATE_LINE"

# Add validation after correlation_id line in create_upstream
sed -i "/let correlation_id = crate::admin::get_correlation_id(&headers);/a\\    \\n    // Validate request\\n    validate_create_upstream_request(&request)?;" /tmp/upstreams_new.rs

# Step 3: Add validation call in update_upstream
# Add validation after correlation_id line in update_upstream
sed -i "0,/let correlation_id = crate::admin::get_correlation_id(&headers);/{//!b};/let correlation_id = crate::admin::get_correlation_id(&headers);/a\\    \\n    // Validate request\\n    validate_update_upstream_request(&request)?;" /tmp/upstreams_new.rs

# Backup original file
cp "$FILE" "$FILE.backup"

# Replace with new file
mv /tmp/upstreams_new.rs "$FILE"

echo "Successfully applied validation!"
echo "Original file backed up to $FILE.backup"
echo ""
echo "Please run:"
echo "  cargo check"
echo "  cargo test --package server"
