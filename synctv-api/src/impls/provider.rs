//! Provider business logic implementation
//!
//! This module handles all provider-related business logic:
//! - Playback generation (caching is internal to providers via ProviderStore)
//! - Media resolution and playback URL extraction

use std::collections::HashMap;
use synctv_core::models::{Media, MediaId, RoomId, UserId};
use synctv_core::provider::store::ProviderStore;
use synctv_core::provider::{
    MediaProvider, PlaybackResult as ProviderPlaybackResult, ProviderContext,
};
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::RoomService;
use synctv_core::service::{CredentialEncryption, ProxySigningKey};

use super::ApiError;
use crate::impls::client::ClientApiImpl;

// ------------------------------------------------------------------
// Shared playback resolution helpers
// ------------------------------------------------------------------

/// Optional runtime dependencies required by provider playback helpers.
#[derive(Default)]
pub struct ProviderPlaybackDeps<'a> {
    pub credential_encryption: Option<&'a CredentialEncryption>,
    pub credential_repo: Option<&'a UserProviderCredentialRepository>,
    pub signing_key: Option<&'a ProxySigningKey>,
    pub store: Option<std::sync::Arc<dyn ProviderStore>>,
}

fn build_provider_context<'a>(
    user_id: &'a UserId,
    room_id: &'a RoomId,
    deps: ProviderPlaybackDeps<'a>,
) -> ProviderContext<'a> {
    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(user_id.as_str())
        .with_room_id(room_id.as_str());
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

/// Verify room membership and look up a specific media item by ID.
///
/// This is the common first phase shared by all provider proxy handlers.
/// Uses a direct media lookup by primary key instead of fetching the entire
/// playlist and scanning linearly, which is O(1) vs O(n) in playlist size.
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

    // Verify the media belongs to the requested room
    if media.room_id != *room_id {
        return Err(ApiError::NotFound(
            "Media not found in playlist".to_string(),
        ));
    }

    Ok(media)
}

/// Resolve a playback URL and headers from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback` -> extract first URL + headers from the default mode.
///
/// Kept consistent with the active playback chain so future callers do not
/// silently lose credential resolution or signed proxy support.
pub async fn resolve_provider_playback_url(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    deps: ProviderPlaybackDeps<'_>,
) -> Result<(String, HashMap<String, String>), ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;
    let ctx = build_provider_context(user_id, room_id, deps);

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

/// Resolve the full `PlaybackResult` from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback`.
///
/// Kept consistent with the active playback chain so future callers do not
/// silently lose credential resolution or signed proxy support.
pub async fn resolve_provider_playback_result(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    deps: ProviderPlaybackDeps<'_>,
) -> Result<ProviderPlaybackResult, ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;
    let ctx = build_provider_context(user_id, room_id, deps);

    provider
        .generate_playback(&ctx, &media.source_config)
        .await
        .map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::{build_provider_context, ProviderPlaybackDeps};
    use std::sync::Arc;
    use synctv_core::provider::store::{InMemoryProviderStore, ProviderStore};
    use synctv_core::repository::UserProviderCredentialRepository;
    use synctv_core::service::{CredentialEncryption, ProxySigningKey};

    #[tokio::test]
    async fn build_provider_context_propagates_all_runtime_dependencies() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let user_id = synctv_core::models::UserId::from_string("user-provider".to_string());
        let room_id = synctv_core::models::RoomId::from_string("room-provider".to_string());
        let credential_repo = UserProviderCredentialRepository::new(pool);
        let credential_encryption =
            CredentialEncryption::new(b"0123456789abcdef0123456789abcdef").expect("encryption");
        let signing_key = ProxySigningKey::derive_from(b"test-signing-key-minimum-32-bytes!!");
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));

        let ctx = build_provider_context(
            &user_id,
            &room_id,
            ProviderPlaybackDeps {
                credential_encryption: Some(&credential_encryption),
                credential_repo: Some(&credential_repo),
                signing_key: Some(&signing_key),
                store: Some(store.clone()),
            },
        );

        assert_eq!(ctx.user_id, Some(user_id.as_str()));
        assert_eq!(ctx.room_id, Some(room_id.as_str()));
        assert!(ctx.credential_encryption.is_some());
        assert!(ctx.credential_repo.is_some());
        assert!(ctx.signing_key.is_some());
        assert!(ctx
            .store
            .as_ref()
            .is_some_and(|loaded| Arc::ptr_eq(loaded, &store)));
    }

    #[test]
    fn build_provider_context_leaves_optional_dependencies_empty_when_not_provided() {
        let user_id = synctv_core::models::UserId::from_string("user-provider".to_string());
        let room_id = synctv_core::models::RoomId::from_string("room-provider".to_string());

        let ctx = build_provider_context(&user_id, &room_id, ProviderPlaybackDeps::default());

        assert_eq!(ctx.user_id, Some(user_id.as_str()));
        assert_eq!(ctx.room_id, Some(room_id.as_str()));
        assert!(ctx.credential_encryption.is_none());
        assert!(ctx.credential_repo.is_none());
        assert!(ctx.signing_key.is_none());
        assert!(ctx.store.is_none());
    }
}
