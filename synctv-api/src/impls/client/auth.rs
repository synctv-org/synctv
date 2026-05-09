//! Auth operations: register, login, `refresh_token`

use super::convert::user_to_proto;
use super::ClientApiImpl;
use crate::impls::ApiError;
use std::future::Future;
use std::net::IpAddr;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{AuthFactorMethod, AuthenticatedLogin};

pub(crate) struct PasskeyAuthChallenge {
    pub session_id: String,
    pub options_json: Vec<u8>,
}

fn mfa_method_to_proto(method: AuthFactorMethod) -> crate::proto::client::MfaMethod {
    match method {
        AuthFactorMethod::Password => crate::proto::client::MfaMethod::Password,
        AuthFactorMethod::WebAuthn => crate::proto::client::MfaMethod::Webauthn,
        AuthFactorMethod::Email => crate::proto::client::MfaMethod::Email,
    }
}

pub(crate) fn login_outcome_to_proto(
    outcome: AuthenticatedLogin,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::LoginResponse {
    match outcome {
        AuthenticatedLogin::Complete {
            user,
            access_token,
            refresh_token,
        } => crate::proto::client::LoginResponse {
            user: Some(user_to_proto(&user, public_id_codec)),
            access_token,
            refresh_token,
            mfa: None,
        },
        AuthenticatedLogin::MfaRequired { user, challenge } => {
            crate::proto::client::LoginResponse {
                user: Some(user_to_proto(&user, public_id_codec)),
                access_token: String::new(),
                refresh_token: String::new(),
                mfa: Some(crate::proto::client::MfaChallenge {
                    required: true,
                    session_id: challenge.session_id,
                    available_methods: challenge
                        .available_methods
                        .into_iter()
                        .map(|method| mfa_method_to_proto(method) as i32)
                        .collect(),
                    masked_email: challenge.masked_email.unwrap_or_default(),
                    expires_at: challenge.expires_at,
                }),
            }
        }
    }
}

fn normalize_optional_email(email: &str) -> Result<Option<String>, ApiError> {
    if email.trim().is_empty() {
        Ok(None)
    } else {
        crate::http::validation::validate_email(email)
            .map(Some)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))
    }
}

fn normalize_optional_identifier(username: &str, email: &str) -> Result<Option<String>, ApiError> {
    let has_username = !username.trim().is_empty();
    let has_email = !email.trim().is_empty();
    if has_username && has_email {
        return Err(ApiError::InvalidInput(
            "Provide at most one of username or email".to_string(),
        ));
    }
    if has_email {
        crate::http::validation::validate_email(email)
            .map(Some)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))
    } else if has_username {
        crate::http::validation::validate_username(username)
            .map(Some)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))
    } else {
        Ok(None)
    }
}

/// Outcome of a logout operation.
///
/// Logout always succeeds (the user's intent to log out is respected),
/// but `message` indicates if token invalidation may be delayed.
pub struct LogoutOutcome {
    pub blacklist_ok: bool,
    pub message: &'static str,
}

impl LogoutOutcome {
    pub const fn success() -> Self {
        Self {
            blacklist_ok: true,
            message: "",
        }
    }

    pub const fn blacklist_failed() -> Self {
        Self {
            blacklist_ok: false,
            message: "Logged out but token invalidation may be delayed",
        }
    }
}

impl ClientApiImpl {
    pub async fn register(
        &self,
        req: crate::proto::client::RegisterRequest,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        self.register_with_control(req, client_ip, None).await
    }

    pub async fn register_with_control(
        &self,
        mut req: crate::proto::client::RegisterRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        // Validate and sanitize username
        req.username = crate::http::validation::validate_username(&req.username)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Validate password strength
        crate::http::validation::validate_password(&req.password)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        crate::impls::validate_proto_request(&req)?;

        let email = if req.email.is_empty() {
            None
        } else {
            Some(req.email.clone())
        };

        // Register user (returns tuple: (User, Option<access_token>, Option<refresh_token>))
        // Tokens are None when email verification is required (user is Pending).
        let (user, access_token, refresh_token) = self
            .user_service
            .register_with_control(req.username, email, req.password, client_ip, control)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(&user, &self.public_id_codec)),
            access_token: access_token.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
        })
    }

    pub async fn login(
        &self,
        req: crate::proto::client::LoginRequest,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        self.login_with_control(req, client_ip, None).await
    }

    pub async fn login_with_control(
        &self,
        req: crate::proto::client::LoginRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let has_username = !req.username.trim().is_empty();
        let has_email = !req.email.trim().is_empty();
        if has_username == has_email {
            return Err(ApiError::InvalidInput(
                "Provide exactly one of username or email".to_string(),
            ));
        }
        if req.password.is_empty() {
            return Err(ApiError::InvalidInput("Password is required".to_string()));
        }
        if !req.email_token.is_empty() {
            return Err(ApiError::InvalidInput(
                "email_token cannot be combined with password login".to_string(),
            ));
        }

        let identifier = if has_email {
            crate::http::validation::validate_email(&req.email)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?
        } else {
            crate::http::validation::validate_username(&req.username)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?
        };

        // Login user (returns tuple: (User, access_token, refresh_token))
        let outcome = self
            .user_service
            .login_with_control(identifier, req.password, client_ip, control)
            .await
            .map_err(ApiError::from)?;

        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn start_opaque_login_with_control(
        &self,
        req: crate::proto::client::StartOpaqueLoginRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::StartOpaqueLoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let has_username = !req.username.trim().is_empty();
        let has_email = !req.email.trim().is_empty();
        if has_username == has_email {
            return Err(ApiError::InvalidInput(
                "Provide exactly one of username or email".to_string(),
            ));
        }

        let identifier = if has_email {
            crate::http::validation::validate_email(&req.email)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?
        } else {
            crate::http::validation::validate_username(&req.username)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?
        };

        let challenge = self
            .user_service
            .start_opaque_login_with_control(identifier, req.credential_request, client_ip, control)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::StartOpaqueLoginResponse {
            session_id: challenge.session_id,
            credential_response: challenge.credential_response,
        })
    }

    pub async fn finish_opaque_login_with_control(
        &self,
        req: crate::proto::client::FinishOpaqueLoginRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let outcome = self
            .user_service
            .finish_opaque_login_with_control(
                &req.session_id,
                req.credential_finalization,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn start_opaque_registration_with_control(
        &self,
        mut req: crate::proto::client::StartOpaqueRegistrationRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::StartOpaqueRegistrationResponse, ApiError> {
        req.username = crate::http::validation::validate_username(&req.username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = if req.email.trim().is_empty() {
            None
        } else {
            Some(
                crate::http::validation::validate_email(&req.email)
                    .map_err(|error| ApiError::InvalidInput(error.to_string()))?,
            )
        };
        req.email = email.clone().unwrap_or_default();
        crate::impls::validate_proto_request(&req)?;

        let challenge = self
            .user_service
            .start_opaque_registration_with_control(
                req.username,
                email,
                req.registration_request,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::StartOpaqueRegistrationResponse {
            session_id: challenge.session_id,
            registration_response: challenge.registration_response,
        })
    }

    pub async fn finish_opaque_registration_with_control(
        &self,
        req: crate::proto::client::FinishOpaqueRegistrationRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let (user, access_token, refresh_token) = self
            .user_service
            .finish_opaque_registration_with_control(
                &req.session_id,
                req.registration_upload,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(&user, &self.public_id_codec)),
            access_token: access_token.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
        })
    }

    pub(crate) async fn start_passkey_registration_challenge_with_control(
        &self,
        username: String,
        email: String,
        name: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<PasskeyAuthChallenge, ApiError> {
        let username = crate::http::validation::validate_username(&username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = normalize_optional_email(&email)?;
        let credential_name = if name.trim().is_empty() {
            None
        } else {
            Some(name.trim().to_string())
        };
        let challenge = self
            .passkey_service()?
            .start_account_registration(username, email, credential_name, client_ip, control)
            .await
            .map_err(ApiError::from)?;
        Ok(PasskeyAuthChallenge {
            session_id: challenge.session_id,
            options_json: challenge.options_json,
        })
    }

    pub async fn start_passkey_registration_with_control(
        &self,
        req: crate::proto::client::StartPasskeyRegistrationRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::StartPasskeyRegistrationResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let challenge = self
            .start_passkey_registration_challenge_with_control(
                req.username,
                req.email,
                req.name,
                client_ip,
                control,
            )
            .await?;
        let options = super::passkey::passkey_options_to_json_bytes(challenge.options_json)?;
        Ok(crate::proto::client::StartPasskeyRegistrationResponse {
            session_id: challenge.session_id,
            options,
        })
    }

    pub(crate) async fn finish_passkey_registration_bytes_with_control(
        &self,
        session_id: &str,
        credential_json: &[u8],
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        let (user, access_token, refresh_token) = self
            .passkey_service()?
            .finish_account_registration(session_id, credential_json, client_ip, control)
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(&user, &self.public_id_codec)),
            access_token,
            refresh_token,
        })
    }

    pub async fn finish_passkey_registration_with_control(
        &self,
        req: crate::proto::client::FinishPasskeyRegistrationRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.finish_passkey_registration_bytes_with_control(
            &req.session_id,
            &req.credential,
            client_ip,
            control,
        )
        .await
    }

    pub(crate) async fn start_passkey_login_challenge_with_control(
        &self,
        username: String,
        email: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<PasskeyAuthChallenge, ApiError> {
        let identifier = normalize_optional_identifier(&username, &email)?;
        let challenge = self
            .passkey_service()?
            .start_login(identifier.as_deref(), client_ip, control)
            .await
            .map_err(ApiError::from)?;
        Ok(PasskeyAuthChallenge {
            session_id: challenge.session_id,
            options_json: challenge.options_json,
        })
    }

    pub async fn start_passkey_login_with_control(
        &self,
        req: crate::proto::client::StartPasskeyLoginRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::StartPasskeyLoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let challenge = self
            .start_passkey_login_challenge_with_control(req.username, req.email, client_ip, control)
            .await?;
        let options = super::passkey::passkey_options_to_json_bytes(challenge.options_json)?;
        Ok(crate::proto::client::StartPasskeyLoginResponse {
            session_id: challenge.session_id,
            options,
        })
    }

    pub(crate) async fn finish_passkey_login_bytes_with_control(
        &self,
        session_id: &str,
        credential_json: &[u8],
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        let outcome = self
            .passkey_service()?
            .finish_login(session_id, credential_json, client_ip, control)
            .await
            .map_err(ApiError::from)?;
        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn finish_passkey_login_with_control(
        &self,
        req: crate::proto::client::FinishPasskeyLoginRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.finish_passkey_login_bytes_with_control(
            &req.session_id,
            &req.credential,
            client_ip,
            control,
        )
        .await
    }

    pub(crate) async fn start_mfa_passkey_challenge_with_control(
        &self,
        mfa_session_id: &str,
    ) -> Result<PasskeyAuthChallenge, ApiError> {
        let user = self
            .user_service
            .get_mfa_session_user_for_method(mfa_session_id, AuthFactorMethod::WebAuthn)
            .await
            .map_err(ApiError::from)?;
        let challenge = self
            .passkey_service()?
            .start_user_verification(&user.id)
            .await
            .map_err(ApiError::from)?;
        Ok(PasskeyAuthChallenge {
            session_id: challenge.session_id,
            options_json: challenge.options_json,
        })
    }

    pub async fn start_mfa_passkey_with_control(
        &self,
        req: crate::proto::client::StartMfaPasskeyRequest,
    ) -> Result<crate::proto::client::StartMfaPasskeyResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let challenge = self
            .start_mfa_passkey_challenge_with_control(&req.mfa_session_id)
            .await?;
        let options = super::passkey::passkey_options_to_json_bytes(challenge.options_json)?;
        Ok(crate::proto::client::StartMfaPasskeyResponse {
            passkey_session_id: challenge.session_id,
            options,
        })
    }

    pub(crate) async fn finish_mfa_passkey_bytes_with_control(
        &self,
        mfa_session_id: &str,
        passkey_session_id: &str,
        credential_json: &[u8],
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        let user = self
            .user_service
            .get_mfa_session_user_for_method(mfa_session_id, AuthFactorMethod::WebAuthn)
            .await
            .map_err(ApiError::from)?;
        self.passkey_service()?
            .finish_user_verification(passkey_session_id, credential_json, &user.id)
            .await
            .map_err(ApiError::from)?;
        let outcome = self
            .user_service
            .complete_mfa_session_with_control(
                mfa_session_id,
                AuthFactorMethod::WebAuthn,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn finish_mfa_passkey_with_control(
        &self,
        req: crate::proto::client::FinishMfaPasskeyRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.finish_mfa_passkey_bytes_with_control(
            &req.mfa_session_id,
            &req.passkey_session_id,
            &req.credential,
            client_ip,
            control,
        )
        .await
    }

    pub async fn verify_mfa_password_with_control(
        &self,
        req: crate::proto::client::VerifyMfaPasswordRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let outcome = self
            .user_service
            .verify_mfa_password_with_control(
                &req.mfa_session_id,
                &req.password,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn refresh_token(
        &self,
        req: crate::proto::client::RefreshTokenRequest,
    ) -> Result<crate::proto::client::RefreshTokenResponse, ApiError> {
        self.refresh_token_with_control(req, None).await
    }

    pub async fn refresh_token_with_control(
        &self,
        req: crate::proto::client::RefreshTokenRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RefreshTokenResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        // Refresh tokens (returns tuple: (new_access_token, new_refresh_token))
        let (access_token, refresh_token) = self
            .user_service
            .refresh_token_with_control(req.refresh_token, control)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RefreshTokenResponse {
            access_token,
            refresh_token,
        })
    }

    /// Logout: blacklist the current access token so it cannot be reused.
    ///
    /// Extracts the JTI from the raw Bearer token, computes the remaining TTL,
    /// and adds it to the token blacklist. The token will be rejected by the
    /// security pipeline on subsequent requests.
    ///
    /// Returns an error when token revocation fails so callers never treat a
    /// non-revoked token as successfully logged out.
    pub async fn logout(&self, raw_token: &str) -> Result<LogoutOutcome, ApiError> {
        revoke_access_token_for_logout(&self.jwt_service, raw_token, |jti, ttl_secs| async move {
            self.user_service
                .blacklist_access_token(&jti, ttl_secs)
                .await
        })
        .await?;
        Ok(LogoutOutcome::success())
    }
}

async fn revoke_access_token_for_logout<F, Fut>(
    jwt_service: &synctv_core::service::JwtService,
    raw_token: &str,
    blacklist: F,
) -> Result<(), ApiError>
where
    F: FnOnce(String, u64) -> Fut,
    Fut: Future<Output = synctv_core::Result<()>>,
{
    let claims = jwt_service
        .verify_access_token(raw_token)
        .map_err(|error| {
            tracing::debug!(
                error = %error,
                "Rejecting logout because the presented token is not a valid access token"
            );
            ApiError::Authentication(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;

    if claims.jti.is_empty() {
        return Err(ApiError::Authentication(
            "Access token missing jti".to_string(),
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0).cast_unsigned();
    if remaining_ttl == 0 {
        return Err(ApiError::Authentication(
            "Access token already expired".to_string(),
        ));
    }

    blacklist(claims.jti.clone(), remaining_ttl)
        .await
        .map_err(|error| {
            tracing::warn!(
                error = %error,
                jti = %claims.jti,
                "Failed to blacklist access token during logout"
            );
            ApiError::from(error)
        })
}

#[cfg(test)]
mod tests {
    use super::revoke_access_token_for_logout;
    use crate::impls::ApiError;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use synctv_core::{
        models::UserId,
        service::{JwtService, TokenType},
    };

    fn create_test_jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890").unwrap()
    }

    #[tokio::test]
    async fn test_logout_blacklist_failure_is_propagated() {
        let jwt_service = create_test_jwt_service();
        let token = jwt_service
            .sign_token(&UserId::new(), TokenType::Access, 0)
            .unwrap();

        let result =
            revoke_access_token_for_logout(&jwt_service, &token, |_jti, _ttl_secs| async {
                Err(synctv_core::Error::Internal(
                    "Blacklist store unavailable".to_string(),
                ))
            })
            .await;

        match result {
            Err(ApiError::Internal(message)) => {
                assert!(message.contains("Blacklist store unavailable"));
            }
            other => panic!("expected propagated internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_logout_invalid_token_is_rejected() {
        let jwt_service = create_test_jwt_service();
        let blacklist_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&blacklist_called);

        let result = revoke_access_token_for_logout(
            &jwt_service,
            "invalid.token.here",
            move |_jti, _ttl| {
                let called = Arc::clone(&called);
                async move {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await;

        match result {
            Err(ApiError::Authentication(message)) => {
                assert!(
                    message == synctv_common::messages::INVALID_OR_EXPIRED_TOKEN,
                    "unexpected authentication error: {message}"
                );
            }
            other => panic!("expected authentication failure, got {other:?}"),
        }
        assert!(!blacklist_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_logout_refresh_token_is_rejected() {
        let jwt_service = create_test_jwt_service();
        let token = jwt_service
            .sign_token(&UserId::new(), TokenType::Refresh, 0)
            .unwrap();

        let result =
            revoke_access_token_for_logout(&jwt_service, &token, |_jti, _ttl| async { Ok(()) })
                .await;

        match result {
            Err(ApiError::Authentication(message)) => {
                assert!(
                    message == synctv_common::messages::INVALID_OR_EXPIRED_TOKEN,
                    "unexpected authentication error: {message}"
                );
            }
            other => panic!("expected refresh token rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_logout_rejects_zero_ttl_access_token() {
        let jwt_service = JwtService::with_durations(
            "test-secret-key-for-jwt-that-is-long-enough-1234567890",
            0,
            30,
            4,
            0,
        )
        .unwrap();
        let token = jwt_service
            .sign_token(&UserId::new(), TokenType::Access, 0)
            .unwrap();

        let result =
            revoke_access_token_for_logout(&jwt_service, &token, |_jti, _ttl| async { Ok(()) })
                .await;

        match result {
            Err(ApiError::Authentication(message)) => {
                assert!(
                    message.contains("expired") || message.contains("Expired"),
                    "unexpected authentication error: {message}"
                );
            }
            other => panic!("expected expired token rejection, got {other:?}"),
        }
    }
}
