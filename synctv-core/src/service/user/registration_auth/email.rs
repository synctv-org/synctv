use std::net::IpAddr;

use synctv_common::ExecutionControl;

use crate::{
    models::{SignupMethod, User, UserStatus},
    repository::{EmailRegistrationTokenRepository, PasswordCredentialMaterial},
    service::{EmailOutboxService, UserService},
    Error, Result,
};

use super::{
    AccountRegistrationOutcome, PendingAccountRegistration, RegistrationMode, RegistrationPolicy,
};

struct CompletedEmailRegistration {
    user: User,
    username: String,
    email: String,
}

impl UserService {
    async fn complete_email_registration_transaction(
        &self,
        email_token: &str,
        password: &str,
        registration_policy: RegistrationPolicy,
    ) -> Result<CompletedEmailRegistration> {
        let mut tx = self.repository.pool().begin().await?;
        let now = crate::SystemClock.now();
        let token_record = EmailRegistrationTokenRepository::lock_valid_for_update_with_executor(
            email_token,
            now,
            &mut tx,
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;
        let username = Self::normalize_username_for_storage(&token_record.username)?;
        let email = token_record.email;

        Self::lock_pending_registration_identity(&mut tx, &username, Some(&email)).await?;

        if self
            .repository
            .get_by_username_with_executor(&username, &mut *tx)
            .await?
            .is_some()
            || self
                .user_email_repository
                .email_exists_with_executor(&email, &mut *tx)
                .await?
            || self
                .has_pending_registration_request_with_executor(&username, Some(&email), &mut *tx)
                .await?
        {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }

        let credential_identifier = Self::opaque_credential_identifier_for_new_user(&username);
        let opaque_record = self
            .opaque_password_service
            .register_password(&credential_identifier, password)?;

        let user = User::new(username.clone(), SignupMethod::Email);
        let user = if registration_policy.need_review {
            let request_id = Self::create_registration_request_with_executor(
                &username,
                Some(&email),
                &opaque_record,
                SignupMethod::Email,
                &mut *tx,
            )
            .await?;
            let mut pending_user =
                User::new_with_status(username.clone(), SignupMethod::Email, UserStatus::Active);
            pending_user.id = request_id;
            pending_user
        } else {
            let created_user = self
                .repository
                .create_with_executor(&user, &mut *tx)
                .await
                .map_err(Self::map_registration_identity_conflict)?;
            self.user_email_repository
                .create_for_user_with_executor(&created_user, Some(&email), &mut *tx)
                .await
                .map_err(Self::map_registration_identity_conflict)?;
            self.user_password_repository
                .create_for_user_with_executor(
                    &created_user,
                    PasswordCredentialMaterial::opaque_only(&opaque_record),
                    &mut *tx,
                )
                .await?;
            created_user
        };

        let used_rows = EmailRegistrationTokenRepository::mark_used_with_executor(
            email_token,
            crate::SystemClock.now(),
            &mut tx,
        )
        .await?;
        if used_rows != 1 {
            return Err(Error::InvalidInput(
                synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
            ));
        }
        tx.commit().await?;

        Ok(CompletedEmailRegistration {
            user,
            username,
            email,
        })
    }

    pub async fn create_email_registration_token_with_control(
        &self,
        username: String,
        email: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.ensure_registration_review_supported(RegistrationMode::Email)?;
        let username = Self::normalize_username_for_storage(&username)?;
        Self::validate_email(&email)?;
        self.validate_email_whitelist_policy(&email)?;

        self.validate_registration_identity_with_control(
            &username,
            Some(&email),
            client_ip,
            control,
        )
        .await?;

        let token = synctv_common::snanoid!(64);
        let expires_at = crate::SystemClock.now() + chrono::Duration::minutes(15);
        self.email_registration_token_repository
            .create_or_replace_unused(&token, &username, &email, expires_at)
            .await?;

        Ok(token)
    }

    pub async fn create_email_registration_and_enqueue_with_control(
        &self,
        outbox: &EmailOutboxService,
        username: String,
        email: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.ensure_registration_review_supported(RegistrationMode::Email)?;
        let username = Self::normalize_username_for_storage(&username)?;
        Self::validate_email(&email)?;
        self.validate_email_whitelist_policy(&email)?;
        self.validate_registration_identity_with_control(
            &username,
            Some(&email),
            client_ip,
            control,
        )
        .await?;
        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let token = synctv_common::snanoid!(64);
        let expires_at = crate::SystemClock.now() + chrono::Duration::minutes(15);
        let job = outbox.prepare_registration(&email, &token, expires_at)?;
        let mut tx = self.repository.pool().begin().await?;
        self.email_registration_token_repository
            .create_or_replace_unused_with_executor(&token, &username, &email, expires_at, &mut tx)
            .await?;
        if !outbox
            .repository()
            .insert_with_executor(&job, &mut tx)
            .await?
        {
            return Err(Error::Internal(
                "Registration email outbox job was unexpectedly deduplicated".to_string(),
            ));
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn complete_email_registration_with_direct_password_transport_with_control(
        &self,
        email_token: &str,
        password: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AccountRegistrationOutcome> {
        let registration_policy =
            self.ensure_registration_review_supported(RegistrationMode::Email)?;
        self.validate_password(&password)?;

        let registration = match self
            .complete_email_registration_transaction(email_token, &password, registration_policy)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.record_registration_bruteforce_failure(client_ip, control)
                    .await;
                return Err(error);
            }
        };

        if registration_policy.need_review {
            return Ok(AccountRegistrationOutcome::PendingReview(
                PendingAccountRegistration {
                    review_request_id: registration.user.id,
                    username: registration.username,
                    email: Some(registration.email),
                },
            ));
        }

        self.cache_username_best_effort(
            &registration.user.id,
            &registration.username,
            "email_direct_password_register",
        )
        .await;

        self.registered_registration_outcome(registration.user, Some(registration.email))
            .await
    }

    pub async fn delete_unused_email_registration_token(&self, token: &str) -> Result<u64> {
        self.email_registration_token_repository
            .delete_unused_token(token)
            .await
    }
}
