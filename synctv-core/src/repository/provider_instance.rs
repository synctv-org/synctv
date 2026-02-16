// Provider Instance Repository
//
// Database access layer for provider instance configuration management.

use crate::models::{ProviderInstance, UserProviderCredential};
use crate::service::CredentialEncryption;
use crate::Result;
use sqlx::PgPool;

/// Provider Instance Repository
pub struct ProviderInstanceRepository {
    pool: PgPool,
}

impl std::fmt::Debug for ProviderInstanceRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstanceRepository")
            .field("pool", &"PgPool")
            .finish()
    }
}

impl ProviderInstanceRepository {
    #[must_use] 
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get all provider instances
    pub async fn get_all(&self) -> Result<Vec<ProviderInstance>> {
        Ok(sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances ORDER BY created_at DESC"
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Get all enabled provider instances
    pub async fn get_all_enabled(&self) -> Result<Vec<ProviderInstance>> {
        Ok(sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE enabled = true ORDER BY created_at DESC"
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Get provider instance by name
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ProviderInstance>> {
        Ok(sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE name = $1"
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Get instances that support a specific provider type
    pub async fn find_by_provider(&self, provider: &str) -> Result<Vec<ProviderInstance>> {
        Ok(sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE $1 = ANY(providers) AND enabled = true"
        )
        .bind(provider)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Create a new provider instance
    pub async fn create(&self, instance: &ProviderInstance) -> Result<()> {
        sqlx::query(
            r"
            INSERT INTO media_provider_instances
            (name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "
        )
        .bind(&instance.name)
        .bind(&instance.endpoint)
        .bind(&instance.comment)
        .bind(&instance.jwt_secret)
        .bind(&instance.custom_ca)
        .bind(&instance.timeout)
        .bind(instance.tls)
        .bind(instance.insecure_tls)
        .bind(&instance.providers)
        .bind(instance.enabled)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update an existing provider instance
    pub async fn update(&self, instance: &ProviderInstance) -> Result<()> {
        sqlx::query(
            r"
            UPDATE media_provider_instances
            SET endpoint = $2, comment = $3, jwt_secret = $4, custom_ca = $5,
                timeout = $6, tls = $7, insecure_tls = $8, providers = $9, enabled = $10,
                updated_at = NOW()
            WHERE name = $1
            "
        )
        .bind(&instance.name)
        .bind(&instance.endpoint)
        .bind(&instance.comment)
        .bind(&instance.jwt_secret)
        .bind(&instance.custom_ca)
        .bind(&instance.timeout)
        .bind(instance.tls)
        .bind(instance.insecure_tls)
        .bind(&instance.providers)
        .bind(instance.enabled)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Delete a provider instance
    pub async fn delete(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM media_provider_instances WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Enable a provider instance
    pub async fn enable(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE media_provider_instances SET enabled = true, updated_at = NOW() WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Disable a provider instance
    pub async fn disable(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE media_provider_instances SET enabled = false, updated_at = NOW() WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

/// User Provider Credential Repository
///
/// Credentials are encrypted at rest using AES-256-GCM when a `CredentialEncryption`
/// instance is provided. During read, both encrypted and plaintext data are supported
/// for backward compatibility during the migration period.
pub struct UserProviderCredentialRepository {
    pool: PgPool,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for UserProviderCredentialRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserProviderCredentialRepository")
            .field("pool", &"PgPool")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl UserProviderCredentialRepository {
    /// Create a new repository without encryption (backward compatible)
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool, encryption: None }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self { pool, encryption: Some(encryption) }
    }

    /// Encrypt credential data before storage (if encryption is configured)
    fn encrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.encrypt_to_value(data),
            None => Ok(data.clone()),
        }
    }

    /// Decrypt credential data after reading (handles both encrypted and plaintext)
    fn decrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.decrypt_value(data),
            None => Ok(data.clone()),
        }
    }

    /// Decrypt credentials on a `UserProviderCredential` in place
    fn decrypt_in_credential(&self, mut cred: UserProviderCredential) -> Result<UserProviderCredential> {
        cred.credential_data = self.decrypt_credential(&cred.credential_data)?;
        Ok(cred)
    }

    /// Decrypt credentials on a list of `UserProviderCredential`
    fn decrypt_credentials(&self, creds: Vec<UserProviderCredential>) -> Result<Vec<UserProviderCredential>> {
        creds.into_iter().map(|c| self.decrypt_in_credential(c)).collect()
    }

    /// Get all credentials for a user (decrypted)
    pub async fn get_by_user(&self, user_id: &str) -> Result<Vec<UserProviderCredential>> {
        let creds = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials WHERE user_id = $1 ORDER BY created_at DESC"
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credentials(creds)
    }

    /// Get credential by ID (decrypted)
    pub async fn get_by_id(&self, id: &str) -> Result<Option<UserProviderCredential>> {
        let cred = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match cred {
            Some(c) => Ok(Some(self.decrypt_in_credential(c)?)),
            None => Ok(None),
        }
    }

    /// Get user credential for a specific provider and server (decrypted)
    pub async fn get_by_provider_and_server(
        &self,
        user_id: &str,
        provider: &str,
        server_id: &str,
    ) -> Result<Option<UserProviderCredential>> {
        let cred = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2 AND server_id = $3"
        )
        .bind(user_id)
        .bind(provider)
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await?;

        match cred {
            Some(c) => Ok(Some(self.decrypt_in_credential(c)?)),
            None => Ok(None),
        }
    }

    /// Get all credentials for a specific provider type (decrypted)
    pub async fn get_by_provider(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Vec<UserProviderCredential>> {
        let creds = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2"
        )
        .bind(user_id)
        .bind(provider)
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credentials(creds)
    }

    /// Create a new user credential (encrypts before storage)
    pub async fn create(&self, credential: &UserProviderCredential) -> Result<()> {
        let encrypted_data = self.encrypt_credential(&credential.credential_data)?;

        sqlx::query(
            r"
            INSERT INTO user_media_provider_credentials
            (id, user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "
        )
        .bind(&credential.id)
        .bind(&credential.user_id)
        .bind(&credential.provider)
        .bind(&credential.server_id)
        .bind(&credential.provider_instance_name)
        .bind(&encrypted_data)
        .bind(credential.expires_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update an existing user credential (encrypts before storage)
    pub async fn update(&self, credential: &UserProviderCredential) -> Result<()> {
        let encrypted_data = self.encrypt_credential(&credential.credential_data)?;

        sqlx::query(
            r"
            UPDATE user_media_provider_credentials
            SET provider_instance_name = $2, credential_data = $3, expires_at = $4, updated_at = NOW()
            WHERE id = $1
            "
        )
        .bind(&credential.id)
        .bind(&credential.provider_instance_name)
        .bind(&encrypted_data)
        .bind(credential.expires_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Delete a user credential
    pub async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM user_media_provider_credentials WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Delete all credentials for a user and provider
    pub async fn delete_by_user_and_provider(&self, user_id: &str, provider: &str) -> Result<()> {
        sqlx::query("DELETE FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2")
            .bind(user_id)
            .bind(provider)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get all expired credentials (for cleanup jobs, decrypted)
    pub async fn get_expired(&self) -> Result<Vec<UserProviderCredential>> {
        let creds = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credentials(creds)
    }

    /// Delete all expired credentials
    pub async fn delete_expired(&self) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM user_media_provider_credentials WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Migrate plaintext credentials to encrypted format
    ///
    /// Reads all credentials, checks if they are plaintext (not encrypted),
    /// and encrypts them in place. This is safe to run multiple times (idempotent).
    ///
    /// Returns the number of credentials migrated.
    pub async fn migrate_plaintext_to_encrypted(&self) -> Result<u64> {
        let encryption = match &self.encryption {
            Some(enc) => enc,
            None => return Err(crate::Error::Internal(
                "Cannot migrate credentials: encryption not configured".to_string()
            )),
        };

        // Fetch all credentials (raw, without decryption)
        let creds = sqlx::query_as::<_, UserProviderCredential>(
            "SELECT * FROM user_media_provider_credentials ORDER BY id"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut migrated_count = 0u64;

        for cred in creds {
            // Skip already-encrypted credentials
            if CredentialEncryption::is_encrypted(&cred.credential_data) {
                continue;
            }

            // Encrypt the plaintext credential data
            let encrypted_data = encryption.encrypt_to_value(&cred.credential_data)?;

            // Update in database
            sqlx::query(
                "UPDATE user_media_provider_credentials SET credential_data = $2, updated_at = NOW() WHERE id = $1"
            )
            .bind(&cred.id)
            .bind(&encrypted_data)
            .execute(&self.pool)
            .await?;

            migrated_count += 1;
        }

        tracing::info!("Migrated {} plaintext credentials to encrypted format", migrated_count);

        Ok(migrated_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These are unit tests for the repository structure.
    // Integration tests with actual database should be in tests/ directory.

    #[tokio::test]
    async fn test_repository_creation() {
        // This test just ensures the types compile correctly
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let _instance_repo = ProviderInstanceRepository::new(pool.clone());
        let _credential_repo = UserProviderCredentialRepository::new(pool);
    }
}
