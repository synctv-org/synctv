//! OAuth2/OIDC provider repository
//!
//! This repository manages `OAuth2` provider mappings (NOT TOKENS).

use std::collections::HashSet;

use crate::{
    models::{
        oauth2_client::{OAuth2Provider, OAuth2UserInfo, UserOAuthProviderMapping},
        UserId,
    },
    Result,
};
use sqlx::PgPool;

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

    /// Return a reference to the underlying connection pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Insert or update `OAuth2` provider mapping
    pub async fn upsert(
        &self,
        user_id: &UserId,
        provider_type: &OAuth2Provider,
        provider_instance_name: &str,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
    ) -> Result<()> {
        self.upsert_with_executor(
            user_id,
            provider_type,
            provider_instance_name,
            provider_user_id,
            user_info,
            &self.pool,
        )
        .await
    }

    /// Insert or update `OAuth2` provider mapping using a provided executor (pool or transaction)
    pub async fn upsert_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        provider_type: &OAuth2Provider,
        provider_instance_name: &str,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            INSERT INTO auth_oauth2_identities (
                provider_type, provider_instance_name, provider_issuer,
                provider_user_id, user_id, username, avatar_url
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (provider_instance_name, provider_user_id)
            DO UPDATE SET
                provider_type = EXCLUDED.provider_type,
                provider_issuer = EXCLUDED.provider_issuer,
                username = EXCLUDED.username,
                avatar_url = EXCLUDED.avatar_url,
                updated_at = CURRENT_TIMESTAMP
            WHERE auth_oauth2_identities.user_id = EXCLUDED.user_id
            ",
            provider_type.as_i16(),
            provider_instance_name,
            user_info.provider_issuer.as_deref(),
            provider_user_id,
            user_id as &UserId,
            user_info.username.as_str(),
            user_info.avatar.as_deref(),
        )
        .execute(executor)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::AlreadyExists(
                "OAuth2 provider identity is already linked to another user".to_string(),
            ));
        }

        Ok(())
    }

    /// Find user by `OAuth2` provider instance and provider user ID
    pub async fn find_by_provider_instance(
        &self,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<Option<UserOAuthProviderMapping>> {
        self.find_by_provider_instance_with_executor(
            provider_instance_name,
            provider_user_id,
            &self.pool,
        )
        .await
    }

    /// Find user by `OAuth2` provider instance and provider user ID using a provided executor
    ///
    /// Allows the lookup to participate in an existing transaction, which is necessary
    /// for the atomic find-or-create pattern used in [`OAuth2Service::find_or_create_and_link`].
    pub async fn find_by_provider_instance_with_executor<'e, E>(
        &self,
        provider_instance_name: &str,
        provider_user_id: &str,
        executor: E,
    ) -> Result<Option<UserOAuthProviderMapping>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query_as!(
            OAuth2ClientRow,
            r#"
            SELECT id, provider_type as "provider: OAuth2Provider",
                   provider_instance_name, provider_issuer, provider_user_id,
                   user_id as "user_id: UserId",
                   username, avatar_url, created_at, updated_at
            FROM auth_oauth2_identities
            WHERE provider_instance_name = $1 AND provider_user_id = $2
            "#,
            provider_instance_name,
            provider_user_id,
        )
        .fetch_optional(executor)
        .await?;

        Ok(row.map(std::convert::Into::into))
    }

    /// Find all `OAuth2` providers for a user
    pub async fn find_by_user(&self, user_id: &UserId) -> Result<Vec<UserOAuthProviderMapping>> {
        self.find_by_user_with_executor(user_id, &self.pool).await
    }

    /// Find all `OAuth2` providers for a user using a provided executor
    pub async fn find_by_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<Vec<UserOAuthProviderMapping>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let rows = sqlx::query_as!(
            OAuth2ClientRow,
            r#"
            SELECT id, provider_type as "provider: OAuth2Provider",
                   provider_instance_name, provider_issuer, provider_user_id,
                   user_id as "user_id: UserId",
                   username, avatar_url, created_at, updated_at
            FROM auth_oauth2_identities
            WHERE user_id = $1
            "#,
            user_id as &UserId,
        )
        .fetch_all(executor)
        .await?;

        Ok(rows.into_iter().map(std::convert::Into::into).collect())
    }

    /// Count `OAuth2` provider mappings whose provider instance still exists.
    pub async fn count_active_by_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        active_provider_keys: &HashSet<(String, OAuth2Provider)>,
        executor: E,
    ) -> Result<usize>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if active_provider_keys.is_empty() {
            return Ok(0);
        }

        let mappings = self.find_by_user_with_executor(user_id, executor).await?;
        Ok(mappings
            .iter()
            .filter(|mapping| {
                active_provider_keys.contains(&(
                    mapping.provider_instance_name.clone(),
                    mapping.provider.clone(),
                ))
            })
            .count())
    }

    /// Delete one `OAuth2` provider instance mapping
    pub async fn delete_instance(
        &self,
        user_id: &UserId,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM auth_oauth2_identities WHERE user_id = $1 AND provider_instance_name = $2 AND provider_user_id = $3",
            user_id as &UserId,
            provider_instance_name,
            provider_user_id,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all `OAuth2` provider mappings for a user (all providers).
    ///
    /// Used during user deletion to clean up all OAuth bindings.
    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.delete_all_for_user_with_executor(user_id, &self.pool)
            .await
    }

    /// Delete all `OAuth2` provider mappings for a user using a provided executor (pool or transaction).
    ///
    /// Used during user deletion to atomically clean up OAuth bindings within the same
    /// transaction as the soft-delete.
    pub async fn delete_all_for_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<u64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            "DELETE FROM auth_oauth2_identities WHERE user_id = $1",
            user_id as &UserId,
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete all `OAuth2` provider mappings for a user and provider type (single query)
    pub async fn delete_by_user_and_provider(
        &self,
        user_id: &UserId,
        provider_type: &OAuth2Provider,
    ) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM auth_oauth2_identities WHERE user_id = $1 AND provider_type = $2",
            user_id as &UserId,
            provider_type.as_i16(),
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}

/// Row representation for SQL queries.
struct OAuth2ClientRow {
    pub id: i64,
    pub provider: OAuth2Provider,
    pub provider_instance_name: String,
    pub provider_issuer: Option<String>,
    pub provider_user_id: String,
    pub user_id: UserId,
    pub username: String,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<OAuth2ClientRow> for UserOAuthProviderMapping {
    fn from(row: OAuth2ClientRow) -> Self {
        Self {
            id: row.id,
            provider: row.provider,
            provider_instance_name: row.provider_instance_name,
            provider_issuer: row.provider_issuer,
            provider_user_id: row.provider_user_id,
            user_id: row.user_id,
            username: row.username,
            avatar_url: row.avatar_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth2_client_row_to_mapping_all_fields() {
        let now = crate::SystemClock.now();
        let row = OAuth2ClientRow {
            id: 1,
            provider: OAuth2Provider::GitHub,
            provider_instance_name: "github-main".to_string(),
            provider_issuer: Some("https://github.com".to_string()),
            provider_user_id: "gh_user_456".to_string(),
            user_id: UserId::expect_positive(42),
            username: "ghuser".to_string(),
            avatar_url: Some("https://avatars.example.com/ghuser.png".to_string()),
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert_eq!(mapping.id, 1);
        assert_eq!(mapping.provider, OAuth2Provider::GitHub);
        assert_eq!(mapping.provider_instance_name, "github-main");
        assert_eq!(
            mapping.provider_issuer.as_deref(),
            Some("https://github.com")
        );
        assert_eq!(mapping.provider_user_id, "gh_user_456");
        assert_eq!(mapping.user_id, UserId::expect_positive(42));
        assert_eq!(mapping.username, "ghuser");
        assert_eq!(
            mapping.avatar_url.as_deref(),
            Some("https://avatars.example.com/ghuser.png")
        );
        assert_eq!(mapping.created_at, now);
        assert_eq!(mapping.updated_at, now);
    }

    #[test]
    fn test_oauth2_client_row_to_mapping_optional_fields_none() {
        let now = crate::SystemClock.now();
        let row = OAuth2ClientRow {
            id: 2,
            provider: OAuth2Provider::Oidc,
            provider_instance_name: "corp_oidc".to_string(),
            provider_issuer: None,
            provider_user_id: "oidc_user_001".to_string(),
            user_id: UserId::expect_positive(2),
            username: "oidcuser".to_string(),
            avatar_url: None,
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert!(mapping.avatar_url.is_none());
    }

    #[test]
    fn test_mapping_provider_enum_from_row() {
        let now = crate::SystemClock.now();
        let row = OAuth2ClientRow {
            id: 3,
            provider: OAuth2Provider::Google,
            provider_instance_name: "google".to_string(),
            provider_issuer: Some("https://accounts.google.com".to_string()),
            provider_user_id: "goog_123".to_string(),
            user_id: UserId::expect_positive(3),
            username: "googleuser".to_string(),
            avatar_url: None,
            created_at: now,
            updated_at: now,
        };

        let mapping: UserOAuthProviderMapping = row.into();
        assert_eq!(mapping.provider, OAuth2Provider::Google);
    }
}
