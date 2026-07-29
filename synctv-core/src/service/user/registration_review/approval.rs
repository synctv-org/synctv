use sqlx::{Postgres, Transaction};

use crate::{
    models::{OpaquePasswordRecord, SignupMethod, User, UserId},
    repository::{
        PasswordCredentialMaterial, ReviewRepository, UserOAuthProviderRepository,
        WebAuthnCredentialRepository,
    },
    service::UserService,
    Error, Result,
};

use super::super::registration_types::PendingRegistrationRequest;

impl UserService {
    async fn approve_oauth2_registration(
        &self,
        created: &User,
        request: &PendingRegistrationRequest,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        let provider = request.oauth2_provider.as_ref().ok_or_else(|| {
            Error::InvalidInput("OAuth2 registration request is missing provider".to_string())
        })?;
        let provider_user_id = request.oauth2_provider_user_id.as_deref().ok_or_else(|| {
            Error::InvalidInput(
                "OAuth2 registration request is missing provider user ID".to_string(),
            )
        })?;
        let provider_instance_name = request
            .oauth2_provider_instance_name
            .as_deref()
            .ok_or_else(|| {
                Error::InvalidInput(
                    "OAuth2 registration request is missing provider instance name".to_string(),
                )
            })?;

        let oauth2_user_info = crate::models::oauth2_client::OAuth2UserInfo {
            provider: provider.clone(),
            provider_instance_name: provider_instance_name.to_string(),
            provider_issuer: request.oauth2_provider_issuer.clone(),
            provider_user_id: provider_user_id.to_string(),
            username: request
                .oauth2_provider_username
                .clone()
                .unwrap_or_else(|| request.username.clone()),
            avatar: request.oauth2_avatar_url.clone(),
        };
        UserOAuthProviderRepository::new(self.repository.pool().clone())
            .upsert_with_executor(
                &created.id,
                provider,
                provider_instance_name,
                provider_user_id,
                &oauth2_user_info,
                &mut **tx,
            )
            .await
    }

    async fn approve_webauthn_registration(
        &self,
        created: &User,
        request: &PendingRegistrationRequest,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        let passkey = request.webauthn_passkey.as_ref().ok_or_else(|| {
            Error::InvalidInput("Registration request is missing WebAuthn passkey".to_string())
        })?;
        let credential_id = request.webauthn_credential_id.as_deref().ok_or_else(|| {
            Error::InvalidInput(
                "Registration request is missing WebAuthn credential ID".to_string(),
            )
        })?;
        if credential_id != AsRef::<[u8]>::as_ref(passkey.cred_id()) {
            return Err(Error::InvalidInput(
                "Registration request WebAuthn credential ID does not match passkey".to_string(),
            ));
        }

        WebAuthnCredentialRepository::new(self.repository.pool().clone())
            .create_with_executor(
                &created.id,
                passkey,
                request.webauthn_credential_name.as_deref(),
                &mut **tx,
            )
            .await?;
        Ok(())
    }

    async fn approve_password_registration(
        &self,
        created: &User,
        request: &PendingRegistrationRequest,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<()> {
        let opaque_record = OpaquePasswordRecord {
            record: request.opaque_record.clone().ok_or_else(|| {
                Error::InvalidInput("Registration request is missing OPAQUE record".to_string())
            })?,
            credential_identifier: request.opaque_credential_identifier.clone().ok_or_else(
                || {
                    Error::InvalidInput(
                        "Registration request is missing OPAQUE credential identifier".to_string(),
                    )
                },
            )?,
            ciphersuite: request.opaque_ciphersuite.clone().ok_or_else(|| {
                Error::InvalidInput(
                    "Registration request is missing OPAQUE ciphersuite".to_string(),
                )
            })?,
            server_setup_version: request.opaque_server_setup_version.ok_or_else(|| {
                Error::InvalidInput(
                    "Registration request is missing OPAQUE setup version".to_string(),
                )
            })?,
        };
        self.user_password_repository
            .create_for_user_with_executor(
                created,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
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

        let approved_email = (!matches!(request.signup_method, SignupMethod::OAuth2))
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

        let user = User::new(request.username.clone(), request.signup_method);
        let created = self
            .repository
            .create_with_executor(&user, &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;
        self.user_email_repository
            .create_for_user_with_executor(&created, approved_email.as_deref(), &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;

        match request.signup_method {
            SignupMethod::OAuth2 => {
                self.approve_oauth2_registration(&created, &request, &mut tx)
                    .await?;
            }
            SignupMethod::WebAuthn => {
                self.approve_webauthn_registration(&created, &request, &mut tx)
                    .await?;
            }
            _ => {
                self.approve_password_registration(&created, &request, &mut tx)
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
