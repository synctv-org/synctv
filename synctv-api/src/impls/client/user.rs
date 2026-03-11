//! User operations: `get_profile`, `set_username`, `set_password`

use crate::impls::ApiError;
use synctv_core::models::UserId;
use synctv_core::validation::UsernameValidator;

use super::convert::user_to_proto;
use super::ClientApiImpl;

impl ClientApiImpl {
    pub async fn update_profile(
        &self,
        user_id: &str,
        username: Option<String>,
        old_password: Option<String>,
        new_password: Option<String>,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        if let Some(ref username) = username {
            UsernameValidator::new()
                .validate(username.trim())
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        if let Some(ref password) = new_password {
            crate::http::validation::validate_password(password)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        let uid = UserId::from_string(user_id.to_string());
        let updated_user = self
            .user_service
            .update_profile(&uid, username, old_password, new_password)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(user_to_proto(&updated_user)),
        })
    }

    pub async fn get_profile(
        &self,
        user_id: &str,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(user_to_proto(&user)),
        })
    }

    pub async fn set_username(
        &self,
        user_id: &str,
        req: crate::proto::client::SetUsernameRequest,
    ) -> Result<crate::proto::client::SetUsernameResponse, ApiError> {
        let response = self
            .update_profile(
                user_id,
                Some(req.new_username.trim().to_string()),
                None,
                None,
            )
            .await?;

        Ok(crate::proto::client::SetUsernameResponse {
            user: response.user,
        })
    }

    pub async fn set_password(
        &self,
        user_id: &str,
        req: crate::proto::client::SetPasswordRequest,
    ) -> Result<crate::proto::client::SetPasswordResponse, ApiError> {
        self.update_profile(
            user_id,
            None,
            Some(req.old_password),
            Some(req.new_password),
        )
        .await?;

        Ok(crate::proto::client::SetPasswordResponse { success: true })
    }
}
