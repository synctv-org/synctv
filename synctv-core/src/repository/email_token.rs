//! Email token repository for database operations

use crate::{models::UserId, service::email_token::EmailTokenType, Error, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Email token record
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmailToken {
    pub id: i64,
    pub token: String,
    pub user_id: UserId,
    pub token_type: i16,
    pub expires_at: chrono::DateTime<Utc>,
    pub used_at: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
}

/// Email token repository
#[derive(Clone)]
pub struct EmailTokenRepository {
    pool: PgPool,
}

impl EmailTokenRepository {
    fn hash_token(token: &str) -> String {
        let digest = Sha256::digest(token.as_bytes());
        hex::encode(digest)
    }

    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new token
    pub async fn create(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<EmailToken> {
        let t = sqlx::query_as::<_, EmailToken>(
            r"
            INSERT INTO auth_email_tokens (token, user_id, token_type, expires_at, created_at)
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
            RETURNING id, token, user_id, token_type, expires_at, used_at, created_at
            ",
        )
        .bind(Self::hash_token(token))
        .bind(user_id)
        .bind(i16::from(token_type))
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(t)
    }

    /// Atomically create or replace the current unused token for a user/type.
    pub async fn create_or_replace_unused(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<EmailToken> {
        let mut tx = self.pool.begin().await.map_err(Error::Database)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text), $2)")
            .bind(user_id)
            .bind(i32::from(i16::from(token_type)))
            .execute(&mut *tx)
            .await
            .map_err(Error::Database)?;

        sqlx::query(
            r"
            DELETE FROM auth_email_tokens
            WHERE user_id = $1
              AND token_type = $2
              AND used_at IS NULL
            ",
        )
        .bind(user_id)
        .bind(i16::from(token_type))
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;

        let t = sqlx::query_as::<_, EmailToken>(
            r"
            INSERT INTO auth_email_tokens (token, user_id, token_type, expires_at, created_at)
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
            RETURNING id, token, user_id, token_type, expires_at, used_at, created_at
            ",
        )
        .bind(Self::hash_token(token))
        .bind(user_id)
        .bind(i16::from(token_type))
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;

        tx.commit().await.map_err(Error::Database)?;

        Ok(t)
    }

    /// Get token by token string
    pub async fn get(&self, token: &str) -> Result<Option<EmailToken>> {
        let token_hash = Self::hash_token(token);
        let t = sqlx::query_as::<_, EmailToken>(
            r"
            SELECT id, token, user_id, token_type, expires_at, used_at, created_at
            FROM auth_email_tokens
            WHERE token = $1
            ",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(t)
    }

    /// Mark token as used
    ///
    /// Returns `Err(InvalidInput)` if the token does not exist or has already
    /// been used (`used_at IS NOT NULL`), preventing double-use and race
    /// conditions where two concurrent requests both try to consume the same
    /// token.
    pub async fn mark_as_used(&self, token: &str) -> Result<EmailToken> {
        let token_hash = Self::hash_token(token);
        let t = sqlx::query_as::<_, EmailToken>(
            r"
            UPDATE auth_email_tokens
            SET used_at = CURRENT_TIMESTAMP
            WHERE token = $1
              AND used_at IS NULL
            RETURNING id, token, user_id, token_type, expires_at, used_at, created_at
            ",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)?;

        match t {
            Some(record) => Ok(record),
            None => Err(Error::InvalidInput(
                "Token not found or already used".to_string(),
            )),
        }
    }

    /// Atomically validate and consume a token.
    ///
    /// In a single UPDATE, checks that the token exists, matches the expected type,
    /// has not been used, and has not expired. If all conditions are met, marks it
    /// as used and returns the record. Returns `None` if any condition fails.
    pub async fn validate_and_consume(
        &self,
        token: &str,
        token_type: EmailTokenType,
    ) -> Result<Option<EmailToken>> {
        let token_hash = Self::hash_token(token);
        let t = sqlx::query_as::<_, EmailToken>(
            r"
            UPDATE auth_email_tokens
            SET used_at = CURRENT_TIMESTAMP
            WHERE token = $1
              AND token_type = $2
              AND used_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            RETURNING id, token, user_id, token_type, expires_at, used_at, created_at
            ",
        )
        .bind(token_hash)
        .bind(i16::from(token_type))
        .fetch_optional(&self.pool)
        .await?;

        Ok(t)
    }

    /// Atomically validate and consume a token for an expected user.
    ///
    /// This prevents mismatched email/user submissions from consuming a valid
    /// one-time token that belongs to someone else.
    pub async fn validate_and_consume_for_user(
        &self,
        token: &str,
        token_type: EmailTokenType,
        expected_user_id: &UserId,
    ) -> Result<Option<EmailToken>> {
        let token_hash = Self::hash_token(token);
        let t = sqlx::query_as::<_, EmailToken>(
            r"
            UPDATE auth_email_tokens
            SET used_at = CURRENT_TIMESTAMP
            WHERE token = $1
              AND token_type = $2
              AND user_id = $3
              AND used_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            RETURNING id, token, user_id, token_type, expires_at, used_at, created_at
            ",
        )
        .bind(token_hash)
        .bind(i16::from(token_type))
        .bind(expected_user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(t)
    }

    /// Delete all tokens of a specific type for a user
    pub async fn delete_user_tokens(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM auth_email_tokens
            WHERE user_id = $1 AND token_type = $2 AND used_at IS NULL
            ",
        )
        .bind(user_id)
        .bind(i16::from(token_type))
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(result.rows_affected())
    }

    /// Delete a specific unused token if it still belongs to the user and type.
    pub async fn delete_unused_token(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM auth_email_tokens
            WHERE token = $1
              AND user_id = $2
              AND token_type = $3
              AND used_at IS NULL
            ",
        )
        .bind(Self::hash_token(token))
        .bind(user_id)
        .bind(i16::from(token_type))
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(result.rows_affected())
    }

    /// Cleanup expired tokens
    pub async fn cleanup_expired(&self) -> Result<usize> {
        let result = sqlx::query(
            r"
            DELETE FROM auth_email_tokens
            WHERE expires_at < CURRENT_TIMESTAMP
            ",
        )
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(usize::try_from(result.rows_affected()).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
mod tests {}
