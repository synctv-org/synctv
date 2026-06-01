use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::user::USER_SELECT_COLUMNS;
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

#[derive(Debug, Clone)]
pub struct UserWithPasswordCredential {
    pub user: User,
    pub credential_state: PasswordCredentialState,
    pub opaque: Option<StoredOpaquePasswordCredential>,
}

struct UserWithPasswordCredentialRow {
    user: User,
    changed_at: Option<DateTime<Utc>>,
    version: Option<i32>,
    opaque_record: Option<Vec<u8>>,
    opaque_credential_identifier: Option<Vec<u8>>,
    opaque_ciphersuite: Option<String>,
    opaque_server_setup_version: Option<i32>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for UserWithPasswordCredentialRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            user: User::from_row(row)?,
            changed_at: row.try_get("changed_at")?,
            version: row.try_get("version")?,
            opaque_record: row.try_get("opaque_record")?,
            opaque_credential_identifier: row.try_get("opaque_credential_identifier")?,
            opaque_ciphersuite: row.try_get("opaque_ciphersuite")?,
            opaque_server_setup_version: row.try_get("opaque_server_setup_version")?,
        })
    }
}

impl TryFrom<UserWithPasswordCredentialRow> for UserWithPasswordCredential {
    type Error = Error;

    fn try_from(row: UserWithPasswordCredentialRow) -> Result<Self> {
        let opaque = match (
            row.opaque_record,
            row.opaque_credential_identifier,
            row.opaque_ciphersuite,
            row.opaque_server_setup_version,
        ) {
            (Some(record), Some(credential_identifier), Some(ciphersuite), Some(version)) => {
                Some(StoredOpaquePasswordCredential {
                    record: OpaquePasswordRecord {
                        record,
                        credential_identifier,
                        ciphersuite,
                        server_setup_version: version,
                    },
                })
            }
            (None, None, None, None) => None,
            _ => {
                return Err(Error::Internal(
                    "Incomplete OPAQUE password credential material".to_string(),
                ));
            }
        };

        Ok(Self {
            credential_state: PasswordCredentialState {
                user_id: row.user.id,
                changed_at: row.changed_at.unwrap_or(row.user.created_at),
                version: row.version.unwrap_or(0),
            },
            user: row.user,
            opaque,
        })
    }
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

        let row = sqlx::query_as::<_, PasswordCredentialStateRow>(
            r"
            INSERT INTO auth_password_credentials (
                user_id, opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                opaque_server_setup_version, changed_at, version,
                created_at, updated_at
            )
            SELECT $1, $2, $3, $4, $5, $7, 0, $6, $7
            WHERE $2::BYTEA IS NOT NULL
            RETURNING user_id, changed_at, version
            ",
        )
        .bind(user.id)
        .bind(opaque_record_bytes)
        .bind(opaque_identifier)
        .bind(opaque_ciphersuite)
        .bind(opaque_server_setup_version)
        .bind(user.created_at)
        .bind(user.updated_at)
        .fetch_optional(executor)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn get_by_username_with_credential(
        &self,
        username: &str,
    ) -> Result<Option<UserWithPasswordCredential>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS},
                   apc.changed_at,
                   apc.version,
                   apc.opaque_record,
                   apc.opaque_credential_identifier,
                   apc.opaque_ciphersuite,
                   apc.opaque_server_setup_version
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE LOWER(u.username) = LOWER($1) AND u.deleted_at IS NULL
            "
        );
        let row = sqlx::query_as::<_, UserWithPasswordCredentialRow>(&sql)
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn get_by_email_with_credential(
        &self,
        email: &str,
    ) -> Result<Option<UserWithPasswordCredential>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS},
                   apc.changed_at,
                   apc.version,
                   apc.opaque_record,
                   apc.opaque_credential_identifier,
                   apc.opaque_ciphersuite,
                   apc.opaque_server_setup_version
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "
        );
        let row = sqlx::query_as::<_, UserWithPasswordCredentialRow>(&sql)
            .bind(email)
            .fetch_optional(&self.pool)
            .await?;

        row.map(TryInto::try_into).transpose()
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
        sqlx::query_scalar::<_, bool>(
            r"
            SELECT EXISTS(
                SELECT 1
                FROM auth_password_credentials
                WHERE user_id = $1
                  AND opaque_record IS NOT NULL
                  AND opaque_credential_identifier IS NOT NULL
                  AND opaque_ciphersuite IS NOT NULL
                  AND opaque_server_setup_version IS NOT NULL
            )
            ",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn has_credential(&self, user_id: &UserId) -> Result<bool> {
        sqlx::query_scalar::<_, bool>(
            r"
            SELECT EXISTS(
                SELECT 1
                FROM auth_password_credentials
                WHERE user_id = $1
                  AND opaque_record IS NOT NULL
                  AND opaque_credential_identifier IS NOT NULL
                  AND opaque_ciphersuite IS NOT NULL
                  AND opaque_server_setup_version IS NOT NULL
            )
            ",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_state(&self, user_id: &UserId) -> Result<PasswordCredentialState> {
        let row = sqlx::query_as::<_, PasswordCredentialStateRow>(
            r"
            SELECT u.id AS user_id,
                   COALESCE(apc.changed_at, u.created_at) AS changed_at,
                   COALESCE(apc.version, 0) AS version
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            ",
        )
        .bind(user_id)
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
        let row = sqlx::query_as::<_, PasswordCredentialStateRow>(
            r"
            SELECT u.id AS user_id,
                   COALESCE(apc.changed_at, u.created_at) AS changed_at,
                   COALESCE(apc.version, 0) AS version
            FROM users u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            FOR UPDATE OF u
            ",
        )
        .bind(user_id)
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
        let now = Utc::now();
        let credentials = credentials.parts();
        let opaque_record = credentials.opaque_record;
        let opaque_record_bytes = opaque_record.map(|record| record.record.as_slice());
        let opaque_identifier = opaque_record.map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = opaque_record.map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = opaque_record.map(|record| record.server_setup_version);

        let row = sqlx::query_as::<_, PasswordCredentialStateRow>(
            r"
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
            RETURNING user_id, changed_at, version
            ",
        )
        .bind(user_id)
        .bind(opaque_record_bytes)
        .bind(opaque_identifier)
        .bind(opaque_ciphersuite)
        .bind(opaque_server_setup_version)
        .bind(now)
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(row.into())
    }
}
