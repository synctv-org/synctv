//! PostgreSQL Credential Storage Implementation
//!
//! Provides persistent credential storage using PostgreSQL.

#[cfg(feature = "postgres")]
use async_trait::async_trait;
#[cfg(feature = "postgres")]
use chrono::{DateTime, Utc};
#[cfg(feature = "postgres")]
use sqlx::PgPool;

#[cfg(feature = "postgres")]
use super::storage::{CredentialStorage, CredentialStorageError, Result, StoredCredential};
#[cfg(feature = "postgres")]
use super::types::{CredentialData, ProviderType};
#[cfg(feature = "postgres")]
use crate::ssrf::check_url;

/// PostgreSQL-backed credential storage
#[cfg(feature = "postgres")]
pub struct PostgresCredentialStorage {
    pool: PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresCredentialStorage {
    /// Create a new PostgreSQL credential storage
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Generate a unique ID for new credentials using nanoid
    fn generate_id() -> String {
        nanoid::nanoid!(12)
    }

    /// Convert database row to StoredCredential
    #[allow(clippy::too_many_arguments)]
    fn row_to_credential(
        id: String,
        user_id: String,
        provider: &str,
        server_id: String,
        provider_instance_name: Option<String>,
        credential_data: serde_json::Value,
        expires_at: Option<DateTime<Utc>>,
        _created_at: DateTime<Utc>,
        _updated_at: DateTime<Utc>,
    ) -> std::result::Result<StoredCredential, CredentialStorageError> {
        let provider_type: ProviderType = provider
            .parse()
            .map_err(|e| CredentialStorageError::InvalidData(format!("Invalid provider: {e}")))?;

        let data: CredentialData = serde_json::from_value(credential_data)?;

        Ok(StoredCredential {
            id,
            user_id,
            provider: provider_type,
            server_id,
            provider_instance_name,
            data,
            expires_at: expires_at.map(|t| t.timestamp()),
        })
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl CredentialStorage for PostgresCredentialStorage {
    async fn get(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<Option<StoredCredential>> {
        let row = sqlx::query_as::<_, (
            String,
            String,
            String,
            String,
            Option<String>,
            serde_json::Value,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
            DateTime<Utc>,
        )>(
            r"
            SELECT id, user_id, provider, server_id, provider_instance_name,
                   credential_data, expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2 AND server_id = $3
            "
        )
        .bind(user_id)
        .bind(provider.as_str())
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CredentialStorageError::Database(e.to_string()))?;

        match row {
            Some((id, uid, prov, sid, pin, data, exp, created, updated)) => {
                let cred = Self::row_to_credential(
                    id, uid, &prov, sid, pin, data, exp, created, updated,
                )?;
                Ok(Some(cred))
            }
            None => Ok(None),
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
            CredentialData::Alist { host, .. } => Some(host.as_str()),
            CredentialData::Emby { host, .. } => Some(host.as_str()),
            CredentialData::Bilibili { .. } => None, // Bilibili has no host URL
        };

        if let Some(url) = host_url {
            let ssrf_result = check_url(url);
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

        let provider = data.provider_type();
        let server_id = data.server_id();
        let credential_data = serde_json::to_value(&data)?;
        let id = Self::generate_id();

        // Try to insert, or update if conflict
        let row = sqlx::query_as::<_, (
            String,
            String,
            String,
            String,
            Option<String>,
            serde_json::Value,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
            DateTime<Utc>,
        )>(
            r"
            INSERT INTO user_media_provider_credentials
                (id, user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, NULL)
            ON CONFLICT (user_id, provider, server_id)
            DO UPDATE SET
                provider_instance_name = EXCLUDED.provider_instance_name,
                credential_data = EXCLUDED.credential_data,
                updated_at = NOW()
            RETURNING id, user_id, provider, server_id, provider_instance_name,
                      credential_data, expires_at, created_at, updated_at
            "
        )
        .bind(&id)
        .bind(user_id)
        .bind(provider.as_str())
        .bind(&server_id)
        .bind(provider_instance_name)
        .bind(&credential_data)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CredentialStorageError::Database(e.to_string()))?;

        Self::row_to_credential(
            row.0, row.1, &row.2, row.3, row.4, row.5, row.6, row.7, row.8,
        )
    }

    async fn delete(
        &self,
        user_id: &str,
        provider: ProviderType,
        server_id: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            r"
            DELETE FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2 AND server_id = $3
            "
        )
        .bind(user_id)
        .bind(provider.as_str())
        .bind(server_id)
        .execute(&self.pool)
        .await
        .map_err(|e| CredentialStorageError::Database(e.to_string()))?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_by_user(&self, user_id: &str) -> Result<Vec<StoredCredential>> {
        let rows = sqlx::query_as::<_, (
            String,
            String,
            String,
            String,
            Option<String>,
            serde_json::Value,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
            DateTime<Utc>,
        )>(
            r"
            SELECT id, user_id, provider, server_id, provider_instance_name,
                   credential_data, expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1
            ORDER BY created_at DESC
            "
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CredentialStorageError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|(id, uid, prov, sid, pin, data, exp, created, updated)| {
                Self::row_to_credential(id, uid, &prov, sid, pin, data, exp, created, updated)
            })
            .collect()
    }

    async fn list_by_provider(
        &self,
        user_id: &str,
        provider: ProviderType,
    ) -> Result<Vec<StoredCredential>> {
        let rows = sqlx::query_as::<_, (
            String,
            String,
            String,
            String,
            Option<String>,
            serde_json::Value,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
            DateTime<Utc>,
        )>(
            r"
            SELECT id, user_id, provider, server_id, provider_instance_name,
                   credential_data, expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2
            ORDER BY created_at DESC
            "
        )
        .bind(user_id)
        .bind(provider.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CredentialStorageError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|(id, uid, prov, sid, pin, data, exp, created, updated)| {
                Self::row_to_credential(id, uid, &prov, sid, pin, data, exp, created, updated)
            })
            .collect()
    }
}

// Re-export for non-postgres builds (to avoid dead code warnings)
#[cfg(not(feature = "postgres"))]
pub struct PostgresCredentialStorage;
