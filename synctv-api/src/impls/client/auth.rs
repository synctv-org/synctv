//! Auth operations: register, login, refresh_token, logout

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
    ) -> Result<crate::proto::client::LoginResponse, ApiError> {
        // Login user (returns tuple: (User, access_token, refresh_token))
        let (user, access_token, refresh_token) = self
            .user_service
            .login(req.username, req.password)
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
        old_access_token: Option<&str>,
    ) -> Result<crate::proto::client::RefreshTokenResponse, ApiError> {
        // Refresh tokens (returns tuple: (new_access_token, new_refresh_token))
        let (access_token, refresh_token) = self
            .user_service
            .refresh_token(req.refresh_token, old_access_token)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RefreshTokenResponse {
            access_token,
            refresh_token,
        })
    }

    /// Logout: validates tokens and returns success.
    ///
    /// **Security Note**: Without Redis-based token blacklisting, tokens are
    /// NOT revoked server-side. They will remain valid until they expire
    /// naturally. Users should:
    /// - Use short token lifetimes to minimize exposure
    /// - Change password if tokens are suspected to be compromised
    ///
    /// This method validates the access token to ensure it's well-formed.
    pub async fn logout(&self, access_token: &str, _refresh_token: Option<&str>) -> Result<crate::proto::client::LogoutResponse, ApiError> {
        // Validate the access token to ensure it's well-formed
        let claims = self.jwt_service.verify_access_token(access_token)
            .map_err(|e| ApiError::Authentication(format!("Invalid access token: {e}")))?;

        tracing::info!(
            user_id = %claims.sub,
            "User logged out (tokens not revoked - will expire naturally)"
        );

        Ok(crate::proto::client::LogoutResponse { success: true })
    }
}
