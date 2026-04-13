//! User operations: `get_profile`, `set_username`, `set_password`

use crate::impls::ApiError;
use synctv_core::models::UserId;
use synctv_core::validation::UsernameValidator;

use super::convert::user_to_proto;
use super::ClientApiImpl;

impl ClientApiImpl {
    pub async fn delete_current_user(&self, user_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let summary = self
            .user_service
            .delete_user_with_summary(&uid)
            .await
            .map_err(ApiError::from)?;

        crate::impls::finalize_user_deletion(crate::impls::UserDeletionFinalizeArgs {
            room_service: &self.room_service,
            connection_service: self.connection_service.as_ref(),
            live_streaming_infrastructure: self.live_streaming_infrastructure.as_ref(),
            cluster_fanout: self.cluster_fanout.as_ref(),
            summary: &summary,
            deleted_by: &uid,
            disconnect_reason: "user_deleted",
        })
        .await;

        Ok(())
    }

    pub async fn update_profile(
        &self,
        user_id: &str,
        username: Option<String>,
        old_password: Option<String>,
        new_password: Option<String>,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let normalized_username = username.as_ref().map(|value| value.trim().to_string());
        let request = crate::proto::client::UpdateUserRequest {
            username: normalized_username.clone(),
            password: new_password.clone(),
            old_password: old_password.clone(),
        };
        crate::impls::validate_proto_request(&request)?;

        if let Some(ref username) = normalized_username {
            UsernameValidator::new()
                .validate(username)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        if let Some(ref password) = new_password {
            crate::http::validation::validate_password(password)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        let uid = UserId::from_string(user_id.to_string());
        let updated_user = self
            .user_service
            .update_profile(&uid, normalized_username, old_password, new_password)
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
