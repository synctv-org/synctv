use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{OpaquePasswordRecord, User, UserId},
    Error, Result,
};

#[derive(sqlx::FromRow)]
struct PasswordCredentialStateRow {
    user_id: UserId,
    changed_at: DateTime<Utc>,
    version: i32,
}

impl From<PasswordCredentialStateRow> for PasswordCredentialState {
    fn from(row: PasswordCredentialStateRow) -> Self {
        Self {
            user_id: row.user_id,
            changed_at: row.changed_at,
            version: row.version,
        }
    }
}

#[derive(Clone, Copy)]
pub enum PasswordCredentialMaterial<'a> {
    None,
    Opaque {
        opaque_record: &'a OpaquePasswordRecord,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct PasswordCredentialParts<'a> {
    pub(crate) opaque_record: Option<&'a OpaquePasswordRecord>,
}

#[derive(Debug, Clone)]
pub struct StoredOpaquePasswordCredential {
    pub record: OpaquePasswordRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordCredentialState {
    pub user_id: UserId,
    pub changed_at: DateTime<Utc>,
    pub version: i32,
}

impl<'a> PasswordCredentialMaterial<'a> {
    #[must_use]
    pub const fn opaque_only(opaque_record: &'a OpaquePasswordRecord) -> Self {
        Self::Opaque { opaque_record }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    pub(crate) fn parts(self) -> PasswordCredentialParts<'a> {
        match self {
            Self::None => PasswordCredentialParts {
                opaque_record: None,
            },
            Self::Opaque { opaque_record } => PasswordCredentialParts {
                opaque_record: Some(opaque_record),
            },
        }
    }
}

#[derive(Clone)]
pub struct UserPasswordRepository {
    pool: PgPool,
}

impl UserPasswordRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn create_for_user_with_executor<'e, E>(
        &self,
        user: &User,
        credentials: PasswordCredentialMaterial<'_>,
        executor: E,
    ) -> Result<Option<PasswordCredentialState>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let credentials = credentials.parts();
        let opaque_record = credentials.opaque_record;
        let opaque_record_bytes = opaque_record.map(|record| record.record.as_slice());
        let opaque_identifier = opaque_record.map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = opaque_record.map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = opaque_record.map(|record| record.server_setup_version);

        let row = sqlx::query_as!(
            PasswordCredentialStateRow,
            r#"
            INSERT INTO auth_password_credentials (
                user_id, opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                opaque_server_setup_version, changed_at, version,
                created_at, updated_at
            )
            SELECT $1, $2, $3, $4, $5, $7, 0, $6, $7
            WHERE $2::BYTEA IS NOT NULL
            RETURNING user_id AS "user_id!: UserId",
                      changed_at AS "changed_at!",
                      version AS "version!"
            "#,
            user.id.as_i64(),
            opaque_record_bytes,
            opaque_identifier,
            opaque_ciphersuite,
            opaque_server_setup_version,
            user.created_at,
            user.updated_at
        )
        .fetch_optional(executor)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn get_opaque_credential(
        &self,
        user_id: &UserId,
    ) -> Result<Option<StoredOpaquePasswordCredential>> {
        let row = sqlx::query!(
            r#"
            SELECT opaque_record as "opaque_record!",
                   opaque_credential_identifier as "opaque_credential_identifier!",
                   opaque_ciphersuite as "opaque_ciphersuite!",
                   opaque_server_setup_version as "opaque_server_setup_version!"
            FROM auth_password_credentials
            WHERE user_id = $1
              AND opaque_record IS NOT NULL
              AND opaque_credential_identifier IS NOT NULL
              AND opaque_ciphersuite IS NOT NULL
              AND opaque_server_setup_version IS NOT NULL
            "#,
            user_id as &UserId,
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            Ok(StoredOpaquePasswordCredential {
                record: OpaquePasswordRecord {
                    record: row.opaque_record,
                    credential_identifier: row.opaque_credential_identifier,
                    ciphersuite: row.opaque_ciphersuite,
                    server_setup_version: row.opaque_server_setup_version,
                },
            })
        })
        .transpose()
        .map_err(Error::Database)
    }

    pub async fn has_opaque_credential(&self, user_id: &UserId) -> Result<bool> {
        sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM auth_password_credentials
                WHERE user_id = $1
                  AND opaque_record IS NOT NULL
                  AND opaque_credential_identifier IS NOT NULL
                  AND opaque_ciphersuite IS NOT NULL
                  AND opaque_server_setup_version IS NOT NULL
            ) AS "exists!"
            "#,
            user_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_state(&self, user_id: &UserId) -> Result<PasswordCredentialState> {
        let row = sqlx::query_as!(
            PasswordCredentialStateRow,
            r#"
            SELECT u.id AS "user_id!: UserId",
                   COALESCE(apc.changed_at, u.created_at) AS "changed_at!",
                   COALESCE(apc.version, 0) AS "version!"
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            "#,
            user_id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(row.into())
    }

    pub async fn get_state_for_update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<PasswordCredentialState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query_as!(
            PasswordCredentialStateRow,
            r#"
            SELECT u.id AS "user_id!: UserId",
                   COALESCE(apc.changed_at, u.created_at) AS "changed_at!",
                   COALESCE(apc.version, 0) AS "version!"
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            FOR UPDATE OF u
            "#,
            user_id.as_i64()
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(row.into())
    }

    pub async fn update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        credentials: PasswordCredentialMaterial<'_>,
        executor: E,
    ) -> Result<PasswordCredentialState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = crate::SystemClock.now();
        let credentials = credentials.parts();
        let opaque_record = credentials.opaque_record;
        let opaque_record_bytes = opaque_record.map(|record| record.record.as_slice());
        let opaque_identifier = opaque_record.map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = opaque_record.map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = opaque_record.map(|record| record.server_setup_version);

        let row = sqlx::query_as!(
            PasswordCredentialStateRow,
            r#"
            WITH existing_user AS (
                SELECT id
                FROM users
                WHERE id = $1 AND deleted_at IS NULL
            )
            INSERT INTO auth_password_credentials (
                user_id, opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                opaque_server_setup_version, changed_at, version,
                created_at, updated_at
            )
            SELECT u.id, $2, $3, $4, $5, $6, COALESCE(existing_apc.version, 0) + 1, $6, $6
            FROM existing_user u
            LEFT JOIN auth_password_credentials existing_apc ON existing_apc.user_id = u.id
            ON CONFLICT (user_id) DO UPDATE
            SET opaque_record = EXCLUDED.opaque_record,
                opaque_credential_identifier = EXCLUDED.opaque_credential_identifier,
                opaque_ciphersuite = EXCLUDED.opaque_ciphersuite,
                opaque_server_setup_version = EXCLUDED.opaque_server_setup_version,
                changed_at = EXCLUDED.changed_at,
                version = auth_password_credentials.version + 1,
                updated_at = EXCLUDED.updated_at
            RETURNING user_id AS "user_id!: UserId",
                      changed_at AS "changed_at!",
                      version AS "version!"
            "#,
            user_id.as_i64(),
            opaque_record_bytes,
            opaque_identifier,
            opaque_ciphersuite,
            opaque_server_setup_version,
            now
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(row.into())
    }
}
