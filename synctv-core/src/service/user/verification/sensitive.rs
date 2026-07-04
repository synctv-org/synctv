use std::{net::IpAddr, time::Duration};

use synctv_common::ExecutionControl;

use crate::{
    models::{User, UserAuthFactors, UserId},
    service::{TokenAuthContext, UserService},
    Error, Result,
};

use super::super::session_types::{
    AuthFactorMethod, SensitiveVerificationChallenge, SensitiveVerificationOutcome,
    SensitiveVerificationSession, SENSITIVE_VERIFICATION_PASSWORD_BRUTE_FORCE_PREFIX,
    SENSITIVE_VERIFICATION_SESSION_TTL_SECS, SENSITIVE_VERIFICATION_SESSION_TTL_SECS_I64,
};

impl UserService {
    fn sensitive_available_methods(
        auth_factors: &UserAuthFactors,
        completed_methods: &[AuthFactorMethod],
    ) -> Vec<AuthFactorMethod> {
        let mut methods = Vec::with_capacity(3);
        if auth_factors.password && !completed_methods.contains(&AuthFactorMethod::Password) {
            methods.push(AuthFactorMethod::Password);
        }
        if auth_factors.webauthn && !completed_methods.contains(&AuthFactorMethod::WebAuthn) {
            methods.push(AuthFactorMethod::WebAuthn);
        }
        if auth_factors.email && !completed_methods.contains(&AuthFactorMethod::Email) {
            methods.push(AuthFactorMethod::Email);
        }
        methods
    }

    fn sensitive_required_methods(auth_factors: &UserAuthFactors) -> Vec<AuthFactorMethod> {
        let mut methods = Vec::with_capacity(3);
        if auth_factors.password {
            methods.push(AuthFactorMethod::Password);
        }
        if auth_factors.webauthn {
            methods.push(AuthFactorMethod::WebAuthn);
        }
        if auth_factors.email {
            methods.push(AuthFactorMethod::Email);
        }
        methods
    }

    async fn sensitive_challenge_from_session(
        &self,
        session_id: &str,
        session: &SensitiveVerificationSession,
    ) -> Result<SensitiveVerificationChallenge> {
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(&session.user_id)
            .await?;
        let email = self
            .user_email_repository
            .get_email(&session.user_id)
            .await?;
        let available_methods =
            Self::sensitive_available_methods(&auth_factors, &session.completed_methods);
        let masked_email = Self::masked_email_for_challenge(
            &available_methods,
            email.as_deref(),
            "Sensitive verification challenge",
        )?;

        Ok(SensitiveVerificationChallenge {
            session_id: session_id.to_string(),
            required_count: session.required_count,
            required_methods: Self::sensitive_required_methods(&auth_factors),
            completed_methods: session.completed_methods.clone(),
            available_methods,
            masked_email,
            expires_at: session.expires_at,
        })
    }

    pub async fn start_sensitive_operation_verification(
        &self,
        user_id: &UserId,
        auth_context: Option<TokenAuthContext>,
    ) -> Result<SensitiveVerificationOutcome> {
        let user = self.get_user(user_id).await?;
        Self::validate_user_access(&user)?;
        let preferences = self
            .user_preferences_repository
            .get_or_default(user_id)
            .await?;
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(user_id)
            .await?;
        let required_count =
            if preferences.two_factor_enabled && auth_context != Some(TokenAuthContext::OAuth2) {
                2
            } else {
                1
            };
        let required_methods = Self::sensitive_required_methods(&auth_factors);
        let can_bootstrap_from_oauth2 =
            auth_context == Some(TokenAuthContext::OAuth2) && required_methods.is_empty();
        if required_methods.len() < required_count && !can_bootstrap_from_oauth2 {
            return Err(Error::InvalidInput(
                "Sensitive operation verification requires enough local authentication methods"
                    .to_string(),
            ));
        }

        let session_id = synctv_common::snanoid!(48);
        let expires_at =
            chrono::Utc::now().timestamp() + SENSITIVE_VERIFICATION_SESSION_TTL_SECS_I64;
        if can_bootstrap_from_oauth2 {
            let session = SensitiveVerificationSession {
                user_id: *user_id,
                required_count: 0,
                completed_methods: Vec::new(),
                expires_at,
            };
            let verification_id = synctv_common::snanoid!(48);
            self.sensitive_verification_session_store
                .store(
                    &verification_id,
                    &session,
                    Duration::from_secs(SENSITIVE_VERIFICATION_SESSION_TTL_SECS),
                )
                .await?;
            return Ok(SensitiveVerificationOutcome::Complete { verification_id });
        }
        let session = SensitiveVerificationSession {
            user_id: *user_id,
            required_count,
            completed_methods: Vec::new(),
            expires_at,
        };
        self.sensitive_verification_session_store
            .store(
                &session_id,
                &session,
                Duration::from_secs(SENSITIVE_VERIFICATION_SESSION_TTL_SECS),
            )
            .await?;
        let challenge = self
            .sensitive_challenge_from_session(&session_id, &session)
            .await?;
        Ok(SensitiveVerificationOutcome::Pending(challenge))
    }

    pub async fn get_sensitive_operation_user_for_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<User> {
        let session = self
            .sensitive_verification_session_store
            .get(session_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let user = self
            .repository
            .get_by_id(&session.user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        Self::validate_user_access(&user)?;
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(&user.id)
            .await?;
        let available =
            Self::sensitive_available_methods(&auth_factors, &session.completed_methods);
        if !available.contains(&method) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Ok(user)
    }

    pub async fn finish_sensitive_operation_password_verification(
        &self,
        session_id: &str,
        password: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<SensitiveVerificationOutcome> {
        let session = self
            .sensitive_verification_session_store
            .get(session_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if session
            .completed_methods
            .contains(&AuthFactorMethod::Password)
        {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        let brute_force_key = format!(
            "{SENSITIVE_VERIFICATION_PASSWORD_BRUTE_FORCE_PREFIX}:{}",
            session.user_id.as_i64()
        );
        self.brute_force
            .check_subject_key_allowed_with_control(&brute_force_key, client_ip, control)
            .await?;
        let opaque_credential = self
            .user_password_repository
            .get_opaque_credential(&session.user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if !self
            .opaque_password_service
            .verify_password(&opaque_credential.record, password)?
        {
            if let Err(error) = self
                .brute_force
                .record_subject_key_failure_with_control(&brute_force_key, client_ip, control)
                .await
            {
                tracing::warn!(
                    error = %error,
                    user_id = %session.user_id,
                    "Failed to record sensitive password verification failure"
                );
            }
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        self.brute_force
            .reset_subject_key_with_control(&brute_force_key, control)
            .await?;
        self.complete_sensitive_operation_method(session_id, AuthFactorMethod::Password)
            .await
    }

    pub async fn finish_sensitive_operation_verified_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<SensitiveVerificationOutcome> {
        if method == AuthFactorMethod::Password {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        self.complete_sensitive_operation_method(session_id, method)
            .await
    }

    async fn complete_sensitive_operation_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<SensitiveVerificationOutcome> {
        let mut session = self
            .sensitive_verification_session_store
            .consume(session_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let user = self
            .repository
            .get_by_id(&session.user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        Self::validate_user_access(&user)?;
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(&user.id)
            .await?;
        let available =
            Self::sensitive_available_methods(&auth_factors, &session.completed_methods);
        if !available.contains(&method) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        session.completed_methods.push(method);
        if session.completed_methods.len() >= session.required_count {
            let verification_id = synctv_common::snanoid!(48);
            self.sensitive_verification_session_store
                .store(
                    &verification_id,
                    &session,
                    Duration::from_secs(SENSITIVE_VERIFICATION_SESSION_TTL_SECS),
                )
                .await?;
            return Ok(SensitiveVerificationOutcome::Complete { verification_id });
        }
        self.sensitive_verification_session_store
            .store(
                session_id,
                &session,
                Duration::from_secs(SENSITIVE_VERIFICATION_SESSION_TTL_SECS),
            )
            .await?;
        let challenge = self
            .sensitive_challenge_from_session(session_id, &session)
            .await?;
        Ok(SensitiveVerificationOutcome::Pending(challenge))
    }

    pub async fn consume_sensitive_operation_verification(
        &self,
        user_id: &UserId,
        verification_id: &str,
    ) -> Result<()> {
        let session = self
            .sensitive_verification_session_store
            .consume(verification_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if session.user_id != *user_id || session.completed_methods.len() < session.required_count {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_email_for_challenge_requires_email_when_method_available() {
        let error = UserService::masked_email_for_challenge(
            &[AuthFactorMethod::Email],
            None,
            "Test challenge",
        )
        .expect_err("email verification method requires a masked email source");

        assert!(
            matches!(error, Error::Internal(message) if message.contains("email verification"))
        );
    }

    #[test]
    fn masked_email_for_challenge_omits_email_for_other_methods() {
        let masked = UserService::masked_email_for_challenge(
            &[AuthFactorMethod::Password],
            Some("user@example.com"),
            "Test challenge",
        )
        .expect("non-email verification challenge should build");

        assert_eq!(masked, None);
    }
}
