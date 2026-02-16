//! User operations: get_profile, set_username, set_password

use crate::impls::ApiError;
use synctv_core::models::UserId;

use super::ClientApiImpl;
use super::convert::user_to_proto;

impl ClientApiImpl {
    pub async fn get_profile(
        &self,
        user_id: &str,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let user = self.user_service.get_user(&uid).await
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
        // Validate username length and charset (consistent with registration rules)
        let username = req.new_username.trim().to_string();
        if username.chars().count() < synctv_core::validation::USERNAME_MIN {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at least {} characters",
                synctv_core::validation::USERNAME_MIN
            )));
        }
        if username.chars().count() > synctv_core::validation::USERNAME_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at most {} characters",
                synctv_core::validation::USERNAME_MAX
            )));
        }
        if !username.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(ApiError::InvalidInput("Username can only contain letters, numbers, underscores, and hyphens".to_string()));
        }
        if username.starts_with('_') || username.starts_with('-') {
            return Err(ApiError::InvalidInput("Username cannot start with underscore or hyphen".to_string()));
        }

        let uid = UserId::from_string(user_id.to_string());
        let user = self.user_service.get_user(&uid).await
            .map_err(ApiError::from)?;

        let updated_user = synctv_core::models::User {
            username,
            ..user
        };

        let result_user = self.user_service.update_user(&updated_user).await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::SetUsernameResponse {
            user: Some(user_to_proto(&result_user)),
        })
    }

    pub async fn set_password(
        &self,
        user_id: &str,
        req: crate::proto::client::SetPasswordRequest,
    ) -> Result<crate::proto::client::SetPasswordResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());

        // Verify old password before allowing change
        self.user_service.change_password(&uid, &req.old_password, &req.new_password).await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::SetPasswordResponse {
            success: true,
        })
    }
}
