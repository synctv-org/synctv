use crate::{models::EmailRegistrationTokenId, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmailRegistrationToken {
    pub id: EmailRegistrationTokenId,
    pub token_hash: String,
    pub username: String,
    pub email: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct EmailRegistrationTokenRepository {
    pool: PgPool,
}

impl EmailRegistrationTokenRepository {
    fn hash_token(token: &str) -> String {
        let digest = Sha256::digest(token.as_bytes());
        hex::encode(digest)
    }

    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_or_replace_unused(
        &self,
        token: &str,
        username: &str,
        email: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<EmailRegistrationToken> {
        let mut tx = self.pool.begin().await?;
        let record = self
            .create_or_replace_unused_with_executor(token, username, email, expires_at, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(record)
    }

    pub async fn create_or_replace_unused_with_executor(
        &self,
        token: &str,
        username: &str,
        email: &str,
        expires_at: DateTime<Utc>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<EmailRegistrationToken> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtext('auth_email_registration_username'), hashtext(LOWER($1::text)))",
            username,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtext('auth_email_registration_email'), hashtext(LOWER($1::text)))",
            email,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query!(
            r#"
            DELETE FROM auth_email_registration_tokens
            WHERE used_at IS NULL
              AND (LOWER(username) = LOWER($1) OR LOWER(email) = LOWER($2))
            "#,
            username,
            email,
        )
        .execute(&mut **tx)
        .await?;

        let record = sqlx::query_as!(
            EmailRegistrationToken,
            r#"
            INSERT INTO auth_email_registration_tokens (
                token_hash, username, email, expires_at, created_at
            )
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
            RETURNING
                id AS "id: EmailRegistrationTokenId",
                token_hash,
                username,
                email,
                expires_at,
                used_at,
                created_at
            "#,
            Self::hash_token(token),
            username,
            email,
            expires_at,
        )
        .fetch_one(&mut **tx)
        .await?;

        Ok(record)
    }

    pub async fn lock_valid_for_update_with_executor(
        token: &str,
        now: DateTime<Utc>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<EmailRegistrationToken>> {
        let record = sqlx::query_as!(
            EmailRegistrationToken,
            r#"
            SELECT
                id AS "id: EmailRegistrationTokenId",
                token_hash,
                username,
                email,
                expires_at,
                used_at,
                created_at
            FROM auth_email_registration_tokens
            WHERE token_hash = $1
              AND used_at IS NULL
              AND expires_at > $2
            FOR UPDATE
            "#,
            Self::hash_token(token),
            now,
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(record)
    }

    pub async fn mark_used_with_executor(
        token: &str,
        now: DateTime<Utc>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            UPDATE auth_email_registration_tokens
            SET used_at = $2
            WHERE token_hash = $1
              AND used_at IS NULL
              AND expires_at > $2
            "#,
            Self::hash_token(token),
            now,
        )
        .execute(&mut **tx)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn delete_unused_token(&self, token: &str) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            DELETE FROM auth_email_registration_tokens
            WHERE token_hash = $1
              AND used_at IS NULL
            "#,
            Self::hash_token(token),
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}
