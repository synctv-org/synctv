use std::collections::HashSet;

use sqlx::{Postgres, Transaction};

use crate::{
    models::{SignupMethod, User, UserAuthFactors, UserId},
    repository::UserOAuthProviderRepository,
    service::user::UserService,
    Error, Result,
};

impl UserService {
    pub async fn start_email_bind(&self, user_id: &UserId, email: &str) -> Result<String> {
        let email = self.validate_email_bind_target(user_id, email).await?;
        let token = synctv_common::snanoid!(64);
        let expires_at =
            chrono::Utc::now() + crate::models::EmailTokenType::EmailBind.expiration_duration();

        self.email_bind_repository
            .create_or_replace_unused(user_id, &email, &token, expires_at)
            .await?;

        Ok(token)
    }

    pub async fn delete_pending_email_bind(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
    ) -> Result<u64> {
        let email = email.trim().to_ascii_lowercase();
        self.email_bind_repository
            .delete_unused(token, user_id, &email)
            .await
    }

    pub async fn confirm_email_bind(
        &self,
        user_id: &UserId,
        email: &str,
        token: &str,
        verification_id: &str,
    ) -> Result<User> {
        let email = self.validate_email_bind_target(user_id, email).await?;
        let mut tx = self.repository.pool().begin().await?;

        let email = self
            .email_bind_repository
            .lock_valid_for_update_with_executor(user_id, &email, token, &mut *tx)
            .await?;

        self.consume_sensitive_operation_verification(user_id, verification_id)
            .await?;

        let (email, now) = self
            .email_bind_repository
            .consume_with_executor(user_id, &email, token, &mut *tx)
            .await?;
        let updated_user = self
            .user_email_repository
            .upsert_with_executor(user_id, &email, now, &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?
            .user;
        tx.commit().await?;
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }

    pub(crate) fn active_oauth2_provider_keys(&self) -> Result<HashSet<(String, String)>> {
        let Some(registry) = self.settings_registry.as_ref() else {
            return Ok(HashSet::new());
        };
        let configs = registry.oauth2_providers.get()?;
        Ok(configs
            .0
            .iter()
            .map(|(instance_name, config)| {
                (
                    instance_name.clone(),
                    config.provider_type.trim().to_string(),
                )
            })
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn count_active_oauth2_identities(
        mappings: &[crate::models::oauth2_client::UserOAuthProviderMapping],
        active_provider_keys: &HashSet<(String, String)>,
    ) -> usize {
        mappings
            .iter()
            .filter(|mapping| {
                active_provider_keys.contains(&(
                    mapping.provider_instance_name.clone(),
                    mapping.provider.clone(),
                ))
            })
            .count()
    }

    pub(crate) async fn active_oauth2_identity_count_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<usize>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let active_provider_keys = self.active_oauth2_provider_keys()?;
        UserOAuthProviderRepository::new(self.repository.pool().clone())
            .count_active_by_user_with_executor(user_id, &active_provider_keys, executor)
            .await
    }

    #[must_use]
    pub const fn sign_in_method_count(
        auth_factors: &UserAuthFactors,
        active_oauth2_identity_count: usize,
    ) -> usize {
        auth_factors.password as usize
            + auth_factors.webauthn as usize
            + auth_factors.email as usize
            + active_oauth2_identity_count
    }

    pub async fn unbind_email(&self, user_id: &UserId, verification_id: &str) -> Result<User> {
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let current_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        if self
            .user_email_repository
            .get_email_with_executor(user_id, &mut *tx)
            .await?
            .is_none()
        {
            return Err(Error::InvalidInput("Email is not bound".to_string()));
        }
        if current_user.signup_method == SignupMethod::Email {
            return Err(Error::InvalidInput(
                "Email signup users must keep their email identity".to_string(),
            ));
        }

        let auth_factors = self
            .user_preferences_repository
            .auth_factors_with_excluded_passkey(user_id, None, &mut *tx)
            .await?;
        let active_oauth2_identity_count = self
            .active_oauth2_identity_count_with_executor(user_id, &mut *tx)
            .await?;
        let remaining_auth_factors = UserAuthFactors {
            email: false,
            ..auth_factors
        };
        let remaining_sign_in_method_count =
            Self::sign_in_method_count(&remaining_auth_factors, active_oauth2_identity_count);
        if remaining_sign_in_method_count == 0 {
            return Err(Error::InvalidInput(
                "Cannot unbind the last sign-in method".to_string(),
            ));
        }

        let remaining_two_factor_method_count =
            usize::from(auth_factors.password) + usize::from(auth_factors.webauthn);
        let two_factor_enabled = self
            .user_preferences_repository
            .two_factor_enabled_with_executor(user_id, &mut *tx)
            .await?;
        if two_factor_enabled && remaining_two_factor_method_count < 2 {
            return Err(Error::InvalidInput(
                "Cannot unbind email while two-factor authentication depends on it".to_string(),
            ));
        }

        self.consume_sensitive_operation_verification(user_id, verification_id)
            .await?;
        let updated_user = self
            .user_email_repository
            .delete_with_executor(user_id, chrono::Utc::now(), &mut *tx)
            .await?;

        tx.commit().await?;
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }

    async fn validate_email_bind_target(&self, user_id: &UserId, email: &str) -> Result<String> {
        let email = email.trim().to_ascii_lowercase();
        Self::validate_email(&email)?;

        self.validate_email_whitelist_policy(&email)?;

        if let Some(existing) = self.user_email_repository.get_by_email(&email).await? {
            if existing.user.id != *user_id {
                return Err(Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                ));
            }
        }

        Ok(email)
    }

    pub async fn cleanup_oauth_providers(&self, user_id: &UserId) -> Result<u64> {
        let repo = UserOAuthProviderRepository::new(self.repository.pool().clone());
        repo.delete_all_for_user(user_id).await
    }
}
