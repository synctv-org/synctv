//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use synctv_core::models::PlaybackSourceIdentity;
use synctv_core::models::{PlaylistId, RoomPlaybackState, UserId};
use synctv_core::provider::{ExecutionControl, ProviderContext};
use synctv_core::service::playback::{
    PlaybackSourceExpectation, PlaybackStatePatch, PlaybackStateUpdateRequest,
};

use super::convert::{
    dynamic_playlist_source_fields, playback_client_profile_from_proto,
    provider_playback_info_to_model, sign_local_bilibili_danmaku_urls,
    try_bilibili_live_danmaku_for_static_media, try_playback_state_to_proto, try_playback_to_proto,
};
use super::playback_lifecycle::ProviderPlaybackLifecycleApi;
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};
use crate::impls::playback::playback_expires_at;
use crate::impls::ApiError;
use crate::playback_fanout::{PlaybackFanoutActor, PreparedPlaybackStateFanout};
use synctv_core::models::MediaId;

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

async fn persist_playback_duration(
    room_service: &synctv_core::service::RoomService,
    identity: PlaybackSourceIdentity,
    duration_seconds: Option<f64>,
) -> Result<(), ApiError> {
    let repo = room_service.playback_service().source_metadata_repository();
    if let Some(duration_seconds) =
        duration_seconds.filter(|duration| duration.is_finite() && *duration > 0.0)
    {
        repo.upsert_provider_duration(&identity, duration_seconds)
            .await
            .map_err(ApiError::from)?;
    } else {
        repo.mark_unknown_if_absent(&identity)
            .await
            .map_err(ApiError::from)?;
    }
    Ok(())
}

fn stale_cached_playback_reference<T>(
    state: &RoomPlaybackState,
    playback_result: &Result<T, ApiError>,
) -> bool {
    if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
        return false;
    }

    matches!(
        playback_result,
        Err(ApiError::NotFound(message))
            if matches!(
                message.as_str(),
                "Media not found" | "Playlist not found" | "Dynamic playlist item not found"
            )
    )
}

#[derive(Debug)]
pub(crate) struct StartPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum PlaybackStateUpdateCommand {
    Patch {
        playing: Option<bool>,
        position: Option<f64>,
        speed: Option<f64>,
        version: Option<i64>,
        expected_source: Option<PlaybackSourceExpectation>,
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
    req: synctv_proto::client::StartPlaybackRequest,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<StartPlaybackTarget, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let synctv_proto::client::StartPlaybackRequest {
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

pub(crate) fn build_playback_state_update(
    update: synctv_proto::client::UpdatePlaybackStateRequest,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<PlaybackStateUpdateCommand, ApiError> {
    crate::impls::validate_proto_request(&update)?;
    let synctv_proto::client::UpdatePlaybackStateRequest {
        r#type,
        playing,
        position,
        speed,
        version,
        expected_media_id,
        expected_playlist_id,
        expected_target_hash,
    } = update;

    let update_type = synctv_proto::client::PlaybackUpdateType::try_from(r#type).map_err(|_| {
        ApiError::InvalidInput("Unsupported playback state update type".to_string())
    })?;

    if let Some(position) = position {
        crate::impls::validation::validate_playback_position(position)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }
    if let Some(speed) = speed {
        crate::impls::validation::validate_playback_speed(speed)
            .map_err(|err| ApiError::InvalidInput(err.to_string()))?;
    }
    let playing = match update_type {
        synctv_proto::client::PlaybackUpdateType::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback state update type is required".to_string(),
            ));
        }
        synctv_proto::client::PlaybackUpdateType::Play => {
            if playing == Some(false) {
                return Err(ApiError::InvalidInput(
                    "playback state play update cannot request paused state".to_string(),
                ));
            }
            Some(true)
        }
        synctv_proto::client::PlaybackUpdateType::Pause => {
            if playing == Some(true) {
                return Err(ApiError::InvalidInput(
                    "playback state pause update cannot request playing state".to_string(),
                ));
            }
            Some(false)
        }
        synctv_proto::client::PlaybackUpdateType::Seek => {
            if position.is_none() {
                return Err(ApiError::InvalidInput(
                    "playback state seek update requires position".to_string(),
                ));
            }
            playing
        }
        synctv_proto::client::PlaybackUpdateType::Speed => {
            if speed.is_none() {
                return Err(ApiError::InvalidInput(
                    "playback state speed update requires speed".to_string(),
                ));
            }
            playing
        }
    };

    let expected_source = build_playback_source_expectation(
        expected_media_id,
        expected_playlist_id,
        expected_target_hash,
        public_id_codec,
    )?;

    Ok(PlaybackStateUpdateCommand::Patch {
        playing,
        position,
        speed,
        version,
        expected_source,
    })
}

pub(crate) fn build_playback_source_expectation(
    expected_media_id: Option<String>,
    expected_playlist_id: Option<String>,
    expected_target_hash: Option<String>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<Option<PlaybackSourceExpectation>, ApiError> {
    if expected_media_id.is_none()
        && expected_playlist_id.is_none()
        && expected_target_hash.is_none()
    {
        return Ok(None);
    }

    let expected_media_id = expected_media_id.ok_or_else(|| {
        ApiError::InvalidInput(
            "expected_media_id is required when expected source is supplied".to_string(),
        )
    })?;
    let expected_playlist_id = expected_playlist_id.ok_or_else(|| {
        ApiError::InvalidInput(
            "expected_playlist_id is required when expected source is supplied".to_string(),
        )
    })?;
    let expected_target_hash = expected_target_hash.ok_or_else(|| {
        ApiError::InvalidInput(
            "expected_target_hash is required when expected source is supplied".to_string(),
        )
    })?;

    let media_id =
        crate::impls::proto_validated_optional_media_id(expected_media_id, public_id_codec)?;
    let playlist_id =
        crate::impls::proto_validated_optional_playlist_id(expected_playlist_id, public_id_codec)?;
    let target_hash = expected_target_hash.trim().to_ascii_lowercase();
    if target_hash.len() != 64 || !target_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::InvalidInput(
            "expected_target_hash must be a SHA-256 hex digest".to_string(),
        ));
    }

    Ok(Some(PlaybackSourceExpectation {
        media_id,
        playlist_id,
        target_hash,
    }))
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
    ) -> Result<ProviderContext<'a>, ApiError> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            })?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room public id: {error}"))
            })?;
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
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        ctx = ctx.with_provider_access_service(self.provider_access_service.clone());
        ctx = ctx.with_signing_key(&self.signing_key);
        Ok(ctx)
    }

    pub(super) fn attach_provider_store<'a>(
        &self,
        ctx: ProviderContext<'a>,
        provider: &dyn synctv_core::provider::MediaProvider,
    ) -> ProviderContext<'a> {
        ctx.with_store(self.provider_stores.load(provider.name()))
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        media: synctv_core::models::Media,
        state: Option<&RoomPlaybackState>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            })?;
        let providers_manager = self.room_service.media_service().providers_manager();

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
            )?
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
                    actor_user_id: user_id,
                    provider: provider.as_ref(),
                    provider_name: media.source_provider.as_str(),
                    provider_instance_name: media.provider_instance_name.as_deref(),
                    credential_owner_id: media.creator_id.as_ref(),
                    source_config: &media.source_config,
                    result: &provider_result,
                },
            )
            .await?;
        }
        let default_mode_expires_at = provider_result
            .playback_infos
            .get(&provider_result.default_mode)
            .and_then(|info| info.expires_at);
        let live_danmaku = try_bilibili_live_danmaku_for_static_media(
            &media,
            &public_user_id,
            &self.public_id_codec,
            &self.signing_key,
            default_mode_expires_at,
        )?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id,
            media.room_id,
            media.name.clone(),
            media.position,
        )
        .id(media.id)
        .default_mode(provider_result.default_mode.clone())
        .duration_seconds(provider_result.duration_seconds);
        persist_playback_duration(
            &self.room_service,
            PlaybackSourceIdentity::static_media(media.room_id, media.id),
            provider_result.duration_seconds,
        )
        .await?;

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
            &self.signing_key,
            default_mode_expires_at,
        );
        let mut playback = try_playback_to_proto(&full_result, &self.public_id_codec)?;
        playback.expires_at = playback_expires_at(&playback);
        Ok(playback)
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        request: DynamicPlaylistPlaybackRequest<'_>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
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
            .get_room_playlist(room_id, playlist_id)
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

        let source_fields = dynamic_playlist_source_fields(&playlist)?;
        let providers_manager = self.room_service.media_service().providers_manager();
        let provider = providers_manager
            .resolve_provider(
                source_fields.provider_name,
                source_fields.provider_instance_name,
            )
            .await
            .map_err(ApiError::from)?;

        let ctx = self.attach_provider_store(
            self.build_provider_context(
                user_id,
                playlist.creator_id.as_ref(),
                room_id,
                source_fields.provider_instance_name,
                playback_client_profile,
                request_control,
            )?,
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
                    actor_user_id: user_id,
                    provider: provider.as_ref(),
                    provider_name: source_fields.provider_name,
                    provider_instance_name: source_fields.provider_instance_name,
                    credential_owner_id: playlist.creator_id.as_ref(),
                    source_config: &item.source_config,
                    result: &provider_result,
                },
            )
            .await?;
        }

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(*playlist_id),
            *room_id,
            item.name.clone(),
            0.0,
        )
        .default_mode(provider_result.default_mode.clone())
        .duration_seconds(provider_result.duration_seconds);
        persist_playback_duration(
            &self.room_service,
            PlaybackSourceIdentity::dynamic_playlist(*room_id, *playlist_id, target),
            provider_result.duration_seconds,
        )
        .await?;

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
        let mut playback = try_playback_to_proto(&full_result, &self.public_id_codec)?;
        playback.expires_at = playback_expires_at(&playback);
        Ok(playback)
    }

    async fn build_playback_from_state(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_room_media(room_id, media_id)
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

        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: self
                .public_id_codec
                .encode_room_id(*room_id)
                .map_err(|error| {
                    ApiError::Internal(format!("Failed to encode room public id: {error}"))
                })?,
            name: String::new(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            expires_at: None,
            duration_seconds: None,
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
                .get_room_media(room_id, media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            let providers_manager = self.room_service.media_service().providers_manager();
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
            )?;

            return provider
                .credential_dependencies(&ctx, &media.source_config)
                .map_err(ApiError::from);
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            let playlist = self
                .room_service
                .playlist_service()
                .get_room_playlist(room_id, playlist_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;
            self.room_service
                .ensure_client_usable_playlist(&playlist)
                .await
                .map_err(ApiError::from)?;

            let source_fields = dynamic_playlist_source_fields(&playlist)?;
            let providers_manager = self.room_service.media_service().providers_manager();
            let provider = providers_manager
                .resolve_provider(
                    source_fields.provider_name,
                    source_fields.provider_instance_name,
                )
                .await
                .map_err(ApiError::from)?;
            let ctx = self.build_provider_context(
                user_id,
                playlist.creator_id.as_ref(),
                room_id,
                source_fields.provider_instance_name,
                None,
                None,
            )?;

            return provider
                .credential_dependencies(&ctx, source_fields.source_config)
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
        req: synctv_proto::client::StartPlaybackRequest,
    ) -> Result<synctv_proto::client::StartPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target = build_start_playback_request(req, &self.public_id_codec)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self.prepare_playback_state_changed(uid).await?;

        let state = self
            .room_service
            .playback_service()
            .switch_with_outbox(
                rid,
                uid,
                target.media_id,
                target.playlist_id,
                target.target,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service.touch_room_activity(rid).await;

        Ok(synctv_proto::client::StartPlaybackResponse {})
    }

    /// Stop current playback
    /// HTTP API: POST /`api/rooms/{room_id}/playback/stop`
    pub async fn stop_playback(
        &self,
        user_id: &UserId,
        room_id: &str,
        _req: synctv_proto::client::StopPlaybackRequest,
    ) -> Result<synctv_proto::client::StopPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self.prepare_playback_state_changed(uid).await?;

        // Permission check (PLAY_PAUSE) is handled by PlaybackService::reset()
        let state = self
            .room_service
            .playback_service()
            .reset_with_outbox(
                rid,
                uid,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        Ok(synctv_proto::client::StopPlaybackResponse {})
    }

    /// Get current playback state and complete playback information
    /// HTTP API: GET /`api/rooms/{room_id}/playback`
    pub async fn get_playback(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::GetPlaybackRequest,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_playback_for_actor(&actor, req, None).await
    }

    pub async fn get_playback_with_context(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::GetPlaybackRequest,
        request_control: &ExecutionControl,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_playback_for_actor(&actor, req, Some(request_control))
            .await
    }

    pub async fn get_playback_as_guest(
        &self,
        access: &GuestRoomAccess,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let state = self
            .room_service
            .get_playback_state(&access.room_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::client::GetPlaybackResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
            playback: None,
        })
    }

    pub async fn get_playback_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetPlaybackRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        match actor {
            RoomActor::User { user_id, .. } => {
                let room_id = self
                    .public_id_codec
                    .encode_room_id(actor.room_id())
                    .map_err(ApiError::InvalidInput)?;
                self.get_playback_internal(user_id, &room_id, req, request_control)
                    .await
            }
            RoomActor::Guest(access) => self.get_playback_as_guest(access).await,
        }
    }

    async fn get_playback_internal(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::GetPlaybackRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let playback_client_profile =
            playback_client_profile_from_proto(req.playback_client_profile.as_ref())?;

        let mut state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;
        let mut playback_result = self
            .build_playback_from_state(
                &uid,
                &rid,
                &state,
                playback_client_profile.as_ref(),
                request_control,
            )
            .await;
        if stale_cached_playback_reference(&state, &playback_result) {
            tracing::info!(
                room_id = %rid,
                user_id = %uid,
                version = state.version,
                media_id = ?state.playing_media_id,
                playlist_id = ?state.playing_playlist_id,
                "Cached playback state references deleted media resources; reloading from database"
            );
            state = self
                .room_service
                .playback_service()
                .reload_state_from_store(&rid)
                .await
                .map_err(ApiError::from)?;
            playback_result = self
                .build_playback_from_state(
                    &uid,
                    &rid,
                    &state,
                    playback_client_profile.as_ref(),
                    request_control,
                )
                .await;
        }
        let playback = match playback_result {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(
                    room_id = %rid,
                    user_id = %uid,
                    error = %error,
                    "Playback generation failed; returning playback state only"
                );
                None
            }
        };

        Ok(synctv_proto::client::GetPlaybackResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
            playback,
        })
    }

    /// Apply a playback state update, then return the final playback state.
    pub async fn update_playback_state(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdatePlaybackStateRequest,
    ) -> Result<synctv_proto::client::UpdatePlaybackStateResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let command = build_playback_state_update(req, &self.public_id_codec)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self.prepare_playback_state_changed(uid).await?;

        let state = match command {
            PlaybackStateUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
                expected_source,
            } => {
                let mut request = PlaybackStateUpdateRequest::new(
                    rid,
                    uid,
                    PlaybackStatePatch::new(playing, position, speed),
                )
                .with_expected_version(version)
                .with_outbox(Some(prepared_fanout.outbox_factory()));
                if let Some(expected_source) = expected_source {
                    request = request.with_expected_source(expected_source);
                }
                self.room_service
                    .playback_service()
                    .update_playback_state(request)
                    .await
                    .map_err(ApiError::from)?
            }
        };
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        Ok(synctv_proto::client::UpdatePlaybackStateResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
        })
    }

    async fn prepare_playback_state_changed(
        &self,
        user_id: UserId,
    ) -> Result<PreparedPlaybackStateFanout, ApiError> {
        let username = self
            .user_service
            .get_username(&user_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::Internal("Playback actor username not found".to_string()))?;
        Ok(self
            .playback_fanout
            .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(user_id, &username)))
    }

    async fn handle_provider_lifecycle_transition_after_commit(
        &self,
        previous: Option<&RoomPlaybackState>,
        current: &RoomPlaybackState,
    ) {
        if let Err(error) = ProviderPlaybackLifecycleApi::handle_provider_lifecycle_transition(
            self, previous, current,
        )
        .await
        {
            tracing::warn!(
                room_id = %current.room_id,
                error = %error,
                "Provider playback lifecycle transition failed after playback state commit"
            );
        }
    }
}

#[async_trait::async_trait]
impl crate::impls::playback::PlaybackService for ClientApiImpl {
    async fn room_playback_state(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Result<synctv_core::models::RoomPlaybackState, ApiError> {
        self.room_service
            .get_playback_state(room_id)
            .await
            .map_err(ApiError::from)
    }

    async fn get_playback(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        self.build_playback_from_state(user_id, room_id, state, playback_client_profile, None)
            .await
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

    async fn handle_provider_lifecycle_transition(
        &self,
        previous: Option<&synctv_core::models::RoomPlaybackState>,
        current: &synctv_core::models::RoomPlaybackState,
    ) {
        self.handle_provider_lifecycle_transition_after_commit(previous, current)
            .await;
    }

    async fn report_provider_playback_progress(
        &self,
        state: &synctv_core::models::RoomPlaybackState,
        position: f64,
        is_paused: bool,
        force: bool,
    ) {
        if let Err(error) = self
            .report_provider_progress_for_state(state, position, is_paused, force)
            .await
        {
            tracing::warn!(
                room_id = %state.room_id,
                error = %error,
                "Provider playback progress report failed"
            );
        }
    }
}

#[cfg(test)]
#[path = "playback_builder_tests.rs"]
mod start_playback_builder_tests;
