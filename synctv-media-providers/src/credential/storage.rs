//! Credential Storage Trait and Implementations
//!
//! Defines the interface for credential persistence and provides an in-memory
//! implementation for testing purposes.

use super::types::{CredentialData, ProviderType};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    #[error("Credential already exists for user {user_id}, provider {provider}, server {server_id}")]
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

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
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
/// Implementations can use different backends (in-memory, PostgreSQL, Redis, etc.)
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
    async fn delete(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<bool>;

    /// List all credentials for a user
    async fn list_by_user(&self, user_id: &str) -> Result<Vec<StoredCredential>>;

    /// List all credentials for a user and provider type
    async fn list_by_provider(
        &self,
        user_id: &str,
        provider: ProviderType,
    ) -> Result<Vec<StoredCredential>>;

    /// Check if a credential exists
    async fn exists(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<bool> {
        Ok(self.get(user_id, provider, server_id).await?.is_some())
    }
}

/// In-Memory Credential Storage
///
/// Simple in-memory implementation for testing purposes.
/// Uses a HashMap protected by RwLock for thread safety.
pub struct InMemoryCredentialStorage {
    credentials: Arc<RwLock<HashMap<String, StoredCredential>>>,
}

impl Default for InMemoryCredentialStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryCredentialStorage {
    /// Create a new in-memory credential storage
    #[must_use]
    pub fn new() -> Self {
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
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
        Ok(credentials.get(&key).cloned())
    }

    async fn set(
        &self,
        user_id: &str,
        provider_instance_name: Option<&str>,
        data: CredentialData,
    ) -> Result<StoredCredential> {
        let provider = data.provider_type();
        let server_id = data.server_id();
        let key = Self::make_key(user_id, provider, &server_id);

        let credential = StoredCredential {
            id: Self::generate_id(),
            user_id: user_id.to_string(),
            provider,
            server_id: server_id.clone(),
            provider_instance_name: provider_instance_name.map(|s| s.to_string()),
            data,
            expires_at: None,
        };

        let mut credentials = self.credentials.write().await;
        credentials.insert(key, credential.clone());

        Ok(credential)
    }

    async fn delete(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<bool> {
        let key = Self::make_key(user_id, provider, server_id);
        let mut credentials = self.credentials.write().await;
        Ok(credentials.remove(&key).is_some())
    }

    async fn list_by_user(&self, user_id: &str) -> Result<Vec<StoredCredential>> {
        let credentials = self.credentials.read().await;
        let result = credentials
            .values()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect();
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
            .cloned()
            .collect();
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
            .set(
                "user1",
                None,
                CredentialData::bilibili(HashMap::new()),
            )
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
        let all = storage.list_by_provider("user1", ProviderType::Alist).await.unwrap();
        assert_eq!(all.len(), 2);

        // Verify they have different server_ids
        let server_ids: std::collections::HashSet<_> = all.iter().map(|c| c.server_id.clone()).collect();
        assert_eq!(server_ids.len(), 2);
    }
}
