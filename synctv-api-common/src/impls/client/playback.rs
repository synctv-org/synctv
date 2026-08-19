//! Playback operations: start, stop, get playback state and info
//!
//! Note: Real-time playback control (play/pause/seek/speed) is handled via WebSocket messages

use synctv_core::models::SourceProvider;
use synctv_core::models::{PlaybackKind, PlaylistId, RoomPlaybackState, UserId};
use synctv_core::provider::{ExecutionControl, ProviderActor, ProviderContext};
use synctv_core::service::{
    PlaybackSourceExpectation, PlaybackStatePatch, PlaybackStateUpdateRequest,
};

use super::convert::{
    dynamic_playlist_source_fields, playback_client_profile_from_proto,
    playback_history_page_to_proto, provider_playback_info_to_model, provider_target_from_proto,
    try_playback_state_to_proto, try_playback_to_proto, PlaybackHttpSigningContext,
};
use super::playback_lifecycle::ProviderPlaybackLifecycleApi;
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};
use crate::impls::playback::{
    normalized_provider_duration, playback_expires_at, playback_generation_error_allows_state_only,
    playback_snapshot_error_indicates_stale_state,
};
use crate::impls::ApiError;
use crate::playback_fanout::{PlaybackFanoutActor, PreparedPlaybackStateFanout};
use synctv_core::models::MediaId;

pub(super) fn static_media_source_provider(
    media: &synctv_core::models::Media,
) -> Result<SourceProvider, ApiError> {
    Ok(media.source_provider)
}

fn apply_live_stream_generation(
    metadata: &mut synctv_proto::client::LivePlaybackMetadata,
    generation: Option<&synctv_livestream::StreamGeneration>,
) {
    if let Some(generation) = generation.filter(|generation| generation.ready_at.is_some()) {
        metadata.availability = synctv_proto::client::LiveStreamAvailability::Live as i32;
        metadata
            .stream_generation_id
            .clone_from(&generation.generation_id);
    } else {
        metadata.availability = synctv_proto::client::LiveStreamAvailability::Offline as i32;
        metadata.stream_generation_id.clear();
    }
}

#[derive(Debug)]
pub struct StartPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<synctv_core::models::ProviderTarget>,
}

#[derive(Debug)]
pub struct PlaybackStateUpdateParts {
    pub playing: Option<bool>,
    pub position: Option<f64>,
    pub speed: Option<f64>,
    pub version: Option<i64>,
    pub expected_source: Option<PlaybackSourceExpectation>,
    pub client_operation_id: Option<String>,
    pub client_time_millis: Option<i64>,
}

struct DynamicPlaylistPlaybackRequest<'a> {
    room_id: &'a synctv_core::models::RoomId,
    actor: PlaybackBuildActor<'a>,
    proxy_authorizer_id: &'a UserId,
    playlist_id: &'a PlaylistId,
    target: &'a synctv_core::models::ProviderTarget,
    state: Option<&'a RoomPlaybackState>,
    playback_client_profile: Option<&'a synctv_core::provider::PlaybackClientProfile>,
    request_control: Option<&'a ExecutionControl>,
}

struct StaticMediaPlaybackRequest<'a> {
    actor: PlaybackBuildActor<'a>,
    proxy_authorizer_id: &'a UserId,
    room_id: &'a synctv_core::models::RoomId,
    media: synctv_core::models::Media,
    state: Option<&'a RoomPlaybackState>,
    playback_client_profile: Option<&'a synctv_core::provider::PlaybackClientProfile>,
    request_control: Option<&'a ExecutionControl>,
}

#[derive(Clone, Copy)]
enum PlaybackBuildActor<'a> {
    User(&'a UserId),
    Guest { guest_id: &'a str },
}

impl<'a> PlaybackBuildActor<'a> {
    const fn user(user_id: &'a UserId) -> Self {
        Self::User(user_id)
    }

    const fn guest(guest_id: &'a str) -> Self {
        Self::Guest { guest_id }
    }

    const fn provider_actor(self) -> ProviderActor {
        match self {
            Self::User(user_id) => ProviderActor::User(*user_id),
            Self::Guest { .. } => ProviderActor::Guest,
        }
    }

    fn public_actor_id(
        self,
        public_id_codec: &synctv_adapter::PublicIdCodec,
    ) -> Result<String, ApiError> {
        match self {
            Self::User(user_id) => public_id_codec.encode_user_id(*user_id).map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            }),
            Self::Guest { guest_id } => Ok(guest_id.to_string()),
        }
    }
}

struct ProviderPlaybackResultBuildRequest {
    provider_result: synctv_core::provider::PlaybackResult,
    playlist_id: Option<synctv_core::models::PlaylistId>,
    room_id: synctv_core::models::RoomId,
    name: String,
    position: f64,
    media_id: Option<synctv_core::models::MediaId>,
    duration_seconds: Option<f64>,
    playback_kind: PlaybackKind,
}

fn build_playback_result_from_provider(
    request: ProviderPlaybackResultBuildRequest,
) -> Result<synctv_core::models::media::PlaybackResult, ApiError> {
    let ProviderPlaybackResultBuildRequest {
        provider_result,
        playlist_id,
        room_id,
        name,
        position,
        media_id,
        duration_seconds,
        playback_kind,
    } = request;
    let mut builder =
        synctv_core::models::media::PlaybackResult::builder(playlist_id, room_id, name, position)
            .provider(provider_result.provider)
            .provider_instance_name(provider_result.provider_instance_name.clone())
            .default_mode(provider_result.default_mode.clone())
            .duration_seconds(duration_seconds)
            .playback_kind(playback_kind);

    if let Some(id) = media_id {
        builder = builder.id(id);
    }

    for (mode_name, provider_info) in provider_result.playback_infos {
        let info = provider_playback_info_to_model(&provider_info);
        builder = builder.add_mode(mode_name, info);
    }
    builder = builder.metadata(provider_result.metadata);

    builder
        .build()
        .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))
}

fn apply_static_direct_url_thumbnail(
    result: &mut synctv_core::models::media::PlaybackResult,
    source_provider: SourceProvider,
    thumbnail: Option<&str>,
) {
    if source_provider != SourceProvider::DirectUrl {
        return;
    }
    let Some(thumbnail) = thumbnail
        .map(str::trim)
        .filter(|thumbnail| !thumbnail.is_empty())
    else {
        return;
    };
    for info in result.playback_infos.values_mut() {
        info.thumbnail = Some(thumbnail.to_string());
    }
}

pub fn build_start_playback_request(
    req: synctv_proto::client::StartPlaybackRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<StartPlaybackTarget, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let synctv_proto::client::StartPlaybackRequest {
        media_id,
        playlist_id,
        target,
        client_operation_id: _,
    } = req;
    let target = provider_target_from_proto(target)?;

    Ok(StartPlaybackTarget {
        media_id: crate::impls::proto_validated_optional_media_id(media_id, public_id_codec)?,
        playlist_id: crate::impls::proto_validated_optional_playlist_id(
            playlist_id,
            public_id_codec,
        )?,
        target,
    })
}

pub fn build_playback_state_update(
    update: synctv_proto::client::UpdatePlaybackStateRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<PlaybackStateUpdateParts, ApiError> {
    crate::impls::validate_proto_request(&update)?;
    let client_operation_id = update.client_operation_id.clone();
    let synctv_proto::client::UpdatePlaybackStateRequest {
        r#type,
        playing,
        position,
        speed,
        version,
        expected_media_id,
        expected_playlist_id,
        expected_target_hash,
        client_operation_id: _,
        client_time_millis,
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

    Ok(PlaybackStateUpdateParts {
        playing,
        position,
        speed,
        version,
        expected_source,
        client_operation_id,
        client_time_millis,
    })
}

pub fn build_playback_source_expectation(
    expected_media_id: Option<String>,
    expected_playlist_id: Option<String>,
    expected_target_hash: Option<String>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<PlaybackSourceExpectation>, ApiError> {
    if expected_media_id.is_none()
        && expected_playlist_id.is_none()
        && expected_target_hash.is_none()
    {
        return Ok(None);
    }

    let expected_target_hash = expected_target_hash.ok_or_else(|| {
        ApiError::InvalidInput(
            "expected_target_hash is required when expected source is supplied".to_string(),
        )
    })?;

    let media_id = expected_media_id
        .map(|value| crate::impls::proto_validated_optional_media_id(value, public_id_codec))
        .transpose()?
        .flatten();
    let playlist_id = expected_playlist_id
        .map(|value| crate::impls::proto_validated_optional_playlist_id(value, public_id_codec))
        .transpose()?
        .flatten();
    if media_id.is_none() && playlist_id.is_none() {
        return Err(ApiError::InvalidInput(
            "expected source requires a media ID or playlist ID".to_string(),
        ));
    }
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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_provider_context<'a>(
        &'a self,
        actor: ProviderActor,
        credential_owner_id: Option<&UserId>,
        room_id: &synctv_core::models::RoomId,
        media_id: Option<MediaId>,
        provider_instance_name: Option<&'a str>,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<ProviderContext<'a>, ApiError> {
        let mut ctx = ProviderContext::new("synctv", actor)
            .with_room_id(*room_id)
            .with_playback_client_profile(playback_client_profile.cloned())
            .with_request_context(request_control.map(ExecutionControl::child));
        if let Some(media_id) = media_id {
            ctx = ctx.with_media_id(media_id);
        }
        if let Some(credential_owner_id) = credential_owner_id {
            ctx = ctx.with_credential_owner_id(*credential_owner_id);
        }
        if let Some(provider_instance_name) = provider_instance_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        ctx = self
            .room_service
            .media_service()
            .attach_provider_credential_context(ctx);
        Ok(ctx)
    }

    pub(super) fn attach_provider_store<'a>(
        &self,
        ctx: ProviderContext<'a>,
        provider: &dyn synctv_core::provider::MediaProvider,
    ) -> ProviderContext<'a> {
        ctx.with_store(self.provider_stores.load(provider.name()))
    }

    async fn static_direct_url_thumbnail(
        &self,
        media: &synctv_core::models::Media,
    ) -> Result<Option<String>, ApiError> {
        if media.source_provider != SourceProvider::DirectUrl {
            return Ok(None);
        }
        let thumbnail = self
            .load_stored_file_reference(media.thumbnail_file_reference_id)
            .await?;
        let Some(thumbnail) = thumbnail.as_ref() else {
            return Ok(None);
        };
        let thumbnail_access = self.stored_file_reference_access(
            thumbnail,
            &synctv_core::service::media_thumbnail_upload_policy(),
        )?;
        Ok(thumbnail_access
            .as_ref()
            .and_then(crate::impls::stored_files::stored_file_object_access_url))
    }

    async fn build_static_media_playback_result(
        &self,
        request: StaticMediaPlaybackRequest<'_>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let StaticMediaPlaybackRequest {
            actor,
            proxy_authorizer_id,
            room_id,
            media,
            state,
            playback_client_profile,
            request_control,
        } = request;
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(ApiError::from)?;
        let providers_manager = self.room_service.media_service().providers_manager();

        let provider = providers_manager
            .resolve_provider(
                static_media_source_provider(&media)?,
                media.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)?;

        let mut ctx = self.attach_provider_store(
            self.build_provider_context(
                actor.provider_actor(),
                media.creator_id.as_ref(),
                room_id,
                Some(media.id),
                media.provider_instance_name.as_deref(),
                playback_client_profile,
                request_control,
            )?,
            provider.as_ref(),
        );
        if let Some(state) = state {
            ctx = ctx
                .with_playback_generation(state.playback_generation)
                .with_playback_is_playing(state.is_playing);
        }
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
        let thumbnail = self.static_direct_url_thumbnail(&media).await?;
        let playback_kind = provider_result
            .playback_kind
            .unwrap_or(PlaybackKind::Regular);
        let duration_seconds =
            normalized_provider_duration(Some(playback_kind), provider_result.duration_seconds);

        let mut full_result =
            build_playback_result_from_provider(ProviderPlaybackResultBuildRequest {
                provider_result,
                playlist_id: media.playlist_id,
                room_id: media.room_id,
                name: media.name.clone(),
                position: media.position,
                media_id: Some(media.id),
                duration_seconds,
                playback_kind,
            })?;
        apply_static_direct_url_thumbnail(
            &mut full_result,
            media.source_provider,
            thumbnail.as_deref(),
        );

        self.sign_and_finalize_playback(&full_result, &ctx, actor, proxy_authorizer_id)
    }

    /// Helper to encode public IDs, sign playback URLs, and set expiration.
    fn sign_and_finalize_playback(
        &self,
        full_result: &synctv_core::models::media::PlaybackResult,
        ctx: &synctv_core::provider::ProviderContext<'_>,
        actor: PlaybackBuildActor<'_>,
        proxy_authorizer_id: &UserId,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let room_id = ctx.room_id().ok_or_else(|| {
            ApiError::Internal(
                "Missing room_id in provider context for playback signing".to_string(),
            )
        })?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room public id: {error}"))
            })?;
        let proxy_authorizer_id = self
            .public_id_codec
            .encode_user_id(*proxy_authorizer_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            })?;
        let actor_id = actor.public_actor_id(&self.public_id_codec)?;
        let playback_generation = ctx.playback_generation().ok_or_else(|| {
            ApiError::Internal(
                "Missing playback_generation in provider context for playback signing".to_string(),
            )
        })?;
        let resource_owner_id = ctx
            .credential_owner_id()
            .map(|resource_owner_id| {
                self.public_id_codec
                    .encode_user_id(*resource_owner_id)
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "Failed to encode resource owner public id: {error}"
                        ))
                    })
            })
            .transpose()?;

        let signing = PlaybackHttpSigningContext {
            signing_key: &self.signing_key,
            media_swarm_signing_key: &self.media_swarm_signing_key,
            room_id: &public_room_id,
            proxy_authorizer_id: &proxy_authorizer_id,
            actor_id: &actor_id,
            playback_generation,
            resource_owner_id: resource_owner_id.as_deref(),
        };
        let mut playback =
            try_playback_to_proto(full_result, &self.public_id_codec, Some(&signing))?;
        playback.expires_at = playback_expires_at(&playback);
        Ok(playback)
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        request: DynamicPlaylistPlaybackRequest<'_>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let DynamicPlaylistPlaybackRequest {
            room_id,
            actor,
            proxy_authorizer_id,
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
            .resolve_dynamic_playlist_item(*room_id, actor.provider_actor(), playlist_id, target)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;

        let source_fields = dynamic_playlist_source_fields(&playlist)?;
        let providers_manager = self.room_service.media_service().providers_manager();
        let provider = providers_manager
            .resolve_provider(source_fields.provider, source_fields.provider_instance_name)
            .await
            .map_err(ApiError::from)?;

        let mut ctx = self.attach_provider_store(
            self.build_provider_context(
                actor.provider_actor(),
                playlist.creator_id.as_ref(),
                room_id,
                None,
                source_fields.provider_instance_name,
                playback_client_profile,
                request_control,
            )?,
            provider.as_ref(),
        );
        ctx = ctx.with_playlist_id(*playlist_id);
        if let Some(state) = state {
            ctx = ctx
                .with_playback_generation(state.playback_generation)
                .with_playback_is_playing(state.is_playing);
        }
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let playback_kind = provider_result
            .playback_kind
            .unwrap_or(PlaybackKind::Regular);
        let duration_seconds =
            normalized_provider_duration(Some(playback_kind), provider_result.duration_seconds);

        let mut full_result =
            build_playback_result_from_provider(ProviderPlaybackResultBuildRequest {
                provider_result,
                playlist_id: Some(*playlist_id),
                room_id: *room_id,
                name: item.name.clone(),
                position: 0.0,
                media_id: None,
                duration_seconds,
                playback_kind,
            })?;

        full_result.target = Some(target.clone());

        self.sign_and_finalize_playback(&full_result, &ctx, actor, proxy_authorizer_id)
    }

    async fn build_playback_from_state(
        &self,
        actor: PlaybackBuildActor<'_>,
        proxy_authorizer_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let mut playback = if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_room_media(room_id, media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            self.build_static_media_playback_result(StaticMediaPlaybackRequest {
                actor,
                proxy_authorizer_id,
                room_id,
                media,
                state: Some(state),
                playback_client_profile,
                request_control,
            })
            .await?
        } else if let Some(ref playlist_id) = state.playing_playlist_id {
            let target = state.target.as_ref().ok_or_else(|| {
                ApiError::InvalidInput(
                    "dynamic playlist playback state requires target".to_string(),
                )
            })?;
            self.build_dynamic_playlist_playback_result(DynamicPlaylistPlaybackRequest {
                room_id,
                actor,
                proxy_authorizer_id,
                playlist_id,
                target,
                state: Some(state),
                playback_client_profile,
                request_control,
            })
            .await?
        } else {
            synctv_proto::client::Playback {
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
                provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                metadata: None,
                expires_at: None,
                duration_seconds: None,
                playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
                target: None,
            }
        };

        self.attach_live_stream_state(room_id, state.playing_media_id.as_ref(), &mut playback)
            .await?;
        Ok(playback)
    }

    async fn attach_live_stream_state(
        &self,
        room_id: &synctv_core::models::RoomId,
        media_id: Option<&MediaId>,
        playback: &mut synctv_proto::client::Playback,
    ) -> Result<(), ApiError> {
        let Some(synctv_proto::client::playback_metadata::Metadata::Live(metadata)) = playback
            .metadata
            .as_mut()
            .and_then(|metadata| metadata.metadata.as_mut())
        else {
            return Ok(());
        };
        let Some(media_id) = media_id else {
            return Ok(());
        };
        let publisher = match &self.live_streaming_infrastructure {
            Some(infrastructure) => infrastructure
                .find_publisher(&room_id.to_string(), &media_id.to_string())
                .await
                .map_err(|error| Self::map_livestream_backend_error(error.as_ref()))?,
            None => None,
        };
        apply_live_stream_generation(metadata, publisher.as_ref());
        Ok(())
    }

    async fn playback_credential_dependencies_from_state(
        &self,
        actor: ProviderActor,
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
            self.room_service
                .ensure_client_usable_media(&media)
                .await
                .map_err(ApiError::from)?;
            let providers_manager = self.room_service.media_service().providers_manager();
            let provider = providers_manager
                .resolve_provider(
                    static_media_source_provider(&media)?,
                    media.provider_instance_name.as_deref(),
                )
                .await
                .map_err(ApiError::from)?;
            let ctx = self.build_provider_context(
                actor,
                media.creator_id.as_ref(),
                room_id,
                Some(media.id),
                media.provider_instance_name.as_deref(),
                None,
                None,
            )?;

            return provider
                .credential_dependencies(
                    &ctx,
                    synctv_core::provider::SourceConfig::media(&media.source_config),
                )
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
                .resolve_provider(source_fields.provider, source_fields.provider_instance_name)
                .await
                .map_err(ApiError::from)?;
            let ctx = self.build_provider_context(
                actor,
                playlist.creator_id.as_ref(),
                room_id,
                None,
                source_fields.provider_instance_name,
                None,
                None,
            )?;

            return provider
                .credential_dependencies(
                    &ctx,
                    synctv_core::provider::SourceConfig::dynamic_playlist(
                        source_fields.source_config,
                    ),
                )
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
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id.clone();
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target = build_start_playback_request(req, &self.public_id_codec)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(uid, client_operation_id.as_deref())
            .await?;

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

        self.playback_state_with_operation(&state, client_operation_id.as_deref())
    }

    /// Stop current playback
    /// HTTP API: POST /`api/rooms/{room_id}/playback/stop`
    pub async fn stop_playback(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::StopPlaybackRequest,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(uid, client_operation_id.as_deref())
            .await?;

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

        self.playback_state_with_operation(&state, client_operation_id.as_deref())
    }

    pub async fn play_next(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::PlayNextRequest,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id;
        let rid = self.parse_room_id(room_id)?;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(*user_id, client_operation_id.as_deref())
            .await?;
        let state = self
            .room_service
            .playback_service()
            .play_next_for_user(
                &rid,
                *user_id,
                &settings,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        if let Some(state) = &state {
            prepared_fanout.publish_after_outbox_commit();
            self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), state)
                .await;
            self.room_service.touch_room_activity(rid).await;
        }
        let state = match state {
            Some(state) => state,
            None => self
                .room_service
                .playback_service()
                .get_state(&rid)
                .await
                .map_err(ApiError::from)?,
        };
        self.playback_state_with_operation(&state, client_operation_id.as_deref())
    }

    pub async fn play_previous(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::PlayPreviousRequest,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id;
        let rid = self.parse_room_id(room_id)?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(*user_id, client_operation_id.as_deref())
            .await?;
        let state = self
            .room_service
            .playback_service()
            .play_previous_for_user(
                &rid,
                *user_id,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        if let Some(state) = &state {
            prepared_fanout.publish_after_outbox_commit();
            self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), state)
                .await;
            self.room_service.touch_room_activity(rid).await;
        }
        let state = match state {
            Some(state) => state,
            None => self
                .room_service
                .playback_service()
                .get_state(&rid)
                .await
                .map_err(ApiError::from)?,
        };
        self.playback_state_with_operation(&state, client_operation_id.as_deref())
    }

    pub async fn list_playback_history(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ListPlaybackHistoryRequest,
    ) -> Result<synctv_proto::client::ListPlaybackHistoryResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.require_room_permission(
            &actor,
            synctv_core::models::RoomPermission::VIEW_PLAYBACK_HISTORY,
        )
        .await?;
        let rid = actor.room_id();
        let before_entry_id = req
            .before_entry_id
            .as_deref()
            .map(|id| self.public_id_codec.decode_playback_history_entry_id(id))
            .transpose()
            .map_err(|_| {
                ApiError::InvalidInput("Invalid playback history before_entry_id".into())
            })?;
        let page = self
            .room_service
            .playback_service()
            .list_playback_history(
                &rid,
                before_entry_id,
                if req.limit == 0 { 50 } else { req.limit },
            )
            .await
            .map_err(ApiError::from)?;
        playback_history_page_to_proto(page, &self.public_id_codec)
    }

    pub async fn play_history_entry(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::PlayHistoryEntryRequest,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id.clone();
        let rid = self.parse_room_id(room_id)?;
        let entry_id = self
            .public_id_codec
            .decode_playback_history_entry_id(&req.entry_id)
            .map_err(|_| ApiError::InvalidInput("Invalid playback history entry_id".into()))?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(*user_id, client_operation_id.as_deref())
            .await?;
        let state = self
            .room_service
            .playback_service()
            .play_history_entry_for_user(
                &rid,
                *user_id,
                entry_id,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;
        self.room_service.touch_room_activity(rid).await;
        self.playback_state_with_operation(&state, client_operation_id.as_deref())
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
        req: synctv_proto::client::GetPlaybackRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let playback_client_profile =
            playback_client_profile_from_proto(req.playback_client_profile.as_ref())?;
        let mut state = self
            .room_service
            .get_playback_state(&access.room_id)
            .await
            .map_err(ApiError::from)?;
        let mut playback_result = self
            .build_playback_for_guest_state(
                access,
                &state,
                playback_client_profile.as_ref(),
                request_control,
            )
            .await;
        if playback_snapshot_error_indicates_stale_state(&state, &playback_result) {
            state = self
                .room_service
                .playback_service()
                .reload_state_from_store(&access.room_id)
                .await
                .map_err(ApiError::from)?;
            playback_result = self
                .build_playback_for_guest_state(
                    access,
                    &state,
                    playback_client_profile.as_ref(),
                    request_control,
                )
                .await;
        }
        let playback = match playback_result {
            Ok(snapshot) => Some(snapshot),
            Err(error) if playback_generation_error_allows_state_only(&error) => {
                tracing::warn!(
                    room_id = %access.room_id,
                    guest_id = %access.guest_id,
                    error = %error,
                    "Transient guest playback generation failed; returning playback state only"
                );
                None
            }
            Err(error) => return Err(error),
        };
        Ok(synctv_proto::client::GetPlaybackResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
            playback,
        })
    }

    async fn build_playback_for_guest_state(
        &self,
        access: &GuestRoomAccess,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let room = self
            .room_service
            .get_room(&access.room_id)
            .await
            .map_err(ApiError::from)?;
        self.build_playback_from_state(
            PlaybackBuildActor::guest(&access.guest_id),
            &room.created_by,
            &access.room_id,
            state,
            playback_client_profile,
            request_control,
        )
        .await
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
            RoomActor::Guest(access) => {
                self.get_playback_as_guest(access, req, request_control)
                    .await
            }
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
                PlaybackBuildActor::user(&uid),
                &uid,
                &rid,
                &state,
                playback_client_profile.as_ref(),
                request_control,
            )
            .await;
        if playback_snapshot_error_indicates_stale_state(&state, &playback_result) {
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
                    PlaybackBuildActor::user(&uid),
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
                if !playback_generation_error_allows_state_only(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    room_id = %rid,
                    user_id = %uid,
                    error = %error,
                    "Transient playback generation failed; returning playback state only"
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
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let update = build_playback_state_update(req, &self.public_id_codec)?;
        let client_operation_id = update.client_operation_id.clone();
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self
            .prepare_playback_state_changed(uid, client_operation_id.as_deref())
            .await?;

        let mut request = PlaybackStateUpdateRequest::new(
            rid,
            uid,
            PlaybackStatePatch::new(update.playing, update.position, update.speed),
        )
        .with_expected_version(update.version)
        .with_client_time_millis(update.client_time_millis)
        .with_outbox(Some(prepared_fanout.outbox_factory()));
        if let Some(expected_source) = update.expected_source {
            request = request.with_expected_source(expected_source);
        }
        let state = self
            .room_service
            .playback_service()
            .update_playback_state(request)
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        self.playback_state_with_operation(&state, client_operation_id.as_deref())
    }

    async fn prepare_playback_state_changed(
        &self,
        user_id: UserId,
        client_operation_id: Option<&str>,
    ) -> Result<PreparedPlaybackStateFanout, ApiError> {
        let username = self
            .user_service
            .get_username(&user_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::Internal("Playback actor username not found".to_string()))?;
        Ok(self.playback_fanout.prepare_state_changed_outbox_fanout(
            PlaybackFanoutActor::new(user_id, &username)
                .with_client_operation_id(client_operation_id),
        ))
    }

    fn playback_state_with_operation(
        &self,
        state: &RoomPlaybackState,
        client_operation_id: Option<&str>,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        let mut state = try_playback_state_to_proto(state, &self.public_id_codec)?;
        state.client_operation_id = client_operation_id.unwrap_or_default().to_string();
        Ok(state)
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

    async fn get_playback_for_actor(
        &self,
        actor: &RoomActor,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        match actor {
            RoomActor::User { user_id, .. } => {
                self.build_playback_from_state(
                    PlaybackBuildActor::user(user_id),
                    user_id,
                    room_id,
                    state,
                    playback_client_profile,
                    None,
                )
                .await
            }
            RoomActor::Guest(access) => {
                self.build_playback_for_guest_state(access, state, playback_client_profile, None)
                    .await
            }
        }
    }

    async fn playback_credential_dependencies(
        &self,
        actor: &RoomActor,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, ApiError> {
        let provider_actor = match actor {
            RoomActor::User { user_id, .. } => ProviderActor::User(*user_id),
            RoomActor::Guest(_) => ProviderActor::Guest,
        };
        self.playback_credential_dependencies_from_state(provider_actor, room_id, state)
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

    async fn reap_provider_playback_sessions(&self, force: bool) {
        if let Err(error) = self.reap_provider_lifecycle_sessions(force).await {
            tracing::warn!(
                error = %error,
                force,
                "Provider playback lifecycle reaper failed"
            );
        }
    }

    async fn refresh_observed_playback_metadata_and_auto_advance(
        &self,
        room_id: &synctv_core::models::RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) {
        let settings = match self.room_service.get_room_settings(room_id).await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Observed playback lifecycle failed to load room settings"
                );
                return;
            }
        };

        if let Some(probe) = self.playback_duration_probe.as_ref() {
            if let Err(error) = probe.probe_active_source_once(state).await {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Observed playback lifecycle duration probe failed"
                );
            }
        }

        let prepared_fanout = self
            .playback_fanout
            .prepare_system_state_changed_outbox_fanout();
        let auto_advance_result = self
            .room_service
            .playback_service()
            .check_and_auto_play_with_outbox(
                room_id,
                &settings,
                state.computed_position(),
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await;
        prepared_fanout.publish_after_outbox_commit();
        if let Err(error) = auto_advance_result {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Observed playback lifecycle auto-advance failed"
            );
        }
    }
}

#[cfg(test)]
#[path = "playback_builder_tests.rs"]
mod start_playback_builder_tests;
