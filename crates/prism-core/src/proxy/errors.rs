use crate::{middleware::validation::ValidationError, upstream::errors::UpstreamError};

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Method not supported: {0}")]
    MethodNotSupported(String),

    #[error("Rate limited")]
    RateLimited,

    #[error("Validation error: {0}")]
    Validation(ValidationError),

    /// Preserves concrete `UpstreamError` type for retry/fallback decisions.
    #[error("Upstream error: {0}")]
    Upstream(#[from] UpstreamError),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<ValidationError> for ProxyError {
    fn from(err: ValidationError) -> Self {
        Self::Validation(err)
    }
}
