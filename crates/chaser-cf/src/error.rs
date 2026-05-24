//! Error types for chaser-cf

/// Result type alias for chaser-cf operations
pub type ChaserResult<T> = Result<T, ChaserError>;

/// chaser-cf error types
#[derive(Debug, thiserror::Error)]
pub enum ChaserError {
    /// Browser not initialized
    #[error("Browser not initialized. Call init() first or use lazy initialization.")]
    NotInitialized,

    /// Browser initialization failed
    #[error("Failed to initialize browser: {0}")]
    InitFailed(String),

    /// Browser context creation failed
    #[error("Failed to create browser context: {0}")]
    ContextFailed(String),

    /// Page creation failed
    #[error("Failed to create page: {0}")]
    PageFailed(String),

    /// Navigation failed
    #[error("Navigation failed: {0}")]
    NavigationFailed(String),

    /// Operation timed out
    #[error("Operation timed out after {0}ms")]
    Timeout(u64),

    /// Captcha solving failed
    #[error("Failed to solve captcha: {0}")]
    CaptchaFailed(String),

    /// Token extraction failed
    #[error("Failed to extract token")]
    TokenExtractionFailed,

    /// Cookie extraction failed
    #[error("Failed to extract cookies: {0}")]
    CookieExtractionFailed(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// Invalid URL
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    /// Missing required parameter
    #[error("Missing required parameter: {0}")]
    MissingParameter(String),

    /// Proxy error
    #[error("Proxy error: {0}")]
    ProxyError(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl ChaserError {
    /// Get error code for FFI
    pub fn code(&self) -> i32 {
        match self {
            ChaserError::NotInitialized => 1,
            ChaserError::InitFailed(_) => 2,
            ChaserError::ContextFailed(_) => 3,
            ChaserError::PageFailed(_) => 4,
            ChaserError::NavigationFailed(_) => 5,
            ChaserError::Timeout(_) => 6,
            ChaserError::CaptchaFailed(_) => 7,
            ChaserError::TokenExtractionFailed => 8,
            ChaserError::CookieExtractionFailed(_) => 9,
            ChaserError::InvalidConfig(_) => 10,
            ChaserError::InvalidUrl(_) => 11,
            ChaserError::MissingParameter(_) => 12,
            ChaserError::ProxyError(_) => 13,
            ChaserError::Internal(_) => 99,
        }
    }
}

impl From<anyhow::Error> for ChaserError {
    fn from(err: anyhow::Error) -> Self {
        ChaserError::Internal(err.to_string())
    }
}

impl From<tokio::time::error::Elapsed> for ChaserError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        ChaserError::Timeout(0)
    }
}

impl From<url::ParseError> for ChaserError {
    fn from(err: url::ParseError) -> Self {
        ChaserError::InvalidUrl(err.to_string())
    }
}
