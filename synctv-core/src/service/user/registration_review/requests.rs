use webauthn_rs::prelude::Passkey;

use super::super::registration_types::PendingRegistrationPasskey;
use crate::{
    models::{OpaquePasswordRecord, ReviewStatus, SignupMethod, User, UserId, UserStatus},
    service::UserService,
    Error, Result,
};

mod identity;
mod load;

impl UserService {
    pub(in crate::service::user) async fn create_registration_request(
        &self,
        username: &str,
        email: Option<&str>,
        opaque_record: &OpaquePasswordRecord,
        signup_method: SignupMethod,
    ) -> Result<User> {
        let mut tx = self.repository.pool().begin().await?;
        Self::lock_pending_registration_identity(&mut tx, username, email).await?;
        let email_exists = match email {
            Some(email) => self.user_email_repository.email_exists(email).await?,
            None => false,
        };
        if self.repository.get_by_username(username).await?.is_some()
            || email_exists
            || self
                .has_pending_registration_request_with_executor(username, email, &mut *tx)
                .await?
        {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }

        let request_id = Self::create_registration_request_with_executor(
            username,
            email,
            opaque_record,
            signup_method,
            &mut *tx,
        )
        .await?;
        tx.commit().await?;

        let mut user =
            User::new_with_status(username.to_string(), signup_method, UserStatus::Active);
        user.id = request_id;
        Ok(user)
    }

    pub(in crate::service::user) async fn create_registration_request_with_executor<'e, E>(
        username: &str,
        email: Option<&str>,
        opaque_record: &OpaquePasswordRecord,
        signup_method: SignupMethod,
        executor: E,
    ) -> Result<UserId>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if !matches!(signup_method, SignupMethod::Email | SignupMethod::Password) {
            return Err(Error::InvalidInput(
                "OPAQUE registration request requires email or password signup method".to_string(),
            ));
        }

        let request_id = sqlx::query_scalar!(
            r#"
            INSERT INTO user_registration_requests (
                username, email, opaque_record,
                opaque_credential_identifier, opaque_ciphersuite,
                opaque_server_setup_version, signup_method, status, requested_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CURRENT_TIMESTAMP)
            RETURNING id AS "id: UserId"
            "#,
            username,
            email,
            &opaque_record.record,
            &opaque_record.credential_identifier,
            opaque_record.ciphersuite.as_str(),
            opaque_record.server_setup_version,
            i16::from(signup_method),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(executor)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                )
            }
            _ => Error::Database(e),
        })?;
        Ok(request_id)
    }

    pub(crate) async fn create_oauth2_registration_request_with_executor<'e, E>(
        &self,
        username: &str,
        provider_user_id: &str,
        user_info: &crate::service::oauth2::OAuth2UserInfo,
        executor: E,
    ) -> Result<UserId>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let request_id = sqlx::query_scalar!(
            r#"
            INSERT INTO user_registration_requests (
                username, signup_method, status, requested_at,
                oauth2_provider_type, oauth2_provider_instance_name, oauth2_provider_issuer,
                oauth2_provider_user_id, oauth2_provider_username, oauth2_avatar_url
            )
            VALUES ($1, $2, $3, CURRENT_TIMESTAMP, $4, $5, $6, $7, $8, $9)
            RETURNING id AS "id: UserId"
            "#,
            username,
            i16::from(SignupMethod::OAuth2),
            i16::from(ReviewStatus::Pending),
            user_info.provider.as_i16(),
            user_info.provider_instance_name.as_str(),
            user_info.provider_issuer.as_deref(),
            provider_user_id,
            user_info.username.as_str(),
            user_info.avatar.as_deref()
        )
        .fetch_one(executor)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                )
            }
            _ => Error::Database(e),
        })?;

        Ok(request_id)
    }

    pub(crate) async fn create_webauthn_registration_request(
        &self,
        username: &str,
        email: Option<&str>,
        passkey: &Passkey,
        credential_name: Option<&str>,
    ) -> Result<User> {
        let credential_id = AsRef::<[u8]>::as_ref(passkey.cred_id()).to_vec();
        let stored_passkey = PendingRegistrationPasskey::from_passkey(passkey);

        let mut tx = self.repository.pool().begin().await?;
        Self::lock_pending_registration_identity(&mut tx, username, email).await?;
        let email_exists = match email {
            Some(email) => self.user_email_repository.email_exists(email).await?,
            None => false,
        };
        if self.repository.get_by_username(username).await?.is_some()
            || email_exists
            || self
                .has_pending_registration_request_with_executor(username, email, &mut *tx)
                .await?
        {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }
        if self
            .has_pending_webauthn_registration_with_executor(&credential_id, &mut *tx)
            .await?
        {
            return Err(Error::AlreadyExists(
                "Passkey credential is already registered".to_string(),
            ));
        }

        let request_id = sqlx::query_scalar!(
            r#"
            INSERT INTO user_registration_requests (
                username, email, signup_method, status, requested_at,
                webauthn_credential_id, webauthn_passkey, webauthn_credential_name
            )
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP, $5, $6, $7)
            RETURNING id AS "id: UserId"
            "#,
            username,
            email,
            i16::from(SignupMethod::WebAuthn),
            i16::from(ReviewStatus::Pending),
            credential_id,
            stored_passkey as PendingRegistrationPasskey,
            credential_name
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                )
            }
            _ => Error::Database(e),
        })?;
        tx.commit().await?;

        let mut user = User::new_with_status(
            username.to_string(),
            SignupMethod::WebAuthn,
            UserStatus::Active,
        );
        user.id = request_id;
        Ok(user)
    }

    async fn has_pending_webauthn_registration_with_executor<'e, E>(
        &self,
        credential_id: &[u8],
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
                  AND status = $2
                  AND webauthn_credential_id = $1
            ) AS "exists!"
            "#,
            credential_id,
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(executor)
        .await?;

        Ok(exists)
    }
}
