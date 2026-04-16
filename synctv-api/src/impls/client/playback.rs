//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use synctv_core::models::{PlaylistId, UserId};
use synctv_core::provider::ProviderContext;

use super::convert::{
    bilibili_live_danmaku_for_static_media, direct_url_embedded_playback_result_to_model,
    playback_snapshot_to_proto, playback_state_to_proto, provider_playback_info_to_model,
    sign_local_bilibili_danmaku_urls,
};
use super::ClientApiImpl;
use crate::impls::playback_snapshot::{
    dynamic_playback_snapshot_version, playback_snapshot_expires_at,
    static_playback_snapshot_version,
};
use crate::impls::ApiError;
use synctv_core::models::MediaId;

fn providers_manager_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Playback providers are not available on this server.".to_string())
}

fn static_media_provider_binding(
    media: &synctv_core::models::Media,
) -> Result<(&str, bool), ApiError> {
    let instance_name = media.provider_instance_name.trim();
    if !instance_name.is_empty() {
        return Ok((instance_name, true));
    }

    let source_provider = media.source_provider.trim();
    if source_provider.is_empty() {
        return Err(ApiError::Internal(format!(
            "Static media '{}' is missing source_provider",
            media.id
        )));
    }

    Ok((source_provider, false))
}

#[derive(Debug)]
pub(crate) struct StartPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum PlaybackUpdateCommand {
    Switch {
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
    },
    Patch {
        playing: Option<bool>,
        position: Option<f64>,
        speed: Option<f64>,
        version: Option<i64>,
    },
}

pub(crate) fn build_start_playback_request(
    req: crate::proto::client::StartPlaybackRequest,
) -> Result<StartPlaybackTarget, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::StartPlaybackRequest {
        media_id,
        playlist_id,
        target,
    } = req;

    Ok(StartPlaybackTarget {
        media_id: crate::impls::proto_validated_optional_media_id(media_id),
        playlist_id: crate::impls::proto_validated_optional_playlist_id(playlist_id),
        target,
    })
}

pub(crate) fn build_update_playback_request(
    req: crate::proto::client::UpdatePlaybackRequest,
) -> Result<PlaybackUpdateCommand, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::UpdatePlaybackRequest {
        media_id,
        playlist_id,
        target,
        state,
        position,
        speed,
        version,
    } = req;

    let playing = match crate::proto::client::PlaybackPatchState::try_from(state)
        .unwrap_or(crate::proto::client::PlaybackPatchState::Unspecified)
    {
        crate::proto::client::PlaybackPatchState::Unspecified => None,
        crate::proto::client::PlaybackPatchState::Playing => Some(true),
        crate::proto::client::PlaybackPatchState::Paused => Some(false),
    };

    if let Some(position) = position {
        crate::http::validation::validate_playback_position(position)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }
    if let Some(speed) = speed {
        crate::http::validation::validate_playback_speed(speed)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }

    let media_id = crate::impls::proto_validated_optional_media_id(media_id);
    let playlist_id = crate::impls::proto_validated_optional_playlist_id(playlist_id);
    let target_requested = media_id.is_some() || playlist_id.is_some() || !target.is_empty();

    if playing.is_none() && position.is_none() && speed.is_none() && !target_requested {
        return Err(ApiError::InvalidInput(
            "No valid playback update field provided (state, position, speed, media_id, or playlist_id)"
                .to_string(),
        ));
    }

    if target_requested {
        if playing.is_some() || position.is_some() || speed.is_some() || version.is_some() {
            return Err(ApiError::InvalidInput(
                "Target switch requests cannot be combined with play/pause/seek/speed/version updates"
                    .to_string(),
            ));
        }

        return Ok(PlaybackUpdateCommand::Switch {
            media_id,
            playlist_id,
            target,
        });
    }

    Ok(PlaybackUpdateCommand::Patch {
        playing,
        position,
        speed,
        version,
    })
}

impl ClientApiImpl {
    fn build_provider_context<'a>(
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

    fn attach_provider_store<'a>(
        &self,
        ctx: ProviderContext<'a>,
        provider: &dyn synctv_core::provider::MediaProvider,
    ) -> ProviderContext<'a> {
        match &self.provider_stores {
            Some(stores) => ctx.with_store(stores.load(provider.name())),
            None => ctx,
        }
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        media: synctv_core::models::Media,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        if let Some(mut embedded_result) = direct_url_embedded_playback_result_to_model(&media)? {
            sign_local_bilibili_danmaku_urls(
                &mut embedded_result,
                user_id,
                self.signing_key.as_deref(),
                None,
            );
            let mut snapshot = playback_snapshot_to_proto(&embedded_result);
            snapshot.version = static_playback_snapshot_version(&media);
            snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
            return Ok(snapshot);
        }

        let providers_manager = self
            .providers_manager
            .as_ref()
            .ok_or_else(providers_manager_unavailable_error)?;

        let (provider_key, is_instance_name) = static_media_provider_binding(&media)?;

        let provider = if is_instance_name {
            providers_manager.get(provider_key).await.ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{provider_key}' not found"))
            })?
        } else {
            providers_manager
                .get_by_type(provider_key)
                .await
                .ok_or_else(|| ApiError::NotFound(format!("Provider '{provider_key}' not found")))?
        };

        let ctx = self.attach_provider_store(
            self.build_provider_context(user_id, room_id),
            provider.as_ref(),
        );
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
        let default_mode_expires_at = provider_result
            .playback_infos
            .get(&provider_result.default_mode)
            .and_then(|info| info.expires_at);
        let live_danmaku = bilibili_live_danmaku_for_static_media(
            &media,
            user_id,
            self.signing_key.as_deref(),
            default_mode_expires_at,
        );

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id.clone(),
            media.room_id.clone(),
            media.name.clone(),
            media.position,
        )
        .id(media.id.clone())
        .default_mode(provider_result.default_mode.clone());

        for (mode_name, provider_info) in provider_result.playback_infos {
            let mut info = provider_playback_info_to_model(&provider_info);
            if let Some(ref danmaku) = live_danmaku {
                info.danmakus.push(danmaku.clone());
            }
            builder = builder.add_mode(mode_name, info);
        }
        for (key, value) in provider_result.metadata {
            builder = builder.add_metadata(key, value);
        }

        let mut full_result = builder
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        sign_local_bilibili_danmaku_urls(
            &mut full_result,
            user_id,
            self.signing_key.as_deref(),
            default_mode_expires_at,
        );
        let mut snapshot = playback_snapshot_to_proto(&full_result);
        snapshot.version = static_playback_snapshot_version(&media);
        snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
        Ok(snapshot)
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        room_id_model: &synctv_core::models::RoomId,
        user_id_model: &UserId,
        playlist_id: &PlaylistId,
        target: &[u8],
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;
        self.room_service
            .ensure_client_usable_playlist(&playlist)
            .await
            .map_err(ApiError::from)?;

        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(
                room_id_model.clone(),
                user_id_model.clone(),
                playlist_id,
                target,
            )
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;

        let provider_name = playlist
            .source_provider
            .as_deref()
            .ok_or_else(|| ApiError::Internal("Dynamic playlist missing provider".to_string()))?;
        let providers_manager = self
            .providers_manager
            .as_ref()
            .ok_or_else(providers_manager_unavailable_error)?;
        let bound_instance = playlist.provider_instance_name.as_deref().and_then(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        let provider = if let Some(instance_name) = bound_instance {
            providers_manager.get(instance_name).await.ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{instance_name}' not found"))
            })?
        } else {
            providers_manager
                .get_by_type(provider_name)
                .await
                .ok_or_else(|| {
                    ApiError::NotFound(format!("Provider '{provider_name}' not found"))
                })?
        };

        let ctx = self.attach_provider_store(
            self.build_provider_context(user_id, room_id),
            provider.as_ref(),
        );
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(playlist_id.clone()),
            room_id_model.clone(),
            item.name.clone(),
            0.0,
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
            .add_metadata(
                "target".to_string(),
                serde_json::Value::String(BASE64_STANDARD.encode(target)),
            )
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        let mut snapshot = playback_snapshot_to_proto(&full_result);
        snapshot.version = dynamic_playback_snapshot_version(&playlist);
        snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
        Ok(snapshot)
    }

    async fn build_playback_snapshot_from_state(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_media(media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            return self
                .build_static_media_playback_result(user_id.as_str(), room_id.as_str(), media)
                .await;
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            return self
                .build_dynamic_playlist_playback_result(
                    user_id.as_str(),
                    room_id.as_str(),
                    room_id,
                    user_id,
                    playlist_id,
                    &state.target,
                )
                .await;
        }

        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: room_id.as_str().to_string(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: state.version.to_string(),
            expires_at: None,
        })
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
        let rid = Self::parse_room_id(room_id)?;
        let target = build_start_playback_request(req)?;

        self.room_service
            .playback_service()
            .switch(
                rid.clone(),
                uid.clone(),
                target.media_id,
                target.playlist_id,
                target.target,
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
        let rid = Self::parse_room_id(room_id)?;

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
        let rid = Self::parse_room_id(room_id)?;

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
        let playback_snapshot = match self
            .build_playback_snapshot_from_state(&uid, &rid, &state)
            .await
        {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(
                    room_id = %rid.as_str(),
                    user_id = %uid.as_str(),
                    error = %error,
                    "Playback snapshot generation failed; returning playback state only"
                );
                None
            }
        };

        Ok(crate::proto::client::GetPlaybackResponse {
            playback_state: Some(playback_state_to_proto(&state)),
            playback_snapshot,
        })
    }

    // ==================== WebSocket Command Handlers ====================
    // These methods are called from WebSocket message handler

    /// Handle Play command from WebSocket
    pub async fn handle_play_command(&self, user_id: &str, room_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

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
        let rid = Self::parse_room_id(room_id)?;

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
        let rid = Self::parse_room_id(room_id)?;

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
        let rid = Self::parse_room_id(room_id)?;

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

#[async_trait::async_trait]
impl crate::impls::playback_snapshot::PlaybackSnapshotService for ClientApiImpl {
    async fn get_playback_snapshot(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        self.build_playback_snapshot_from_state(user_id, room_id, state)
            .await
    }
}

#[cfg(test)]
mod start_playback_builder_tests {
    use super::{
        build_start_playback_request, build_update_playback_request, PlaybackUpdateCommand,
    };

    #[test]
    fn test_build_start_playback_request_rejects_proto_contract_violation() {
        let err = build_start_playback_request(crate::proto::client::StartPlaybackRequest {
            media_id: "media-1".into(),
            playlist_id: "playlist-1".into(),
            target: Vec::new(),
        })
        .unwrap_err();

        assert!(err.to_string().contains("start_playback"));
    }

    #[test]
    fn test_build_start_playback_request_parses_dynamic_target() {
        let playlist_id = synctv_common::snanoid!(12);
        let target = br#"{"path":"/tv/ep1.mp4"}"#.to_vec();
        let parsed = build_start_playback_request(crate::proto::client::StartPlaybackRequest {
            media_id: String::new(),
            playlist_id: playlist_id.clone(),
            target: target.clone(),
        })
        .unwrap();

        assert!(parsed.media_id.is_none());
        assert_eq!(
            parsed
                .playlist_id
                .as_ref()
                .map(synctv_core::models::PlaylistId::as_str),
            Some(playlist_id.as_str())
        );
        assert_eq!(parsed.target, target);
    }

    #[test]
    fn test_build_update_playback_request_rejects_empty_patch() {
        let err = build_update_playback_request(crate::proto::client::UpdatePlaybackRequest {
            state: crate::proto::client::PlaybackPatchState::Unspecified as i32,
            position: None,
            speed: None,
            media_id: String::new(),
            playlist_id: String::new(),
            target: Vec::new(),
            version: None,
        })
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("No valid playback update field provided"));
    }

    #[test]
    fn test_build_update_playback_request_rejects_mixed_switch_and_patch_fields() {
        let playlist_id = synctv_common::snanoid!(12);
        let err = build_update_playback_request(crate::proto::client::UpdatePlaybackRequest {
            state: crate::proto::client::PlaybackPatchState::Playing as i32,
            position: None,
            speed: None,
            media_id: String::new(),
            playlist_id,
            target: br#"{"item_id":"provider-item-1"}"#.to_vec(),
            version: None,
        })
        .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("update_playback.switch_patch_exclusive")
                || message.contains("cannot be combined"),
            "{message}"
        );
    }

    #[test]
    fn test_build_update_playback_request_parses_switch_request() {
        let playlist_id = synctv_common::snanoid!(12);
        let target = br#"{"item_id":"provider-item-1"}"#.to_vec();
        let parsed = build_update_playback_request(crate::proto::client::UpdatePlaybackRequest {
            state: crate::proto::client::PlaybackPatchState::Unspecified as i32,
            position: None,
            speed: None,
            media_id: String::new(),
            playlist_id: playlist_id.clone(),
            target: target.clone(),
            version: None,
        })
        .unwrap();

        match parsed {
            PlaybackUpdateCommand::Switch {
                media_id,
                playlist_id: parsed_playlist_id,
                target: parsed_target,
            } => {
                assert!(media_id.is_none());
                assert_eq!(
                    parsed_playlist_id
                        .as_ref()
                        .map(synctv_core::models::PlaylistId::as_str),
                    Some(playlist_id.as_str())
                );
                assert_eq!(parsed_target, target);
            }
            other => panic!("expected switch command, got {other:?}"),
        }
    }

    #[test]
    fn test_build_update_playback_request_parses_patch_request() {
        let parsed = build_update_playback_request(crate::proto::client::UpdatePlaybackRequest {
            state: crate::proto::client::PlaybackPatchState::Paused as i32,
            position: Some(42.5),
            speed: Some(1.5),
            media_id: String::new(),
            playlist_id: String::new(),
            target: Vec::new(),
            version: Some(9),
        })
        .unwrap();

        match parsed {
            PlaybackUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
            } => {
                assert_eq!(playing, Some(false));
                assert_eq!(position, Some(42.5));
                assert_eq!(speed, Some(1.5));
                assert_eq!(version, Some(9));
            }
            other => panic!("expected patch command, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_start_playback_request, build_update_playback_request,
        providers_manager_unavailable_error, static_media_provider_binding, PlaybackUpdateCommand,
    };
    use crate::impls::ErrorKind;
    use chrono::Utc;
    use synctv_core::models::{Media, MediaId, RoomId};

    fn make_media(provider_instance_name: &str) -> Media {
        Media {
            id: MediaId::from_string("media_static".to_string()),
            playlist_id: None,
            room_id: RoomId::from_string("room_static".to_string()),
            creator_id: None,
            name: "Static Media".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
            provider_instance_name: provider_instance_name.to_string(),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_static_media_provider_binding_uses_explicit_instance() {
        let media = make_media("direct_url");
        let (key, is_instance_name) = static_media_provider_binding(&media).unwrap();
        assert_eq!(key, "direct_url");
        assert!(is_instance_name);
    }

    #[test]
    fn test_static_media_provider_binding_falls_back_to_source_provider() {
        let media = make_media("");
        let (key, is_instance_name) = static_media_provider_binding(&media).unwrap();
        assert_eq!(key, "direct_url");
        assert!(!is_instance_name);
    }

    #[test]
    fn test_providers_manager_missing_is_service_unavailable() {
        let err = providers_manager_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Playback providers are not available on this server."
        );
    }

    #[test]
    fn test_build_start_playback_request_converts_proto_validated_ids_without_reparsing() {
        let target = build_start_playback_request(crate::proto::client::StartPlaybackRequest {
            media_id: "AbC123xYz890".to_string(),
            playlist_id: String::new(),
            target: Vec::new(),
        })
        .expect("valid playback request");

        assert_eq!(target.media_id.unwrap().as_str(), "AbC123xYz890");
        assert!(target.playlist_id.is_none());
        assert!(target.target.is_empty());
    }

    #[test]
    fn test_build_update_playback_request_converts_proto_validated_switch_ids_without_reparsing() {
        let command = build_update_playback_request(crate::proto::client::UpdatePlaybackRequest {
            media_id: String::new(),
            playlist_id: "ZyX098wVu765".to_string(),
            target: vec![9, 8, 7],
            ..Default::default()
        })
        .expect("valid switch request");

        match command {
            PlaybackUpdateCommand::Switch {
                media_id,
                playlist_id,
                target,
            } => {
                assert!(media_id.is_none());
                assert_eq!(playlist_id.unwrap().as_str(), "ZyX098wVu765");
                assert_eq!(target, vec![9, 8, 7]);
            }
            other => panic!("expected switch command, got {other:?}"),
        }
    }
}
