use std::{net::IpAddr, time::Duration};

use synctv_common::ExecutionControl;

use crate::{
    models::{OpaquePasswordRecord, SignupMethod, User},
    repository::PasswordCredentialMaterial,
    service::user::UserService,
    Error, Result,
};

use super::{
    identity_policy::password_binding,
    session_types::{
        OpaqueRegistrationPurpose, OpaqueRegistrationSession, OpaqueRegistrationStartChallenge,
        OPAQUE_REGISTRATION_SESSION_TTL_SECS,
    },
    AccountRegistrationOutcome, PendingAccountRegistration, RegistrationMode, RegistrationPolicy,
};

mod admin_create;
mod email;

impl UserService {
    async fn issue_registration_tokens(&self, user: &User) -> Result<(String, String)> {
        let session_id = synctv_common::snanoid!(32);
        let password_version = self
            .user_password_repository
            .get_state(&user.id)
            .await?
            .version;
        let credential_binding = password_binding(password_version);
        let access_token = self
            .jwt_service
            .sign_access_token_with_auth_context_and_session(
                &user.id,
                password_version,
                None,
                Some(&session_id),
                &credential_binding,
            )?;
        let refresh_token = self.jwt_service.sign_refresh_token_with_session(
            &user.id,
            password_version,
            None,
            &session_id,
            &credential_binding,
        )?;
        Ok((access_token, refresh_token))
    }

    async fn registered_registration_outcome(
        &self,
        user: User,
        email: Option<String>,
    ) -> Result<AccountRegistrationOutcome> {
        let (access_token, refresh_token) = self.issue_registration_tokens(&user).await?;
        Ok(AccountRegistrationOutcome::Registered {
            user,
            email,
            access_token,
            refresh_token,
        })
    }

    pub(super) async fn record_registration_bruteforce_failure(
        &self,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) {
        if let Err(error) = self
            .brute_force
            .record_failure_with_control(
                synctv_common::reserved::REGISTRATION_BRUTE_FORCE_SCOPE,
                client_ip,
                control,
            )
            .await
        {
            tracing::warn!(error = %error, "Failed to record registration brute-force failure");
        }
    }

    pub(crate) async fn validate_registration_identity_with_control(
        &self,
        username: &str,
        email: Option<&str>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.brute_force
            .check_allowed_with_control(
                synctv_common::reserved::REGISTRATION_BRUTE_FORCE_SCOPE,
                client_ip,
                control,
            )
            .await?;

        let username = match Self::normalize_username_for_storage(username) {
            Ok(username) => username,
            Err(error) => {
                self.record_registration_bruteforce_failure(client_ip, control)
                    .await;
                return Err(error);
            }
        };

        if let Some(email_addr) = email {
            if let Err(error) = Self::validate_email(email_addr) {
                self.record_registration_bruteforce_failure(client_ip, control)
                    .await;
                return Err(error);
            }

            if let Err(error) = self.validate_email_whitelist_policy(email_addr) {
                self.record_registration_bruteforce_failure(client_ip, control)
                    .await;
                return Err(error);
            }
        }

        let email_conflicts = match email {
            Some(email_addr) => self.user_email_repository.email_exists(email_addr).await?,
            None => false,
        };
        if self.repository.get_by_username(&username).await?.is_some() || email_conflicts {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }
        if self
            .has_pending_registration_request(&username, email)
            .await?
        {
            return Err(Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ));
        }

        Ok(())
    }

    pub(crate) fn registration_policy(&self, mode: RegistrationMode) -> Result<RegistrationPolicy> {
        if let RegistrationMode::Password = mode {
            if let Some(policy) = self.password_registration_policy_override {
                return Ok(policy);
            }
        }

        let Some(registry) = self.settings_registry.as_ref() else {
            return Ok(RegistrationPolicy {
                enabled: false,
                need_review: false,
            });
        };

        Ok(match mode {
            RegistrationMode::Password => RegistrationPolicy {
                enabled: registry.enable_password_signup.get()?,
                need_review: registry.password_signup_need_review.get()?,
            },
            RegistrationMode::Email => RegistrationPolicy {
                enabled: registry.enable_email_signup.get()?,
                need_review: registry.email_signup_need_review.get()?,
            },
            RegistrationMode::OAuth2 => RegistrationPolicy {
                enabled: false,
                need_review: false,
            },
            RegistrationMode::WebAuthn => RegistrationPolicy {
                enabled: registry.enable_webauthn_signup.get()?,
                need_review: registry.webauthn_signup_need_review.get()?,
            },
        })
    }

    pub(crate) fn ensure_registration_review_supported(
        &self,
        mode: RegistrationMode,
    ) -> Result<RegistrationPolicy> {
        let policy = self.registration_policy(mode)?;
        if !policy.enabled {
            return Err(Error::Authorization(format!(
                "{} registration is disabled",
                mode.as_str()
            )));
        }
        if policy.need_review && !mode.supports_review() {
            return Err(Error::InvalidInput(format!(
                "{} registration review is not supported yet",
                mode.as_str()
            )));
        }
        Ok(policy)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn complete_password_registration_with_opaque_record(
        &self,
        username: String,
        email: Option<String>,
        opaque_record: OpaquePasswordRecord,
        registration_policy: RegistrationPolicy,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
        cache_reason: &'static str,
    ) -> Result<AccountRegistrationOutcome> {
        if registration_policy.need_review {
            let pending_user = self
                .create_registration_request(
                    &username,
                    email.as_deref(),
                    &opaque_record,
                    SignupMethod::Password,
                )
                .await?;
            return Ok(AccountRegistrationOutcome::PendingReview(
                PendingAccountRegistration {
                    review_request_id: pending_user.id,
                    username: pending_user.username,
                    email,
                },
            ));
        }

        let user = User::new(username.clone(), SignupMethod::Password);
        let created_user = match async {
            let mut tx = self.repository.pool().begin().await?;
            let created_user = self
                .repository
                .create_with_executor(&user, &mut *tx)
                .await?;
            self.user_email_repository
                .create_for_user_with_executor(&created_user, email.as_deref(), &mut *tx)
                .await?;
            self.user_password_repository
                .create_for_user_with_executor(
                    &created_user,
                    PasswordCredentialMaterial::opaque_only(&opaque_record),
                    &mut *tx,
                )
                .await?;
            tx.commit().await?;
            Ok::<_, Error>(created_user)
        }
        .await
        {
            Ok(created_user) => created_user,
            Err(Error::AlreadyExists(_)) => {
                return Err(Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                ));
            }
            Err(error) => {
                self.record_registration_bruteforce_failure(client_ip, control)
                    .await;
                return Err(error);
            }
        };

        self.cache_username_best_effort(&created_user.id, &username, cache_reason)
            .await;

        self.registered_registration_outcome(created_user, email)
            .await
    }

    pub(crate) fn map_registration_identity_conflict(error: Error) -> Error {
        match error {
            Error::AlreadyExists(_) => Error::AlreadyExists(
                synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
            ),
            other => other,
        }
    }

    pub(crate) fn is_username_conflict(error: &Error) -> bool {
        matches!(error, Error::AlreadyExists(message) if message.contains("Username"))
    }

    pub async fn register_with_direct_password_transport_with_control(
        &self,
        username: String,
        email: Option<String>,
        password: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AccountRegistrationOutcome> {
        let registration_policy =
            self.ensure_registration_review_supported(RegistrationMode::Password)?;
        let username = Self::normalize_username_for_storage(&username)?;
        self.validate_password(&password)?;

        self.validate_registration_identity_with_control(
            &username,
            email.as_deref(),
            client_ip,
            control,
        )
        .await?;

        let credential_identifier = Self::opaque_credential_identifier_for_new_user(&username);
        let opaque_record = self
            .opaque_password_service
            .register_password(&credential_identifier, &password)?;

        self.complete_password_registration_with_opaque_record(
            username,
            email,
            opaque_record,
            registration_policy,
            client_ip,
            control,
            "direct_password_register",
        )
        .await
    }

    pub async fn start_opaque_registration_with_control(
        &self,
        username: String,
        email: Option<String>,
        registration_request: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<OpaqueRegistrationStartChallenge> {
        self.ensure_registration_review_supported(RegistrationMode::Password)?;
        let username = Self::normalize_username_for_storage(&username)?;

        self.validate_registration_identity_with_control(
            &username,
            email.as_deref(),
            client_ip,
            control,
        )
        .await?;

        let credential_identifier = Self::opaque_credential_identifier_for_new_user(&username);
        let registration_start = self
            .opaque_password_service
            .start_registration(&credential_identifier, &registration_request)?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_registration_session_store
            .store(
                &session_id,
                &OpaqueRegistrationSession {
                    credential_identifier,
                    purpose: OpaqueRegistrationPurpose::Account { username, email },
                },
                Duration::from_secs(OPAQUE_REGISTRATION_SESSION_TTL_SECS),
            )
            .await?;

        Ok(OpaqueRegistrationStartChallenge {
            session_id,
            credential_response: Vec::new(),
            registration_response: registration_start.registration_response,
        })
    }

    pub async fn finish_opaque_registration_with_control(
        &self,
        session_id: &str,
        registration_upload: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AccountRegistrationOutcome> {
        let Some(session) = self
            .opaque_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let OpaqueRegistrationPurpose::Account { username, email } = session.purpose else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        let username = Self::normalize_username_for_storage(&username)?;

        self.validate_registration_identity_with_control(
            &username,
            email.as_deref(),
            client_ip,
            control,
        )
        .await?;

        let opaque_record = self
            .opaque_password_service
            .finish_registration(session.credential_identifier, &registration_upload)?;

        let registration_policy =
            self.ensure_registration_review_supported(RegistrationMode::Password)?;

        self.complete_password_registration_with_opaque_record(
            username,
            email,
            opaque_record,
            registration_policy,
            client_ip,
            control,
            "opaque_register",
        )
        .await
    }

    /// Generate JWT tokens and populate username cache for a newly created user.
    pub async fn finalize_registration(&self, user: &User) -> Result<(String, String)> {
        let (access_token, refresh_token) = self.issue_registration_tokens(user).await?;
        self.cache_username_best_effort(&user.id, &user.username, "finalize_registration")
            .await;
        Ok((access_token, refresh_token))
    }
}
