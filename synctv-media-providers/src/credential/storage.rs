//! Credential Storage Trait and Implementations
//!
//! Defines the interface for credential persistence and provides an in-memory
//! implementation for testing purposes.
//!
//! # Encryption
//!
//! When encryption is enabled (via `with_encryption`), sensitive credential fields
//! are encrypted before storage and decrypted after retrieval:
//! - Alist: `password` field
//! - Emby: `api_key` field
//!
//! The encryption uses AES-256-GCM with a 32-byte key.

use super::encryption::FieldEncryption;
use super::types::{CredentialData, ProviderType};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::ssrf::check_url_async;

/// Error type for credential storage operations
#[derive(Debug, thiserror::Error)]
pub enum CredentialStorageError {
    /// Credential not found
    #[error("Credential not found for user {user_id}, provider {provider}, server {server_id}")]
    NotFound {
        user_id: String,
        provider: String,
        server_id: String,
    },

    /// Credential already exists
    #[error(
        "Credential already exists for user {user_id}, provider {provider}, server {server_id}"
    )]
    AlreadyExists {
        user_id: String,
        provider: String,
        server_id: String,
    },

    /// Invalid credential data
    #[error("Invalid credential data: {0}")]
    InvalidData(String),

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Encryption error
    #[error("Encryption error: {0}")]
    Encryption(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<super::encryption::EncryptionError> for CredentialStorageError {
    fn from(err: super::encryption::EncryptionError) -> Self {
        Self::Encryption(err.to_string())
    }
}

/// Result type for credential storage operations
pub type Result<T> = std::result::Result<T, CredentialStorageError>;

/// Stored credential record
#[derive(Debug, Clone)]
pub struct StoredCredential {
    /// Unique credential ID
    pub id: String,
    /// User ID who owns this credential
    pub user_id: String,
    /// Provider type
    pub provider: ProviderType,
    /// Server identifier
    pub server_id: String,
    /// Associated provider instance name (optional)
    pub provider_instance_name: Option<String>,
    /// The credential data
    pub data: CredentialData,
    /// Optional expiration timestamp (Unix timestamp in seconds)
    pub expires_at: Option<i64>,
}

/// Credential Storage Trait
///
/// Defines the interface for persisting and retrieving provider credentials.
/// Implementations can use different backends (in-memory, `PostgreSQL`, Redis, etc.)
#[async_trait]
pub trait CredentialStorage: Send + Sync {
    /// Get a credential by user, provider, and server
    async fn get(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<Option<StoredCredential>>;

    /// Store a credential (creates new or updates existing)
    async fn set(
        &self,
        user_id: &str,
        provider_instance_name: Option<&str>,
        data: CredentialData,
    ) -> Result<StoredCredential>;

    /// Delete a credential
    async fn delete(&self, user_id: &str, provider: ProviderType, server_id: &str) -> Result<bool>;

    /// List all credentials for a user
    async fn list_by_user(&self, user_id: &str) -> Result<Vec<StoredCredential>>;

    /// List all credentials for a user and provider type
    async fn list_by_provider(
        &self,
        user_id: &str,
        provider: ProviderType,
    ) -> Result<Vec<StoredCredential>>;

    /// Check if a credential exists
    async fn exists(&self, user_id: &str, provider: ProviderType, server_id: &str) -> Result<bool> {
        Ok(self.get(user_id, provider, server_id).await?.is_some())
    }
}

/// In-Memory Credential Storage
///
/// Simple in-memory implementation for testing purposes.
/// Uses a `HashMap` protected by `RwLock` for thread safety.
///
/// # Encryption
///
/// When created with `with_encryption`, sensitive credential fields are encrypted:
/// - Alist: `password` field
/// - Emby: `api_key` field
pub struct InMemoryCredentialStorage {
    credentials: Arc<RwLock<HashMap<String, StoredCredential>>>,
    encryption: Option<FieldEncryption>,
}

impl Default for InMemoryCredentialStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryCredentialStorage {
    /// Create a new in-memory credential storage without encryption
    #[must_use]
    pub fn new() -> Self {
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
            encryption: None,
        }
    }

    /// Create a new in-memory credential storage with encryption enabled
    ///
    /// # Arguments
    /// * `key_bytes` - 32-byte encryption key (AES-256)
    ///
    /// # Panics
    /// Panics if the key is not exactly 32 bytes.
    #[must_use]
    pub fn with_encryption(key_bytes: &[u8]) -> Self {
        let encryption =
            FieldEncryption::new(key_bytes).expect("Encryption key must be exactly 32 bytes");
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
            encryption: Some(encryption),
        }
    }

    /// Generate a unique key for storing credentials
    fn make_key(user_id: &str, provider: ProviderType, server_id: &str) -> String {
        format!("{}:{}:{}", user_id, provider.as_str(), server_id)
    }

    /// Generate a unique ID for new credentials
    fn generate_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        format!("cred_{}", COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    /// Encrypt sensitive fields in credential data before storage
    fn encrypt_data(&self, data: CredentialData) -> Result<CredentialData> {
        let Some(enc) = &self.encryption else {
            return Ok(data);
        };

        match data {
            CredentialData::Alist {
                host,
                username,
                password,
            } => {
                let encrypted_password = enc.encrypt(&password)?;
                Ok(CredentialData::Alist {
                    host,
                    username,
                    password: encrypted_password,
                })
            }
            CredentialData::Emby {
                host,
                api_key,
                emby_user_id,
            } => {
                let encrypted_api_key = enc.encrypt(&api_key)?;
                Ok(CredentialData::Emby {
                    host,
                    api_key: encrypted_api_key,
                    emby_user_id,
                })
            }
            // Bilibili cookies don't need field-level encryption (no password-like secrets)
            other @ CredentialData::Bilibili { .. } => Ok(other),
        }
    }

    /// Decrypt sensitive fields in credential data after retrieval
    fn decrypt_data(&self, data: CredentialData) -> Result<CredentialData> {
        let Some(enc) = &self.encryption else {
            return Ok(data);
        };

        match data {
            CredentialData::Alist {
                host,
                username,
                password,
            } => {
                // Only decrypt if it looks encrypted
                let decrypted_password = if FieldEncryption::is_encrypted(&password) {
                    enc.decrypt(&password)?
                } else {
                    password
                };
                Ok(CredentialData::Alist {
                    host,
                    username,
                    password: decrypted_password,
                })
            }
            CredentialData::Emby {
                host,
                api_key,
                emby_user_id,
            } => {
                // Only decrypt if it looks encrypted
                let decrypted_api_key = if FieldEncryption::is_encrypted(&api_key) {
                    enc.decrypt(&api_key)?
                } else {
                    api_key
                };
                Ok(CredentialData::Emby {
                    host,
                    api_key: decrypted_api_key,
                    emby_user_id,
                })
            }
            // Bilibili cookies pass through unchanged
            other @ CredentialData::Bilibili { .. } => Ok(other),
        }
    }
}

#[async_trait]
impl CredentialStorage for InMemoryCredentialStorage {
    async fn get(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<Option<StoredCredential>> {
        let key = Self::make_key(user_id, provider, server_id);
        let credentials = self.credentials.read().await;
        if let Some(cred) = credentials.get(&key) {
            // Decrypt sensitive fields before returning
            let decrypted_data = self.decrypt_data(cred.data.clone())?;
            Ok(Some(StoredCredential {
                data: decrypted_data,
                ..cred.clone()
            }))
        } else {
            Ok(None)
        }
    }

    async fn set(
        &self,
        user_id: &str,
        provider_instance_name: Option<&str>,
        data: CredentialData,
    ) -> Result<StoredCredential> {
        // SSRF validation: Check host URL for Alist and Emby credentials
        let host_url = match &data {
            CredentialData::Alist { host, .. } | CredentialData::Emby { host, .. } => {
                Some(host.as_str())
            }
            CredentialData::Bilibili { .. } => None, // Bilibili has no host URL
        };

        if let Some(url) = host_url {
            let ssrf_result = check_url_async(url).await;
            if !ssrf_result.is_ok() {
                return Err(CredentialStorageError::InvalidData(format!(
                    "SSRF validation failed: {}",
                    match ssrf_result {
                        crate::ssrf::SsrfCheckResult::Blocked(reason) => reason,
                        crate::ssrf::SsrfCheckResult::Ok => unreachable!(),
                    }
                )));
            }
        }

        // Encrypt sensitive fields before storage
        let encrypted_data = self.encrypt_data(data)?;

        let provider = encrypted_data.provider_type();
        let server_id = encrypted_data.server_id();
        let key = Self::make_key(user_id, provider, &server_id);

        // Store with encrypted data
        let credential = StoredCredential {
            id: Self::generate_id(),
            user_id: user_id.to_string(),
            provider,
            server_id: server_id.clone(),
            provider_instance_name: provider_instance_name.map(std::string::ToString::to_string),
            data: encrypted_data,
            expires_at: None,
        };

        self.credentials
            .write()
            .await
            .insert(key, credential.clone());

        // Return credential with decrypted data for caller convenience
        let decrypted_data = self.decrypt_data(credential.data.clone())?;
        Ok(StoredCredential {
            data: decrypted_data,
            ..credential
        })
    }

    async fn delete(&self, user_id: &str, provider: ProviderType, server_id: &str) -> Result<bool> {
        let key = Self::make_key(user_id, provider, server_id);
        let mut credentials = self.credentials.write().await;
        Ok(credentials.remove(&key).is_some())
    }

    async fn list_by_user(&self, user_id: &str) -> Result<Vec<StoredCredential>> {
        let credentials = self.credentials.read().await;
        let result = credentials
            .values()
            .filter(|c| c.user_id == user_id)
            .map(|c| {
                let decrypted_data = self.decrypt_data(c.data.clone())?;
                Ok(StoredCredential {
                    data: decrypted_data,
                    ..c.clone()
                })
            })
            .collect::<Result<Vec<_>>>()?;
        drop(credentials);
        Ok(result)
    }

    async fn list_by_provider(
        &self,
        user_id: &str,
        provider: ProviderType,
    ) -> Result<Vec<StoredCredential>> {
        let credentials = self.credentials.read().await;
        let result = credentials
            .values()
            .filter(|c| c.user_id == user_id && c.provider == provider)
            .map(|c| {
                let decrypted_data = self.decrypt_data(c.data.clone())?;
                Ok(StoredCredential {
                    data: decrypted_data,
                    ..c.clone()
                })
            })
            .collect::<Result<Vec<_>>>()?;
        drop(credentials);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_storage_set_and_get() {
        let storage = InMemoryCredentialStorage::new();

        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "test_session".to_string());

        let cred = storage
            .set("user123", None, CredentialData::bilibili(cookies.clone()))
            .await
            .unwrap();

        assert_eq!(cred.user_id, "user123");
        assert_eq!(cred.provider, ProviderType::Bilibili);
        assert_eq!(cred.server_id, "bilibili");

        // Retrieve the credential
        let retrieved = storage
            .get("user123", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();

        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, cred.id);
        assert_eq!(retrieved.user_id, "user123");
    }

    #[tokio::test]
    async fn test_in_memory_storage_get_not_found() {
        let storage = InMemoryCredentialStorage::new();

        let result = storage
            .get("nonexistent", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_storage_delete() {
        let storage = InMemoryCredentialStorage::new();

        // Create a credential
        storage
            .set("user123", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();

        // Delete it
        let deleted = storage
            .delete("user123", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();
        assert!(deleted);

        // Verify it's gone
        let result = storage
            .get("user123", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();
        assert!(result.is_none());

        // Delete non-existent returns false
        let deleted_again = storage
            .delete("user123", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn test_in_memory_storage_list_by_user() {
        let storage = InMemoryCredentialStorage::new();

        // Create credentials for user1
        storage
            .set("user1", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();
        storage
            .set(
                "user1",
                None,
                CredentialData::alist(
                    "https://alist.example.com".into(),
                    "user".into(),
                    "pass".into(),
                ),
            )
            .await
            .unwrap();

        // Create credential for user2
        storage
            .set("user2", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();

        // List user1's credentials
        let user1_creds = storage.list_by_user("user1").await.unwrap();
        assert_eq!(user1_creds.len(), 2);

        // List user2's credentials
        let user2_creds = storage.list_by_user("user2").await.unwrap();
        assert_eq!(user2_creds.len(), 1);
    }

    #[tokio::test]
    async fn test_in_memory_storage_list_by_provider() {
        let storage = InMemoryCredentialStorage::new();

        // Create multiple Alist credentials (different hosts)
        storage
            .set(
                "user1",
                None,
                CredentialData::alist(
                    "https://alist1.example.com".into(),
                    "user".into(),
                    "pass".into(),
                ),
            )
            .await
            .unwrap();
        storage
            .set(
                "user1",
                None,
                CredentialData::alist(
                    "https://alist2.example.com".into(),
                    "user".into(),
                    "pass".into(),
                ),
            )
            .await
            .unwrap();
        storage
            .set("user1", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();

        // List only Alist credentials
        let alist_creds = storage
            .list_by_provider("user1", ProviderType::Alist)
            .await
            .unwrap();
        assert_eq!(alist_creds.len(), 2);

        // List only Bilibili credentials
        let bilibili_creds = storage
            .list_by_provider("user1", ProviderType::Bilibili)
            .await
            .unwrap();
        assert_eq!(bilibili_creds.len(), 1);

        // List Emby credentials (none)
        let emby_creds = storage
            .list_by_provider("user1", ProviderType::Emby)
            .await
            .unwrap();
        assert!(emby_creds.is_empty());
    }

    #[tokio::test]
    async fn test_in_memory_storage_update() {
        let storage = InMemoryCredentialStorage::new();

        // Create initial credential
        let cred1 = storage
            .set("user1", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();

        // Update with new cookies
        let mut new_cookies = HashMap::new();
        new_cookies.insert("SESSDATA".to_string(), "new_session".to_string());

        let cred2 = storage
            .set("user1", None, CredentialData::bilibili(new_cookies))
            .await
            .unwrap();

        // Should be the same key but new ID
        assert_ne!(cred1.id, cred2.id);

        // Should only have one credential
        let all = storage.list_by_user("user1").await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn test_in_memory_storage_exists() {
        let storage = InMemoryCredentialStorage::new();

        assert!(!storage
            .exists("user1", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap());

        storage
            .set("user1", None, CredentialData::bilibili(HashMap::new()))
            .await
            .unwrap();

        assert!(storage
            .exists("user1", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_storage_multiple_alist_servers() {
        let storage = InMemoryCredentialStorage::new();

        // User can have multiple Alist servers
        let host1 = "https://alist1.example.com";
        let host2 = "https://alist2.example.com";

        storage
            .set(
                "user1",
                Some("instance1"),
                CredentialData::alist(host1.into(), "user".into(), "pass".into()),
            )
            .await
            .unwrap();

        storage
            .set(
                "user1",
                Some("instance2"),
                CredentialData::alist(host2.into(), "user".into(), "pass".into()),
            )
            .await
            .unwrap();

        // Both should exist independently
        let all = storage
            .list_by_provider("user1", ProviderType::Alist)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);

        // Verify they have different server_ids
        let server_ids: std::collections::HashSet<_> =
            all.iter().map(|c| c.server_id.clone()).collect();
        assert_eq!(server_ids.len(), 2);
    }

    // ========== Encryption Tests ==========

    fn test_encryption_key() -> Vec<u8> {
        vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]
    }

    #[tokio::test]
    async fn test_encryption_alist_password_round_trip() {
        let storage = InMemoryCredentialStorage::with_encryption(&test_encryption_key());

        let plain_password = "my_secret_password_123";

        // Store Alist credential
        let stored = storage
            .set(
                "user1",
                Some("my_alist"),
                CredentialData::alist(
                    "https://alist.example.com".to_string(),
                    "admin".to_string(),
                    plain_password.to_string(),
                ),
            )
            .await
            .expect("Failed to store credential");

        // The returned credential should have decrypted password (for caller convenience)
        let (_, _, password) = stored
            .data
            .as_alist()
            .expect("Expected Alist credential data");
        assert_eq!(
            password, plain_password,
            "Returned password should be decrypted"
        );

        // Retrieve the credential
        let retrieved = storage
            .get("user1", ProviderType::Alist, &stored.server_id)
            .await
            .expect("Failed to get credential")
            .expect("Credential should exist");

        // The retrieved password should be decrypted
        let (_, _, password) = retrieved
            .data
            .as_alist()
            .expect("Expected Alist credential data");
        assert_eq!(
            password, plain_password,
            "Retrieved password should be decrypted"
        );
    }

    #[tokio::test]
    async fn test_encryption_emby_api_key_round_trip() {
        let storage = InMemoryCredentialStorage::with_encryption(&test_encryption_key());

        let api_key = "secret_api_key_12345";

        // Store Emby credential
        let stored = storage
            .set(
                "user1",
                Some("my_emby"),
                CredentialData::emby(
                    "https://emby.example.com".to_string(),
                    api_key.to_string(),
                    "user_id".to_string(),
                ),
            )
            .await
            .expect("Failed to store credential");

        // The returned credential should have decrypted api_key
        let (_, key, _) = stored
            .data
            .as_emby()
            .expect("Expected Emby credential data");
        assert_eq!(key, api_key, "Returned api_key should be decrypted");

        // Retrieve the credential
        let retrieved = storage
            .get("user1", ProviderType::Emby, &stored.server_id)
            .await
            .expect("Failed to get credential")
            .expect("Credential should exist");

        // The retrieved api_key should be decrypted
        let (_, key, _) = retrieved
            .data
            .as_emby()
            .expect("Expected Emby credential data");
        assert_eq!(key, api_key, "Retrieved api_key should be decrypted");
    }

    #[tokio::test]
    async fn test_encryption_bilibili_unaffected() {
        let storage = InMemoryCredentialStorage::with_encryption(&test_encryption_key());

        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "test_session".to_string());

        // Store Bilibili credential
        let stored = storage
            .set("user1", None, CredentialData::bilibili(cookies.clone()))
            .await
            .expect("Failed to store credential");

        // Bilibili cookies should not be encrypted
        let c = stored
            .data
            .as_bilibili()
            .expect("Expected Bilibili credential data");
        assert_eq!(c.get("SESSDATA"), Some(&"test_session".to_string()));
    }

    // ========== Type Mismatch Tests ==========

    #[test]
    fn test_as_alist_returns_error_on_type_mismatch() {
        let emby_data = CredentialData::emby(
            "https://emby.example.com".to_string(),
            "key".to_string(),
            "uid".to_string(),
        );
        let result = emby_data.as_alist();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("Expected Alist"),
            "Error message should mention expected type"
        );

        let bilibili_data = CredentialData::bilibili(HashMap::new());
        assert!(bilibili_data.as_alist().is_err());
    }

    #[test]
    fn test_as_emby_returns_error_on_type_mismatch() {
        let alist_data = CredentialData::alist(
            "https://alist.example.com".to_string(),
            "user".to_string(),
            "pass".to_string(),
        );
        let result = alist_data.as_emby();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("Expected Emby"),
            "Error message should mention expected type"
        );

        let bilibili_data = CredentialData::bilibili(HashMap::new());
        assert!(bilibili_data.as_emby().is_err());
    }

    #[test]
    fn test_as_bilibili_returns_error_on_type_mismatch() {
        let alist_data = CredentialData::alist(
            "https://alist.example.com".to_string(),
            "user".to_string(),
            "pass".to_string(),
        );
        let result = alist_data.as_bilibili();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("Expected Bilibili"),
            "Error message should mention expected type"
        );

        let emby_data = CredentialData::emby(
            "https://emby.example.com".to_string(),
            "key".to_string(),
            "uid".to_string(),
        );
        assert!(emby_data.as_bilibili().is_err());
    }

    #[test]
    fn test_as_alist_returns_correct_fields() {
        let data = CredentialData::alist(
            "https://alist.example.com".to_string(),
            "admin".to_string(),
            "secret".to_string(),
        );
        let (host, username, password) = data.as_alist().unwrap();
        assert_eq!(host, "https://alist.example.com");
        assert_eq!(username, "admin");
        assert_eq!(password, "secret");
    }

    #[test]
    fn test_as_emby_returns_correct_fields() {
        let data = CredentialData::emby(
            "https://emby.example.com".to_string(),
            "my_api_key".to_string(),
            "user_42".to_string(),
        );
        let (host, api_key, emby_user_id) = data.as_emby().unwrap();
        assert_eq!(host, "https://emby.example.com");
        assert_eq!(api_key, "my_api_key");
        assert_eq!(emby_user_id, "user_42");
    }

    #[test]
    fn test_as_bilibili_returns_correct_fields() {
        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "abc123".to_string());
        let data = CredentialData::bilibili(cookies);
        let c = data.as_bilibili().unwrap();
        assert_eq!(c.get("SESSDATA"), Some(&"abc123".to_string()));
    }
}
