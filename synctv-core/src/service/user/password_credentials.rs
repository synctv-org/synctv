use sqlx::{Postgres, Transaction};

use crate::{
    models::{User, UserId},
    repository::PasswordCredentialMaterial,
    service::UserService,
    Error, Result,
};

mod opaque;

impl UserService {
    pub async fn get_password_credential_state(
        &self,
        user_id: &UserId,
    ) -> Result<crate::repository::user_password::PasswordCredentialState> {
        self.user_password_repository.get_state(user_id).await
    }

    pub async fn has_usable_password_authentication(&self, user: &User) -> Result<bool> {
        self.user_password_repository.has_credential(&user.id).await
    }

    /// Revoke a user's password credential and invalidate password-bound tokens.
    ///
    /// The next password is installed through the OPAQUE password reset flow, so
    /// operators never receive or submit the user's replacement password.
    pub async fn force_password_reset(&self, user_id: &UserId) -> Result<User> {
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;

        self.user_password_repository
            .update_with_executor(user_id, PasswordCredentialMaterial::none(), &mut *tx)
            .await?;
        let updated_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        tx.commit().await?;

        // Invalidate user cache across all replicas
        self.notify_user_invalidation(user_id).await;

        tracing::info!("Password credential revoked for user {user_id}");

        Ok(updated_user)
    }

    pub async fn set_direct_password(&self, user_id: &UserId, password: &str) -> Result<User> {
        self.validate_password(password)?;
        let credential_identifier = Self::opaque_credential_identifier_for_user_id(user_id);
        let opaque_record = self
            .opaque_password_service
            .register_password(&credential_identifier, password)?;

        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        self.user_password_repository
            .update_with_executor(
                user_id,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
                &mut *tx,
            )
            .await?;
        let updated_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        tx.commit().await?;

        self.notify_user_invalidation(user_id).await;
        tracing::info!("Direct password credential updated for user {user_id}");

        Ok(updated_user)
    }
}
