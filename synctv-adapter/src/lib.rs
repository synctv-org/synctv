pub mod chat;
pub mod client;
pub mod client_ip;
pub mod error;
pub mod grpc;
pub mod public_id;
pub mod source_config;

pub use public_id::{
    PublicIdCodec, PublicIdConfig, PublicIdKind, PublicIdSqidsConfig, PublicIdType,
};

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AdapterError {
    message: String,
}

pub type AdapterResult<T> = Result<T, AdapterError>;

impl AdapterError {
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
