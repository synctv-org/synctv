use std::time::Duration;

use sqlx::{Postgres, Transaction};

use crate::{
    models::{User, UserId},
    repository::PasswordCredentialMaterial,
    service::UserService,
    Error, Result,
};

use super::super::session_types::{
    OpaquePasswordUpdateVerification, OpaqueRegistrationPurpose, OpaqueRegistrationSession,
    OpaqueRegistrationStartChallenge, OPAQUE_REGISTRATION_SESSION_TTL_SECS,
};

impl UserService {
    pub async fn start_opaque_password_update(
        &self,
        user_id: &UserId,
        credential_request: Vec<u8>,
        registration_request: Vec<u8>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        let opaque_credential = self
            .user_password_repository
            .get_opaque_credential(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let password_version = self
            .user_password_repository
            .get_state(user_id)
            .await?
            .version;

        let login_start = self.opaque_password_service.start_login(
            Some(&opaque_credential.record),
            &opaque_credential.record.credential_identifier,
            &credential_request,
        )?;

        self.start_opaque_password_registration_session(
            user_id,
            registration_request,
            OpaqueRegistrationPurpose::PasswordUpdate {
                user_id: *user_id,
                expected_password_version: password_version,
                verification: OpaquePasswordUpdateVerification::CurrentOpaquePassword {
                    server_login_state: login_start.server_login_state,
                },
            },
            login_start.credential_response,
        )
        .await
    }

    pub async fn start_opaque_password_update_after_external_verification(
        &self,
        user_id: &UserId,
        registration_request: Vec<u8>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.start_opaque_password_update_after_verification(
            user_id,
            registration_request,
            OpaquePasswordUpdateVerification::VerifiedExternal,
        )
        .await
    }

    pub async fn start_opaque_password_reset_after_external_verification(
        &self,
        user_id: &UserId,
        registration_request: Vec<u8>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        let password_version = self
            .user_password_repository
            .get_state(user_id)
            .await?
            .version;

        self.start_opaque_password_registration_session(
            user_id,
            registration_request,
            OpaqueRegistrationPurpose::PasswordReset {
                user_id: *user_id,
                expected_password_version: password_version,
            },
            Vec::new(),
        )
        .await
    }

    pub async fn start_opaque_password_update_pending_passkey_verification(
        &self,
        user_id: &UserId,
        registration_request: Vec<u8>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.start_opaque_password_update_after_verification(
            user_id,
            registration_request,
            OpaquePasswordUpdateVerification::PendingPasskey,
        )
        .await
    }

    async fn start_opaque_password_update_after_verification(
        &self,
        user_id: &UserId,
        registration_request: Vec<u8>,
        verification: OpaquePasswordUpdateVerification,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        let password_version = self
            .user_password_repository
            .get_state(user_id)
            .await?
            .version;

        self.start_opaque_password_registration_session(
            user_id,
            registration_request,
            OpaqueRegistrationPurpose::PasswordUpdate {
                user_id: *user_id,
                expected_password_version: password_version,
                verification,
            },
            Vec::new(),
        )
        .await
    }

    async fn start_opaque_password_registration_session(
        &self,
        user_id: &UserId,
        registration_request: Vec<u8>,
        purpose: OpaqueRegistrationPurpose,
        credential_response: Vec<u8>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        let credential_identifier = Self::opaque_credential_identifier_for_user_id(user_id);
        let registration_start = self
            .opaque_password_service
            .start_registration(&credential_identifier, &registration_request)?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_registration_session_store
            .store(
                &session_id,
                &OpaqueRegistrationSession {
                    credential_identifier,
                    purpose,
                },
                Duration::from_secs(OPAQUE_REGISTRATION_SESSION_TTL_SECS),
            )
            .await?;

        Ok(OpaqueRegistrationStartChallenge {
            session_id,
            credential_response,
            registration_response: registration_start.registration_response,
        })
    }

    pub async fn finish_opaque_password_update(
        &self,
        user_id: &UserId,
        session_id: &str,
        credential_finalization: Vec<u8>,
        registration_upload: Vec<u8>,
    ) -> Result<User> {
        let Some(session) = self
            .opaque_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let OpaqueRegistrationPurpose::PasswordUpdate {
            user_id: session_user_id,
            expected_password_version,
            verification:
                OpaquePasswordUpdateVerification::CurrentOpaquePassword { server_login_state },
        } = session.purpose
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if session_user_id != *user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        self.opaque_password_service
            .finish_login(&server_login_state, &credential_finalization)?;

        self.finish_opaque_password_update_after_verified_session(
            user_id,
            session.credential_identifier,
            expected_password_version,
            registration_upload,
        )
        .await
    }

    pub async fn finish_opaque_password_update_after_external_verification(
        &self,
        user_id: &UserId,
        session_id: &str,
        registration_upload: Vec<u8>,
    ) -> Result<User> {
        let (credential_identifier, expected_password_version) = self
            .consume_verified_password_update_session(
                user_id,
                session_id,
                OpaquePasswordUpdateVerification::VerifiedExternal,
            )
            .await?;
        self.finish_opaque_password_update_after_verified_session(
            user_id,
            credential_identifier,
            expected_password_version,
            registration_upload,
        )
        .await
    }

    pub async fn finish_opaque_password_update_after_passkey_verification(
        &self,
        user_id: &UserId,
        session_id: &str,
        registration_upload: Vec<u8>,
    ) -> Result<User> {
        let (credential_identifier, expected_password_version) = self
            .consume_verified_password_update_session(
                user_id,
                session_id,
                OpaquePasswordUpdateVerification::PendingPasskey,
            )
            .await?;
        self.finish_opaque_password_update_after_verified_session(
            user_id,
            credential_identifier,
            expected_password_version,
            registration_upload,
        )
        .await
    }

    async fn consume_verified_password_update_session(
        &self,
        user_id: &UserId,
        session_id: &str,
        expected_verification: OpaquePasswordUpdateVerification,
    ) -> Result<(Vec<u8>, i32)> {
        let Some(session) = self
            .opaque_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let OpaqueRegistrationPurpose::PasswordUpdate {
            user_id: session_user_id,
            expected_password_version,
            verification,
        } = session.purpose
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        let verification_matches = matches!(
            (verification, expected_verification),
            (
                OpaquePasswordUpdateVerification::VerifiedExternal,
                OpaquePasswordUpdateVerification::VerifiedExternal
            ) | (
                OpaquePasswordUpdateVerification::PendingPasskey,
                OpaquePasswordUpdateVerification::PendingPasskey
            )
        );
        if session_user_id != *user_id || !verification_matches {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        Ok((session.credential_identifier, expected_password_version))
    }

    pub async fn finish_opaque_password_reset_after_external_verification(
        &self,
        session_id: &str,
        registration_upload: Vec<u8>,
    ) -> Result<User> {
        let Some(session) = self
            .opaque_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let OpaqueRegistrationPurpose::PasswordReset {
            user_id,
            expected_password_version,
        } = session.purpose
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        self.finish_opaque_password_update_after_verified_session(
            &user_id,
            session.credential_identifier,
            expected_password_version,
            registration_upload,
        )
        .await
    }

    async fn finish_opaque_password_update_after_verified_session(
        &self,
        user_id: &UserId,
        credential_identifier: Vec<u8>,
        expected_password_version: i32,
        registration_upload: Vec<u8>,
    ) -> Result<User> {
        let opaque_record = self
            .opaque_password_service
            .finish_registration(credential_identifier, &registration_upload)?;

        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let credential_state = self
            .user_password_repository
            .get_state_for_update_with_executor(user_id, &mut *tx)
            .await?;
        if credential_state.version != expected_password_version {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

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
        tracing::info!("OPAQUE password credential updated for user {user_id}");

        Ok(updated_user)
    }
}
