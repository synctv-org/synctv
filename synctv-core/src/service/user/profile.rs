use sqlx::{Postgres, Transaction};

use crate::{
    models::{User, UserAuthFactors, UserId, UserPreferences},
    service::UserService,
    Error, Result,
};

impl UserService {
    pub async fn get_user_preferences(
        &self,
        user_id: &UserId,
    ) -> Result<(UserPreferences, UserAuthFactors)> {
        self.get_user(user_id).await?;
        let preferences = self
            .user_preferences_repository
            .get_or_default(user_id)
            .await?;
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(user_id)
            .await?;
        Ok((preferences, auth_factors))
    }

    pub async fn is_two_factor_enabled(&self, user_id: &UserId) -> Result<bool> {
        Ok(self
            .user_preferences_repository
            .get_or_default(user_id)
            .await?
            .two_factor_enabled)
    }

    pub async fn set_two_factor_enabled(
        &self,
        user_id: &UserId,
        enabled: bool,
    ) -> Result<(UserPreferences, UserAuthFactors)> {
        self.update_user_preferences(
            user_id,
            crate::models::UserPreferencesUpdate {
                two_factor_enabled: Some(enabled),
                ..crate::models::UserPreferencesUpdate::default()
            },
        )
        .await
    }

    pub async fn set_two_factor_enabled_with_verification(
        &self,
        user_id: &UserId,
        enabled: bool,
        verification_id: &str,
    ) -> Result<(UserPreferences, UserAuthFactors)> {
        self.update_user_preferences_inner(
            user_id,
            crate::models::UserPreferencesUpdate {
                two_factor_enabled: Some(enabled),
                ..crate::models::UserPreferencesUpdate::default()
            },
            Some(verification_id),
        )
        .await
    }

    pub async fn update_user_preferences(
        &self,
        user_id: &UserId,
        update: crate::models::UserPreferencesUpdate,
    ) -> Result<(UserPreferences, UserAuthFactors)> {
        self.update_user_preferences_inner(user_id, update, None)
            .await
    }

    async fn update_user_preferences_inner(
        &self,
        user_id: &UserId,
        update: crate::models::UserPreferencesUpdate,
        verification_id: Option<&str>,
    ) -> Result<(UserPreferences, UserAuthFactors)> {
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        self.repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;

        let auth_factors = self
            .user_preferences_repository
            .auth_factors_with_excluded_passkey(user_id, None, &mut *tx)
            .await?;
        if update.two_factor_enabled == Some(true) && !auth_factors.supports_two_factor() {
            return Err(Error::InvalidInput(
                "Two-factor authentication requires at least two usable verification methods: password, passkey, authenticator app, or verified email".to_string(),
            ));
        }

        if let Some(verification_id) = verification_id {
            self.consume_sensitive_operation_verification(user_id, verification_id)
                .await?;
        }

        let preferences = self
            .user_preferences_repository
            .update_with_executor(user_id, &update, &mut *tx)
            .await?;
        tx.commit().await?;
        Ok((preferences, auth_factors))
    }

    /// Get multiple users by IDs.
    pub async fn get_users_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        self.repository.get_by_ids(user_ids).await
    }

    pub async fn get_users_by_ids_eventually_consistent(
        &self,
        user_ids: &[UserId],
    ) -> Result<Vec<User>> {
        self.repository
            .get_by_ids_eventually_consistent(user_ids)
            .await
    }

    /// Get user by username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<User> {
        let username = Self::canonical_username(username);
        if username.is_empty() {
            return Err(Error::InvalidInput("Username is empty".to_string()));
        }

        self.repository
            .get_by_username(&username)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    /// Get user by email
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        Ok(self
            .user_email_repository
            .get_by_email(email)
            .await?
            .map(|user_with_email| user_with_email.user))
    }

    pub async fn get_email(&self, user_id: &UserId) -> Result<Option<String>> {
        self.user_email_repository.get_email(user_id).await
    }

    pub async fn get_user_with_email(
        &self,
        user_id: &UserId,
    ) -> Result<crate::repository::UserWithEmail> {
        self.user_email_repository
            .get_by_user_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    /// Update user (entire user object) with optimistic locking.
    ///
    /// Pass the `version` value from the previously-read user to detect
    /// concurrent modifications. The update increments `version` atomically,
    /// so concurrent writes will see a mismatch and fail.
    /// Returns `Error::OptimisticLockConflict` if the user was modified since
    /// it was read.
    pub async fn update_user(&self, user: &User, old_version: i32) -> Result<User> {
        self.repository
            .get_by_id(&user.id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;
        let mut candidate = user.clone();
        candidate.username = Self::normalize_username_for_storage(&candidate.username)?;

        let updated = self.repository.update(&candidate, old_version).await?;
        self.invalidate_username_cache_best_effort(&candidate.id, "update_user")
            .await;
        self.notify_user_invalidation(&candidate.id).await;
        Ok(updated)
    }

    /// Update only a user's global role.
    pub async fn update_role(
        &self,
        user_id: &UserId,
        role: crate::models::UserRole,
        old_version: i32,
    ) -> Result<User> {
        let updated = self
            .repository
            .update_role(user_id, role, old_version)
            .await?;
        self.notify_user_invalidation(user_id).await;
        Ok(updated)
    }

    /// Update a user's own profile atomically.
    pub async fn update_profile(
        &self,
        user_id: &UserId,
        new_username: Option<String>,
    ) -> Result<User> {
        if new_username.is_none() {
            return Err(Error::InvalidInput(
                "No valid update fields provided (username)".to_string(),
            ));
        }

        let new_username = new_username
            .map(|username| Self::normalize_username_for_storage(&username))
            .transpose()?;

        if let Some(ref username) = new_username {
            debug_assert!(Self::validate_username(username).is_ok());
        }

        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let current_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        let target_username = new_username.unwrap_or_else(|| current_user.username.clone());

        let updated_user = self
            .repository
            .update_profile_with_executor(user_id, &target_username, current_user.version, &mut *tx)
            .await?;

        tx.commit().await?;

        if updated_user.username != current_user.username {
            self.invalidate_username_cache_best_effort(user_id, "update_profile")
                .await;
        }
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }
}
