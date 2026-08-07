use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres};

use crate::{models::UserId, Result};

#[derive(Debug, Clone)]
pub struct TotpCredential {
    pub encrypted_secret: serde_json::Value,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub last_used_step: Option<i64>,
    pub recovery_code_hashes: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct TotpCredentialRepository {
    pool: PgPool,
}

impl TotpCredentialRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn start_setup(
        &self,
        user_id: &UserId,
        encrypted_secret: &serde_json::Value,
        setup_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            INSERT INTO auth_totp_credentials (
                user_id, encrypted_secret, setup_id, setup_expires_at,
                confirmed_at, last_used_step, recovery_code_hashes
            ) VALUES ($1, $2, $3, $4, NULL, NULL, '{}')
            ON CONFLICT (user_id) DO UPDATE SET
                encrypted_secret = EXCLUDED.encrypted_secret,
                setup_id = EXCLUDED.setup_id,
                setup_expires_at = EXCLUDED.setup_expires_at,
                confirmed_at = NULL,
                last_used_step = NULL,
                recovery_code_hashes = '{}'
            WHERE auth_totp_credentials.confirmed_at IS NULL
            "#,
            user_id.as_i64(),
            encrypted_secret,
            setup_id,
            expires_at,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn get(&self, user_id: &UserId) -> Result<Option<TotpCredential>> {
        let row = sqlx::query!(
            r#"
            SELECT encrypted_secret, confirmed_at, last_used_step,
                   recovery_code_hashes AS "recovery_code_hashes!: Vec<Vec<u8>>"
            FROM auth_totp_credentials
            WHERE user_id = $1
            "#,
            user_id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| TotpCredential {
            encrypted_secret: row.encrypted_secret,
            confirmed_at: row.confirmed_at,
            last_used_step: row.last_used_step,
            recovery_code_hashes: row.recovery_code_hashes,
        }))
    }

    pub async fn get_pending(
        &self,
        user_id: &UserId,
        setup_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<TotpCredential>> {
        let row = sqlx::query!(
            r#"
            SELECT encrypted_secret, confirmed_at, last_used_step,
                   recovery_code_hashes AS "recovery_code_hashes!: Vec<Vec<u8>>"
            FROM auth_totp_credentials
            WHERE user_id = $1 AND setup_id = $2 AND confirmed_at IS NULL
              AND setup_expires_at > $3
            "#,
            user_id.as_i64(),
            setup_id,
            now,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| TotpCredential {
            encrypted_secret: row.encrypted_secret,
            confirmed_at: row.confirmed_at,
            last_used_step: row.last_used_step,
            recovery_code_hashes: row.recovery_code_hashes,
        }))
    }

    pub async fn confirm(
        &self,
        user_id: &UserId,
        setup_id: &str,
        accepted_step: i64,
        recovery_code_hashes: &[Vec<u8>],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE auth_totp_credentials
            SET setup_id = NULL, setup_expires_at = NULL, confirmed_at = $4,
                last_used_step = $3, recovery_code_hashes = $5
            WHERE user_id = $1 AND setup_id = $2 AND confirmed_at IS NULL
              AND setup_expires_at > $4
            "#,
            user_id.as_i64(),
            setup_id,
            accepted_step,
            now,
            recovery_code_hashes,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn advance_step(&self, user_id: &UserId, accepted_step: i64) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE auth_totp_credentials
            SET last_used_step = $2
            WHERE user_id = $1 AND confirmed_at IS NOT NULL
              AND (last_used_step IS NULL OR last_used_step < $2)
            "#,
            user_id.as_i64(),
            accepted_step,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn consume_recovery_code(&self, user_id: &UserId, code_hash: &[u8]) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE auth_totp_credentials
            SET recovery_code_hashes = array_remove(recovery_code_hashes, $2::bytea)
            WHERE user_id = $1 AND confirmed_at IS NOT NULL
              AND $2::bytea = ANY(recovery_code_hashes)
            "#,
            user_id.as_i64(),
            code_hash,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn replace_recovery_codes(
        &self,
        user_id: &UserId,
        recovery_code_hashes: &[Vec<u8>],
    ) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE auth_totp_credentials SET recovery_code_hashes = $2
            WHERE user_id = $1 AND confirmed_at IS NOT NULL
            "#,
            user_id.as_i64(),
            recovery_code_hashes,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_with_executor<'e, E>(&self, user_id: &UserId, executor: E) -> Result<bool>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let result = sqlx::query!(
            "DELETE FROM auth_totp_credentials WHERE user_id = $1 AND confirmed_at IS NOT NULL",
            user_id.as_i64(),
        )
        .execute(executor)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
