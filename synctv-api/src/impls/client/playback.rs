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
    public_id_codec: &crate::PublicIdCodec,
) -> Result<StartPlaybackTarget, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::StartPlaybackRequest {
        media_id,
        playlist_id,
        target,
    } = req;

    Ok(StartPlaybackTarget {
        media_id: crate::impls::proto_validated_optional_media_id(media_id, public_id_codec)?,
        playlist_id: crate::impls::proto_validated_optional_playlist_id(
            playlist_id,
            public_id_codec,
        )?,
        target,
    })
}

pub(crate) fn build_update_playback(
    update: crate::proto::client::UpdatePlayback,
) -> Result<PlaybackUpdateCommand, ApiError> {
    crate::impls::validate_proto_request(&update)?;
    let crate::proto::client::UpdatePlayback {
        r#type,
        playing,
        position,
        speed,
        version,
    } = update;

    let update_type = crate::proto::client::PlaybackUpdateType::try_from(r#type)
        .unwrap_or(crate::proto::client::PlaybackUpdateType::Unspecified);

    if let Some(position) = position {
        crate::http::validation::validate_playback_position(position)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }
    if let Some(speed) = speed {
        crate::http::validation::validate_playback_speed(speed)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }

    match update_type {
        crate::proto::client::PlaybackUpdateType::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback update type is required".to_string(),
            ));
        }
        crate::proto::client::PlaybackUpdateType::Play => {
            if playing == Some(false) {
                return Err(ApiError::InvalidInput(
                    "play update cannot request paused state".to_string(),
                ));
            }
            return Ok(PlaybackUpdateCommand::Patch {
                playing: Some(true),
                position,
                speed,
                version,
            });
        }
        crate::proto::client::PlaybackUpdateType::Pause => {
            if playing == Some(true) {
                return Err(ApiError::InvalidInput(
                    "pause update cannot request playing state".to_string(),
                ));
            }
            return Ok(PlaybackUpdateCommand::Patch {
                playing: Some(false),
                position,
                speed,
                version,
            });
        }
        crate::proto::client::PlaybackUpdateType::Seek => {
            if position.is_none() {
                return Err(ApiError::InvalidInput(
                    "seek update requires position".to_string(),
                ));
            }
        }
        crate::proto::client::PlaybackUpdateType::Speed => {
            if speed.is_none() {
                return Err(ApiError::InvalidInput(
                    "speed update requires speed".to_string(),
                ));
            }
        }
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
        user_id: &UserId,
        credential_owner_id: Option<&UserId>,
        room_id: &synctv_core::models::RoomId,
        provider_instance_name: Option<&'a str>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> ProviderContext<'a> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .expect("positive user id must encode as public ID");
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .expect("positive room id must encode as public ID");
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(*user_id)
            .with_public_user_id(public_user_id)
            .with_room_id(*room_id)
            .with_public_room_id(public_room_id)
            .with_playback_client_profile(playback_client_profile.cloned())
            .with_request_context(request_control.map(ExecutionControl::child));
        if let Some(credential_owner_id) = credential_owner_id {
            ctx = ctx.with_credential_owner_id(*credential_owner_id);
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
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        media: synctv_core::models::Media,
        state: Option<&RoomPlaybackState>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .expect("positive user id must encode as public ID");
        if let Some(mut embedded_result) = direct_url_embedded_playback_result_to_model(&media)? {
            sign_local_bilibili_danmaku_urls(
                &mut embedded_result,
                &public_user_id,
                self.signing_key.as_deref(),
                None,
            );
            let mut snapshot = playback_snapshot_to_proto(&embedded_result, &self.public_id_codec);
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
                media.creator_id.as_ref(),
                room_id,
                media.provider_instance_name.as_deref(),
                playback_client_profile,
                request_control,
            )
            .with_media_id(media.id),
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
                    credential_owner_id: media.creator_id.as_ref(),
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
            &public_user_id,
            &self.public_id_codec,
            self.signing_key.as_deref(),
            default_mode_expires_at,
        );

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id,
            media.room_id,
            media.name.clone(),
            media.position,
        )
        .id(media.id)
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
            &public_user_id,
            self.signing_key.as_deref(),
            default_mode_expires_at,
        );
        let mut snapshot = playback_snapshot_to_proto(&full_result, &self.public_id_codec);
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
            .resolve_dynamic_playlist_item(*room_id, *user_id, playlist_id, target)
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
                user_id,
                playlist.creator_id.as_ref(),
                room_id,
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
                    credential_owner_id: playlist.creator_id.as_ref(),
                    source_config: &item.source_config,
                    result: &provider_result,
                },
            )
            .await;
        }

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(*playlist_id),
            *room_id,
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
        let mut snapshot = playback_snapshot_to_proto(&full_result, &self.public_id_codec);
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
                    user_id,
                    room_id,
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
            room_id: self
                .public_id_codec
                .encode_room_id(*room_id)
                .expect("positive room id must encode as public ID"),
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
                user_id,
                media.creator_id.as_ref(),
                room_id,
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
                user_id,
                playlist.creator_id.as_ref(),
                room_id,
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
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::StartPlaybackRequest,
    ) -> Result<crate::proto::client::StartPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target = build_start_playback_request(req, &self.public_id_codec)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        let state = self
            .room_service
            .playback_service()
            .switch(rid, uid, target.media_id, target.playlist_id, target.target)
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
        user_id: &UserId,
        room_id: &str,
        _req: crate::proto::client::StopPlaybackRequest,
    ) -> Result<crate::proto::client::StopPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::reset()
        let state = self
            .room_service
            .playback_service()
            .reset(rid, uid)
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
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::GetPlaybackRequest,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        self.get_playback_internal(user_id, room_id, req, None)
            .await
    }

    pub async fn get_playback_with_context(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::GetPlaybackRequest,
        request_control: &ExecutionControl,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        self.get_playback_internal(user_id, room_id, req, Some(request_control))
            .await
    }

    async fn get_playback_internal(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::GetPlaybackRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

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
                    room_id = %rid,
                    user_id = %uid,
                    error = %error,
                    "Playback snapshot generation failed; returning playback state only"
                );
                None
            }
        };

        Ok(crate::proto::client::GetPlaybackResponse {
            playback_state: Some(playback_state_to_proto(&state, &self.public_id_codec)),
            playback_snapshot,
        })
    }

    /// Apply a playback state update, then return the final playback state.
    pub async fn update_playback(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdatePlayback,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let command = build_update_playback(req)?;
        let previous_state = self.state_before_playback_update(&rid).await;

        let state = match command {
            PlaybackUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
            } => self
                .room_service
                .playback_service()
                .update_multiple_with_version(rid, uid, playing, position, speed, version)
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
        build_start_playback_request, build_update_playback, providers_manager_unavailable_error,
        static_media_source_provider, PlaybackUpdateCommand,
    };
    use crate::impls::ErrorKind;
    use chrono::Utc;
    use synctv_core::models::{Media, MediaId, PlaylistId, RoomId};

    #[test]
    fn test_build_start_playback_request_rejects_proto_contract_violation() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_start_playback_request(
            crate::proto::client::StartPlaybackRequest {
                media_id: codec.encode_media_id(MediaId::from(1)).unwrap(),
                playlist_id: codec.encode_playlist_id(PlaylistId::from(2)).unwrap(),
                target: Vec::new(),
            },
            &codec,
        )
        .unwrap_err();

        assert!(err.to_string().contains("start_playback"));
    }

    #[test]
    fn test_build_start_playback_request_parses_dynamic_target() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let playlist_id = PlaylistId::from(123);
        let playlist_public_id = codec.encode_playlist_id(playlist_id).unwrap();
        let target = br#"{"path":"/tv/ep1.mp4"}"#.to_vec();
        let parsed = build_start_playback_request(
            crate::proto::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: playlist_public_id,
                target: target.clone(),
            },
            &codec,
        )
        .unwrap();

        assert!(parsed.media_id.is_none());
        assert_eq!(parsed.playlist_id, Some(playlist_id));
        assert_eq!(parsed.target, target);
    }

    #[test]
    fn test_build_update_playback_rejects_missing_type() {
        let err =
            build_update_playback(crate::proto::client::UpdatePlayback::default()).unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("update_playback.type_required") || message.contains("type"),
            "{message}"
        );
    }

    #[test]
    fn test_build_update_playback_rejects_playing_false_for_play() {
        let err = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Play as i32,
            playing: Some(false),
            position: None,
            speed: None,
            version: None,
        })
        .unwrap_err();

        assert!(err.to_string().contains("cannot request paused state"));
    }

    #[test]
    fn test_build_update_playback_play_defaults_to_playing() {
        let parsed = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Play as i32,
            playing: None,
            position: Some(12.5),
            speed: Some(1.25),
            version: Some(8),
        })
        .unwrap();

        match parsed {
            PlaybackUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
            } => {
                assert_eq!(playing, Some(true));
                assert_eq!(position, Some(12.5));
                assert_eq!(speed, Some(1.25));
                assert_eq!(version, Some(8));
            }
        }
    }

    #[test]
    fn test_build_update_playback_pause_defaults_to_paused() {
        let parsed = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Pause as i32,
            playing: None,
            position: None,
            speed: None,
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
                assert_eq!(position, None);
                assert_eq!(speed, None);
                assert_eq!(version, Some(9));
            }
        }
    }

    #[test]
    fn test_build_update_playback_seek_requires_position() {
        let err = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Seek as i32,
            playing: None,
            position: None,
            speed: Some(1.5),
            version: None,
        })
        .unwrap_err();

        assert!(err.to_string().contains("requires position"));
    }

    #[test]
    fn test_build_update_playback_speed_requires_speed() {
        let err = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Speed as i32,
            playing: Some(true),
            position: Some(5.0),
            speed: None,
            version: None,
        })
        .unwrap_err();

        assert!(err.to_string().contains("requires speed"));
    }

    #[test]
    fn test_build_update_playback_seek_parses_full_state() {
        let parsed = build_update_playback(crate::proto::client::UpdatePlayback {
            r#type: crate::proto::client::PlaybackUpdateType::Seek as i32,
            playing: Some(false),
            position: Some(42.5),
            speed: Some(1.5),
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
        }
    }

    fn make_media(provider_instance_name: &str) -> Media {
        Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: RoomId::new(),
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
        let codec = crate::PublicIdCodec::default_for_tests();
        let media_id = MediaId::from(123);
        let target = build_start_playback_request(
            crate::proto::client::StartPlaybackRequest {
                media_id: codec.encode_media_id(media_id).unwrap(),
                playlist_id: String::new(),
                target: Vec::new(),
            },
            &codec,
        )
        .expect("valid playback request");

        assert_eq!(target.media_id, Some(media_id));
        assert!(target.playlist_id.is_none());
        assert!(target.target.is_empty());
    }
}
