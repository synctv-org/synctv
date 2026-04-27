//! User operations: `get_profile`, `set_username`, `set_password`

use crate::impls::ApiError;
use crate::realtime_lifecycle::DeletedRoomFanoutReservation;
use synctv_core::models::{PageParams, RoomId, UserId};
use synctv_core::validation::UsernameValidator;

use super::convert::user_to_proto;
use super::ClientApiImpl;

const USER_ROOM_DELETION_PAGE_SIZE: u32 = 100;

async fn list_owned_room_ids(
    api: &ClientApiImpl,
    user_id: &UserId,
) -> Result<Vec<RoomId>, ApiError> {
    let mut page = 1;
    let mut room_ids = Vec::new();

    loop {
        let (rooms, total) = api
            .room_service
            .list_rooms_by_creator(
                user_id,
                PageParams::new(Some(page), Some(USER_ROOM_DELETION_PAGE_SIZE)),
            )
            .await
            .map_err(ApiError::from)?;

        if rooms.is_empty() {
            break;
        }

        room_ids.extend(rooms.into_iter().map(|room| room.id));
        if i64::try_from(room_ids.len()).unwrap_or(i64::MAX) >= total {
            break;
        }

        page += 1;
    }

    Ok(room_ids)
}

impl ClientApiImpl {
    pub async fn delete_current_user(&self, user_id: &UserId) -> Result<(), ApiError> {
        let uid = *user_id;
        let owned_room_ids = list_owned_room_ids(self, &uid).await?;
        let mut deleted_room_fanout = Vec::with_capacity(owned_room_ids.len());
        for room_id in owned_room_ids {
            deleted_room_fanout.push(DeletedRoomFanoutReservation {
                room_id,
                reservation: self.room_lifecycle_fanout.reserve_room_deleted().await?,
            });
        }
        let summary = self
            .user_service
            .delete_user_with_summary(&uid)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .finalize_user_deletion(
                self.room_service.as_ref(),
                &summary,
                &uid,
                "user_deleted",
                deleted_room_fanout,
            )
            .await;

        Ok(())
    }

    pub async fn update_profile(
        &self,
        user_id: &UserId,
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

        let uid = *user_id;
        let updated_user = self
            .user_service
            .update_profile(&uid, normalized_username, old_password, new_password)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(user_to_proto(&updated_user, &self.public_id_codec)),
        })
    }

    pub async fn get_profile(
        &self,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let uid = *user_id;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(user_to_proto(&user, &self.public_id_codec)),
        })
    }

    pub async fn set_username(
        &self,
        user_id: &UserId,
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
        user_id: &UserId,
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
