use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres};

use crate::{
    models::{EmailTokenType, User, UserId},
    Error, Result,
};

use super::user::{USER_ROW_RETURNING_COLUMNS, USER_SELECT_COLUMNS};

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

        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtext($1::text), $2)",
            user_id.to_string(),
            i32::from(i16::from(EmailTokenType::EmailBind))
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            r"
            DELETE FROM auth_email_bind_requests
            WHERE user_id = $1
              AND used_at IS NULL
            ",
            user_id as &UserId,
        )
        .execute(&mut *tx)
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
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

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

    pub async fn consume_and_bind_email(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
    ) -> Result<User> {
        let mut tx = self.pool.begin().await?;
        let now = Utc::now();
        let token_hash = Self::hash_token(token);

        let email = sqlx::query_scalar!(
            r"
            UPDATE auth_email_bind_requests
            SET used_at = $4
            WHERE token = $1
              AND user_id = $2
              AND LOWER(email) = LOWER($3)
              AND used_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            RETURNING email
            ",
            token_hash,
            user_id as &UserId,
            email,
            now,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;

        let user = upsert_verified_email_with_executor(user_id, &email, now, &mut *tx).await?;
        tx.commit().await?;

        Ok(user)
    }
}

async fn upsert_verified_email_with_executor<'e, E>(
    user_id: &UserId,
    email: &str,
    now: chrono::DateTime<Utc>,
    executor: E,
) -> Result<User>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let sql = format!(
        r"
        WITH updated_user AS (
            UPDATE users
            SET updated_at = $3, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING {USER_ROW_RETURNING_COLUMNS}
        ),
        aei AS (
            INSERT INTO auth_email_identities (
                user_id, email, created_at, updated_at
            )
            SELECT id, $2, $3, $3
            FROM updated_user
            ON CONFLICT (user_id)
            DO UPDATE SET
                email = EXCLUDED.email,
                updated_at = EXCLUDED.updated_at
            RETURNING user_id, email
        )
        SELECT {USER_SELECT_COLUMNS}
        FROM updated_user u
        LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
        LEFT JOIN aei ON aei.user_id = u.id
        "
    );

    sqlx::query_as::<_, User>(&sql)
        .bind(user_id)
        .bind(email)
        .bind(now)
        .fetch_optional(executor)
        .await
        .map_err(Error::from)?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
}
