// Provider Error Types

/// Provider-specific errors
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Authentication required")]
    AuthRequired,

    #[error("Credential required")]
    CredentialRequired,

    #[error("Invalid credential type")]
    InvalidCredentialType,

    #[error("Resource not found")]
    NotFound,

    #[error("Provider API error: {0}")]
    ApiError(String),

    #[error("Upstream HTTP {status} for {url}")]
    UpstreamHttp { status: u16, url: String },

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Missing provider instance")]
    MissingInstance,

    #[error("Provider instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Credential encryption required for sensitive provider '{0}'. Configure credential_encryption in server settings.")]
    EncryptionRequired(&'static str),

    #[error("Route registration failed: {0}")]
    RouteRegistrationFailed(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ProviderError>;
