//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use synctv_core::models::{MediaId, PlaylistId, UserId};
use synctv_core::provider::ProviderContext;

use super::convert::{
    playback_result_to_proto, playback_state_to_proto, provider_playback_info_to_model,
};
use super::ClientApiImpl;
use crate::impls::ApiError;

impl ClientApiImpl {
    async fn build_provider_context<'a>(
        &'a self,
        user_id: &'a str,
        room_id: &'a str,
    ) -> ProviderContext<'a> {
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_room_id(room_id);
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref key) = self.signing_key {
            ctx = ctx.with_signing_key(key);
        }
        ctx
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        media: synctv_core::models::Media,
    ) -> Result<crate::proto::client::PlaybackResult, ApiError> {
        let providers_manager = self
            .providers_manager
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Providers manager not configured".to_string()))?;

        let instance_name = media
            .provider_instance_name
            .as_deref()
            .unwrap_or(&media.source_provider);

        let provider = providers_manager.get(instance_name).await.ok_or_else(|| {
            ApiError::NotFound(format!("Provider instance '{instance_name}' not found"))
        })?;

        let ctx = self.build_provider_context(user_id, room_id).await;
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id.clone(),
            media.room_id.clone(),
            media.name.clone(),
            media.position,
        )
        .id(media.id.clone())
        .default_mode(provider_result.default_mode.clone());

        for (mode_name, provider_info) in provider_result.playback_infos {
            let info = provider_playback_info_to_model(&provider_info);
            builder = builder.add_mode(mode_name, info);
        }
        for (key, value) in provider_result.metadata {
            builder = builder.add_metadata(key, value);
        }

        let full_result = builder
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        Ok(playback_result_to_proto(&full_result))
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        room_id_model: &synctv_core::models::RoomId,
        user_id_model: &UserId,
        playlist_id: &PlaylistId,
        relative_path: &str,
    ) -> Result<crate::proto::client::PlaybackResult, ApiError> {
        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(
                room_id_model.clone(),
                user_id_model.clone(),
                playlist_id,
                relative_path,
            )
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;

        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;

        let provider_name = playlist
            .source_provider
            .as_deref()
            .ok_or_else(|| ApiError::Internal("Dynamic playlist missing provider".to_string()))?;
        let providers_manager = self
            .providers_manager
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Providers manager not configured".to_string()))?;
        let provider = providers_manager.get_by_type(provider_name).await.ok_or_else(|| {
            ApiError::NotFound(format!("Provider '{provider_name}' not found"))
        })?;

        let ctx = self.build_provider_context(user_id, room_id).await;
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(playlist_id.clone()),
            room_id_model.clone(),
            item.name.clone(),
            0,
        )
        .default_mode(provider_result.default_mode.clone());

        for (mode_name, provider_info) in provider_result.playback_infos {
            let info = provider_playback_info_to_model(&provider_info);
            builder = builder.add_mode(mode_name, info);
        }
        for (key, value) in provider_result.metadata {
            builder = builder.add_metadata(key, value);
        }

        let full_result = builder
            .add_metadata("relative_path".to_string(), serde_json::json!(item.relative_path))
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        Ok(playback_result_to_proto(&full_result))
    }

    /// Start playback of either a static media item or a dynamic playlist item
    /// HTTP API: POST /`api/rooms/{room_id}/playback/start`
    pub async fn start_playback(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::StartPlaybackRequest,
    ) -> Result<crate::proto::client::StartPlaybackResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let media_id = if req.media_id.is_empty() {
            None
        } else {
            crate::http::validation::validate_id(&req.media_id, "media_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;
            Some(MediaId::from_string(req.media_id))
        };
        let playlist_id = if req.playlist_id.is_empty() {
            None
        } else {
            crate::http::validation::validate_id(&req.playlist_id, "playlist_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
            Some(PlaylistId::from_string(req.playlist_id))
        };

        self.room_service
            .playback_service()
            .switch(
                rid.clone(),
                uid.clone(),
                media_id,
                playlist_id,
                req.relative_path,
            )
            .await
            .map_err(ApiError::from)?;

        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service.touch_room_activity(rid).await;

        Ok(crate::proto::client::StartPlaybackResponse {})
    }

    /// Stop current playback
    /// HTTP API: POST /`api/rooms/{room_id}/playback/stop`
    pub async fn stop_playback(
        &self,
        user_id: &str,
        room_id: &str,
        _req: crate::proto::client::StopPlaybackRequest,
    ) -> Result<crate::proto::client::StopPlaybackResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::reset()
        self.room_service
            .playback_service()
            .reset(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Broadcast PlaybackStateChanged via WebSocket
        // (handled by room_service internally)

        Ok(crate::proto::client::StopPlaybackResponse {})
    }

    /// Get current playback state and complete playback information
    /// HTTP API: GET /`api/rooms/{room_id}/playback`
    pub async fn get_playback(
        &self,
        user_id: &str,
        room_id: &str,
        _req: crate::proto::client::GetPlaybackRequest,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        // Get playback state
        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;

        let playback_result = if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_media(media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            self.build_static_media_playback_result(user_id, room_id, media)
                .await?
        } else if let Some(ref playlist_id) = state.playing_playlist_id {
            self.build_dynamic_playlist_playback_result(
                user_id,
                room_id,
                &rid,
                &uid,
                playlist_id,
                &state.relative_path,
            )
            .await?
        } else {
            // No media playing, return empty playback result
            crate::proto::client::PlaybackResult {
                media_id: String::new(),
                playlist_id: String::new(),
                room_id: rid.as_str().to_string(),
                name: String::new(),
                position: 0,
                playback_infos: std::collections::HashMap::new(),
                default_mode: String::new(),
                metadata: std::collections::HashMap::new(),
            }
        };

        Ok(crate::proto::client::GetPlaybackResponse {
            playback_state: Some(playback_state_to_proto(&state)),
            playback_result: Some(playback_result),
        })
    }

    // ==================== WebSocket Command Handlers ====================
    // These methods are called from WebSocket message handler

    /// Handle Play command from WebSocket
    pub async fn handle_play_command(&self, user_id: &str, room_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        self.room_service
            .playback_service()
            .set_playing(rid, uid, true)
            .await
            .map_err(ApiError::from)?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle Pause command from WebSocket
    pub async fn handle_pause_command(&self, user_id: &str, room_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        self.room_service
            .playback_service()
            .set_playing(rid, uid, false)
            .await
            .map_err(ApiError::from)?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle Seek command from WebSocket
    pub async fn handle_seek_command(
        &self,
        user_id: &str,
        room_id: &str,
        current_time: f64,
    ) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Permission check (SEEK) is handled by PlaybackService::seek()
        let response = self
            .room_service
            .playback_service()
            .seek(rid, uid, current_time)
            .await
            .map_err(ApiError::from)?;

        // Log warning if seek was not applied due to contention
        if !response.seek_applied {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                requested_time = current_time,
                actual_time = response.state.current_time,
                message = ?response.message,
                "Seek command returned degraded response"
            );
        }

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle `SetPlaybackSpeed` command from WebSocket
    pub async fn handle_set_speed_command(
        &self,
        user_id: &str,
        room_id: &str,
        speed: f64,
    ) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Permission check (CHANGE_SPEED) is handled by PlaybackService::change_speed()
        self.room_service
            .playback_service()
            .change_speed(rid, uid, speed)
            .await
            .map_err(ApiError::from)?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }
}
