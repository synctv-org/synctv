//! Credential Storage Module
//!
//! Provides trait definitions and implementations for persisting provider credentials.
//! This enables credential sharing across multiple service replicas.

mod encryption;
mod storage;
mod types;

pub use encryption::FieldEncryption;
pub use storage::{
    CredentialStorage, CredentialStorageError, InMemoryCredentialStorage, Result, StoredCredential,
};
pub use types::{CredentialData, ProviderType};
