use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{
    models::{EmailTokenType, UserId},
    Error, Result,
};

#[derive(Clone)]
pub struct EmailBindRepository {
    pool: PgPool,
}

impl EmailBindRepository {
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
        user_id: &UserId,
        email: &str,
        token: &str,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.create_or_replace_unused_with_executor(user_id, email, token, expires_at, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_or_replace_unused_with_executor(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
        expires_at: chrono::DateTime<Utc>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtext($1::text), $2)",
            user_id.to_string(),
            i32::from(i16::from(EmailTokenType::EmailBind))
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query!(
            r"
            DELETE FROM auth_email_bind_requests
            WHERE user_id = $1
              AND used_at IS NULL
            ",
            user_id as &UserId,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query!(
            r"
            INSERT INTO auth_email_bind_requests (
                user_id, email, token, expires_at, created_at
            )
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
            ",
            user_id as &UserId,
            email,
            Self::hash_token(token),
            expires_at,
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub async fn delete_unused(&self, token: &str, user_id: &UserId, email: &str) -> Result<u64> {
        let result = sqlx::query!(
            r"
            DELETE FROM auth_email_bind_requests
            WHERE token = $1
              AND user_id = $2
              AND LOWER(email) = LOWER($3)
              AND used_at IS NULL
            ",
            Self::hash_token(token),
            user_id as &UserId,
            email,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn consume(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
    ) -> Result<(String, chrono::DateTime<Utc>)> {
        let mut tx = self.pool.begin().await?;
        let now = crate::SystemClock.now();
        let (email, now) = self
            .consume_with_executor(user_id, email, token, now, &mut *tx)
            .await?;
        tx.commit().await?;

        Ok((email, now))
    }

    pub async fn consume_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
        now: chrono::DateTime<Utc>,
        executor: E,
    ) -> Result<(String, chrono::DateTime<Utc>)>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let token_hash = Self::hash_token(token);

        let email = sqlx::query_scalar!(
            r"
            UPDATE auth_email_bind_requests
            SET used_at = $4
            WHERE token = $1
              AND user_id = $2
              AND LOWER(email) = LOWER($3)
              AND used_at IS NULL
              AND expires_at > $4
            RETURNING email
            ",
            token_hash,
            user_id as &UserId,
            email,
            now,
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;

        Ok((email, now))
    }

    pub async fn lock_valid_for_update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
        now: chrono::DateTime<Utc>,
        executor: E,
    ) -> Result<String>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let token_hash = Self::hash_token(token);

        sqlx::query_scalar!(
            r"
            SELECT email
            FROM auth_email_bind_requests
            WHERE token = $1
              AND user_id = $2
              AND LOWER(email) = LOWER($3)
              AND used_at IS NULL
              AND expires_at > $4
            FOR UPDATE
            ",
            token_hash,
            user_id as &UserId,
            email,
            now,
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })
    }
}
