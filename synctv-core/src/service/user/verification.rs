use std::{net::IpAddr, time::Duration};

use synctv_common::ExecutionControl;

use crate::{
    models::{User, UserAuthFactors, UserId},
    service::{user::UserService, TokenAuthContext, TokenCredentialBinding},
    Error, Result,
};

use super::session_types::{
    AuthFactorMethod, AuthenticatedLogin, MfaChallenge, MfaSession, TokenIssueContext,
    MFA_SESSION_TTL_SECS, MFA_SESSION_TTL_SECS_I64, TWO_FACTOR_REQUIRED_MESSAGE,
};

mod sensitive;

impl UserService {
    pub(super) fn validate_user_access(user: &User) -> Result<()> {
        if user.is_banned || user.is_deleted() {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        Ok(())
    }

    pub(super) async fn complete_authenticated_login_with_control(
        &self,
        user: User,
        first_factor: AuthFactorMethod,
        credential_binding: TokenCredentialBinding,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        if let Err(error) = Self::validate_user_access(&user) {
            if let Err(bf_err) = self
                .brute_force
                .record_failure_with_control(brute_force_key, client_ip, control)
                .await
            {
                tracing::warn!(error = %bf_err, "Failed to record login failure for brute-force tracking");
                return Err(bf_err);
            }
            return Err(error);
        }

        let preferences = self
            .user_preferences_repository
            .get_or_default(&user.id)
            .await?;
        if preferences.two_factor_enabled {
            let auth_factors = self
                .user_preferences_repository
                .auth_factors(&user.id)
                .await?;
            let available_methods = Self::available_mfa_methods(&auth_factors, first_factor);
            if available_methods.is_empty() {
                return Err(Error::Authentication(
                    TWO_FACTOR_REQUIRED_MESSAGE.to_string(),
                ));
            }
            let session_id = synctv_common::snanoid!(48);
            let expires_at = crate::SystemClock.now().timestamp() + MFA_SESSION_TTL_SECS_I64;
            let session = MfaSession {
                user_id: user.id,
                first_factor,
                credential_binding,
                brute_force_key: brute_force_key.to_string(),
                expires_at,
            };
            self.mfa_session_store
                .store(
                    &session_id,
                    &session,
                    Duration::from_secs(MFA_SESSION_TTL_SECS),
                )
                .await?;
            let email = self.user_email_repository.get_email(&user.id).await?;
            let challenge = Self::mfa_challenge_from_session(
                &session_id,
                &session,
                email.as_deref(),
                available_methods,
            )?;
            return Ok(AuthenticatedLogin::MfaRequired {
                user,
                email,
                challenge,
            });
        }

        let password_version = self
            .user_password_repository
            .get_state(&user.id)
            .await?
            .version;
        let (access_token, refresh_token) = self
            .issue_tokens_after_successful_authentication(
                &user,
                password_version,
                brute_force_key,
                client_ip,
                TokenIssueContext {
                    auth_context: None,
                    credential_binding: &credential_binding,
                },
                control,
            )
            .await?;

        let email = self.user_email_repository.get_email(&user.id).await?;
        Ok(AuthenticatedLogin::Complete {
            user,
            email,
            access_token,
            refresh_token,
        })
    }

    fn available_mfa_methods(
        auth_factors: &UserAuthFactors,
        first_factor: AuthFactorMethod,
    ) -> Vec<AuthFactorMethod> {
        let mut methods = Vec::with_capacity(4);
        if auth_factors.webauthn && first_factor != AuthFactorMethod::WebAuthn {
            methods.push(AuthFactorMethod::WebAuthn);
        }
        if auth_factors.totp {
            methods.push(AuthFactorMethod::Totp);
            if auth_factors.totp_recovery_codes_remaining > 0 {
                methods.push(AuthFactorMethod::RecoveryCode);
            }
        }
        if auth_factors.email && first_factor != AuthFactorMethod::Email {
            methods.push(AuthFactorMethod::Email);
        }
        methods
    }

    fn mfa_challenge_from_session(
        session_id: &str,
        session: &MfaSession,
        email: Option<&str>,
        available_methods: Vec<AuthFactorMethod>,
    ) -> Result<MfaChallenge> {
        let masked_email = Self::masked_email_for_mfa_methods(&available_methods, email)?;
        Ok(MfaChallenge {
            session_id: session_id.to_string(),
            available_methods,
            masked_email,
            expires_at: session.expires_at,
        })
    }

    fn masked_email_for_mfa_methods(
        available_methods: &[AuthFactorMethod],
        email: Option<&str>,
    ) -> Result<Option<String>> {
        Self::masked_email_for_challenge(available_methods, email, "MFA challenge")
    }

    fn masked_email_for_challenge(
        available_methods: &[AuthFactorMethod],
        email: Option<&str>,
        context: &str,
    ) -> Result<Option<String>> {
        if available_methods.contains(&AuthFactorMethod::Email) {
            return email
                .map(crate::service::mask_email)
                .map(Some)
                .ok_or_else(|| {
                    Error::Internal(format!(
                        "{context} includes email verification without a user email"
                    ))
                });
        }

        Ok(None)
    }

    pub(super) async fn issue_tokens_after_successful_authentication(
        &self,
        user: &User,
        password_version: i32,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        issue_context: TokenIssueContext<'_>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        if let Err(error) = self
            .brute_force
            .reset_with_control(brute_force_key, control)
            .await
        {
            tracing::warn!(error = %error, "Failed to reset brute-force counter after successful login");
        }
        if let Some(ip) = client_ip {
            if let Err(error) = self.brute_force.reset_ip_with_control(&ip, control).await {
                tracing::warn!(error = %error, "Failed to reset IP brute-force counter after successful login");
            }
        }

        let session_id = synctv_common::snanoid!(32);
        let access_token = self
            .jwt_service
            .sign_access_token_with_auth_context_and_session(
                &user.id,
                password_version,
                issue_context.auth_context,
                Some(&session_id),
                issue_context.credential_binding,
            )?;
        let refresh_token = self.jwt_service.sign_refresh_token_with_session(
            &user.id,
            password_version,
            issue_context.auth_context,
            &session_id,
            issue_context.credential_binding,
        )?;

        Ok((access_token, refresh_token))
    }

    pub async fn login_with_verified_email(
        &self,
        user_id: &UserId,
        expected_email: &str,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<AuthenticatedLogin> {
        self.login_with_verified_email_with_control(
            user_id,
            expected_email,
            brute_force_key,
            client_ip,
            None,
        )
        .await
    }

    pub async fn login_with_verified_email_with_control(
        &self,
        user_id: &UserId,
        expected_email: &str,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let mut tx = self.repository.pool().begin().await?;
        let user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let email = self
            .user_email_repository
            .get_email_with_executor(&user.id, &mut *tx)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if !email.eq_ignore_ascii_case(expected_email) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let login = self
            .complete_authenticated_login_with_control(
                user,
                AuthFactorMethod::Email,
                TokenCredentialBinding::Email { email },
                brute_force_key,
                client_ip,
                control,
            )
            .await?;
        tx.commit().await?;
        Ok(login)
    }

    pub async fn get_mfa_challenge(&self, session_id: &str) -> Result<MfaChallenge> {
        let session = self
            .mfa_session_store
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
        let available_methods = Self::available_mfa_methods(&auth_factors, session.first_factor);
        if available_methods.is_empty() {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        let email = self.user_email_repository.get_email(&user.id).await?;
        Self::mfa_challenge_from_session(session_id, &session, email.as_deref(), available_methods)
    }

    pub async fn get_mfa_session_user_for_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<User> {
        let (_session, user) = self
            .get_mfa_session_and_user_for_method(session_id, method)
            .await?;
        Ok(user)
    }

    async fn get_mfa_session_and_user_for_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<(MfaSession, User)> {
        let session = self
            .mfa_session_store
            .get(session_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let user = self
            .repository
            .get_by_id(&session.user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        self.ensure_mfa_method_available(&session, &user, method)
            .await?;
        Ok((session, user))
    }

    async fn ensure_mfa_method_available(
        &self,
        session: &MfaSession,
        user: &User,
        method: AuthFactorMethod,
    ) -> Result<()> {
        if session.first_factor == method {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Self::validate_user_access(user)?;
        let auth_factors = self
            .user_preferences_repository
            .auth_factors(&user.id)
            .await?;
        let available_methods = Self::available_mfa_methods(&auth_factors, session.first_factor);
        if !available_methods.contains(&method) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Ok(())
    }

    pub async fn complete_mfa_session_with_control(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let session = self
            .mfa_session_store
            .consume(session_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        let user = self
            .repository
            .get_by_id(&session.user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        self.ensure_mfa_method_available(&session, &user, method)
            .await?;
        let password_version = self
            .user_password_repository
            .get_state(&user.id)
            .await?
            .version;
        let (access_token, refresh_token) = self
            .issue_tokens_after_successful_authentication(
                &user,
                password_version,
                &session.brute_force_key,
                client_ip,
                TokenIssueContext {
                    auth_context: Some(TokenAuthContext::LocalTwoFactor),
                    credential_binding: &session.credential_binding,
                },
                control,
            )
            .await?;
        let email = self.user_email_repository.get_email(&user.id).await?;
        Ok(AuthenticatedLogin::Complete {
            user,
            email,
            access_token,
            refresh_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_email_for_mfa_methods_requires_email_when_method_available() {
        let error = UserService::masked_email_for_mfa_methods(&[AuthFactorMethod::Email], None)
            .expect_err("email MFA method requires a masked email source");

        assert!(
            matches!(error, Error::Internal(message) if message.contains("email verification"))
        );
    }

    #[test]
    fn masked_email_for_mfa_methods_omits_email_for_other_methods() {
        let masked = UserService::masked_email_for_mfa_methods(
            &[AuthFactorMethod::WebAuthn],
            Some("user@example.com"),
        )
        .expect("non-email MFA challenge should build");

        assert_eq!(masked, None);
    }
}
