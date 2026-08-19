use synctv_core::{
    models::{
        PlaybackKind, PlaylistId, ProviderTarget, RoomId, RoomPlaybackState, UserId, UserStatus,
    },
    service::{PlaybackStatePatch, PlaybackStateUpdateRequest},
};

use super::{
    playback_client_profile_from_proto, playback_expires_at,
    playback_generation_error_allows_state_only, playback_snapshot_error_indicates_stale_state,
    provider_playback_info_to_model, public_id_encode_error, try_playback_state_to_proto,
    try_playback_to_proto, AdminApiImpl, ApiError, PlaybackFanoutActor,
    ProviderPlaybackLifecycleApi, RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use crate::impls::client::convert::{dynamic_playlist_source_fields, PlaybackHttpSigningContext};
use crate::impls::playback::normalized_provider_duration;

struct DynamicPlaylistPlaybackRequest<'a> {
    room_id_model: &'a RoomId,
    user_id_model: &'a UserId,
    playlist_id: &'a PlaylistId,
    target: &'a ProviderTarget,
    state: &'a RoomPlaybackState,
    playback_client_profile: Option<&'a synctv_core::provider::PlaybackClientProfile>,
}

impl AdminApiImpl {
    async fn management_playback_candidate_is_usable(
        &self,
        room_id: &RoomId,
        candidate_id: &UserId,
    ) -> Result<bool, ApiError> {
        let user = match self.user_service.get_user(candidate_id).await {
            Ok(user) => user,
            Err(synctv_core::Error::NotFound(_)) => return Ok(false),
            Err(error) => return Err(ApiError::from(error)),
        };
        if user.status != UserStatus::Active || user.deleted_at.is_some() {
            return Ok(false);
        }
        if self
            .room_service
            .check_membership(room_id, candidate_id)
            .await
            .is_err()
        {
            return Ok(false);
        }

        Ok(true)
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        media: synctv_core::models::Media,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(ApiError::from)?;
        let providers_manager = self.room_service.media_service().providers_manager();
        let provider = providers_manager
            .resolve_provider(
                media.source_provider,
                media.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)?;

        let mut ctx = synctv_core::provider::ProviderContext::new(
            "synctv",
            synctv_core::provider::ProviderActor::User(*user_id),
        )
        .with_room_id(*room_id)
        .with_playback_generation(state.playback_generation)
        .with_playback_is_playing(state.is_playing)
        .with_media_id(media.id)
        .with_playback_client_profile(playback_client_profile.cloned());
        if let Some(creator_id) = media.creator_id.as_ref() {
            ctx = ctx.with_credential_owner_id(*creator_id);
        }
        if let Some(provider_instance_name) = media.provider_instance_name.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        ctx = self
            .room_service
            .media_service()
            .attach_provider_credential_context(ctx);
        ctx = ctx.with_store(self.provider_stores.load(provider.name()));
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
        let playback_kind = provider_result
            .playback_kind
            .unwrap_or(PlaybackKind::Regular);
        let duration_seconds =
            normalized_provider_duration(Some(playback_kind), provider_result.duration_seconds);

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id,
            media.room_id,
            media.name.clone(),
            media.position,
        )
        .id(media.id)
        .provider(provider_result.provider)
        .provider_instance_name(provider_result.provider_instance_name.clone())
        .default_mode(provider_result.default_mode.clone())
        .duration_seconds(duration_seconds)
        .playback_kind(playback_kind);

        for (mode_name, provider_info) in provider_result.playback_infos {
            let info = provider_playback_info_to_model(&provider_info);
            builder = builder.add_mode(mode_name, info);
        }
        builder = builder.metadata(provider_result.metadata);

        let full_result = builder
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;

        let room_id = ctx.room_id().ok_or(ApiError::Internal(
            "Missing room_id in provider context for playback signing".into(),
        ))?;
        let user_id = ctx.user_id().ok_or(ApiError::Internal(
            "Missing user_id in provider context for playback signing".into(),
        ))?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .map_err(|error| public_id_encode_error("room", &error))?;
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .map_err(|error| public_id_encode_error("user", &error))?;
        let resource_owner_id = ctx
            .credential_owner_id()
            .map(|resource_owner_id| {
                self.public_id_codec
                    .encode_user_id(*resource_owner_id)
                    .map_err(|error| public_id_encode_error("resource owner", &error))
            })
            .transpose()?;

        let signing = PlaybackHttpSigningContext {
            signing_key: &self.signing_key,
            media_swarm_signing_key: &self.media_swarm_signing_key,
            room_id: &public_room_id,
            proxy_authorizer_id: &public_user_id,
            actor_id: &public_user_id,
            playback_generation: state.playback_generation,
            resource_owner_id: resource_owner_id.as_deref(),
        };
        let mut playback =
            try_playback_to_proto(&full_result, &self.public_id_codec, Some(&signing))?;
        playback.expires_at = playback_expires_at(&playback);
        Ok(playback)
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        request: DynamicPlaylistPlaybackRequest<'_>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let DynamicPlaylistPlaybackRequest {
            room_id_model,
            user_id_model,
            playlist_id,
            target,
            state,
            playback_client_profile,
        } = request;

        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(
                *room_id_model,
                synctv_core::provider::ProviderActor::User(*user_id_model),
                playlist_id,
                target,
            )
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;

        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(room_id_model, playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;

        let source_fields = dynamic_playlist_source_fields(&playlist)?;
        let providers_manager = self.room_service.media_service().providers_manager();
        let provider = providers_manager
            .resolve_provider(source_fields.provider, source_fields.provider_instance_name)
            .await
            .map_err(ApiError::from)?;

        let mut ctx = synctv_core::provider::ProviderContext::new(
            "synctv",
            synctv_core::provider::ProviderActor::User(*user_id_model),
        )
        .with_room_id(*room_id_model)
        .with_playlist_id(*playlist_id)
        .with_playback_generation(state.playback_generation)
        .with_playback_is_playing(state.is_playing)
        .with_playback_client_profile(playback_client_profile.cloned());
        if let Some(creator_id) = playlist.creator_id.as_ref() {
            ctx = ctx.with_credential_owner_id(*creator_id);
        }
        if let Some(provider_instance_name) = source_fields.provider_instance_name {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        ctx = self
            .room_service
            .media_service()
            .attach_provider_credential_context(ctx);
        ctx = ctx.with_store(self.provider_stores.load(provider.name()));
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let playback_kind = provider_result
            .playback_kind
            .unwrap_or(PlaybackKind::Regular);
        let duration_seconds =
            normalized_provider_duration(Some(playback_kind), provider_result.duration_seconds);

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(*playlist_id),
            *room_id_model,
            item.name.clone(),
            0.0,
        )
        .provider(provider_result.provider)
        .provider_instance_name(provider_result.provider_instance_name.clone())
        .default_mode(provider_result.default_mode.clone())
        .duration_seconds(duration_seconds)
        .playback_kind(playback_kind);

        for (mode_name, provider_info) in provider_result.playback_infos {
            let info = provider_playback_info_to_model(&provider_info);
            builder = builder.add_mode(mode_name, info);
        }
        builder = builder.metadata(provider_result.metadata);

        let full_result = builder
            .target(target.clone())
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id_model)
            .map_err(|error| public_id_encode_error("room", &error))?;
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id_model)
            .map_err(|error| public_id_encode_error("user", &error))?;
        let resource_owner_id = ctx
            .credential_owner_id()
            .map(|resource_owner_id| {
                self.public_id_codec
                    .encode_user_id(*resource_owner_id)
                    .map_err(|error| public_id_encode_error("resource owner", &error))
            })
            .transpose()?;
        let signing = PlaybackHttpSigningContext {
            signing_key: &self.signing_key,
            media_swarm_signing_key: &self.media_swarm_signing_key,
            room_id: &public_room_id,
            proxy_authorizer_id: &public_user_id,
            actor_id: &public_user_id,
            playback_generation: state.playback_generation,
            resource_owner_id: resource_owner_id.as_deref(),
        };
        let mut playback =
            try_playback_to_proto(&full_result, &self.public_id_codec, Some(&signing))?;
        playback.expires_at = playback_expires_at(&playback);
        Ok(playback)
    }

    async fn build_playback_from_state(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &synctv_core::models::RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
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
                    state,
                    playback_client_profile,
                )
                .await;
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            let target = state.target.as_ref().ok_or_else(|| {
                ApiError::InvalidInput(
                    "dynamic playlist playback state requires target".to_string(),
                )
            })?;
            return self
                .build_dynamic_playlist_playback_result(DynamicPlaylistPlaybackRequest {
                    room_id_model: room_id,
                    user_id_model: user_id,
                    playlist_id,
                    target,
                    state,
                    playback_client_profile,
                })
                .await;
        }

        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: self
                .public_id_codec
                .encode_room_id(*room_id)
                .map_err(|error| public_id_encode_error("room", &error))?,
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
        })
    }

    async fn resolve_management_playback_user_id(
        &self,
        room_id: &RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<UserId, ApiError> {
        let mut candidate_ids = Vec::new();

        if let Some(media_id) = state.playing_media_id.as_ref() {
            if let Some(media) = self
                .room_service
                .media_service()
                .get_room_media(room_id, media_id)
                .await
                .map_err(ApiError::from)?
            {
                if let Some(creator_id) = media.creator_id {
                    candidate_ids.push(creator_id);
                }
            }
        }

        if let Some(playlist_id) = state.playing_playlist_id.as_ref() {
            if let Some(playlist) = self
                .room_service
                .playlist_service()
                .get_room_playlist(room_id, playlist_id)
                .await
                .map_err(ApiError::from)?
            {
                if let Some(creator_id) = playlist.creator_id {
                    candidate_ids.push(creator_id);
                }
            }
        }

        let room = self
            .room_service
            .get_room(room_id)
            .await
            .map_err(ApiError::from)?;
        candidate_ids.push(room.created_by);

        for member in self
            .room_service
            .member_service()
            .list_members(room_id)
            .await
            .map_err(ApiError::from)?
        {
            if member.is_active {
                candidate_ids.push(member.user_id);
            }
        }

        let mut seen = std::collections::HashSet::new();
        for candidate_id in candidate_ids {
            if !seen.insert(candidate_id.to_string()) {
                continue;
            }

            if !self
                .management_playback_candidate_is_usable(room_id, &candidate_id)
                .await?
            {
                continue;
            }

            return Ok(candidate_id);
        }

        Err(ApiError::NotFound(
            "No active room member available to sign management playback media".to_string(),
        ))
    }

    async fn resolve_playback_user_id(
        &self,
        admin_user_id: &UserId,
        room_id: &RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<UserId, ApiError> {
        if *admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID
            && self
                .management_playback_candidate_is_usable(room_id, admin_user_id)
                .await?
        {
            return Ok(*admin_user_id);
        }

        self.resolve_management_playback_user_id(room_id, state)
            .await
    }

    async fn handle_provider_lifecycle_transition_after_commit(
        &self,
        previous: Option<&synctv_core::models::RoomPlaybackState>,
        current: &synctv_core::models::RoomPlaybackState,
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

    pub async fn start_playback(
        &self,
        room_id: &str,
        req: synctv_proto::client::StartPlaybackRequest,
        recorded_actor_user_id: Option<UserId>,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let client_operation_id = req.client_operation_id.clone();
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let target =
            crate::impls::client::build_start_playback_request(req, &self.public_id_codec)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
            PlaybackFanoutActor::new(*actor.user_id(), actor.username())
                .with_client_operation_id(client_operation_id.as_deref()),
        );

        let state = self
            .room_service
            .admin_start_playback_as_with_outbox(
                rid,
                &actor,
                recorded_actor_user_id,
                synctv_core::service::SwitchPlaybackTarget {
                    media_id: target.media_id,
                    playlist_id: target.playlist_id,
                    target: target.target,
                },
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        self.room_service.touch_room_activity(rid).await;

        tracing::info!(
            room_id = %rid,
            admin_user_id = %admin_user_id,
            media_id = target.media_id.as_ref().map_or_else(String::new, ToString::to_string),
            playlist_id = target.playlist_id.as_ref().map_or_else(String::new, ToString::to_string),
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin started playback"
        );

        let mut response = try_playback_state_to_proto(&state, &self.public_id_codec)?;
        response.client_operation_id = client_operation_id.unwrap_or_default();
        Ok(response)
    }

    pub async fn stop_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    *actor.user_id(),
                    actor.username(),
                ));

        let state = self
            .room_service
            .admin_stop_playback_as_with_outbox(
                rid,
                &actor,
                Some(prepared_fanout.outbox_factory_with_source_changed(true)),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        tracing::info!(
            room_id = %rid,
            admin_user_id = %admin_user_id,
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin stopped playback"
        );

        try_playback_state_to_proto(&state, &self.public_id_codec)
    }

    pub async fn get_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        playback_client_profile: Option<synctv_proto::client::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::GetPlaybackResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let playback_client_profile =
            playback_client_profile_from_proto(playback_client_profile.as_ref())?;

        self.require_admin_actor(admin_user_id).await?;

        let mut state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;
        let mut playback_result = match self
            .resolve_playback_user_id(admin_user_id, &rid, &state)
            .await
        {
            Ok(snapshot_user_id) => {
                self.build_playback_from_state(
                    &snapshot_user_id,
                    &rid,
                    &state,
                    playback_client_profile.as_ref(),
                )
                .await
            }
            Err(error) => Err(error),
        };
        if playback_snapshot_error_indicates_stale_state(&state, &playback_result) {
            state = self
                .room_service
                .playback_service()
                .reload_state_from_store(&rid)
                .await
                .map_err(ApiError::from)?;
            playback_result = match self
                .resolve_playback_user_id(admin_user_id, &rid, &state)
                .await
            {
                Ok(snapshot_user_id) => {
                    self.build_playback_from_state(
                        &snapshot_user_id,
                        &rid,
                        &state,
                        playback_client_profile.as_ref(),
                    )
                    .await
                }
                Err(error) => Err(error),
            };
        }
        let playback = match playback_result {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                if !playback_generation_error_allows_state_only(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    room_id = %rid,
                    admin_user_id = %admin_user_id,
                    error = %error,
                    "Transient admin playback generation failed; returning playback state only"
                );
                None
            }
        };

        Ok(synctv_proto::client::GetPlaybackResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
            playback,
        })
    }

    pub async fn update_playback_state(
        &self,
        room_id: &str,
        req: synctv_proto::client::UpdatePlaybackStateRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let update = crate::impls::client::build_playback_state_update(req, &self.public_id_codec)?;
        let client_operation_id = update.client_operation_id.clone();
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
            PlaybackFanoutActor::new(*actor.user_id(), actor.username())
                .with_client_operation_id(client_operation_id.as_deref()),
        );

        let mut request = PlaybackStateUpdateRequest::new(
            rid,
            *actor.user_id(),
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
            .admin_update_playback_as_request(&actor, request)
            .await
            .map_err(ApiError::from)?;
        prepared_fanout.publish_after_outbox_commit();
        self.handle_provider_lifecycle_transition_after_commit(Some(&previous_state), &state)
            .await;

        self.room_service.touch_room_activity(rid).await;

        tracing::info!(
            room_id = %rid,
            admin_user_id = %admin_user_id,
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin updated playback state"
        );

        let mut response = try_playback_state_to_proto(&state, &self.public_id_codec)?;
        response.client_operation_id = client_operation_id.unwrap_or_default();
        Ok(response)
    }
}
