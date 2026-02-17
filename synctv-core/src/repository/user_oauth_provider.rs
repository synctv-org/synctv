//! OAuth2/OIDC provider repository
//!
//! This repository manages `OAuth2` provider mappings (NOT TOKENS).

use sqlx::{PgPool, FromRow};
use crate::{
    models::{oauth2_client::{OAuth2Provider, OAuth2UserInfo, UserOAuthProviderMapping}, UserId},
    Result,
};

/// OAuth2/OIDC provider repository
///
/// Manages mappings between `OAuth2` providers and local users.
/// Tokens are NOT stored - only provider identity information.
#[derive(Clone)]
pub struct UserOAuthProviderRepository {
    pool: PgPool,
}

impl UserOAuthProviderRepository {
    /// Create new repository
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert or update `OAuth2` provider mapping
    pub async fn upsert(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
    ) -> Result<()> {
        self.upsert_with_executor(user_id, provider, provider_user_id, user_info, &self.pool).await
    }

    /// Insert or update `OAuth2` provider mapping using a provided executor (pool or transaction)
    pub async fn upsert_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let id = nanoid::nanoid!(12);

        sqlx::query(
            r"
            INSERT INTO oauth2_clients (id, provider, provider_user_id, user_id, username, email, avatar_url)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (provider, provider_user_id)
            DO UPDATE SET
                user_id = EXCLUDED.user_id,
                username = EXCLUDED.username,
                email = EXCLUDED.email,
                avatar_url = EXCLUDED.avatar_url,
                updated_at = CURRENT_TIMESTAMP
            "
        )
        .bind(&id)
        .bind(provider.as_str())
        .bind(provider_user_id)
        .bind(user_id.as_str())
        .bind(&user_info.username)
        .bind(&user_info.email)
        .bind(&user_info.avatar)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Find user by `OAuth2` provider and provider user ID
    pub async fn find_by_provider(
        &self,
        provider: &OAuth2Provider,
        provider_user_id: &str,
    ) -> Result<Option<UserOAuthProviderMapping>> {
        let row = sqlx::query_as::<_, OAuth2ClientRow>(
            "SELECT * FROM oauth2_clients WHERE provider = $1 AND provider_user_id = $2"
        )
        .bind(provider.as_str())
        .bind(provider_user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(std::convert::Into::into))
    }

    /// Find all `OAuth2` providers for a user
    pub async fn find_by_user(&self, user_id: &UserId) -> Result<Vec<UserOAuthProviderMapping>> {
        let rows = sqlx::query_as::<_, OAuth2ClientRow>(
            "SELECT * FROM oauth2_clients WHERE user_id = $1"
        )
        .bind(user_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(std::convert::Into::into).collect())
    }

    /// Delete `OAuth2` provider mapping
    pub async fn delete(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM oauth2_clients WHERE user_id = $1 AND provider = $2 AND provider_user_id = $3"
        )
        .bind(user_id.as_str())
        .bind(provider.as_str())
        .bind(provider_user_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all `OAuth2` provider mappings for a user (all providers).
    ///
    /// Used during user deletion to clean up all OAuth bindings.
    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.delete_all_for_user_with_executor(user_id, &self.pool).await
    }

    /// Delete all `OAuth2` provider mappings for a user using a provided executor (pool or transaction).
    ///
    /// Used during user deletion to atomically clean up OAuth bindings within the same
    /// transaction as the soft-delete.
    pub async fn delete_all_for_user_with_executor<'e, E>(&self, user_id: &UserId, executor: E) -> Result<u64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query(
            "DELETE FROM oauth2_clients WHERE user_id = $1"
        )
        .bind(user_id.as_str())
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete all `OAuth2` provider mappings for a user and provider type (single query)
    pub async fn delete_by_user_and_provider(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM oauth2_clients WHERE user_id = $1 AND provider = $2"
        )
        .bind(user_id.as_str())
        .bind(provider.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}

/// Row representation for SQL queries (`user_id` as String)
#[derive(FromRow)]
struct OAuth2ClientRow {
    pub id: String,
    pub provider: String,
    pub provider_user_id: String,
    pub user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<OAuth2ClientRow> for UserOAuthProviderMapping {
    fn from(row: OAuth2ClientRow) -> Self {
        Self {
            id: row.id,
            provider: row.provider,
            provider_user_id: row.provider_user_id,
            user_id: UserId(row.user_id),
            username: row.username,
            email: row.email,
            avatar_url: row.avatar_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_oauth2_client_row_to_mapping_all_fields() {
        let now = Utc::now();
        let row = OAuth2ClientRow {
            id: "abc123".to_string(),
            provider: "github".to_string(),
            provider_user_id: "gh_user_456".to_string(),
            user_id: "local_user_789".to_string(),
            username: "ghuser".to_string(),
            email: Some("ghuser@example.com".to_string()),
            avatar_url: Some("https://avatars.example.com/ghuser.png".to_string()),
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert_eq!(mapping.id, "abc123");
        assert_eq!(mapping.provider, "github");
        assert_eq!(mapping.provider_user_id, "gh_user_456");
        assert_eq!(mapping.user_id.as_str(), "local_user_789");
        assert_eq!(mapping.username, "ghuser");
        assert_eq!(mapping.email.as_deref(), Some("ghuser@example.com"));
        assert_eq!(
            mapping.avatar_url.as_deref(),
            Some("https://avatars.example.com/ghuser.png")
        );
        assert_eq!(mapping.created_at, now);
        assert_eq!(mapping.updated_at, now);
    }

    #[test]
    fn test_oauth2_client_row_to_mapping_optional_fields_none() {
        let now = Utc::now();
        let row = OAuth2ClientRow {
            id: "def456".to_string(),
            provider: "oidc".to_string(),
            provider_user_id: "oidc_user_001".to_string(),
            user_id: "user_002".to_string(),
            username: "oidcuser".to_string(),
            email: None,
            avatar_url: None,
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert!(mapping.email.is_none());
        assert!(mapping.avatar_url.is_none());
    }

    #[test]
    fn test_mapping_provider_enum_from_row() {
        let now = Utc::now();
        let row = OAuth2ClientRow {
            id: "id".to_string(),
            provider: "google".to_string(),
            provider_user_id: "goog_123".to_string(),
            user_id: "user".to_string(),
            username: "googleuser".to_string(),
            email: None,
            avatar_url: None,
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert_eq!(
            mapping.provider_enum(),
            Some(crate::models::oauth2_client::OAuth2Provider::Google)
        );
    }
}
