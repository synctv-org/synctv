use sqlx::PgPool;

use crate::{
    models::{
        RoomSettings, UserAuthFactors, UserId, UserNotificationPreferences, UserPreferences,
        UserPreferencesUpdate,
    },
    Error, Result,
};

#[derive(Clone)]
pub struct UserPreferencesRepository {
    pool: PgPool,
}

struct UserPreferencesRow {
    user_id: UserId,
    two_factor_enabled: bool,
    notify_room_invitation_in_app: bool,
    notify_room_event_in_app: bool,
    notify_system_announcement_in_app: bool,
    notify_room_invitation_email: bool,
    notify_room_event_email: bool,
    notify_system_announcement_email: bool,
    settings: RoomSettings,
}

impl UserPreferencesRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn preferences_from_row(row: UserPreferencesRow) -> UserPreferences {
        UserPreferences {
            user_id: row.user_id,
            two_factor_enabled: row.two_factor_enabled,
            notifications: UserNotificationPreferences {
                room_invitation_in_app: row.notify_room_invitation_in_app,
                room_event_in_app: row.notify_room_event_in_app,
                system_announcement_in_app: row.notify_system_announcement_in_app,
                room_invitation_email: row.notify_room_invitation_email,
                room_event_email: row.notify_room_event_email,
                system_announcement_email: row.notify_system_announcement_email,
            },
            settings: row.settings,
        }
    }

    pub async fn get_or_default(&self, user_id: &UserId) -> Result<UserPreferences> {
        let row = sqlx::query_as!(
            UserPreferencesRow,
            r#"
            SELECT user_id AS "user_id: UserId",
                   two_factor_enabled,
                   notify_room_invitation_in_app,
                   notify_room_event_in_app,
                   notify_system_announcement_in_app,
                   notify_room_invitation_email,
                   notify_room_event_email,
                   notify_system_announcement_email,
                   settings AS "settings: RoomSettings"
            FROM user_preferences
            WHERE user_id = $1
            "#,
            user_id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map_or_else(
            || UserPreferences::default_for_user(*user_id),
            Self::preferences_from_row,
        ))
    }

    pub async fn update(
        &self,
        user_id: &UserId,
        update: &UserPreferencesUpdate,
    ) -> Result<UserPreferences> {
        self.update_with_executor(user_id, update, &self.pool).await
    }

    pub async fn update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        update: &UserPreferencesUpdate,
        executor: E,
    ) -> Result<UserPreferences>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if update.is_empty() {
            return Err(Error::InvalidInput(
                "No valid user preference fields provided".to_string(),
            ));
        }
        let default_settings = RoomSettings::default();

        let row = sqlx::query_as!(
            UserPreferencesRow,
            r#"
            INSERT INTO user_preferences (
                user_id,
                two_factor_enabled,
                notify_room_invitation_in_app,
                notify_room_event_in_app,
                notify_system_announcement_in_app,
                notify_room_invitation_email,
                notify_room_event_email,
                notify_system_announcement_email,
                settings
            )
            VALUES (
                $1,
                COALESCE($2, FALSE),
                COALESCE($3, TRUE),
                COALESCE($4, TRUE),
                COALESCE($5, TRUE),
                COALESCE($6, FALSE),
                COALESCE($7, FALSE),
                COALESCE($8, TRUE),
                $9
            )
            ON CONFLICT (user_id) DO UPDATE
            SET two_factor_enabled = COALESCE($2, user_preferences.two_factor_enabled),
                notify_room_invitation_in_app = COALESCE($3, user_preferences.notify_room_invitation_in_app),
                notify_room_event_in_app = COALESCE($4, user_preferences.notify_room_event_in_app),
                notify_system_announcement_in_app = COALESCE($5, user_preferences.notify_system_announcement_in_app),
                notify_room_invitation_email = COALESCE($6, user_preferences.notify_room_invitation_email),
                notify_room_event_email = COALESCE($7, user_preferences.notify_room_event_email),
                notify_system_announcement_email = COALESCE($8, user_preferences.notify_system_announcement_email),
                updated_at = CURRENT_TIMESTAMP
            RETURNING user_id AS "user_id: UserId",
                      two_factor_enabled,
                      notify_room_invitation_in_app,
                      notify_room_event_in_app,
                      notify_system_announcement_in_app,
                      notify_room_invitation_email,
                      notify_room_event_email,
                      notify_system_announcement_email,
                      settings AS "settings: RoomSettings"
            "#,
            user_id.as_i64(),
            update.two_factor_enabled,
            update
                .notifications
                .as_ref()
                .map(|value| value.room_invitation_in_app),
            update
                .notifications
                .as_ref()
                .map(|value| value.room_event_in_app),
            update
                .notifications
                .as_ref()
                .map(|value| value.system_announcement_in_app),
            update
                .notifications
                .as_ref()
                .map(|value| value.room_invitation_email),
            update
                .notifications
                .as_ref()
                .map(|value| value.room_event_email),
            update
                .notifications
                .as_ref()
                .map(|value| value.system_announcement_email),
            &default_settings as &RoomSettings
        )
        .fetch_one(executor)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(ref database_error)
                if database_error.constraint() == Some("user_preferences_user_id_fkey") =>
            {
                Error::NotFound("User not found".to_string())
            }
            other => Error::Database(other),
        })?;

        Ok(Self::preferences_from_row(row))
    }

    pub async fn auth_factors(&self, user_id: &UserId) -> Result<UserAuthFactors> {
        self.auth_factors_with_excluded_passkey(user_id, None, &self.pool)
            .await
    }

    pub async fn auth_factors_with_excluded_passkey<'e, E>(
        &self,
        user_id: &UserId,
        excluded_credential_id: Option<&[u8]>,
        executor: E,
    ) -> Result<UserAuthFactors>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query!(
            r#"
            SELECT
                EXISTS (
                    SELECT 1
                    FROM auth_password_credentials
                    WHERE user_id = $1
                      AND opaque_record IS NOT NULL
                      AND opaque_credential_identifier IS NOT NULL
                      AND opaque_ciphersuite IS NOT NULL
                      AND opaque_server_setup_version IS NOT NULL
                ) AS "password!",
                EXISTS (
                    SELECT 1
                    FROM auth_webauthn_credentials
                    WHERE user_id = $1
                      AND ($2::bytea IS NULL OR credential_id <> $2)
                ) AS "webauthn!",
                EXISTS (
                    SELECT 1
                    FROM auth_totp_credentials
                    WHERE user_id = $1 AND confirmed_at IS NOT NULL
                ) AS "totp!",
                COALESCE((
                    SELECT cardinality(recovery_code_hashes)
                    FROM auth_totp_credentials
                    WHERE user_id = $1 AND confirmed_at IS NOT NULL
                ), 0)::int4 AS "totp_recovery_codes_remaining!",
                EXISTS (
                    SELECT 1
                    FROM auth_email_identities
                    WHERE user_id = $1 AND deleted_at IS NULL
                ) AS "email!"
            "#,
            user_id.as_i64(),
            excluded_credential_id
        )
        .fetch_one(executor)
        .await?;

        Ok(UserAuthFactors {
            password: row.password,
            webauthn: row.webauthn,
            totp: row.totp,
            totp_recovery_codes_remaining: row
                .totp_recovery_codes_remaining
                .try_into()
                .map_err(|_| Error::Internal("Invalid TOTP recovery code count".to_string()))?,
            email: row.email,
        })
    }

    pub async fn two_factor_enabled_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let enabled = sqlx::query_scalar!(
            r#"
            SELECT COALESCE(
                (SELECT two_factor_enabled FROM user_preferences WHERE user_id = $1),
                FALSE
            ) AS "enabled!"
            "#,
            user_id.as_i64()
        )
        .fetch_one(executor)
        .await?;

        Ok(enabled)
    }
}
