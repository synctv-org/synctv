use chrono::{DateTime, Utc};
use sqlx::PgPool;
use webauthn_rs::prelude::Passkey;

use crate::{models::UserId, Error, InternalExt, Result};

fn count_value(value: Option<i64>, query_description: &str) -> Result<i64> {
    value.ok_or_else(|| {
        Error::Internal(format!(
            "{query_description} COUNT query returned no scalar value"
        ))
    })
}

#[derive(Debug, Clone)]
pub struct WebAuthnCredential {
    pub id: i64,
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    pub passkey: Passkey,
    pub sign_count: i64,
    pub name: Option<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct WebAuthnCredentialRepository {
    pool: PgPool,
}

impl WebAuthnCredentialRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn row_to_credential(row: WebAuthnCredentialRow) -> Result<WebAuthnCredential> {
        let passkey = serde_json::from_value(row.passkey)
            .internal_with_err("Failed to deserialize stored WebAuthn passkey")?;

        Ok(WebAuthnCredential {
            id: row.id,
            user_id: row.user_id,
            credential_id: row.credential_id,
            passkey,
            sign_count: row.sign_count,
            name: row.name,
            last_used_at: row.last_used_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    pub async fn create(
        &self,
        user_id: &UserId,
        passkey: &Passkey,
        name: Option<&str>,
    ) -> Result<WebAuthnCredential> {
        self.create_with_executor(user_id, passkey, name, &self.pool)
            .await
    }

    pub async fn create_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        passkey: &Passkey,
        name: Option<&str>,
        executor: E,
    ) -> Result<WebAuthnCredential>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let credential_id = AsRef::<[u8]>::as_ref(passkey.cred_id()).to_vec();
        let passkey_json = serde_json::to_value(passkey)
            .internal_with_err("Failed to serialize WebAuthn passkey")?;
        let public_key_json = serde_json::to_value(passkey.get_public_key())
            .internal_with_err("Failed to serialize WebAuthn public key")?;
        let row = sqlx::query_as!(
            WebAuthnCredentialRow,
            r#"
            INSERT INTO auth_webauthn_credentials (
                user_id, credential_id, passkey, public_key, name
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id,
                      user_id as "user_id: UserId",
                      credential_id,
                      passkey as "passkey!: serde_json::Value",
                      sign_count,
                      name,
                      last_used_at,
                      created_at,
                      updated_at
            "#,
            user_id as &UserId,
            credential_id,
            passkey_json,
            public_key_json,
            name,
        )
        .fetch_one(executor)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(ref database_error) if database_error.constraint().is_some() => {
                Error::AlreadyExists("Passkey credential is already registered".to_string())
            }
            other => Error::Database(other),
        })?;

        Self::row_to_credential(row)
    }

    pub async fn list_by_user(&self, user_id: &UserId) -> Result<Vec<WebAuthnCredential>> {
        let rows = sqlx::query_as!(
            WebAuthnCredentialRow,
            r#"
            SELECT id,
                   user_id as "user_id: UserId",
                   credential_id,
                   passkey as "passkey!: serde_json::Value",
                   sign_count,
                   name,
                   last_used_at,
                   created_at,
                   updated_at
            FROM auth_webauthn_credentials
            WHERE user_id = $1
            ORDER BY created_at ASC, id ASC
            "#,
            user_id as &UserId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(Self::row_to_credential).collect()
    }

    pub async fn get_by_credential_id(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<WebAuthnCredential>> {
        let row = sqlx::query_as!(
            WebAuthnCredentialRow,
            r#"
            SELECT id,
                   user_id as "user_id: UserId",
                   credential_id,
                   passkey as "passkey!: serde_json::Value",
                   sign_count,
                   name,
                   last_used_at,
                   created_at,
                   updated_at
            FROM auth_webauthn_credentials
            WHERE credential_id = $1
            "#,
            credential_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(Self::row_to_credential).transpose()
    }

    pub async fn update_after_authentication(
        &self,
        credential_id: &[u8],
        passkey: &Passkey,
        sign_count: i64,
    ) -> Result<()> {
        let passkey_json = serde_json::to_value(passkey)
            .internal_with_err("Failed to serialize updated WebAuthn passkey")?;
        let public_key_json = serde_json::to_value(passkey.get_public_key())
            .internal_with_err("Failed to serialize updated WebAuthn public key")?;
        let result = sqlx::query!(
            r"
            UPDATE auth_webauthn_credentials
            SET passkey = $2,
                public_key = $3,
                sign_count = $4,
                last_used_at = NOW()
            WHERE credential_id = $1
            ",
            credential_id,
            passkey_json,
            public_key_json,
            sign_count,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound("WebAuthn credential not found".to_string()));
        }
        Ok(())
    }

    pub async fn delete_for_user(&self, user_id: &UserId, credential_id: &[u8]) -> Result<bool> {
        self.delete_for_user_with_executor(user_id, credential_id, &self.pool)
            .await
    }

    pub async fn delete_for_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        credential_id: &[u8],
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            "DELETE FROM auth_webauthn_credentials WHERE user_id = $1 AND credential_id = $2",
            user_id as &UserId,
            credential_id,
        )
        .execute(executor)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn count_by_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<i64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM auth_webauthn_credentials WHERE user_id = $1",
            user_id as &UserId,
        )
        .fetch_one(executor)
        .await?;
        count_value(count, "WebAuthn credential")
    }

    pub async fn exists_for_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        credential_id: &[u8],
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM auth_webauthn_credentials
                WHERE user_id = $1 AND credential_id = $2
            ) as "exists!"
            "#,
            user_id as &UserId,
            credential_id,
        )
        .fetch_one(executor)
        .await?;
        Ok(exists)
    }
}

struct WebAuthnCredentialRow {
    id: i64,
    user_id: UserId,
    credential_id: Vec<u8>,
    passkey: serde_json::Value,
    sign_count: i64,
    name: Option<String>,
    last_used_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
