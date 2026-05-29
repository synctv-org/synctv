use chrono::Utc;
use sqlx::PgPool;

use super::query_builder::{escape_ilike, WhereClauseBuilder};
use crate::{
    models::{OpaquePasswordRecord, User, UserId, UserListQuery, UserListSortBy},
    Error, Result,
};

pub(crate) const USER_SELECT_COLUMNS: &str = "
    u.id, u.username, aei.email,
    COALESCE(apc.legacy_password_hash, '') AS password_hash,
    u.signup_method, u.role,
    u.created_at, u.updated_at,
    COALESCE(apc.password_changed_at, u.created_at) AS password_changed_at,
    COALESCE(apc.password_version, 0) AS password_version,
    u.version, u.deleted_at, COALESCE(aei.email_verified, false) AS email_verified,
    EXISTS (
        SELECT 1 FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
    ) AS is_banned,
    (
        SELECT ub.starts_at FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_at,
    (
        SELECT ub.banned_by FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_by,
    (
        SELECT ub.reason FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_reason";

const USER_SELECT_COLUMNS_WITH_UPDATED_PASSWORD_CREDENTIAL: &str = "
    u.id, u.username, aei.email,
    CASE
        WHEN updated_apc.user_id IS NOT NULL THEN COALESCE(updated_apc.legacy_password_hash, '')
        ELSE COALESCE(existing_apc.legacy_password_hash, '')
    END AS password_hash,
    u.signup_method, u.role,
    u.created_at, u.updated_at,
    COALESCE(updated_apc.password_changed_at, existing_apc.password_changed_at, u.created_at) AS password_changed_at,
    COALESCE(updated_apc.password_version, existing_apc.password_version, 0) AS password_version,
    u.version, u.deleted_at, COALESCE(aei.email_verified, false) AS email_verified,
    EXISTS (
        SELECT 1 FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
    ) AS is_banned,
    (
        SELECT ub.starts_at FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_at,
    (
        SELECT ub.banned_by FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_by,
    (
        SELECT ub.reason FROM user_bans ub
        WHERE ub.user_id = u.id
          AND ub.revoked_at IS NULL
          AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ORDER BY ub.starts_at DESC
        LIMIT 1
    ) AS banned_reason";

const AUTH_PASSWORD_CREDENTIAL_JOIN: &str =
    "LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
     LEFT JOIN auth_email_identities aei ON aei.user_id = u.id";

pub(crate) const USER_ROW_RETURNING_COLUMNS: &str = "
    id, username, signup_method, role, created_at, updated_at,
    version, deleted_at";

#[derive(Clone, Copy)]
pub enum PasswordCredentialMaterial<'a> {
    None,
    LegacyOnly {
        legacy_password_hash: &'a str,
    },
    OpaqueOnly {
        opaque_record: &'a OpaquePasswordRecord,
    },
    LegacyAndOpaque {
        legacy_password_hash: &'a str,
        opaque_record: &'a OpaquePasswordRecord,
    },
}

#[derive(Clone, Copy)]
struct PasswordCredentialParts<'a> {
    legacy_password_hash: Option<&'a str>,
    opaque_record: Option<&'a OpaquePasswordRecord>,
}

#[derive(Debug, Clone)]
pub struct StoredOpaquePasswordCredential {
    pub record: OpaquePasswordRecord,
}

impl<'a> PasswordCredentialMaterial<'a> {
    #[must_use]
    pub const fn legacy_only(legacy_password_hash: &'a str) -> Self {
        Self::LegacyOnly {
            legacy_password_hash,
        }
    }

    #[must_use]
    pub const fn opaque_only(opaque_record: &'a OpaquePasswordRecord) -> Self {
        Self::OpaqueOnly { opaque_record }
    }

    #[must_use]
    pub const fn legacy_and_opaque(
        legacy_password_hash: &'a str,
        opaque_record: &'a OpaquePasswordRecord,
    ) -> Self {
        Self::LegacyAndOpaque {
            legacy_password_hash,
            opaque_record,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    fn parts(self) -> PasswordCredentialParts<'a> {
        match self {
            Self::None => PasswordCredentialParts {
                legacy_password_hash: None,
                opaque_record: None,
            },
            Self::LegacyOnly {
                legacy_password_hash,
            } => PasswordCredentialParts {
                legacy_password_hash: Some(legacy_password_hash),
                opaque_record: None,
            },
            Self::OpaqueOnly { opaque_record } => PasswordCredentialParts {
                legacy_password_hash: None,
                opaque_record: Some(opaque_record),
            },
            Self::LegacyAndOpaque {
                legacy_password_hash,
                opaque_record,
            } => PasswordCredentialParts {
                legacy_password_hash: Some(legacy_password_hash),
                opaque_record: Some(opaque_record),
            },
        }
    }
}

const ACTIVE_USER_BAN_EXISTS_SQL: &str = "EXISTS (
    SELECT 1 FROM user_bans ub
    WHERE ub.user_id = u.id
      AND ub.revoked_at IS NULL
      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
)";
const ACTIVE_USER_BAN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM user_bans ub
    WHERE ub.user_id = u.id
      AND ub.revoked_at IS NULL
      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
)";

/// User repository for database operations
#[derive(Clone)]
pub struct UserRepository {
    pool: PgPool,
}

impl UserRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get the database pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create a new user
    ///
    /// Relies on database UNIQUE constraints on `username` and `email` columns
    /// to prevent duplicates atomically (no TOCTOU race condition).
    pub async fn create(&self, user: &User) -> Result<User> {
        self.create_with_executor(user, &self.pool).await
    }

    /// Create a new user using a provided executor (pool or transaction)
    ///
    /// Relies on database UNIQUE constraints on `username` and `email` columns
    /// to prevent duplicates atomically (no TOCTOU race condition).
    pub async fn create_with_executor<'e, E>(&self, user: &User, executor: E) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let credentials = if user.password_hash.trim().is_empty() {
            PasswordCredentialMaterial::none()
        } else {
            PasswordCredentialMaterial::legacy_only(&user.password_hash)
        };
        self.create_with_password_credentials(user, credentials, executor)
            .await
    }

    pub async fn create_with_password_credentials<'e, E>(
        &self,
        user: &User,
        credentials: PasswordCredentialMaterial<'_>,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let credentials = credentials.parts();
        let opaque_record_bytes = credentials
            .opaque_record
            .map(|record| record.record.as_slice());
        let opaque_identifier = credentials
            .opaque_record
            .map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = credentials
            .opaque_record
            .map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = credentials
            .opaque_record
            .map(|record| record.server_setup_version);
        let sql = format!(
            r"
            WITH inserted_user AS (
                INSERT INTO users (username, signup_method, role, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING {USER_ROW_RETURNING_COLUMNS}
            ),
            inserted_email_identity AS (
                INSERT INTO auth_email_identities (
                    user_id, email, email_verified, created_at, updated_at
                )
                SELECT id, $6, $7, $4, $5
                FROM inserted_user
                WHERE NULLIF($6::TEXT, '') IS NOT NULL
                RETURNING *
            ),
            inserted_password_credential AS (
                INSERT INTO auth_password_credentials (
                    user_id, legacy_password_hash, legacy_password_algorithm,
                    opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                    opaque_server_setup_version, password_changed_at, password_version,
                    created_at, updated_at
                )
                SELECT id, $8, CASE WHEN $8::TEXT IS NULL THEN NULL ELSE 'argon2id' END,
                       $9, $10, $11, $12, $5, 0, $4, $5
                FROM inserted_user
                WHERE NULLIF($8::TEXT, '') IS NOT NULL OR $9::BYTEA IS NOT NULL
                RETURNING *
            )
            SELECT {USER_SELECT_COLUMNS}
            FROM inserted_user u
            LEFT JOIN inserted_password_credential apc ON apc.user_id = u.id
            LEFT JOIN inserted_email_identity aei ON aei.user_id = u.id
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(&user.username)
            .bind(user.signup_method)
            .bind(user.role)
            .bind(user.created_at)
            .bind(user.updated_at)
            .bind(user.email.as_ref())
            .bind(user.email_verified)
            .bind(credentials.legacy_password_hash)
            .bind(opaque_record_bytes)
            .bind(opaque_identifier)
            .bind(opaque_ciphersuite)
            .bind(opaque_server_setup_version)
            .fetch_one(executor)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                    let constraint = db_err.constraint().unwrap_or("");
                    if constraint.contains("username") {
                        Error::AlreadyExists("Username already taken".to_string())
                    } else if constraint.contains("email") {
                        Error::AlreadyExists("Email already taken".to_string())
                    } else {
                        Error::AlreadyExists(
                            synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                        )
                    }
                }
                _ => Error::Database(e),
            })?;

        Ok(u)
    }

    /// Get user by ID
    pub async fn get_by_id(&self, user_id: &UserId) -> Result<Option<User>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE u.id = $1 AND u.deleted_at IS NULL
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(u)
    }

    /// Get user by ID using a provided executor and lock the row for update.
    pub async fn get_by_id_for_update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<Option<User>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE u.id = $1 AND u.deleted_at IS NULL
            FOR UPDATE OF u
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .fetch_optional(executor)
            .await?;

        Ok(u)
    }

    /// Get multiple users by IDs in a single batch query
    pub async fn get_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = user_ids
            .iter()
            .map(super::super::models::id::UserId::as_i64)
            .collect();
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE u.id = ANY($1) AND u.deleted_at IS NULL
            "
        );
        let users = sqlx::query_as::<_, User>(&sql)
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?;

        Ok(users)
    }

    /// Get user by username
    pub async fn get_by_username(&self, username: &str) -> Result<Option<User>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE LOWER(u.username) = LOWER($1) AND u.deleted_at IS NULL
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;

        Ok(u)
    }

    /// Get user by email
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(email)
            .fetch_optional(&self.pool)
            .await?;

        Ok(u)
    }

    pub async fn get_opaque_password_credential(
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

    pub async fn has_opaque_password_credential(&self, user_id: &UserId) -> Result<bool> {
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
            ) as "exists!"
            "#,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Update user with optimistic locking.
    ///
    /// The caller must pass the `version` value from the previously-read user.
    /// The update atomically increments `version` in the database and only
    /// succeeds when the row's `version` still matches `old_version`.
    ///
    /// Using an integer version column avoids two problems with timestamp-based
    /// locking:
    /// - Clock skew between the DB server and app server causing spurious conflicts.
    /// - Two updates in the same millisecond both seeing the same timestamp.
    ///
    /// Returns `Error::OptimisticLockConflict` when another concurrent update
    /// already changed the row, so the caller can retry with a fresh read.
    pub async fn update(&self, user: &User, old_version: i32) -> Result<User> {
        self.update_with_executor(user, old_version, &self.pool)
            .await
    }

    /// Update only the user's global role with optimistic locking.
    pub async fn update_role(
        &self,
        user_id: &UserId,
        role: crate::models::UserRole,
        old_version: i32,
    ) -> Result<User> {
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET role = $2,
                    updated_at = $3,
                    version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $4
                RETURNING {USER_ROW_RETURNING_COLUMNS}
            )
            SELECT {USER_SELECT_COLUMNS}
            FROM updated_user u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .bind(role)
            .bind(Utc::now())
            .bind(old_version)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {user_id} not found")))
            }
        }
    }

    /// Update user with optimistic locking using a provided executor.
    pub async fn update_with_executor<'e, E>(
        &self,
        user: &User,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET username = $2, role = $4,
                    updated_at = $6, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $7
                RETURNING {USER_ROW_RETURNING_COLUMNS}
            ),
            deleted_email_identity AS (
                DELETE FROM auth_email_identities
                USING updated_user
                WHERE auth_email_identities.user_id = updated_user.id
                  AND $3::TEXT IS NULL
            ),
            aei AS (
                INSERT INTO auth_email_identities (
                    user_id, email, email_verified, created_at, updated_at
                )
                SELECT id, $3, $5, $6, $6
                FROM updated_user
                WHERE $3::TEXT IS NOT NULL
                ON CONFLICT (user_id)
                DO UPDATE SET
                    email = EXCLUDED.email,
                    email_verified = EXCLUDED.email_verified,
                    updated_at = EXCLUDED.updated_at
                RETURNING user_id, email, email_verified
            )
            SELECT {USER_SELECT_COLUMNS}
            FROM updated_user u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            LEFT JOIN aei ON aei.user_id = u.id
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user.id)
            .bind(&user.username)
            .bind(user.email.as_ref())
            .bind(user.role)
            .bind(user.email_verified)
            .bind(Utc::now())
            .bind(old_version)
            .fetch_optional(executor)
            .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            // Check if the user exists at all to distinguish
            // "not found" from "concurrent modification"
            let exists = self.get_by_id(&user.id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {} not found", user.id)))
            }
        }
    }

    /// Update the user profile atomically with optimistic locking.
    ///
    /// Supports updating username alone, password alone, or both in one write.
    /// When `password_hash` is `Some`, the password metadata is updated and
    /// `password_version` is incremented exactly once in the same statement.
    pub async fn update_profile_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        username: &str,
        password_credentials: Option<PasswordCredentialMaterial<'_>>,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let password_credentials = password_credentials.map(PasswordCredentialMaterial::parts);
        let legacy_password_hash =
            password_credentials.and_then(|credentials| credentials.legacy_password_hash);
        let opaque_record = password_credentials.and_then(|credentials| credentials.opaque_record);
        let opaque_record_bytes = opaque_record.map(|record| record.record.as_slice());
        let opaque_identifier = opaque_record.map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = opaque_record.map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = opaque_record.map(|record| record.server_setup_version);
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET username = $2,
                    updated_at = $8,
                    version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $9
                RETURNING {USER_ROW_RETURNING_COLUMNS}
            ),
            updated_password_credential AS (
                INSERT INTO auth_password_credentials (
                    user_id, legacy_password_hash, legacy_password_algorithm,
                    opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                    opaque_server_setup_version, password_changed_at, password_version,
                    created_at, updated_at
                )
                SELECT u.id, $3, CASE WHEN $3::TEXT IS NULL THEN NULL ELSE 'argon2id' END,
                       $4, $5, $6, $7, $8, COALESCE(existing_apc.password_version, 0) + 1, $8, $8
                FROM updated_user u
                LEFT JOIN auth_password_credentials existing_apc ON existing_apc.user_id = u.id
                WHERE $3::TEXT IS NOT NULL OR $4::BYTEA IS NOT NULL
                ON CONFLICT (user_id) DO UPDATE
                SET legacy_password_hash = EXCLUDED.legacy_password_hash,
                    legacy_password_algorithm = EXCLUDED.legacy_password_algorithm,
                    opaque_record = EXCLUDED.opaque_record,
                    opaque_credential_identifier = EXCLUDED.opaque_credential_identifier,
                    opaque_ciphersuite = EXCLUDED.opaque_ciphersuite,
                    opaque_server_setup_version = EXCLUDED.opaque_server_setup_version,
                    password_changed_at = EXCLUDED.password_changed_at,
                    password_version = auth_password_credentials.password_version + 1,
                    updated_at = EXCLUDED.updated_at
                RETURNING *
            )
            SELECT {USER_SELECT_COLUMNS_WITH_UPDATED_PASSWORD_CREDENTIAL}
            FROM updated_user u
            LEFT JOIN auth_password_credentials existing_apc ON existing_apc.user_id = u.id
            LEFT JOIN updated_password_credential updated_apc ON updated_apc.user_id = u.id
            LEFT JOIN auth_email_identities aei ON aei.user_id = u.id
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .bind(username)
            .bind(legacy_password_hash)
            .bind(opaque_record_bytes)
            .bind(opaque_identifier)
            .bind(opaque_ciphersuite)
            .bind(opaque_server_setup_version)
            .bind(now)
            .bind(old_version)
            .fetch_optional(executor)
            .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {user_id} not found")))
            }
        }
    }

    /// Soft delete user
    pub async fn delete(&self, user_id: &UserId) -> Result<bool> {
        self.delete_with_executor(user_id, &self.pool).await
    }

    /// Soft delete user using a provided executor (pool or transaction)
    pub async fn delete_with_executor<'e, E>(&self, user_id: &UserId, executor: E) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE users
            SET deleted_at = $2, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            ",
            user_id as &UserId,
            Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn update_password_credentials_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        credentials: PasswordCredentialMaterial<'_>,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let credentials = credentials.parts();
        let legacy_password_hash = credentials.legacy_password_hash;
        let opaque_record = credentials.opaque_record;
        let opaque_record_bytes = opaque_record.map(|record| record.record.as_slice());
        let opaque_identifier = opaque_record.map(|record| record.credential_identifier.as_slice());
        let opaque_ciphersuite = opaque_record.map(|record| record.ciphersuite.as_str());
        let opaque_server_setup_version = opaque_record.map(|record| record.server_setup_version);
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET updated_at = $7, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING {USER_ROW_RETURNING_COLUMNS}
            ),
            updated_password_credential AS (
                INSERT INTO auth_password_credentials (
                    user_id, legacy_password_hash, legacy_password_algorithm,
                    opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                    opaque_server_setup_version, password_changed_at, password_version,
                    created_at, updated_at
                )
                SELECT u.id, $2, CASE WHEN $2::TEXT IS NULL THEN NULL ELSE 'argon2id' END,
                       $3, $4, $5, $6, $7, COALESCE(existing_apc.password_version, 0) + 1, $7, $7
                FROM updated_user u
                LEFT JOIN auth_password_credentials existing_apc ON existing_apc.user_id = u.id
                ON CONFLICT (user_id) DO UPDATE
                SET legacy_password_hash = EXCLUDED.legacy_password_hash,
                    legacy_password_algorithm = EXCLUDED.legacy_password_algorithm,
                    opaque_record = EXCLUDED.opaque_record,
                    opaque_credential_identifier = EXCLUDED.opaque_credential_identifier,
                    opaque_ciphersuite = EXCLUDED.opaque_ciphersuite,
                    opaque_server_setup_version = EXCLUDED.opaque_server_setup_version,
                    password_changed_at = EXCLUDED.password_changed_at,
                    password_version = auth_password_credentials.password_version + 1,
                    updated_at = EXCLUDED.updated_at
                RETURNING *
            )
            SELECT {USER_SELECT_COLUMNS_WITH_UPDATED_PASSWORD_CREDENTIAL}
            FROM updated_user u
            LEFT JOIN auth_password_credentials existing_apc ON existing_apc.user_id = u.id
            LEFT JOIN updated_password_credential updated_apc ON updated_apc.user_id = u.id
            LEFT JOIN auth_email_identities aei ON aei.user_id = u.id
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .bind(legacy_password_hash)
            .bind(opaque_record_bytes)
            .bind(opaque_identifier)
            .bind(opaque_ciphersuite)
            .bind(opaque_server_setup_version)
            .bind(now)
            .fetch_optional(executor)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(u)
    }

    /// Update user email verification status
    pub async fn update_email_verified(
        &self,
        user_id: &UserId,
        email_verified: bool,
    ) -> Result<User> {
        let sql = format!(
            r"
            WITH aei AS (
                UPDATE auth_email_identities
                SET email_verified = $2, updated_at = $3
                WHERE user_id = $1
                RETURNING user_id, email, email_verified
            ),
            updated_user AS (
                UPDATE users u
                SET updated_at = $3, version = version + 1
                FROM aei
                WHERE u.id = aei.user_id AND u.deleted_at IS NULL
                RETURNING u.{USER_ROW_RETURNING_COLUMNS}
            )
            SELECT {USER_SELECT_COLUMNS}
            FROM updated_user u
            LEFT JOIN auth_password_credentials apc ON apc.user_id = u.id
            LEFT JOIN aei ON aei.user_id = u.id
            "
        );
        let u = sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .bind(email_verified)
            .bind(Utc::now())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(u)
    }

    /// Globally ban a user without changing lifecycle status.
    pub async fn ban(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<User> {
        self.insert_ban_with_executor(user_id, banned_by, reason, &self.pool)
            .await?;
        self.get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    /// Insert a global ban record using a provided executor.
    pub async fn insert_ban_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let lock_key = format!("user-ban:{user_id}");
        let inserted = sqlx::query(
            r"
            WITH _lock AS (
                SELECT pg_advisory_xact_lock(hashtextextended($5, 0))
            )
            INSERT INTO user_bans (user_id, banned_by, reason, starts_at)
            SELECT u.id, $2, $3, $4
            FROM users u, _lock
            WHERE u.id = $1 AND u.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM user_bans ub
                  WHERE ub.user_id = u.id
                    AND ub.revoked_at IS NULL
                    AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
            ",
        )
        .bind(user_id.as_i64())
        .bind(banned_by.map(UserId::as_i64))
        .bind(reason)
        .bind(now)
        .bind(lock_key)
        .execute(executor)
        .await?;

        if inserted.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "User {user_id} not found or already banned"
            )));
        }

        Ok(())
    }

    /// Clear a global user ban without changing lifecycle status.
    pub async fn unban(&self, user_id: &UserId) -> Result<User> {
        let result = sqlx::query!(
            r"
            UPDATE user_bans ub
            SET revoked_at = $2
            FROM users u
            WHERE ub.user_id = u.id
              AND u.id = $1
              AND u.deleted_at IS NULL
              AND ub.revoked_at IS NULL
              AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
            ",
            user_id as &UserId,
            Utc::now(),
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "User {user_id} not found or not banned"
            )));
        }

        let u = self
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(u)
    }

    /// Check whether a user has an active global ban.
    pub async fn is_banned(&self, user_id: &UserId) -> Result<bool> {
        let is_banned = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM user_bans ub
                JOIN users u ON u.id = ub.user_id
                WHERE u.id = $1
                  AND u.deleted_at IS NULL
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
            ) as "exists!"
            "#,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(is_banned)
    }

    /// Build the shared WHERE clause conditions for user list queries.
    fn build_user_list_conditions(query: &UserListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("u.deleted_at IS NULL");

        if query.search.is_some() {
            wb.push_param("(u.username ILIKE ${idx} OR aei.email ILIKE ${idx})");
        }
        if query.role.is_some() {
            wb.push_param("u.role = ${idx}");
        }
        match query.status {
            Some(crate::models::UserStatus::Active) => {
                wb.push_literal(ACTIVE_USER_BAN_NOT_EXISTS_SQL);
            }
            Some(crate::models::UserStatus::Banned) => {
                wb.push_literal(ACTIVE_USER_BAN_EXISTS_SQL);
            }
            None => {}
        }
        match query.is_banned {
            Some(true) => wb.push_literal(ACTIVE_USER_BAN_EXISTS_SQL),
            Some(false) => wb.push_literal(ACTIVE_USER_BAN_NOT_EXISTS_SQL),
            None => {}
        }

        wb
    }

    fn build_order_by(query: &UserListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            UserListSortBy::Username => format!("username {direction}, id {direction}"),
            UserListSortBy::Email => format!("email {direction} NULLS LAST, id {direction}"),
            UserListSortBy::Status => {
                format!("is_banned {direction}, created_at {direction}, id {direction}")
            }
            UserListSortBy::Role => {
                format!("role {direction}, created_at {direction}, id {direction}")
            }
            UserListSortBy::UpdatedAt => format!("updated_at {direction}, id {direction}"),
            UserListSortBy::CreatedAt => format!("created_at {direction}, id {direction}"),
        }
    }

    /// Bind the filter parameters (search, status, role) onto a sqlx query in order.
    /// This is used by both count and list queries to avoid duplicating bind logic.
    fn bind_user_filters<'q, O>(
        mut qb: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
        search_pattern: Option<&'q String>,
        role_enum: Option<&'q crate::models::UserRole>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        if let Some(pattern) = search_pattern {
            qb = qb.bind(pattern);
        }
        if let Some(role) = role_enum {
            qb = qb.bind(role);
        }
        qb
    }

    /// List users with pagination
    pub async fn list(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        let limit = i64::try_from(query.pagination.limit()).unwrap_or(i64::MAX);
        let offset = i64::try_from(query.pagination.offset()).unwrap_or(i64::MAX);

        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let wb = Self::build_user_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!(
            "SELECT COUNT(*) as count FROM users u LEFT JOIN auth_email_identities aei ON aei.user_id = u.id WHERE {count_where}"
        );

        // We need to use query_scalar which returns a different type, so bind manually
        let mut count_qb = sqlx::query_scalar::<_, i64>(&count_sql);
        if let Some(ref pattern) = search_pattern {
            count_qb = count_qb.bind(pattern);
        }
        if let Some(ref role) = &query.role {
            count_qb = count_qb.bind(role);
        }
        let count: i64 = count_qb.fetch_one(&self.pool).await?;

        // List query: $1=LIMIT, $2=OFFSET, then filters start at $3
        let (list_where, _) = wb.build(3);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}
            FROM users u
            {AUTH_PASSWORD_CREDENTIAL_JOIN}
            WHERE {list_where}
            ORDER BY {order_by}
            LIMIT $1 OFFSET $2
            "
        );

        // Use query_as with the shared bind helper
        let list_qb = sqlx::query_as::<_, User>(&list_sql)
            .bind(limit)
            .bind(offset);
        let list_qb =
            Self::bind_user_filters(list_qb, search_pattern.as_ref(), query.role.as_ref());
        let users: Vec<User> = list_qb.fetch_all(&self.pool).await?;

        Ok((users, count))
    }

    /// List admin-capable users (root + admin) with pagination.
    pub async fn list_admins(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        let limit = i64::try_from(query.pagination.limit()).unwrap_or(i64::MAX);
        let offset = i64::try_from(query.pagination.offset()).unwrap_or(i64::MAX);
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let order_by = Self::build_order_by(query);

        let mut count_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT COUNT(*) FROM users u LEFT JOIN auth_email_identities aei ON aei.user_id = u.id WHERE u.deleted_at IS NULL AND u.role IN (",
        );
        count_qb.push_bind(crate::models::UserRole::Root);
        count_qb.push(", ");
        count_qb.push_bind(crate::models::UserRole::Admin);
        count_qb.push(")");
        if let Some(pattern) = &search_pattern {
            count_qb.push(" AND (u.username ILIKE ");
            count_qb.push_bind(pattern.clone());
            count_qb.push(" OR aei.email ILIKE ");
            count_qb.push_bind(pattern.clone());
            count_qb.push(")");
        }
        let count = count_qb
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;

        let mut list_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users u {AUTH_PASSWORD_CREDENTIAL_JOIN} WHERE u.deleted_at IS NULL AND u.role IN ("
        ));
        list_qb.push_bind(crate::models::UserRole::Root);
        list_qb.push(", ");
        list_qb.push_bind(crate::models::UserRole::Admin);
        list_qb.push(")");
        if let Some(pattern) = &search_pattern {
            list_qb.push(" AND (u.username ILIKE ");
            list_qb.push_bind(pattern.clone());
            list_qb.push(" OR aei.email ILIKE ");
            list_qb.push_bind(pattern.clone());
            list_qb.push(")");
        }
        list_qb.push(format!(" ORDER BY {order_by} LIMIT "));
        list_qb.push_bind(limit);
        list_qb.push(" OFFSET ");
        list_qb.push_bind(offset);

        let users = list_qb
            .build_query_as::<User>()
            .fetch_all(&self.pool)
            .await?;

        Ok((users, count))
    }

    /// Check if username exists
    pub async fn username_exists(&self, username: &str) -> Result<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            r"
            SELECT EXISTS(
                SELECT 1
                FROM users
                WHERE LOWER(username) = LOWER($1) AND deleted_at IS NULL
            )
            ",
        )
        .bind(username)
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Check if email exists
    pub async fn email_exists(&self, email: &str) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM auth_email_identities aei
                JOIN users u ON u.id = aei.user_id
                WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            ) as "exists!"
            "#,
            email,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SignupMethod;
    use sqlx::Row;
    use synctv_core_testing::create_test_pool;

    #[test]
    fn test_build_user_list_conditions_no_filters() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        let (sql, next_idx) = wb.build(1);
        assert_eq!(sql, "u.deleted_at IS NULL");
        assert_eq!(next_idx, 1); // no params consumed
        assert_eq!(wb.param_count(), 0);
    }

    #[test]
    fn test_build_user_list_conditions_with_search() {
        let query = UserListQuery {
            search: Some("alice".to_string()),
            status: None,
            role: None,
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, next_idx) = wb.build(1);
        assert!(sql.contains("username ILIKE"));
        assert!(sql.contains("email ILIKE"));
        assert_eq!(next_idx, 2);
    }

    #[test]
    fn test_build_user_list_conditions_with_role() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: Some(crate::models::UserRole::Admin),
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, _) = wb.build(1);
        assert!(sql.contains("role = $1"));
    }

    #[test]
    fn test_build_user_list_conditions_with_active_status_filters_out_bans() {
        let query = UserListQuery {
            search: None,
            status: Some(crate::models::UserStatus::Active),
            role: None,
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);
        let (sql, _) = wb.build(1);

        assert!(sql.contains("NOT EXISTS"));
        assert!(sql.contains("user_bans"));
    }

    #[test]
    fn test_build_user_list_conditions_with_banned_status_requires_active_ban() {
        let query = UserListQuery {
            search: None,
            status: Some(crate::models::UserStatus::Banned),
            role: None,
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);
        let (sql, _) = wb.build(1);

        assert!(sql.contains("EXISTS"));
        assert!(!sql.contains("NOT EXISTS"));
        assert!(sql.contains("user_bans"));
    }

    #[test]
    fn test_build_user_list_conditions_all_filters() {
        let query = UserListQuery {
            search: Some("bob".to_string()),
            status: None,
            role: Some(crate::models::UserRole::User),
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 2);

        // Count query: params start at $1
        let (sql, next) = wb.build(1);
        assert!(sql.contains("deleted_at IS NULL"));
        assert!(sql.contains("username ILIKE $1"));
        assert!(sql.contains("role = $2"));
        assert_eq!(next, 3);

        // List query: params start at $3 (after LIMIT/OFFSET)
        let (sql, next) = wb.build(3);
        assert!(sql.contains("username ILIKE $3"));
        assert!(sql.contains("role = $4"));
        assert_eq!(next, 5);
    }

    #[test]
    fn test_build_order_by_uses_requested_sort() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            is_banned: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::Username,
            sort_direction: crate::models::SortDirection::Asc,
        };

        assert_eq!(
            UserRepository::build_order_by(&query),
            "username ASC, id ASC"
        );
    }

    #[test]
    fn test_user_list_order_clause_supports_username_ascending() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            is_banned: None,
            sort_by: crate::models::UserListSortBy::Username,
            sort_direction: crate::models::SortDirection::Asc,
            pagination: crate::models::PageParams::default(),
        };

        assert_eq!(
            UserRepository::build_order_by(&query),
            "username ASC, id ASC"
        );
    }

    #[test]
    fn test_user_list_order_clause_supports_effective_status() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            is_banned: None,
            sort_by: crate::models::UserListSortBy::Status,
            sort_direction: crate::models::SortDirection::Desc,
            pagination: crate::models::PageParams::default(),
        };

        assert_eq!(
            UserRepository::build_order_by(&query),
            "is_banned DESC, created_at DESC, id DESC"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new(
            "testuser".into(),
            Some("test@example.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        let created = repo.create(&user).await.unwrap();
        assert_eq!(created.username, "testuser");
        assert_eq!(created.email, Some("test@example.com".into()));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user_with_password_credentials_persists_legacy_and_opaque() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new(
            "credential_user".into(),
            Some("credential@example.com".into()),
            "legacy-hash".into(),
            SignupMethod::Email,
        );
        let opaque_record = OpaquePasswordRecord {
            record: b"opaque-record".to_vec(),
            credential_identifier: b"synctv:user:credential_user".to_vec(),
            ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
            server_setup_version: 1,
        };

        let created = repo
            .create_with_password_credentials(
                &user,
                PasswordCredentialMaterial::legacy_and_opaque(&user.password_hash, &opaque_record),
                &pool,
            )
            .await
            .expect("user should be created with password credentials");

        let row = sqlx::query(
            r"
            SELECT legacy_password_hash, legacy_password_algorithm, opaque_record,
                   opaque_credential_identifier, opaque_ciphersuite,
                   opaque_server_setup_version, password_version
            FROM auth_password_credentials
            WHERE user_id = $1
            ",
        )
        .bind(created.id.as_i64())
        .fetch_one(&pool)
        .await
        .expect("password credential row should exist");

        assert_eq!(
            row.try_get::<Option<String>, _>("legacy_password_hash")
                .unwrap(),
            Some("legacy-hash".to_string())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("legacy_password_algorithm")
                .unwrap(),
            Some("argon2id".to_string())
        );
        assert_eq!(
            row.try_get::<Option<Vec<u8>>, _>("opaque_record").unwrap(),
            Some(b"opaque-record".to_vec())
        );
        assert_eq!(
            row.try_get::<Option<Vec<u8>>, _>("opaque_credential_identifier")
                .unwrap(),
            Some(b"synctv:user:credential_user".to_vec())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("opaque_ciphersuite")
                .unwrap(),
            Some("opaque-ristretto255-sha512-argon2id".to_string())
        );
        assert_eq!(
            row.try_get::<Option<i32>, _>("opaque_server_setup_version")
                .unwrap(),
            Some(1)
        );
        assert_eq!(row.try_get::<i32, _>("password_version").unwrap(), 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_password_credentials_opaque_only_clears_legacy_password_material() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new(
            "opaque_update_user".into(),
            Some("opaque-update@example.com".into()),
            "legacy-hash".into(),
            SignupMethod::Email,
        );
        let initial_opaque_record = OpaquePasswordRecord {
            record: b"opaque-record-v1".to_vec(),
            credential_identifier: b"synctv:user:opaque_update_user".to_vec(),
            ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
            server_setup_version: 1,
        };
        let created = repo
            .create_with_password_credentials(
                &user,
                PasswordCredentialMaterial::legacy_and_opaque(
                    &user.password_hash,
                    &initial_opaque_record,
                ),
                &pool,
            )
            .await
            .expect("user should be created with legacy and OPAQUE credentials");

        let updated_opaque_record = OpaquePasswordRecord {
            record: b"opaque-record-v2".to_vec(),
            credential_identifier: b"synctv:user-id:42".to_vec(),
            ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
            server_setup_version: 1,
        };
        let updated = repo
            .update_password_credentials_with_executor(
                &created.id,
                PasswordCredentialMaterial::opaque_only(&updated_opaque_record),
                &pool,
            )
            .await
            .expect("opaque-only password update should succeed");
        assert_eq!(
            updated.password_hash, "",
            "opaque-only updates must clear the legacy password hash exposed on User"
        );

        let row = sqlx::query(
            r"
            SELECT legacy_password_hash, legacy_password_algorithm, opaque_record,
                   opaque_credential_identifier, password_version
            FROM auth_password_credentials
            WHERE user_id = $1
            ",
        )
        .bind(created.id.as_i64())
        .fetch_one(&pool)
        .await
        .expect("password credential row should exist");

        assert_eq!(
            row.try_get::<Option<String>, _>("legacy_password_hash")
                .unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("legacy_password_algorithm")
                .unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<Vec<u8>>, _>("opaque_record").unwrap(),
            Some(b"opaque-record-v2".to_vec())
        );
        assert_eq!(
            row.try_get::<Option<Vec<u8>>, _>("opaque_credential_identifier")
                .unwrap(),
            Some(b"synctv:user-id:42".to_vec())
        );
        assert_eq!(row.try_get::<i32, _>("password_version").unwrap(), 1);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_oauth2_user_without_password_skips_password_credentials() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new_with_status(
            "oauth_without_password".into(),
            Some("oauth@example.com".into()),
            String::new(),
            SignupMethod::OAuth2,
            crate::models::UserStatus::Active,
        );

        let created = repo
            .create(&user)
            .await
            .expect("OAuth2 user should be created without password credentials");
        let credential_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM auth_password_credentials WHERE user_id = $1)",
        )
        .bind(created.id.as_i64())
        .fetch_one(&pool)
        .await
        .expect("credential existence query should succeed");

        assert!(!credential_exists);
        assert_eq!(created.password_hash, "");
        assert_eq!(created.password_version, 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user_duplicate_username_returns_already_exists() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user1 = User::new(
            "same_name".into(),
            Some("a@b.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        repo.create(&user1).await.unwrap();
        let user2 = User::new(
            "same_name".into(),
            Some("c@d.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        let err = repo.create(&user2).await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists(_)));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_soft_delete_user() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new("deleteme".into(), None, "hash".into(), SignupMethod::Email);
        let created = repo.create(&user).await.unwrap();
        assert!(repo.delete(&created.id).await.unwrap());
        // Soft-deleted users should not be returned by get_by_id
        assert!(repo.get_by_id(&created.id).await.unwrap().is_none());
    }
}
