//! Auth operations: register, login, `refresh_token`, logout

use crate::impls::ApiError;
use super::ClientApiImpl;
use super::convert::user_to_proto;

impl ClientApiImpl {
    pub async fn register(
        &self,
        req: crate::proto::client::RegisterRequest,
    ) -> Result<crate::proto::client::RegisterResponse, ApiError> {
        // Validation is handled by UserService::register() using production validators
        let email = if req.email.is_empty() {
            None
        } else {
            Some(req.email.clone())
        };

        // Register user (returns tuple: (User, Option<access_token>, Option<refresh_token>))
        // Tokens are None when email verification is required (user is Pending).
        let (user, access_token, refresh_token) = self
            .user_service
            .register(req.username, email, req.password)
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

    /// Logout: validates the access token and blacklists it via Redis (if available).
    ///
    /// When Redis is configured, the token's JTI is added to a blacklist with a TTL
    /// equal to the token's remaining lifetime. Subsequent requests with this token
    /// will be rejected by the `SecurityPipeline`.
    ///
    /// When Redis is NOT configured, this is a no-op on the server side (tokens
    /// remain valid until natural expiration).
    pub async fn logout(&self, access_token: &str, refresh_token: Option<&str>) -> Result<crate::proto::client::LogoutResponse, ApiError> {
        // Validate the access token to ensure it's well-formed
        let claims = self.jwt_service.verify_access_token(access_token)
            .map_err(|e| ApiError::Authentication(format!("Invalid access token: {e}")))?;

        // Blacklist access token via SecurityPipeline (Redis-backed)
        if let Some(ref pipeline) = self.security_pipeline {
            let now = chrono::Utc::now().timestamp();
            let remaining = (claims.exp - now).max(0) as u64;

            if let Err(e) = pipeline.blacklist_token(&claims.jti, remaining).await {
                tracing::warn!(user_id = %claims.sub, error = %e, "Failed to blacklist access token on logout");
            }

            // Also blacklist the refresh token if provided
            if let Some(rt) = refresh_token {
                if let Ok(rt_claims) = self.jwt_service.verify_token(rt) {
                    let rt_remaining = (rt_claims.exp - now).max(0) as u64;
                    if let Err(e) = pipeline.blacklist_token(&rt_claims.jti, rt_remaining).await {
                        tracing::warn!(user_id = %claims.sub, error = %e, "Failed to blacklist refresh token on logout");
                    }
                }
            }

            tracing::info!(user_id = %claims.sub, "User logged out (tokens revoked)");
        } else {
            tracing::info!(
                user_id = %claims.sub,
                "User logged out (token blacklist not available - tokens expire naturally)"
            );
        }

        Ok(crate::proto::client::LogoutResponse { success: true })
    }
}
