use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    Error, Result,
};

#[derive(Debug, Clone)]
pub struct UserWithEmail {
    pub user: User,
    pub email: Option<String>,
}

#[derive(sqlx::FromRow)]
struct UserWithEmailRow {
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
    version: i32,
    deleted_at: Option<DateTime<Utc>>,
    email: Option<String>,
}

impl UserWithEmailRow {
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
            version: self.version,
            deleted_at: self.deleted_at,
        }
    }
}

impl From<UserWithEmailRow> for UserWithEmail {
    fn from(row: UserWithEmailRow) -> Self {
        Self {
            user: row.to_user(),
            email: row.email,
        }
    }
}

#[derive(Clone)]
pub struct UserEmailRepository {
    pool: PgPool,
}

impl UserEmailRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_for_user_with_executor<'e, E>(
        &self,
        user: &User,
        email: Option<&str>,
        executor: E,
    ) -> Result<Option<String>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let Some(email) = email.filter(|email| !email.trim().is_empty()) else {
            return Ok(None);
        };

        let inserted = sqlx::query_scalar!(
            r"
            INSERT INTO auth_email_identities (
                user_id, email, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4)
            RETURNING email
            ",
            user.id.as_i64(),
            email,
            user.created_at,
            user.updated_at
        )
        .fetch_one(executor)
        .await
        .map_err(map_email_identity_error)?;

        Ok(Some(inserted))
    }

    pub async fn upsert_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        email: &str,
        now: DateTime<Utc>,
        executor: E,
    ) -> Result<UserWithEmail>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query_as!(
            UserWithEmailRow,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET updated_at = $3, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            ),
            updated_email AS (
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
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.signup_method AS "signup_method!: SignupMethod",
                   u.role AS "role!: UserRole",
                   u.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "version!",
                   u.deleted_at,
                   updated_email.email
            FROM updated_user u
            LEFT JOIN updated_email ON updated_email.user_id = u.id
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = u.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user_id.as_i64(),
            email,
            now
        )
        .fetch_optional(executor)
        .await
        .map_err(map_email_identity_error)?
        .map(Into::into)
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    pub async fn delete_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        now: DateTime<Utc>,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query_as!(
            User,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET updated_at = $2, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            ),
            deleted_email AS (
                DELETE FROM auth_email_identities
                USING updated_user
                WHERE auth_email_identities.user_id = updated_user.id
            )
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.signup_method AS "signup_method!: SignupMethod",
                   u.role AS "role!: UserRole",
                   u.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "version!",
                   u.deleted_at
            FROM updated_user u
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = u.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user_id.as_i64(),
            now
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    pub async fn get_email(&self, user_id: &UserId) -> Result<Option<String>> {
        self.get_email_with_executor(user_id, &self.pool).await
    }

    pub async fn get_email_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<Option<String>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query_scalar!(
            r"
            SELECT aei.email
            FROM auth_email_identities aei
            JOIN users u ON u.id = aei.user_id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            ",
            user_id.as_i64()
        )
        .fetch_optional(executor)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_by_user_id(&self, user_id: &UserId) -> Result<Option<UserWithEmail>> {
        let row = sqlx::query_as!(
            UserWithEmailRow,
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
                   u.version AS "version!",
                   u.deleted_at,
                   aei.email
            FROM user_account_profiles u
            LEFT JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            "#,
            user_id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn get_by_email(&self, email: &str) -> Result<Option<UserWithEmail>> {
        let row = sqlx::query_as!(
            UserWithEmailRow,
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
                   u.version AS "version!",
                   u.deleted_at,
                   aei.email
            FROM user_account_profiles u
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "#,
            email
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn get_by_email_with_executor<'e, E>(
        &self,
        email: &str,
        executor: E,
    ) -> Result<Option<UserWithEmail>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query_as!(
            UserWithEmailRow,
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
                   u.version AS "version!",
                   u.deleted_at,
                   aei.email
            FROM user_account_profiles u
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "#,
            email
        )
        .fetch_optional(executor)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn email_exists(&self, email: &str) -> Result<bool> {
        self.email_exists_with_executor(email, &self.pool).await
    }

    pub async fn email_exists_with_executor<'e, E>(&self, email: &str, executor: E) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM auth_email_identities aei
                JOIN users u ON u.id = aei.user_id
                WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            ) AS "exists!"
            "#,
            email
        )
        .fetch_one(executor)
        .await
        .map_err(Error::Database)
    }
}

fn map_email_identity_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
            let constraint = db_err.constraint().unwrap_or("");
            if constraint.contains("email") {
                Error::AlreadyExists("Email already taken".to_string())
            } else {
                Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                )
            }
        }
        other => Error::Database(other),
    }
}
