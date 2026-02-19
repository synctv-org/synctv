//! Auth operations: register, login, `refresh_token`

use crate::impls::ApiError;
use super::ClientApiImpl;
use super::convert::user_to_proto;

impl ClientApiImpl {
    pub async fn register(
        &self,
        req: crate::proto::client::RegisterRequest,
        client_ip: Option<std::net::IpAddr>,
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
}
