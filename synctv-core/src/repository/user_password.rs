use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{OpaquePasswordRecord, SignupMethod, User, UserId, UserRole, UserStatus},
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

#[derive(sqlx::FromRow)]
struct UserWithPasswordCredentialRow {
    id: UserId,
    username: String,
    signup_method: SignupMethod,
    role: UserRole,
    avatar_file_reference_id: Option<i64>,
    status: UserStatus,
    is_banned: bool,
    banned_at: Option<DateTime<Utc>>,
    banned_by: Option<UserId>,
    banned_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    user_version: i32,
    deleted_at: Option<DateTime<Utc>>,
    changed_at: Option<DateTime<Utc>>,
    credential_version: Option<i32>,
    opaque_record: Option<Vec<u8>>,
    opaque_credential_identifier: Option<Vec<u8>>,
    opaque_ciphersuite: Option<String>,
    opaque_server_setup_version: Option<i32>,
}

impl UserWithPasswordCredentialRow {
    fn to_user(&self) -> User {
        User {
            id: self.id,
            username: self.username.clone(),
            role: self.role,
            avatar_file_reference_id: self.avatar_file_reference_id,
            status: self.status,
            is_banned: self.is_banned,
            banned_at: self.banned_at,
            banned_by: self.banned_by,
            banned_reason: self.banned_reason.clone(),
            signup_method: self.signup_method,
            created_at: self.created_at,
            updated_at: self.updated_at,
            version: self.user_version,
            deleted_at: self.deleted_at,
        }
    }
}

impl TryFrom<UserWithPasswordCredentialRow> for UserWithPasswordCredential {
    type Error = Error;

    fn try_from(row: UserWithPasswordCredentialRow) -> Result<Self> {
        let user = row.to_user();
        let user_id = row.id;
        let user_created_at = row.created_at;
        let credential_state = match (row.changed_at, row.credential_version) {
            (Some(changed_at), Some(version)) => PasswordCredentialState {
                user_id,
                changed_at,
                version,
            },
            (None, None) => PasswordCredentialState {
                user_id,
                changed_at: user_created_at,
                version: 0,
            },
            _ => {
                return Err(Error::Internal(
                    "Incomplete password credential state".to_string(),
                ));
            }
        };
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
            user,
            credential_state,
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

    pub async fn get_by_username_with_credential(
        &self,
        username: &str,
    ) -> Result<Option<UserWithPasswordCredential>> {
        let row = sqlx::query_as!(
            UserWithPasswordCredentialRow,
            r#"
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.signup_method AS "signup_method!: SignupMethod",
                   u.role AS "role!: UserRole",
                   u.avatar_file_reference_id,
                   u.status AS "status!: UserStatus",
                   u.is_banned AS "is_banned!",
                   u.banned_at,
                   u.banned_by AS "banned_by?: UserId",
                   u.banned_reason,
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "user_version!",
                   u.deleted_at,
                   apc.changed_at,
                   apc.version AS credential_version,
                   apc.opaque_record,
                   apc.opaque_credential_identifier,
                   apc.opaque_ciphersuite,
                   apc.opaque_server_setup_version
            FROM user_account_profiles u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            WHERE LOWER(u.username) = LOWER($1) AND u.deleted_at IS NULL
            "#,
            username
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn get_by_email_with_credential(
        &self,
        email: &str,
    ) -> Result<Option<UserWithPasswordCredential>> {
        let row = sqlx::query_as!(
            UserWithPasswordCredentialRow,
            r#"
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.signup_method AS "signup_method!: SignupMethod",
                   u.role AS "role!: UserRole",
                   u.avatar_file_reference_id,
                   u.status AS "status!: UserStatus",
                   u.is_banned AS "is_banned!",
                   u.banned_at,
                   u.banned_by AS "banned_by?: UserId",
                   u.banned_reason,
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "user_version!",
                   u.deleted_at,
                   apc.changed_at,
                   apc.version AS credential_version,
                   apc.opaque_record,
                   apc.opaque_credential_identifier,
                   apc.opaque_ciphersuite,
                   apc.opaque_server_setup_version
            FROM user_account_profiles u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL AND aei.deleted_at IS NULL
            "#,
            email
        )
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

    pub async fn has_credential(&self, user_id: &UserId) -> Result<bool> {
        self.has_opaque_credential(user_id).await
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
