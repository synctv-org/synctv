//! Auth operations: register, login, `refresh_token`

use super::convert::user_to_proto;
use super::ClientApiImpl;
use crate::impls::ApiError;

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
    pub async fn logout(&self, raw_token: &str) -> Result<(), ApiError> {
        match self.jwt_service.verify_access_token(raw_token) {
            Ok(claims) => {
                if !claims.jti.is_empty() {
                    let now = chrono::Utc::now().timestamp();
                    let remaining_ttl = (claims.exp - now).max(0) as u64;
                    if remaining_ttl > 0 {
                        if let Err(e) = self
                            .user_service
                            .blacklist_access_token(&claims.jti, remaining_ttl)
                            .await
                        {
                            // Log the failure but still return success to the client.
                            // The token will expire naturally if blacklisting fails.
                            tracing::warn!(
                                error = %e,
                                jti = %claims.jti,
                                "Failed to blacklist access token on logout; token will expire naturally",
                            );
                        }
                    }
                }
            }
            Err(e) => {
                // Token may be expired or malformed. Still succeed; the client
                // is logging out and the token is no longer useful anyway.
                tracing::debug!(error = %e, "Could not parse token during logout; skipping blacklist");
            }
        }
        Ok(())
    }
}
