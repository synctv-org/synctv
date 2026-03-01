//! User operations: `get_profile`, `set_username`, `set_password`

use crate::impls::ApiError;
use synctv_core::models::UserId;
use synctv_core::validation::UsernameValidator;

use super::convert::user_to_proto;
use super::ClientApiImpl;

impl ClientApiImpl {
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
        // Validate username using centralized validator (includes reserved word check)
        let username = req.new_username.trim().to_string();
        UsernameValidator::new()
            .validate(&username)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        let uid = UserId::from_string(user_id.to_string());
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        let old_version = user.version;
        let updated_user = synctv_core::models::User { username, ..user };

        let result_user = self
            .user_service
            .update_user(&updated_user, old_version)
            .await
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

        // B10 FIX: Validate new password strength at the API layer, consistent with
        // the register endpoint. The core service also validates, but the HTTP-level
        // check catches common/weak passwords (dictionary check) and provides
        // user-friendly error messages before hitting the database.
        crate::http::validation::validate_password(&req.new_password)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Verify old password before allowing change
        self.user_service
            .change_password(&uid, &req.old_password, &req.new_password)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::SetPasswordResponse { success: true })
    }
}
