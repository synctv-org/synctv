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
            email,
            access_token,
            refresh_token,
        } => crate::proto::client::LoginResponse {
            user: Some(user_to_proto(&user, email.as_deref(), public_id_codec)),
            access_token,
            refresh_token,
            mfa: None,
        },
        AuthenticatedLogin::MfaRequired {
            user,
            email,
            challenge,
        } => crate::proto::client::LoginResponse {
            user: Some(user_to_proto(&user, email.as_deref(), public_id_codec)),
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
        },
    }
}

fn normalize_optional_email(email: Option<String>) -> Result<Option<String>, ApiError> {
    email
        .filter(|email| !email.trim().is_empty())
        .map(|email| {
            crate::impls::validation::validate_email(&email)
                .map_err(|error| ApiError::InvalidInput(error.to_string()))
        })
        .transpose()
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
        crate::impls::validation::validate_email(email)
            .map(Some)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))
    } else if has_username {
        crate::impls::validation::validate_username(username)
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
    pub async fn register_with_control(
        &self,
        mut req: crate::proto::client::RegisterRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        req.username = crate::impls::validation::validate_username(&req.username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = normalize_optional_email(req.email.clone())?;
        crate::impls::validate_proto_request(&req)?;
        let (user, access_token, refresh_token) = self
            .user_service
            .register_with_control(
                req.username,
                email.clone(),
                req.password,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(
                &user,
                email.as_deref(),
                &self.public_id_codec,
            )),
            access_token: access_token.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
        })
    }

    pub async fn login_with_control(
        &self,
        req: crate::proto::client::LoginRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let identifier = match req.identifier {
            Some(crate::proto::client::login_request::Identifier::Email(email)) => {
                crate::impls::validation::validate_email(&email)
                    .map_err(|e| ApiError::InvalidInput(e.to_string()))?
            }
            Some(crate::proto::client::login_request::Identifier::Username(username)) => {
                crate::impls::validation::validate_login_username(&username)
                    .map_err(|e| ApiError::InvalidInput(e.to_string()))?
            }
            None => {
                return Err(ApiError::InvalidInput(
                    "Login identifier is required".to_string(),
                ));
            }
        };
        let outcome = self
            .user_service
            .login_with_control(identifier, req.password, client_ip, control)
            .await
            .map_err(ApiError::from)?;
        Ok(login_outcome_to_proto(outcome, &self.public_id_codec))
    }

    pub async fn confirm_email_login_with_control(
        &self,
        email_api: Option<&crate::impls::EmailApiImpl>,
        req: crate::proto::client::ConfirmEmailLoginRequest,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let email_api = email_api.ok_or_else(|| {
            ApiError::ServiceUnavailable(
                synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string(),
            )
        })?;
        let result = email_api
            .confirm_email_login_with_control(&req.email, &req.email_token, client_ip, control)
            .await?;

        Ok(login_outcome_to_proto(result.login, &self.public_id_codec))
    }

    pub async fn create_guest_token_with_control(
        &self,
        req: crate::proto::client::CreateGuestTokenRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::CreateGuestTokenResponse, ApiError> {
        let _ = control;
        crate::impls::validate_proto_request(&req)?;

        let room_id = self.parse_room_id(&req.room_id)?;
        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(ApiError::from)?;
        if room.is_banned {
            return Err(ApiError::Authorization(
                "This room has been banned".to_string(),
            ));
        }
        if room.status.is_closed() {
            return Err(ApiError::Authorization(
                "This room is closed and not accepting new connections".to_string(),
            ));
        }

        self.room_service
            .check_guest_allowed(&room_id, self.settings_registry.as_ref().map(AsRef::as_ref))
            .await
            .map_err(ClientApiImpl::map_room_access_error)?;

        let guest_version = self
            .room_service
            .get_room_guest_version(&room_id)
            .await
            .map_err(ApiError::from)?;
        let token = self
            .jwt_service
            .sign_guest_token_with_version(&room_id, guest_version)
            .map_err(ApiError::from)?;
        let claims = self
            .jwt_service
            .verify_guest_token(&token)
            .map_err(ApiError::from)?;
        let guest_id = crate::impls::messaging::guest_public_id(&claims.session_id);
        let display_name = crate::impls::messaging::guest_display_name(&claims.session_id);
        let now = chrono::Utc::now().timestamp();
        let expires_in_secs = claims.exp.saturating_sub(now).try_into().unwrap_or(0);

        Ok(crate::proto::client::CreateGuestTokenResponse {
            token,
            room_id: req.room_id,
            guest_id,
            display_name,
            expires_at: claims.exp,
            expires_in_secs,
            usage: "Pass this token as Authorization: Bearer <token> for supported room APIs in the bound public room."
                .to_string(),
        })
    }

    pub async fn start_opaque_login_with_control(
        &self,
        req: crate::proto::client::StartOpaqueLoginRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::StartOpaqueLoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let identifier = match req.identifier {
            Some(crate::proto::client::start_opaque_login_request::Identifier::Email(email)) => {
                crate::impls::validation::validate_email(&email)
                    .map_err(|e| ApiError::InvalidInput(e.to_string()))?
            }
            Some(crate::proto::client::start_opaque_login_request::Identifier::Username(
                username,
            )) => crate::impls::validation::validate_login_username(&username)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?,
            None => {
                return Err(ApiError::InvalidInput(
                    "Login identifier is required".to_string(),
                ));
            }
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
        req.username = crate::impls::validation::validate_username(&req.username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = normalize_optional_email(req.email.clone())?;
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
        let email = self
            .user_service
            .get_email(&user.id)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(
                &user,
                email.as_deref(),
                &self.public_id_codec,
            )),
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
        let username = crate::impls::validation::validate_username(&username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = normalize_optional_email(Some(email))?;
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
        let email = self
            .user_service
            .get_email(&user.id)
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(
                &user,
                email.as_deref(),
                &self.public_id_codec,
            )),
            access_token: access_token.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
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
        let (username, email) = match req.identifier {
            Some(crate::proto::client::start_passkey_login_request::Identifier::Username(
                username,
            )) => (username, String::new()),
            Some(crate::proto::client::start_passkey_login_request::Identifier::Email(email)) => {
                (String::new(), email)
            }
            None => (String::new(), String::new()),
        };
        let challenge = self
            .start_passkey_login_challenge_with_control(username, email, client_ip, control)
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

    /// Logout: revoke the current authenticated session.
    ///
    /// Extracts the JTI and session id from the raw Bearer token. The matching
    /// refresh-token session is revoked first so a transient failure can be
    /// retried with the current access token, then the access token is
    /// blacklisted so it cannot be reused.
    ///
    /// Returns an error when token revocation fails so callers never treat a
    /// non-revoked token as successfully logged out.
    pub async fn logout(&self, raw_token: &str) -> Result<LogoutOutcome, ApiError> {
        revoke_session_for_logout(&self.jwt_service, raw_token, |logout_token| async move {
            revoke_logout_token_in_order(
                logout_token,
                |user_id, session_id, revoked_at| async move {
                    self.user_service
                        .revoke_refresh_token_session(&user_id, session_id.as_deref(), revoked_at)
                        .await
                },
                |jti, remaining_ttl_secs| async move {
                    self.user_service
                        .blacklist_access_token(&jti, remaining_ttl_secs)
                        .await
                },
            )
            .await
        })
        .await?;
        Ok(LogoutOutcome::success())
    }
}

struct LogoutToken {
    user_id: synctv_core::models::UserId,
    session_id: Option<String>,
    jti: String,
    remaining_ttl_secs: u64,
    revoked_at: i64,
}

async fn revoke_logout_token_in_order<FR, FB, FutR, FutB>(
    logout_token: LogoutToken,
    revoke_refresh_session: FR,
    blacklist_access_token: FB,
) -> synctv_core::Result<()>
where
    FR: FnOnce(synctv_core::models::UserId, Option<String>, i64) -> FutR,
    FB: FnOnce(String, u64) -> FutB,
    FutR: Future<Output = synctv_core::Result<()>>,
    FutB: Future<Output = synctv_core::Result<()>>,
{
    let LogoutToken {
        user_id,
        session_id,
        jti,
        remaining_ttl_secs,
        revoked_at,
    } = logout_token;

    revoke_refresh_session(user_id, session_id, revoked_at).await?;
    blacklist_access_token(jti, remaining_ttl_secs).await
}

async fn revoke_session_for_logout<F, Fut>(
    jwt_service: &synctv_core::service::JwtService,
    raw_token: &str,
    revoke: F,
) -> Result<(), ApiError>
where
    F: FnOnce(LogoutToken) -> Fut,
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
    let user_id = claims.user_id().map_err(ApiError::from)?;

    revoke(LogoutToken {
        user_id,
        session_id: claims.sid.clone(),
        jti: claims.jti.clone(),
        remaining_ttl_secs: remaining_ttl,
        revoked_at: now,
    })
    .await
    .map_err(|error| {
        tracing::warn!(
            error = %error,
            jti = %claims.jti,
            "Failed to revoke session during logout"
        );
        ApiError::from(error)
    })
}

#[cfg(test)]
mod tests {
    use super::{revoke_logout_token_in_order, revoke_session_for_logout, LogoutToken};
    use crate::impls::ApiError;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use synctv_core::{
        models::UserId,
        service::{JwtService, TokenType},
    };

    fn create_test_jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890").unwrap()
    }

    #[test]
    fn opaque_login_username_validation_allows_bootstrap_root_identifier() {
        let username = crate::impls::validation::validate_login_username("root")
            .expect("login must allow an existing bootstrap root username");

        assert_eq!(username, "root");
    }

    #[test]
    fn opaque_registration_username_validation_still_rejects_reserved_root() {
        let error = crate::impls::validation::validate_username("root")
            .expect_err("registration/update validation must keep reserved username protection");

        assert!(
            error.to_string().contains("reserved"),
            "reserved-word validation should remain in place: {error}"
        );
    }

    #[tokio::test]
    async fn test_logout_blacklist_failure_is_propagated() {
        let jwt_service = create_test_jwt_service();
        let token = jwt_service
            .sign_token(&UserId::new(), TokenType::Access, 0)
            .unwrap();

        let result = revoke_session_for_logout(&jwt_service, &token, |_logout_token| async {
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

        let result =
            revoke_session_for_logout(&jwt_service, "invalid.token.here", move |_logout_token| {
                let called = Arc::clone(&called);
                async move {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                }
            })
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
            revoke_session_for_logout(&jwt_service, &token, |_logout_token| async { Ok(()) }).await;

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
            revoke_session_for_logout(&jwt_service, &token, |_logout_token| async { Ok(()) }).await;

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

    #[tokio::test]
    async fn test_logout_passes_session_context_to_revoker() {
        let jwt_service = create_test_jwt_service();
        let user_id = UserId::new();
        let session_id = "session-for-logout";
        let token = jwt_service
            .sign_token_with_auth_context_and_session(
                &user_id,
                TokenType::Access,
                0,
                None,
                Some(session_id),
                &synctv_core::service::auth::TokenCredentialBinding::Password { version: 0 },
            )
            .unwrap();

        let result = revoke_session_for_logout(&jwt_service, &token, |logout_token| async move {
            assert_eq!(logout_token.user_id, user_id);
            assert_eq!(logout_token.session_id.as_deref(), Some(session_id));
            assert!(!logout_token.jti.is_empty());
            assert!(logout_token.remaining_ttl_secs > 0);
            assert!(logout_token.revoked_at > 0);
            Ok(())
        })
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_logout_revokes_refresh_session_before_blacklisting_access_token() {
        let order = Arc::new(AtomicUsize::new(0));
        let refresh_order = Arc::clone(&order);
        let blacklist_order = Arc::clone(&order);
        let user_id = UserId::new();

        let result = revoke_logout_token_in_order(
            LogoutToken {
                user_id,
                session_id: Some("logout-session".to_string()),
                jti: "logout-jti".to_string(),
                remaining_ttl_secs: 60,
                revoked_at: 1_700_000_000,
            },
            move |actual_user_id, actual_session_id, revoked_at| {
                let refresh_order = Arc::clone(&refresh_order);
                async move {
                    assert_eq!(actual_user_id, user_id);
                    assert_eq!(actual_session_id.as_deref(), Some("logout-session"));
                    assert_eq!(revoked_at, 1_700_000_000);
                    assert_eq!(refresh_order.fetch_add(1, Ordering::SeqCst), 0);
                    Ok(())
                }
            },
            move |jti, ttl_secs| {
                let blacklist_order = Arc::clone(&blacklist_order);
                async move {
                    assert_eq!(jti, "logout-jti");
                    assert_eq!(ttl_secs, 60);
                    assert_eq!(blacklist_order.fetch_add(1, Ordering::SeqCst), 1);
                    Ok(())
                }
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(order.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_logout_refresh_revocation_failure_does_not_blacklist_access_token() {
        let blacklist_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&blacklist_called);

        let result = revoke_logout_token_in_order(
            LogoutToken {
                user_id: UserId::new(),
                session_id: Some("logout-session".to_string()),
                jti: "logout-jti".to_string(),
                remaining_ttl_secs: 60,
                revoked_at: 1_700_000_000,
            },
            |_user_id, _session_id, _revoked_at| async {
                Err(synctv_core::Error::Internal(
                    "refresh session revocation unavailable".to_string(),
                ))
            },
            move |_jti, _ttl_secs| {
                let called = Arc::clone(&called);
                async move {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await;

        assert!(
            result.is_err(),
            "logout should fail when refresh session revocation fails"
        );
        assert!(
            !blacklist_called.load(Ordering::SeqCst),
            "access token must remain usable for a logout retry"
        );
    }
}
