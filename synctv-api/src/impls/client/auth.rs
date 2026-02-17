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

    /// Logout: blacklist both the access token and refresh token server-side.
    ///
    /// Fail-fast: returns an error if token revocation fails (e.g. Redis is
    /// down). This prevents the user from believing they are logged out while
    /// their tokens remain valid -- a security concern on shared/public devices.
    ///
    /// Retries up to 3 times with exponential backoff before giving up.
    pub async fn logout(&self, access_token: &str, refresh_token: Option<&str>) -> Result<crate::proto::client::LogoutResponse, ApiError> {
        const MAX_RETRIES: u32 = 3;
        let mut last_err = None;

        for attempt in 0..MAX_RETRIES {
            match self.user_service.logout(access_token, refresh_token).await {
                Ok(()) => return Ok(crate::proto::client::LogoutResponse { success: true }),
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        error = %e,
                        "Logout token revocation failed, retrying"
                    );
                    last_err = Some(e);
                    if attempt + 1 < MAX_RETRIES {
                        let backoff = tokio::time::Duration::from_millis(100 * 2u64.pow(attempt));
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }

        let err = last_err.unwrap();
        tracing::error!(error = %err, "Logout failed after {MAX_RETRIES} attempts: token revocation unsuccessful");
        Err(ApiError::Internal(
            "Logout failed: could not revoke token. Please try again later.".to_string(),
        ))
    }
}
