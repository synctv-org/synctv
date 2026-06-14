use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

use synctv_core::{
    models::{PlaylistId, RoomId, UserId, UserStatus},
    service::{PlaybackStatePatch, PlaybackStateUpdateRequest},
};

use super::{
    playback_client_profile_from_proto, playback_expires_at, provider_playback_info_to_model,
    public_id_encode_error, sign_local_bilibili_danmaku_urls,
    try_bilibili_live_danmaku_for_static_media, try_playback_state_to_proto, try_playback_to_proto,
    AdminApiImpl, ApiError, PlaybackFanoutActor, ProviderPlaybackLifecycleApi, RequestContext,
    LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use crate::impls::client::convert::dynamic_playlist_source_fields;

struct DynamicPlaylistPlaybackRequest<'a> {
    room_id_model: &'a RoomId,
    user_id_model: &'a UserId,
    playlist_id: &'a PlaylistId,
    target: &'a [u8],
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
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id)
            .map_err(|error| public_id_encode_error("user", &error))?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .map_err(|error| public_id_encode_error("room", &error))?;

        let providers_manager = self.room_service.media_service().providers_manager();
        let provider_name = media.source_provider.trim();
        if provider_name.is_empty() {
            return Err(ApiError::Internal(format!(
                "Static media '{}' is missing source_provider",
                media.id
            )));
        }
        let provider = providers_manager
            .resolve_provider(provider_name, media.provider_instance_name.as_deref())
            .await
            .map_err(ApiError::from)?;

        let mut ctx = synctv_core::provider::ProviderContext::new("synctv")
            .with_user_id(*user_id)
            .with_public_user_id(public_user_id.clone())
            .with_room_id(*room_id)
            .with_public_room_id(public_room_id)
            .with_media_id(media.id)
            .with_playback_client_profile(playback_client_profile.cloned())
            .with_signing_key(&self.signing_key);
        if let Some(creator_id) = media.creator_id.as_ref() {
            ctx = ctx.with_credential_owner_id(*creator_id);
        }
        if let Some(provider_instance_name) = media.provider_instance_name.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        ctx = ctx.with_provider_access_service(self.provider_access_service.clone());
        ctx = ctx.with_store(self.provider_stores.load(provider.name()));
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
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
            room_id_model,
            user_id_model,
            playlist_id,
            target,
            playback_client_profile,
        } = request;

        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(*room_id_model, *user_id_model, playlist_id, target)
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
            .resolve_provider(
                source_fields.provider_name,
                source_fields.provider_instance_name,
            )
            .await
            .map_err(ApiError::from)?;

        let public_user_id = self
            .public_id_codec
            .encode_user_id(*user_id_model)
            .map_err(|error| public_id_encode_error("user", &error))?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id_model)
            .map_err(|error| public_id_encode_error("room", &error))?;
        let mut ctx = synctv_core::provider::ProviderContext::new("synctv")
            .with_user_id(*user_id_model)
            .with_public_user_id(public_user_id)
            .with_room_id(*room_id_model)
            .with_public_room_id(public_room_id)
            .with_playback_client_profile(playback_client_profile.cloned())
            .with_signing_key(&self.signing_key);
        if let Some(creator_id) = playlist.creator_id.as_ref() {
            ctx = ctx.with_credential_owner_id(*creator_id);
        }
        if let Some(provider_instance_name) = source_fields.provider_instance_name {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        ctx = ctx.with_provider_access_service(self.provider_access_service.clone());
        ctx = ctx.with_store(self.provider_stores.load(provider.name()));
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(*playlist_id),
            *room_id_model,
            item.name.clone(),
            0.0,
        )
        .default_mode(provider_result.default_mode.clone())
        .duration_seconds(provider_result.duration_seconds);

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
                    playback_client_profile,
                )
                .await;
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            return self
                .build_dynamic_playlist_playback_result(DynamicPlaylistPlaybackRequest {
                    room_id_model: room_id,
                    user_id_model: user_id,
                    playlist_id,
                    target: &state.target,
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
            metadata: std::collections::HashMap::new(),
            expires_at: None,
            duration_seconds: None,
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
            "No active room member available to sign management playback URLs".to_string(),
        ))
    }

    async fn resolve_playback_user_id(
        &self,
        admin_user_id: &UserId,
        room_id: &RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<UserId, ApiError> {
        if *admin_user_id == LOCAL_MANAGEMENT_ACTOR_USER_ID {
            return self
                .resolve_management_playback_user_id(room_id, state)
                .await;
        }

        Ok(*admin_user_id)
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
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::StartPlaybackResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let target =
            crate::impls::client::build_start_playback_request(req, &self.public_id_codec)?;
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
            .admin_start_playback_as_with_outbox(
                rid,
                &actor,
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

        Ok(synctv_proto::client::StartPlaybackResponse {})
    }

    pub async fn stop_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::StopPlaybackResponse, ApiError> {
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

        Ok(synctv_proto::client::StopPlaybackResponse {})
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

        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback = match self
            .resolve_playback_user_id(admin_user_id, &rid, &state)
            .await
        {
            Ok(snapshot_user_id) => match self
                .build_playback_from_state(
                    &snapshot_user_id,
                    &rid,
                    &state,
                    playback_client_profile.as_ref(),
                )
                .await
            {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    tracing::warn!(
                        room_id = %rid,
                        admin_user_id = %admin_user_id,
                        signing_user_id = %snapshot_user_id,
                        error = %error,
                        "Admin playback generation failed; returning playback state only"
                    );
                    None
                }
            },
            Err(error) => {
                tracing::warn!(
                    room_id = %rid,
                    admin_user_id = %admin_user_id,
                    error = %error,
                    "Admin playback generation failed; returning playback state only"
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
    ) -> Result<synctv_proto::client::UpdatePlaybackStateResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let command =
            crate::impls::client::build_playback_state_update(req, &self.public_id_codec)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let previous_state = self.state_before_playback_state_update(&rid).await?;
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    *actor.user_id(),
                    actor.username(),
                ));

        let state = match command {
            crate::impls::client::PlaybackStateUpdateCommand::Patch {
                playing,
                position,
                speed,
                version,
                expected_source,
            } => {
                let mut request = PlaybackStateUpdateRequest::new(
                    rid,
                    *actor.user_id(),
                    PlaybackStatePatch::new(playing, position, speed),
                )
                .with_expected_version(version)
                .with_outbox(Some(prepared_fanout.outbox_factory()));
                if let Some(expected_source) = expected_source {
                    request = request.with_expected_source(expected_source);
                }
                self.room_service
                    .admin_update_playback_as_request(&actor, request)
                    .await
            }
        }
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

        Ok(synctv_proto::client::UpdatePlaybackStateResponse {
            playback_state: Some(try_playback_state_to_proto(&state, &self.public_id_codec)?),
        })
    }
}
