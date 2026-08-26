use thiserror::Error;

#[derive(Error, Debug)]
pub enum StreamError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Redis error: {0}")]
    RedisError(String),

    #[error("gRPC error: {0}")]
    GrpcError(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Stream not found: {0}")]
    StreamNotFound(String),

    #[error("No publisher found for room: {0}")]
    NoPublisher(String),

    #[error("Registry error: {0}")]
    RegistryError(String),

    #[error("Stream hub error: {0}")]
    StreamHubError(String),

    #[error("Invalid stream key: {0}")]
    InvalidStreamKey(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Resource exhausted: {0}")]
    ResourceExhausted(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Stale lease_epoch (split-brain detected): {0}")]
    StaleEpoch(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}

pub type StreamResult<T> = Result<T, StreamError>;

impl From<redis::RedisError> for StreamError {
    fn from(err: redis::RedisError) -> Self {
        Self::RedisError(err.to_string())
    }
}

impl From<tonic::transport::Error> for StreamError {
    fn from(err: tonic::transport::Error) -> Self {
        Self::GrpcError(err.to_string())
    }
}
