//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use synctv_core::models::{MediaId, UserId};
use synctv_core::provider::ProviderContext;

use super::convert::{
    playback_result_to_proto, playback_state_to_proto, provider_playback_info_to_model,
};
use super::ClientApiImpl;
use crate::impls::ApiError;

impl ClientApiImpl {
    /// Start playback of a specific media
    /// HTTP API: POST /`api/rooms/{room_id}/playback/start`
    pub async fn start_playback(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::StartPlaybackRequest,
    ) -> Result<crate::proto::client::StartPlaybackResponse, ApiError> {
        crate::http::validation::validate_id(&req.media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let media_id = MediaId::from_string(req.media_id);

        // Permission check (SWITCH_MEDIA) is handled by PlaybackService::switch_media()
        self.room_service
            .playback_service()
            .switch_media(rid.clone(), uid.clone(), media_id.clone())
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
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // Get playback state
        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;

        // Get currently playing media (if any)
        let playing_media = self
            .room_service
            .get_playing_media(&rid)
            .await
            .map_err(ApiError::from)?;

        // Generate playback result with provider
        let playback_result = if let Some(media) = playing_media {
            // All media goes through ProvidersManager to generate playback
            let providers_manager = self.providers_manager.as_ref().ok_or_else(|| {
                ApiError::Internal("Providers manager not configured".to_string())
            })?;

            let instance_name = media
                .provider_instance_name
                .as_deref()
                .unwrap_or(&media.source_provider);

            let provider = providers_manager.get(instance_name).await.ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{instance_name}' not found"))
            })?;

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

            // Generate playback (caching is internal to providers via ProviderStore)
            let provider_result = provider
                .generate_playback(&ctx, &media.source_config)
                .await
                .map_err(ApiError::from)?;

            // Build full PlaybackResult from provider result + media fields
            let mut builder = synctv_core::models::media::PlaybackResult::builder(
                media.playlist_id.clone(),
                media.room_id.clone(),
                media.name.clone(),
                media.position,
            )
            .id(media.id.clone())
            .default_mode(provider_result.default_mode.clone());

            // Add all playback modes from provider result
            for (mode_name, provider_info) in provider_result.playback_infos {
                let info = provider_playback_info_to_model(&provider_info);
                builder = builder.add_mode(mode_name, info);
            }

            // Add metadata from provider result
            for (key, value) in provider_result.metadata {
                builder = builder.add_metadata(key, value);
            }

            let full_result = builder
                .build()
                .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;

            playback_result_to_proto(&full_result)
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
