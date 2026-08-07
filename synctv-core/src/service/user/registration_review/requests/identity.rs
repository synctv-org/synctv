use sqlx::{Postgres, Transaction};

use crate::{
    models::{ReviewStatus, SignupMethod, UserId},
    service::UserService,
    Result,
};

use super::super::super::PendingRegistrationConflict;

const USER_REGISTRATION_PENDING_LOCK_NS: i32 = 20_260_406;
const OAUTH2_PENDING_REGISTRATION_LOCK_NS: i32 = 20_260_407;

impl UserService {
    pub(in crate::service::user) async fn has_pending_registration_request(
        &self,
        username: &str,
        email: Option<&str>,
    ) -> Result<bool> {
        self.has_pending_registration_request_with_executor(username, email, self.repository.pool())
            .await
    }

    pub(in crate::service::user) async fn has_pending_registration_request_with_executor<'e, E>(
        &self,
        username: &str,
        email: Option<&str>,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM user_registration_requests
                WHERE reviewed_at IS NULL
                  AND (
                      LOWER(username) = LOWER($1)
                      OR ($2::TEXT IS NOT NULL AND LOWER(email) = LOWER($2))
                  )
            ) AS "exists!"
            "#,
            username,
            email,
        )
        .fetch_one(executor)
        .await?;

        Ok(exists)
    }

    pub(crate) async fn pending_oauth2_registration_conflict<'e, E>(
        &self,
        username: &str,
        email: Option<&str>,
        provider_instance_name: &str,
        provider_user_id: &str,
        executor: E,
    ) -> Result<Option<PendingRegistrationConflict>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query!(
            r#"
            SELECT
                (
                    SELECT id
                    FROM user_registration_requests
                    WHERE reviewed_at IS NULL
                      AND status = $4
                      AND oauth2_provider_instance_name = $5
                      AND oauth2_provider_user_id = $6
                    ORDER BY requested_at DESC, id DESC
                    LIMIT 1
                ) AS "oauth2_request_id: UserId",
                EXISTS (
                    SELECT 1
                    FROM user_registration_requests
                    WHERE reviewed_at IS NULL
                      AND status = $4
                      AND LOWER(username) = LOWER($1)
                      AND (
                          signup_method != $3
                          OR oauth2_provider_instance_name IS DISTINCT FROM $5
                          OR oauth2_provider_user_id IS DISTINCT FROM $6
                      )
                ) AS "username_exists!",
                EXISTS (
                    SELECT 1
                    FROM user_registration_requests
                    WHERE reviewed_at IS NULL
                      AND status = $4
                      AND $2::TEXT IS NOT NULL
                      AND LOWER(email) = LOWER($2)
                      AND (
                          signup_method != $3
                          OR oauth2_provider_instance_name IS DISTINCT FROM $5
                          OR oauth2_provider_user_id IS DISTINCT FROM $6
                      )
                ) AS "email_exists!"
            "#,
            username,
            email,
            SignupMethod::OAuth2 as SignupMethod,
            i16::from(ReviewStatus::Pending),
            provider_instance_name,
            provider_user_id,
        )
        .fetch_one(executor)
        .await?;

        if let Some(request_id) = row.oauth2_request_id {
            return Ok(Some(PendingRegistrationConflict::OAuth2Identity(
                request_id,
            )));
        }
        if row.email_exists {
            return Ok(Some(PendingRegistrationConflict::Email));
        }
        if row.username_exists {
            return Ok(Some(PendingRegistrationConflict::Username));
        }

        Ok(None)
    }

    pub(in crate::service::user) async fn lock_pending_registration_identity(
        tx: &mut Transaction<'_, Postgres>,
        username: &str,
        email: Option<&str>,
    ) -> Result<()> {
        let normalized_username = username.to_ascii_lowercase();
        let normalized_email = email.map(str::to_ascii_lowercase);
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            USER_REGISTRATION_PENDING_LOCK_NS,
            normalized_username,
        )
        .execute(&mut **tx)
        .await?;

        if let Some(email) = normalized_email {
            sqlx::query!(
                "SELECT pg_advisory_xact_lock($1, hashtext($2))",
                USER_REGISTRATION_PENDING_LOCK_NS,
                email,
            )
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    pub(crate) async fn lock_oauth2_pending_registration_identity(
        tx: &mut Transaction<'_, Postgres>,
        username: &str,
        email: Option<&str>,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<()> {
        Self::lock_pending_registration_identity(tx, username, email).await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            OAUTH2_PENDING_REGISTRATION_LOCK_NS,
            format!("{provider_instance_name}:{provider_user_id}"),
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}
