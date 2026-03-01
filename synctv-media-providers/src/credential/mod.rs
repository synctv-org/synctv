//! Credential Storage Module
//!
//! Provides trait definitions and implementations for persisting provider credentials.
//! This enables credential sharing across multiple service replicas.

mod encryption;
mod storage;
mod types;

#[cfg(feature = "postgres")]
mod postgres;

pub use encryption::{EncryptionError, EncryptionResult, FieldEncryption};
pub use storage::{
    CredentialStorage, CredentialStorageError, InMemoryCredentialStorage, Result, StoredCredential,
};
pub use types::{CredentialData, ProviderType};

#[cfg(feature = "postgres")]
pub use postgres::PostgresCredentialStorage;
