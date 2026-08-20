use crate::{
    models::{
        ChatMessage, Media, MediaId, PageParams, RoomId, RoomPermission, RoomPlaybackState, UserId,
    },
    service::RoomService,
    Result,
};

impl RoomService {
    pub async fn get_room_root_media(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        self.media_service.get_room_root_media(room_id).await
    }

    pub async fn get_room_root_media_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        self.media_service
            .get_room_root_media_paginated(room_id, pagination)
            .await
    }

    pub async fn get_playing_media(&self, room_id: &RoomId) -> Result<Option<Media>> {
        let state = self.playback_service.get_state(room_id).await?;
        if let Some(media_id) = state.playing_media_id {
            let media = self
                .media_service
                .get_room_media(room_id, &media_id)
                .await?;
            if let Some(media) = &media {
                self.ensure_client_usable_media(media).await?;
            }
            Ok(media)
        } else {
            Ok(None)
        }
    }

    pub async fn edit_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
        name: Option<String>,
    ) -> Result<Media> {
        let request = crate::service::EditMediaRequest {
            media_id,
            name,
            description: None,
            playback_proxy_mode: None,
            source_config: None,
            provider_instance_name: None,
        };
        self.media_service
            .edit_media(room_id, user_id, request)
            .await
    }

    pub async fn move_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: crate::service::MoveMediaRequest,
    ) -> Result<Vec<Media>> {
        self.media_service
            .move_media(room_id, user_id, request)
            .await
    }

    pub async fn update_playback_state(
        &self,
        room_id: RoomId,
        user_id: UserId,
        update_fn: impl Fn(&mut RoomPlaybackState),
        required_permission: RoomPermission,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, required_permission)
            .await?;
        self.playback_service.update_state(room_id, update_fn).await
    }

    pub async fn get_playback_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        self.playback_service.get_state(room_id).await
    }

    pub async fn get_chat_message_from_primary(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        self.chat_repo
            .get_by_room_and_id_from_primary(room_id, message_id)
            .await
    }

    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        let room = self.get_room(room_id).await?;
        self.ensure_room_creator_is_active_for_access(&room, user_id)
            .await?;

        self.permission_service
            .check_permission(room_id, user_id, permission)
            .await
    }
}
