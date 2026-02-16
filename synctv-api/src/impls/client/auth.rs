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

        // Register user (returns tuple: (User, access_token, refresh_token))
        let (user, access_token, refresh_token) = self
            .user_service
            .register(req.username, email, req.password)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::RegisterResponse {
            user: Some(user_to_proto(&user)),
            access_token,
            refresh_token,
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

    /// Logout: blacklist both the access token and refresh token server-side.
    ///
    /// Best-effort: logs failures but always returns success. This ensures
    /// consistent behavior between HTTP and gRPC transports -- a failed
    /// blacklist just means the old token remains valid until expiry.
    pub async fn logout(&self, access_token: &str, refresh_token: Option<&str>) -> crate::proto::client::LogoutResponse {
        if let Err(e) = self.user_service.logout(access_token, refresh_token).await {
            tracing::warn!(error = %e, "Failed to blacklist token during logout");
        }
        crate::proto::client::LogoutResponse { success: true }
    }
}
