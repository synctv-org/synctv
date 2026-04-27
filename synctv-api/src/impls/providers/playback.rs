use std::collections::HashMap;
use synctv_core::models::{Media, MediaId, RoomId, UserId};
use synctv_core::provider::store::ProviderStore;
use synctv_core::provider::{
    ExecutionControl, MediaProvider, PlaybackResult as ProviderPlaybackResult, ProviderContext,
};
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::RoomService;
use synctv_core::service::{CredentialEncryption, ProxySigningKey};

use crate::impls::client::ClientApiImpl;
use crate::impls::ApiError;

#[derive(Default)]
pub struct ProviderPlaybackDeps<'a> {
    pub credential_encryption: Option<&'a CredentialEncryption>,
    pub credential_repo: Option<&'a UserProviderCredentialRepository>,
    pub signing_key: Option<&'a ProxySigningKey>,
    pub store: Option<std::sync::Arc<dyn ProviderStore>>,
    pub public_id_codec: Option<&'a crate::PublicIdCodec>,
    pub playback_client_profile: Option<&'a synctv_core::provider::PlaybackClientProfile>,
    pub request_context: Option<&'a ExecutionControl>,
}

fn build_provider_context<'a>(
    user_id: &'a UserId,
    credential_owner_id: Option<&'a UserId>,
    room_id: &'a RoomId,
    media_id: Option<&'a MediaId>,
    provider_instance_name: Option<&'a str>,
    deps: ProviderPlaybackDeps<'a>,
) -> ProviderContext<'a> {
    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(*user_id)
        .with_room_id(*room_id)
        .with_playback_client_profile(deps.playback_client_profile.cloned())
        .with_request_context(deps.request_context.cloned());
    if let Some(public_id_codec) = deps.public_id_codec {
        ctx = ctx
            .with_public_user_id(
                public_id_codec
                    .encode_user_id(*user_id)
                    .expect("positive user id must encode as public sqid"),
            )
            .with_public_room_id(
                public_id_codec
                    .encode_room_id(*room_id)
                    .expect("positive room id must encode as public sqid"),
            );
    }
    if let Some(credential_owner_id) = credential_owner_id {
        ctx = ctx.with_credential_owner_id(*credential_owner_id);
    }
    if let Some(media_id) = media_id {
        ctx = ctx.with_media_id(*media_id);
    }
    if let Some(provider_instance_name) = provider_instance_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        ctx = ctx.with_provider_instance_name(provider_instance_name);
    }
    if let Some(enc) = deps.credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }
    if let Some(repo) = deps.credential_repo {
        ctx = ctx.with_credential_repo(repo);
    }
    if let Some(key) = deps.signing_key {
        ctx = ctx.with_signing_key(key);
    }
    if let Some(store) = deps.store {
        ctx = ctx.with_store(store);
    }
    ctx
}

pub async fn resolve_media_from_playlist(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    room_service: &RoomService,
) -> Result<Media, ApiError> {
    room_service
        .check_membership(room_id, user_id)
        .await
        .map_err(ClientApiImpl::map_room_access_error)?;

    let media = room_service
        .media_service()
        .get_media(media_id)
        .await
        .map_err(|e| ClientApiImpl::map_media_lookup_error(e, "Media not found in playlist"))?
        .ok_or_else(|| ApiError::NotFound("Media not found in playlist".to_string()))?;

    if media.room_id != *room_id {
        return Err(ApiError::NotFound(
            "Media not found in playlist".to_string(),
        ));
    }

    Ok(media)
}

pub async fn resolve_provider_playback_url(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    deps: ProviderPlaybackDeps<'_>,
) -> Result<(String, HashMap<String, String>), ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;
    let ctx = build_provider_context(
        user_id,
        media.creator_id.as_ref(),
        room_id,
        Some(media_id),
        media.provider_instance_name.as_deref(),
        deps,
    );

    let playback_result = provider
        .generate_playback(&ctx, &media.source_config)
        .await
        .map_err(ApiError::from)?;

    let default_mode = &playback_result.default_mode;
    let playback_info = playback_result
        .playback_infos
        .get(default_mode)
        .ok_or_else(|| ApiError::Internal("Default playback mode not found".to_string()))?;

    let url = playback_info
        .urls
        .first()
        .ok_or_else(|| ApiError::Internal("No URLs in playback info".to_string()))?;

    Ok((url.clone(), playback_info.headers.clone()))
}

pub async fn resolve_provider_playback_result(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    deps: ProviderPlaybackDeps<'_>,
) -> Result<ProviderPlaybackResult, ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;
    let ctx = build_provider_context(
        user_id,
        media.creator_id.as_ref(),
        room_id,
        Some(media_id),
        media.provider_instance_name.as_deref(),
        deps,
    );

    provider
        .generate_playback(&ctx, &media.source_config)
        .await
        .map_err(ApiError::from)
}

impl ClientApiImpl {
    pub async fn get_live_proxy_source_url(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Option<String> {
        let media = self
            .room_service
            .media_service()
            .get_media(media_id)
            .await
            .ok()??;

        if media.room_id != *room_id || media.source_provider != "live_proxy" {
            return None;
        }

        media
            .source_config
            .get("url")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    }
}
