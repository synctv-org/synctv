use sqlx::{Postgres, Transaction};
use webauthn_rs::prelude::Passkey;

use crate::{
    models::oauth2_client::OAuth2UserInfo,
    models::{OpaquePasswordRecord, SignupMethod, User, UserId},
    repository::{
        PasswordCredentialMaterial, ReviewRepository, UserOAuthProviderRepository,
        WebAuthnCredentialRepository,
    },
    service::UserService,
    Error, Result,
};

use super::super::registration_types::PendingRegistrationCredential;

impl UserService {
    async fn approve_oauth2_registration(
        &self,
        created: &User,
        user_info: &OAuth2UserInfo,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        UserOAuthProviderRepository::new(self.repository.pool().clone())
            .upsert_with_executor(
                &created.id,
                &user_info.provider,
                &user_info.provider_instance_name,
                &user_info.provider_user_id,
                user_info,
                &mut **tx,
            )
            .await
    }

    async fn approve_webauthn_registration(
        &self,
        created: &User,
        passkey: &Passkey,
        credential_name: Option<&str>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        WebAuthnCredentialRepository::new(self.repository.pool().clone())
            .create_with_executor(&created.id, passkey, credential_name, &mut **tx)
            .await?;
        Ok(())
    }

    async fn approve_password_registration(
        &self,
        created: &User,
        opaque_record: &OpaquePasswordRecord,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        self.user_password_repository
            .create_for_user_with_executor(
                created,
                PasswordCredentialMaterial::opaque_only(opaque_record),
                &mut **tx,
            )
            .await?;
        Ok(())
    }

    pub async fn approve_registration_request(
        &self,
        request_id: &UserId,
        reviewed_by: Option<&UserId>,
    ) -> Result<User> {
        let mut tx = self.repository.pool().begin().await?;
        let request = Self::load_pending_registration_request_for_update(request_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Pending registration request {request_id} not found"
                ))
            })?;

        let signup_method = request.credential.signup_method();
        let approved_email = (!matches!(signup_method, SignupMethod::OAuth2))
            .then(|| request.email.clone())
            .flatten();

        if self
            .repository
            .get_by_username_with_executor(&request.username, &mut *tx)
            .await?
            .is_some()
            || match approved_email.as_deref() {
                Some(email) => {
                    self.user_email_repository
                        .email_exists_with_executor(email, &mut *tx)
                        .await?
                }
                None => false,
            }
        {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }

        let user = User::new(request.username.clone(), signup_method);
        let created = self
            .repository
            .create_with_executor(&user, &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;
        self.user_email_repository
            .create_for_user_with_executor(&created, approved_email.as_deref(), &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;

        match &request.credential {
            PendingRegistrationCredential::OAuth2(user_info) => {
                self.approve_oauth2_registration(&created, user_info, &mut tx)
                    .await?;
            }
            PendingRegistrationCredential::WebAuthn {
                passkey,
                credential_name,
            } => {
                self.approve_webauthn_registration(
                    &created,
                    passkey,
                    credential_name.as_deref(),
                    &mut tx,
                )
                .await?;
            }
            PendingRegistrationCredential::Password { opaque_record, .. } => {
                self.approve_password_registration(&created, opaque_record, &mut tx)
                    .await?;
            }
        }

        let approved = ReviewRepository::approve_user_registration_with_executor(
            &mut *tx,
            *request_id,
            reviewed_by.copied(),
        )
        .await?;
        if approved == 0 {
            return Err(Error::NotFound(format!(
                "Pending registration request {request_id} not found"
            )));
        }

        tx.commit().await?;

        self.cache_username_best_effort(
            &created.id,
            &created.username,
            "approve_registration_request",
        )
        .await;
        self.notify_user_invalidation(&created.id).await;

        Ok(created)
    }

    pub async fn reject_registration_request(
        &self,
        request_id: &UserId,
        reviewed_by: Option<&UserId>,
        reason: &str,
    ) -> Result<()> {
        let result = ReviewRepository::reject_user_registration_with_executor(
            self.repository.pool(),
            *request_id,
            reviewed_by.copied(),
            reason,
        )
        .await?;

        if result == 0 {
            return Err(Error::NotFound(format!(
                "Pending registration request {request_id} not found"
            )));
        }

        Ok(())
    }
}
