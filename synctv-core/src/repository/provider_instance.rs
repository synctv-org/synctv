// Provider Instance Repository
//
// Database access layer for provider instance configuration management.

use crate::models::{ProviderInstance, UserProviderCredential};
use crate::service::CredentialEncryption;
use crate::Result;
use sqlx::PgPool;

/// Provider Instance Repository
///
/// Encrypts sensitive fields (`jwt_secret`, `custom_ca`) using `CredentialEncryption`
/// before storage and decrypts after read. Encryption is mandatory.
pub struct ProviderInstanceRepository {
    pool: PgPool,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for ProviderInstanceRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstanceRepository")
            .field("pool", &"PgPool")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl ProviderInstanceRepository {
    fn sensitive_fields_present(instance: &ProviderInstance) -> bool {
        instance
            .jwt_secret
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
            || instance
                .custom_ca
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pool,
            encryption: Some(encryption),
        }
    }

    /// Encrypt a string field before storage (if encryption is configured).
    /// Returns the encrypted string or the original if encryption is not configured.
    fn encrypt_field(&self, plaintext: &Option<String>) -> Result<Option<String>> {
        match (&self.encryption, plaintext) {
            (Some(enc), Some(value)) if !value.is_empty() => {
                let json_value = serde_json::Value::String(value.clone());
                let encrypted = enc.encrypt(&json_value)?;
                Ok(Some(encrypted))
            }
            _ => Ok(plaintext.clone()),
        }
    }

    fn ensure_encryption_for_sensitive_fields(&self, instance: &ProviderInstance) -> Result<()> {
        if self.encryption.is_none() && Self::sensitive_fields_present(instance) {
            return Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider instance secrets"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Decrypt a string field after reading. Only encrypted values (enc: prefix) are supported.
    fn decrypt_field(&self, stored: &Option<String>) -> Result<Option<String>> {
        match (&self.encryption, stored) {
            (Some(enc), Some(value)) if value.starts_with("enc:") => {
                let decrypted = enc.decrypt(value)?;
                match decrypted {
                    serde_json::Value::String(s) => Ok(Some(s)),
                    other => Ok(Some(other.to_string())),
                }
            }
            (Some(_), Some(value)) if !value.is_empty() => Err(crate::Error::Internal(
                "Provider instance contains plaintext sensitive data while credential encryption is enabled"
                    .to_string(),
            )),
            _ => Ok(stored.clone()),
        }
    }

    /// Decrypt sensitive fields on a `ProviderInstance` after reading from DB.
    fn decrypt_instance(&self, mut instance: ProviderInstance) -> Result<ProviderInstance> {
        instance.jwt_secret = self.decrypt_field(&instance.jwt_secret)?;
        instance.custom_ca = self.decrypt_field(&instance.custom_ca)?;
        Ok(instance)
    }

    /// Decrypt sensitive fields on a list of `ProviderInstance`.
    fn decrypt_instances(&self, instances: Vec<ProviderInstance>) -> Result<Vec<ProviderInstance>> {
        instances
            .into_iter()
            .map(|i| self.decrypt_instance(i))
            .collect()
    }

    /// Get all provider instances (sensitive fields decrypted)
    pub async fn get_all(&self) -> Result<Vec<ProviderInstance>> {
        let instances = sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instances(instances)
    }

    /// Get all enabled provider instances (sensitive fields decrypted)
    pub async fn get_all_enabled(&self) -> Result<Vec<ProviderInstance>> {
        let instances = sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE enabled = true ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instances(instances)
    }

    /// Get provider instance by name (sensitive fields decrypted)
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ProviderInstance>> {
        let instance = sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        match instance {
            Some(i) => Ok(Some(self.decrypt_instance(i)?)),
            None => Ok(None),
        }
    }

    /// Get instances that support a specific provider type (sensitive fields decrypted)
    pub async fn find_by_provider(&self, provider: &str) -> Result<Vec<ProviderInstance>> {
        let instances = sqlx::query_as::<_, ProviderInstance>(
            "SELECT * FROM media_provider_instances WHERE $1 = ANY(providers) AND enabled = true",
        )
        .bind(provider)
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instances(instances)
    }

    /// Create a new provider instance (encrypts sensitive fields before storage)
    pub async fn create(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret = self.encrypt_field(&instance.jwt_secret)?;
        let encrypted_custom_ca = self.encrypt_field(&instance.custom_ca)?;
        let result = sqlx::query(
            r"
            INSERT INTO media_provider_instances
            (name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "
        )
        .bind(&instance.name)
        .bind(&instance.endpoint)
        .bind(&instance.comment)
        .bind(&encrypted_jwt_secret)
        .bind(&encrypted_custom_ca)
        .bind(&instance.timeout)
        .bind(instance.tls)
        .bind(instance.insecure_tls)
        .bind(&instance.providers)
        .bind(instance.enabled)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("23505") => {
                Err(crate::Error::AlreadyExists(format!(
                    "Provider instance '{}' already exists",
                    instance.name
                )))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Update an existing provider instance (encrypts sensitive fields before storage)
    pub async fn update(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret = self.encrypt_field(&instance.jwt_secret)?;
        let encrypted_custom_ca = self.encrypt_field(&instance.custom_ca)?;

        let result = sqlx::query(
            r"
            UPDATE media_provider_instances
            SET endpoint = $2, comment = $3, jwt_secret = $4, custom_ca = $5,
                timeout = $6, tls = $7, insecure_tls = $8, providers = $9, enabled = $10,
                updated_at = NOW()
            WHERE name = $1
            ",
        )
        .bind(&instance.name)
        .bind(&instance.endpoint)
        .bind(&instance.comment)
        .bind(&encrypted_jwt_secret)
        .bind(&encrypted_custom_ca)
        .bind(&instance.timeout)
        .bind(instance.tls)
        .bind(instance.insecure_tls)
        .bind(&instance.providers)
        .bind(instance.enabled)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{}' not found",
                instance.name
            )));
        }

        Ok(())
    }

    /// Delete a provider instance
    pub async fn delete(&self, name: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM media_provider_instances WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Enable a provider instance
    pub async fn enable(&self, name: &str) -> Result<()> {
        let result = sqlx::query("UPDATE media_provider_instances SET enabled = true, updated_at = NOW() WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Disable a provider instance
    pub async fn disable(&self, name: &str) -> Result<()> {
        let result = sqlx::query("UPDATE media_provider_instances SET enabled = false, updated_at = NOW() WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }
}

/// User Provider Credential Repository
///
/// Credentials are encrypted at rest using AES-256-GCM. Encryption is mandatory.
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
    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pool,
            encryption: Some(encryption),
        }
    }

    /// Encrypt credential data before storage (if encryption is configured)
    fn encrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.encrypt_to_value(data),
            None => Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider credentials"
                    .to_string(),
            )),
        }
    }

    /// Decrypt credential data after reading
    fn decrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.decrypt_value(data),
            None => Ok(data.clone()),
        }
    }

    /// Decrypt credentials on a `UserProviderCredential` in place
    fn decrypt_in_credential(
        &self,
        mut cred: UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        cred.credential_data = self.decrypt_credential(&cred.credential_data)?;
        Ok(cred)
    }

    /// Decrypt credentials on a list of `UserProviderCredential`
    fn decrypt_credentials(
        &self,
        creds: Vec<UserProviderCredential>,
    ) -> Result<Vec<UserProviderCredential>> {
        creds
            .into_iter()
            .map(|c| self.decrypt_in_credential(c))
            .collect()
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
            "SELECT * FROM user_media_provider_credentials WHERE id = $1",
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
            "SELECT * FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2",
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
            ",
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

        let result = sqlx::query(
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

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{}' not found",
                credential.id
            )));
        }

        Ok(())
    }

    /// Delete a user credential
    pub async fn delete(&self, id: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM user_media_provider_credentials WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{id}' not found"
            )));
        }

        Ok(())
    }

    /// Delete all credentials for a user and provider
    pub async fn delete_by_user_and_provider(&self, user_id: &str, provider: &str) -> Result<()> {
        let result = sqlx::query(
            "DELETE FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2",
        )
        .bind(user_id)
        .bind(provider)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "No credentials found for user '{user_id}' and provider '{provider}'"
            )));
        }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::CredentialEncryption;
    use serde_json::json;

    // Note: These are unit tests for the repository structure.
    // Integration tests with actual database should be in tests/ directory.

    #[tokio::test]
    async fn test_provider_instance_repo_rejects_plaintext_sensitive_fields_when_encryption_enabled(
    ) {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let encryption = CredentialEncryption::new(&[7u8; 32]).unwrap();
        let repo = ProviderInstanceRepository::new_with_encryption(pool, encryption);

        let err = repo
            .decrypt_field(&Some("plaintext-secret".to_string()))
            .unwrap_err();
        assert!(
            err.to_string().contains("plaintext sensitive data"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_user_provider_credential_repo_rejects_plaintext_json_when_encryption_enabled() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let encryption = CredentialEncryption::new(&[9u8; 32]).unwrap();
        let repo = UserProviderCredentialRepository::new_with_encryption(pool, encryption);

        let err = repo
            .decrypt_credential(&json!({"token": "plaintext"}))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Plaintext credentials are no longer supported"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_provider_instance_repo_requires_encryption_when_sensitive_fields_present() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = ProviderInstanceRepository::new(pool);

        let err = repo
            .ensure_encryption_for_sensitive_fields(&ProviderInstance {
                name: "remote".to_string(),
                endpoint: "http://remote.example.com:50051".to_string(),
                comment: None,
                jwt_secret: Some("secret".to_string()),
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: false,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = UserProviderCredentialRepository::new(pool);

        let err = repo
            .encrypt_credential(&json!({"token": "plaintext"}))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
    }
}
