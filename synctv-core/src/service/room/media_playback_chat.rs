use chrono::{DateTime, Utc};

use crate::{
    models::{
        ChatMessage, ChatMessageType, Media, MediaId, PageParams, RoomId, RoomPermission,
        RoomPlaybackState, UserId,
    },
    service::room::RoomService,
    Error, Result,
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
            Ok(self
                .media_service
                .get_room_media(room_id, &media_id)
                .await?)
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
        let request = crate::service::media::EditMediaRequest {
            media_id,
            name,
            description: None,
        };
        self.media_service
            .edit_media(room_id, user_id, request)
            .await
    }

    pub async fn move_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: crate::service::media::MoveMediaRequest,
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

    pub async fn get_chat_history_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<(DateTime<Utc>, i64)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(DateTime<Utc>, i64)>)> {
        let cursor =
            cursor.map(|(created_at, id)| crate::models::ChatHistoryCursor { created_at, id });
        let (messages, next) = self
            .chat_repo
            .list_by_room_cursor(room_id, cursor, limit, true)
            .await?;
        Ok((
            messages
                .into_iter()
                .map(|message| message.message)
                .collect(),
            next.map(|cursor| (cursor.created_at, cursor.id)),
        ))
    }

    pub async fn save_chat_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> Result<ChatMessage> {
        if content.is_empty() {
            return Err(Error::InvalidInput(
                "Chat message cannot be empty".to_string(),
            ));
        }
        if content.chars().count() > 2000 {
            return Err(Error::InvalidInput(
                "Chat message cannot exceed 2000 characters".to_string(),
            ));
        }

        let message = ChatMessage {
            id: 0,
            room_id,
            user_id: Some(user_id),
            client_message_id: None,
            content,
            message_type: ChatMessageType::Text,
            status: crate::models::ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: crate::models::ChatMetadata::default(),
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            created_at: Utc::now(),
        };
        self.chat_repo.create(&message).await
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
