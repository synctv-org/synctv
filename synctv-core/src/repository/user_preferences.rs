use sqlx::{PgPool, Row};

use crate::{
    models::{
        UserAuthFactors, UserId, UserNotificationPreferences, UserPreferences,
        UserPreferencesUpdate, UserProviderDefaults,
    },
    Error, Result,
};

#[derive(Clone)]
pub struct UserPreferencesRepository {
    pool: PgPool,
}

impl UserPreferencesRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn optional_trimmed(value: Option<&String>) -> Option<String> {
        value.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    }

    fn preferences_from_row(
        row: &sqlx::postgres::PgRow,
        user_id: UserId,
    ) -> Result<UserPreferences> {
        Ok(UserPreferences {
            user_id,
            two_factor_enabled: row.try_get("two_factor_enabled")?,
            notifications: UserNotificationPreferences {
                room_invitation_in_app: row.try_get("notify_room_invitation_in_app")?,
                room_event_in_app: row.try_get("notify_room_event_in_app")?,
                system_announcement_in_app: row.try_get("notify_system_announcement_in_app")?,
                room_invitation_email: row.try_get("notify_room_invitation_email")?,
                room_event_email: row.try_get("notify_room_event_email")?,
                system_announcement_email: row.try_get("notify_system_announcement_email")?,
            },
            provider_defaults: UserProviderDefaults {
                alist_instance_name: row.try_get("default_alist_instance_name")?,
                emby_instance_name: row.try_get("default_emby_instance_name")?,
                bilibili_instance_name: row.try_get("default_bilibili_instance_name")?,
            },
            settings: row.try_get("settings")?,
        })
    }

    pub async fn get_or_default(&self, user_id: &UserId) -> Result<UserPreferences> {
        let row = sqlx::query(
            r"
            SELECT *
            FROM user_preferences
            WHERE user_id = $1
            ",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| Self::preferences_from_row(&row, *user_id))
            .transpose()
            .map(|preferences| {
                preferences.unwrap_or_else(|| UserPreferences::default_for_user(*user_id))
            })
    }

    pub async fn set_two_factor_enabled_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        enabled: bool,
        executor: E,
    ) -> Result<UserPreferences>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let update = UserPreferencesUpdate {
            two_factor_enabled: Some(enabled),
            ..UserPreferencesUpdate::default()
        };
        self.update_with_executor(user_id, &update, executor).await
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

        let default_alist_instance_name = update
            .provider_defaults
            .as_ref()
            .and_then(|value| Self::optional_trimmed(value.alist_instance_name.as_ref()));
        let default_emby_instance_name = update
            .provider_defaults
            .as_ref()
            .and_then(|value| Self::optional_trimmed(value.emby_instance_name.as_ref()));
        let default_bilibili_instance_name = update
            .provider_defaults
            .as_ref()
            .and_then(|value| Self::optional_trimmed(value.bilibili_instance_name.as_ref()));
        let provider_defaults_present = update.provider_defaults.is_some();

        let row = sqlx::query(
            r"
            INSERT INTO user_preferences (
                user_id,
                two_factor_enabled,
                notify_room_invitation_in_app,
                notify_room_event_in_app,
                notify_system_announcement_in_app,
                notify_room_invitation_email,
                notify_room_event_email,
                notify_system_announcement_email,
                default_alist_instance_name,
                default_emby_instance_name,
                default_bilibili_instance_name,
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
                $9,
                $10,
                $11,
                '{}'::jsonb
            )
            ON CONFLICT (user_id) DO UPDATE
            SET two_factor_enabled = COALESCE($2, user_preferences.two_factor_enabled),
                notify_room_invitation_in_app = COALESCE($3, user_preferences.notify_room_invitation_in_app),
                notify_room_event_in_app = COALESCE($4, user_preferences.notify_room_event_in_app),
                notify_system_announcement_in_app = COALESCE($5, user_preferences.notify_system_announcement_in_app),
                notify_room_invitation_email = COALESCE($6, user_preferences.notify_room_invitation_email),
                notify_room_event_email = COALESCE($7, user_preferences.notify_room_event_email),
                notify_system_announcement_email = COALESCE($8, user_preferences.notify_system_announcement_email),
                default_alist_instance_name = CASE WHEN $12::BOOLEAN IS FALSE THEN user_preferences.default_alist_instance_name ELSE $9 END,
                default_emby_instance_name = CASE WHEN $12::BOOLEAN IS FALSE THEN user_preferences.default_emby_instance_name ELSE $10 END,
                default_bilibili_instance_name = CASE WHEN $12::BOOLEAN IS FALSE THEN user_preferences.default_bilibili_instance_name ELSE $11 END,
                updated_at = CURRENT_TIMESTAMP
            RETURNING *
            ",
        )
        .bind(user_id)
        .bind(update.two_factor_enabled)
        .bind(update.notifications.as_ref().map(|value| value.room_invitation_in_app))
        .bind(update.notifications.as_ref().map(|value| value.room_event_in_app))
        .bind(update.notifications.as_ref().map(|value| value.system_announcement_in_app))
        .bind(update.notifications.as_ref().map(|value| value.room_invitation_email))
        .bind(update.notifications.as_ref().map(|value| value.room_event_email))
        .bind(update.notifications.as_ref().map(|value| value.system_announcement_email))
        .bind(default_alist_instance_name)
        .bind(default_emby_instance_name)
        .bind(default_bilibili_instance_name)
        .bind(provider_defaults_present)
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

        Self::preferences_from_row(&row, *user_id)
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
        let row = sqlx::query(
            r"
            SELECT
                EXISTS (
                    SELECT 1
                    FROM auth_password_credentials
                    WHERE user_id = $1
                      AND legacy_password_hash IS NOT NULL
                ) AS password,
                EXISTS (
                    SELECT 1
                    FROM auth_webauthn_credentials
                    WHERE user_id = $1
                      AND ($2::bytea IS NULL OR credential_id <> $2)
                ) AS webauthn,
                EXISTS (
                    SELECT 1
                    FROM auth_email_identities
                    WHERE user_id = $1
                      AND email_verified = TRUE
                ) AS email
            ",
        )
        .bind(user_id)
        .bind(excluded_credential_id)
        .fetch_one(executor)
        .await?;

        Ok(UserAuthFactors {
            password: row.try_get("password")?,
            webauthn: row.try_get("webauthn")?,
            email: row.try_get("email")?,
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
        let enabled = sqlx::query_scalar::<_, bool>(
            r"
            SELECT COALESCE(
                (SELECT two_factor_enabled FROM user_preferences WHERE user_id = $1),
                FALSE
            )
            ",
        )
        .bind(user_id)
        .fetch_one(executor)
        .await?;

        Ok(enabled)
    }

    pub async fn assert_can_enable_two_factor(&self, user_id: &UserId) -> Result<UserAuthFactors> {
        let factors = self.auth_factors(user_id).await?;
        if !factors.supports_two_factor() {
            return Err(Error::InvalidInput(
                "Two-factor authentication requires at least two usable verification methods: password, passkey, or verified email".to_string(),
            ));
        }
        Ok(factors)
    }
}
