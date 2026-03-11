//! Auth operations: register, login, `refresh_token`

use super::convert::user_to_proto;
use super::ClientApiImpl;
use crate::impls::ApiError;
use std::future::Future;

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
        mut req: crate::proto::client::RegisterRequest,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        // Validate and sanitize username
        req.username = crate::http::validation::validate_username(&req.username)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Validate password strength
        crate::http::validation::validate_password(&req.password)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        let email = if req.email.is_empty() {
            None
        } else {
            Some(req.email.clone())
        };

        // Register user (returns tuple: (User, Option<access_token>, Option<refresh_token>))
        // Tokens are None when email verification is required (user is Pending).
        let (user, access_token, refresh_token) = self
            .user_service
            .register(req.username, email, req.password, client_ip)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(&user)),
            access_token: access_token.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
        })
    }

    pub async fn login(
        &self,
        req: crate::proto::client::LoginRequest,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        // Login user (returns tuple: (User, access_token, refresh_token))
        let (user, access_token, refresh_token) = self
            .user_service
            .login(req.username, req.password, client_ip)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::LoginResponse {
            user: Some(user_to_proto(&user)),
            access_token,
            refresh_token,
        })
    }

    pub async fn refresh_token(
        &self,
        req: crate::proto::client::RefreshTokenRequest,
    ) -> Result<crate::proto::client::RefreshTokenResponse, ApiError> {
        // Refresh tokens (returns tuple: (new_access_token, new_refresh_token))
        let (access_token, refresh_token) = self
            .user_service
            .refresh_token(req.refresh_token)
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
            ApiError::Authentication(error.to_string())
        })?;

    if claims.jti.is_empty() {
        return Err(ApiError::Authentication(
            "Access token missing jti".to_string(),
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0) as u64;
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
                    message.contains("Invalid token")
                        || message.contains("verification failed")
                        || message.contains("invalid"),
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
                    message.contains("Not an access token"),
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
