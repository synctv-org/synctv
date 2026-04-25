//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use synctv_core::models::{PlaylistId, RoomPlaybackState, UserId};
use synctv_core::provider::{ExecutionControl, ProviderContext};

use super::convert::{
    bilibili_live_danmaku_for_static_media, direct_url_embedded_playback_result_to_model,
    playback_client_profile_from_proto, playback_snapshot_to_proto, playback_state_to_proto,
    provider_playback_info_to_model, sign_local_bilibili_danmaku_urls,
};
use super::ClientApiImpl;
use crate::impls::playback_snapshot::{
    compose_playback_snapshot_version, dynamic_playback_snapshot_version,
    playback_snapshot_expires_at, provider_credential_dependency_fingerprint,
    static_playback_snapshot_version,
};
use crate::impls::ApiError;
use synctv_core::models::MediaId;

pub(super) fn providers_manager_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Playback providers are not available on this server.".to_string())
}

pub(super) fn static_media_source_provider(
    media: &synctv_core::models::Media,
) -> Result<&str, ApiError> {
    let source_provider = media.source_provider.trim();
    if source_provider.is_empty() {
        return Err(ApiError::Internal(format!(
            "Static media '{}' is missing source_provider",
            media.id
        )));
    }

    Ok(source_provider)
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

struct DynamicPlaylistPlaybackRequest<'a> {
    room_id: &'a synctv_core::models::RoomId,
    user_id: &'a UserId,
    playlist_id: &'a PlaylistId,
    target: &'a [u8],
    state: Option<&'a RoomPlaybackState>,
    playback_client_profile: Option<&'a synctv_core::provider::PlaybackClientProfile>,
    request_control: Option<&'a ExecutionControl>,
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
    pub(super) fn build_provider_context<'a>(
        &'a self,
        user_id: &'a str,
        credential_owner_id: Option<&'a str>,
        room_id: &'a str,
        provider_instance_name: Option<&'a str>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> ProviderContext<'a> {
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_room_id(room_id)
            .with_playback_client_profile(playback_client_profile.cloned())
            .with_request_context(request_control.map(ExecutionControl::child));
        if let Some(credential_owner_id) = credential_owner_id {
            ctx = ctx.with_credential_owner_id(credential_owner_id);
        }
        if let Some(provider_instance_name) = provider_instance_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
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

    pub(super) fn attach_provider_store<'a>(
        &self,
        ctx: ProviderContext<'a>,
        provider: &dyn synctv_core::provider::MediaProvider,
    ) -> ProviderContext<'a> {
        match &self.provider_stores {
            Some(stores) => ctx.with_store(stores.load(provider.name())),
            None => ctx,
        }
    }

    async fn playback_snapshot_version_from_state(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<String, ApiError> {
        let base_version = if let Some(media_id) = &state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_media(media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            static_playback_snapshot_version(&media)
        } else if let Some(playlist_id) = &state.playing_playlist_id {
            let playlist = self
                .room_service
                .playlist_service()
                .get_playlist(playlist_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;
            dynamic_playback_snapshot_version(&playlist)
        } else {
            state.version.to_string()
        };

        let dependencies = self
            .playback_credential_dependencies_from_state(user_id, room_id, state)
            .await?;
        let credential_fingerprint = provider_credential_dependency_fingerprint(
            self.credential_repo.as_deref(),
            &dependencies,
        )
        .await?;

        Ok(compose_playback_snapshot_version(
            base_version,
            credential_fingerprint.as_deref(),
        ))
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        media: synctv_core::models::Media,
        state: Option<&RoomPlaybackState>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
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

        let provider = providers_manager
            .resolve_provider(
                static_media_source_provider(&media)?,
                media.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)?;

        let ctx = self.attach_provider_store(
            self.build_provider_context(
                user_id,
                media
                    .creator_id
                    .as_ref()
                    .map(synctv_core::models::UserId::as_str),
                room_id,
                media.provider_instance_name.as_deref(),
                playback_client_profile,
                request_control,
            )
            .with_media_id(media.id.as_str()),
            provider.as_ref(),
        );
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
        if let Some(state) = state {
            self.register_provider_playback_session(
                super::playback_lifecycle::ProviderPlaybackRegistration {
                    state,
                    provider: provider.as_ref(),
                    provider_name: media.source_provider.as_str(),
                    provider_instance_name: media.provider_instance_name.as_deref(),
                    credential_owner_id: media
                        .creator_id
                        .as_ref()
                        .map(synctv_core::models::UserId::as_str),
                    source_config: &media.source_config,
                    result: &provider_result,
                },
            )
            .await;
        }
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
        request: DynamicPlaylistPlaybackRequest<'_>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let DynamicPlaylistPlaybackRequest {
            room_id,
            user_id,
            playlist_id,
            target,
            state,
            playback_client_profile,
            request_control,
        } = request;
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
            .resolve_dynamic_playlist_item(room_id.clone(), user_id.clone(), playlist_id, target)
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
        let provider = providers_manager
            .resolve_provider(provider_name, bound_instance)
            .await
            .map_err(ApiError::from)?;

        let ctx = self.attach_provider_store(
            self.build_provider_context(
                user_id.as_str(),
                playlist
                    .creator_id
                    .as_ref()
                    .map(synctv_core::models::UserId::as_str),
                room_id.as_str(),
                playlist.provider_instance_name.as_deref(),
                playback_client_profile,
                request_control,
            ),
            provider.as_ref(),
        );
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;
        if let Some(state) = state {
            self.register_provider_playback_session(
                super::playback_lifecycle::ProviderPlaybackRegistration {
                    state,
                    provider: provider.as_ref(),
                    provider_name,
                    provider_instance_name: playlist.provider_instance_name.as_deref(),
                    credential_owner_id: playlist
                        .creator_id
                        .as_ref()
                        .map(synctv_core::models::UserId::as_str),
                    source_config: &item.source_config,
                    result: &provider_result,
                },
            )
            .await;
        }

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(playlist_id.clone()),
            room_id.clone(),
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
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
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
                .build_static_media_playback_result(
                    user_id.as_str(),
                    room_id.as_str(),
                    media,
                    Some(state),
                    playback_client_profile,
                    request_control,
                )
                .await;
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            return self
                .build_dynamic_playlist_playback_result(DynamicPlaylistPlaybackRequest {
                    room_id,
                    user_id,
                    playlist_id,
                    target: &state.target,
                    state: Some(state),
                    playback_client_profile,
                    request_control,
                })
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

    async fn playback_credential_dependencies_from_state(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, ApiError> {
        if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_media(media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;

            if direct_url_embedded_playback_result_to_model(&media)?.is_some() {
                return Ok(Vec::new());
            }

            let providers_manager = self
                .providers_manager
                .as_ref()
                .ok_or_else(providers_manager_unavailable_error)?;
            let provider = providers_manager
                .resolve_provider(
                    static_media_source_provider(&media)?,
                    media.provider_instance_name.as_deref(),
                )
                .await
                .map_err(ApiError::from)?;
            let ctx = self.build_provider_context(
                user_id.as_str(),
                media
                    .creator_id
                    .as_ref()
                    .map(synctv_core::models::UserId::as_str),
                room_id.as_str(),
                media.provider_instance_name.as_deref(),
                None,
                None,
            );

            return provider
                .credential_dependencies(&ctx, &media.source_config)
                .map_err(ApiError::from);
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
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

            let provider_name = playlist.source_provider.as_deref().ok_or_else(|| {
                ApiError::Internal("Dynamic playlist missing provider".to_string())
            })?;
            let source_config = playlist.source_config.as_ref().ok_or_else(|| {
                ApiError::Internal("Dynamic playlist missing source_config".to_string())
            })?;
            let providers_manager = self
                .providers_manager
                .as_ref()
                .ok_or_else(providers_manager_unavailable_error)?;
            let provider = providers_manager
                .resolve_provider(provider_name, playlist.provider_instance_name.as_deref())
                .await
                .map_err(ApiError::from)?;
            let ctx = self.build_provider_context(
                user_id.as_str(),
                playlist
                    .creator_id
                    .as_ref()
                    .map(synctv_core::models::UserId::as_str),
                room_id.as_str(),
                playlist.provider_instance_name.as_deref(),
                None,
                None,
            );

            return provider
                .credential_dependencies(&ctx, source_config)
                .map_err(ApiError::from);
        }

        Ok(Vec::new())
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
        let previous_state = self.state_before_playback_update(&rid).await;

        let state = self
            .room_service
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
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

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
        let previous_state = self.state_before_playback_update(&rid).await;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::reset()
        let state = self
            .room_service
            .playback_service()
            .reset(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

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
        req: crate::proto::client::GetPlaybackRequest,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        self.get_playback_internal(user_id, room_id, req, None)
            .await
    }

    pub async fn get_playback_with_context(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetPlaybackRequest,
        request_control: &ExecutionControl,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        self.get_playback_internal(user_id, room_id, req, Some(request_control))
            .await
    }

    async fn get_playback_internal(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetPlaybackRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let playback_client_profile =
            playback_client_profile_from_proto(req.playback_client_profile.as_ref());

        // Get playback state
        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback_snapshot = match self
            .build_playback_snapshot_from_state(
                &uid,
                &rid,
                &state,
                playback_client_profile.as_ref(),
                request_control,
            )
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

    /// Apply a playback update patch or target switch, then return the final playback state.
    pub async fn update_playback(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdatePlaybackRequest,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let command = build_update_playback_request(req)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        let state = match command {
            PlaybackUpdateCommand::Switch {
                media_id,
                playlist_id,
                target,
            } => self
                .room_service
                .playback_service()
                .switch(rid.clone(), uid.clone(), media_id, playlist_id, target)
                .await
                .map_err(ApiError::from)?,
            PlaybackUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
            } => self
                .room_service
                .playback_service()
                .update_multiple_with_version(
                    rid.clone(),
                    uid.clone(),
                    playing,
                    position,
                    speed,
                    version,
                )
                .await
                .map_err(ApiError::from)?,
        };
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

        self.get_playback(
            user_id,
            room_id,
            crate::proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
    }

    // These methods are called from WebSocket message handler

    /// Handle Play command from WebSocket
    pub async fn handle_play_command(&self, user_id: &str, room_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        let state = self
            .room_service
            .playback_service()
            .set_playing(rid.clone(), uid, true)
            .await
            .map_err(ApiError::from)?;
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle Pause command from WebSocket
    pub async fn handle_pause_command(&self, user_id: &str, room_id: &str) -> Result<(), ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        let state = self
            .room_service
            .playback_service()
            .set_playing(rid.clone(), uid, false)
            .await
            .map_err(ApiError::from)?;
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

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
        self.report_provider_progress_for_state(
            &response.state,
            response.state.current_time,
            !response.state.is_playing,
            true,
        )
        .await;

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
        let previous_state = self.state_before_playback_update(&rid).await;

        // Permission check (CHANGE_SPEED) is handled by PlaybackService::change_speed()
        let state = self
            .room_service
            .playback_service()
            .change_speed(rid.clone(), uid, speed)
            .await
            .map_err(ApiError::from)?;
        self.handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
            .await;

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
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let mut snapshot = self
            .build_playback_snapshot_from_state(
                user_id,
                room_id,
                state,
                playback_client_profile,
                None,
            )
            .await?;
        snapshot.version = self
            .playback_snapshot_version_from_state(user_id, room_id, state)
            .await?;
        Ok(snapshot)
    }

    async fn playback_credential_dependencies(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, ApiError> {
        self.playback_credential_dependencies_from_state(user_id, room_id, state)
            .await
    }

    async fn playback_snapshot_version_for_state(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<Option<String>, ApiError> {
        self.playback_snapshot_version_from_state(user_id, room_id, state)
            .await
            .map(Some)
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        previous: Option<&synctv_core::models::RoomPlaybackState>,
        current: &synctv_core::models::RoomPlaybackState,
    ) {
        ClientApiImpl::handle_provider_lifecycle_transition(self, previous, current).await;
    }

    async fn report_provider_playback_progress(
        &self,
        state: &synctv_core::models::RoomPlaybackState,
        position: f64,
        is_paused: bool,
        force: bool,
    ) {
        self.report_provider_progress_for_state(state, position, is_paused, force)
            .await;
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
        providers_manager_unavailable_error, static_media_source_provider, PlaybackUpdateCommand,
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
            provider_instance_name: Some(provider_instance_name.to_string()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_static_media_source_provider_ignores_explicit_instance_binding() {
        let media = make_media("direct_url");
        assert_eq!(static_media_source_provider(&media).unwrap(), "direct_url");
    }

    #[test]
    fn test_static_media_source_provider_accepts_default_instance_binding() {
        let media = make_media("");
        assert_eq!(static_media_source_provider(&media).unwrap(), "direct_url");
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
