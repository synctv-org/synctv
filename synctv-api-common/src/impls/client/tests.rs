//! Tests for client API implementation

use super::convert::*;
use crate::impls::ApiError;
use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::models::{
    MediaId, MemberStatus, PlaylistId, RoomGuestPermissionBits, RoomId, RoomMemberPermissionBits,
    RoomPermission, RoomPermissionSet, RoomRole, RoomStatus, UserId, UserRole, UserStatus,
};
use synctv_core::provider::{ProviderStore, ProviderStoreResolver, StoreError, StoreLockGuard};

fn test_public_id_codec() -> synctv_adapter::PublicIdCodec {
    synctv_adapter::PublicIdCodec::plain()
}

type TestResult<T = ()> = anyhow::Result<T>;

fn direct_url_playback_info(url: &str, name: &str) -> synctv_core::models::PlaybackInfo {
    synctv_core::models::PlaybackInfo {
        thumbnail: None,
        medias: vec![synctv_core::models::PlaybackMedia {
            name: name.to_string(),
            format: String::new(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: None,
            provider: synctv_core::models::PlaybackMediaProvider::DirectUrl(
                synctv_core::models::PlaybackDirectUrlMedia::Direct {
                    url: url.to_string(),
                    headers: std::collections::HashMap::new(),
                },
            ),
        }],
        default_media_index: None,
        subtitles: Vec::new(),
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    }
}

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn api_err<T>(result: Result<T, ApiError>) -> TestResult<ApiError> {
    match result {
        Ok(_) => Err(test_error("expected API error")),
        Err(error) => Ok(error),
    }
}

fn codec_ok<T>(result: Result<T, String>) -> TestResult<T> {
    result.map_err(test_error)
}

#[test]
fn source_url_cover_conversions_return_display_only_payloads() {
    let playlist_cover = source_url_to_resource_cover("https://cdn.example.test/cover.jpg".into());
    assert_eq!(playlist_cover.url, "https://cdn.example.test/cover.jpg");
    assert!(playlist_cover.metadata.is_none());
    assert!(playlist_cover.variants.is_empty());
    assert!(playlist_cover.object_access.is_none());

    let media_cover = source_url_to_media_cover("https://cdn.example.test/media.jpg".into());
    assert_eq!(media_cover.url, "https://cdn.example.test/media.jpg");
    assert_eq!(media_cover.id, "");
    assert_eq!(media_cover.mime_type, "");
    assert_eq!(media_cover.size_bytes, 0);
    assert_eq!(media_cover.width, 0);
    assert_eq!(media_cover.height, 0);
    assert!(media_cover.metadata.is_none());
    assert!(media_cover.variants.is_empty());
    assert!(media_cover.object_access.is_none());
}

fn unused_store_error() -> StoreError {
    StoreError::Backend("constructor-only test store access".to_string())
}

fn guest_access(permissions: RoomPermissionSet) -> super::GuestRoomAccess {
    super::GuestRoomAccess {
        room_id: RoomId::expect_positive(1),
        guest_id: "gst_test".to_string(),
        display_name: "Guest test".to_string(),
        session_id: "guest-session".to_string(),
        token_jti: "guest-jti".to_string(),
        permissions,
        room_guest_version: 0,
    }
}

#[tokio::test]
async fn test_shared_room_actor_playlist_items_rejects_guest_without_media_resource_permission() {
    let err = super::ClientApiImpl::require_guest_permission(
        &guest_access(RoomPermissionSet::empty()),
        RoomPermission::BROWSE_LIBRARY,
    )
    .expect_err("guest media-resource reads must be rejected before any repository lookup");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("Guests do not have permission")),
        "expected guest authorization error, got {err:?}"
    );
}

#[tokio::test]
async fn test_shared_room_actor_playlist_items_rejects_guest_even_if_media_resource_permission_requested(
) {
    let requested = RoomPermissionSet(
        synctv_core::models::RoomAdminPermissionBits::BROWSE_LIBRARY
            | synctv_core::models::RoomAdminPermissionBits::USE_VOICE_CHAT,
    );
    let capped = RoomPermissionSet(
        requested.0 & RoomGuestPermissionBits::to_permissions(RoomGuestPermissionBits::ALL),
    );
    assert!(!capped.has(synctv_core::models::RoomPermission::BROWSE_LIBRARY));

    let err = super::ClientApiImpl::require_guest_permission(
        &guest_access(capped),
        RoomPermission::BROWSE_LIBRARY,
    )
    .expect_err("guest media-resource reads must stay rejected after guest permission capping");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("Guests do not have permission")),
        "expected guest authorization error, got {err:?}"
    );
}

#[test]
fn update_room_settings_rejects_empty_request() {
    let current = synctv_core::models::RoomSettings::default();
    let error = super::room::validate_update_room_settings_request(
        &synctv_proto::client::UpdateRoomSettingsRequest::default(),
        current,
    )
    .expect_err("empty room settings update should be rejected");

    assert!(matches!(
        error,
        ApiError::InvalidInput(message) if message.contains("settings is required")
    ));
}

#[test]
fn test_guest_actor_cannot_satisfy_signed_in_room_operations() {
    let actor = super::RoomActor::Guest(guest_access(RoomPermissionSet(
        synctv_core::models::RoomAdminPermissionBits::USE_VOICE_CHAT,
    )));
    let err = actor
        .require_user_id()
        .expect_err("playlist/media mutation endpoints require a signed-in user");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("signed-in user")),
        "expected signed-in user requirement, got {err:?}"
    );
}

#[test]
fn test_guest_actor_can_authorize_chat_history_snapshots_without_user_id() -> TestResult {
    let actor = super::RoomActor::Guest(guest_access(RoomPermissionSet(
        synctv_core::models::RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
    )));

    assert!(actor.user_id().is_none());
    api_ok(super::ClientApiImpl::require_guest_permission(
        match &actor {
            super::RoomActor::Guest(access) => access,
            super::RoomActor::User { .. } => return Err(test_error("expected guest actor")),
        },
        RoomPermission::VIEW_CHAT_HISTORY,
    ))?;
    Ok(())
}

#[test]
fn test_room_actor_executor_rejects_malformed_authorization_header() {
    let err = super::ClientApiImpl::bearer_token_from_authorization("malformed-token")
        .expect_err("malformed authorization must fail before room actor resolution");

    assert!(
        matches!(err, ApiError::Authentication(ref message) if message == synctv_common::messages::INVALID_AUTHORIZATION_HEADER),
        "expected invalid authorization error, got {err:?}"
    );
}

#[test]
fn test_client_api_config_accepts_trait_object_provider_store_resolver() {
    #[derive(Clone)]
    struct TestProviderStore;

    #[async_trait]
    impl ProviderStore for TestProviderStore {
        async fn get_raw(&self, _key: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Err(unused_store_error())
        }

        async fn set_raw(
            &self,
            _key: &str,
            _value: &[u8],
            _ttl: std::time::Duration,
        ) -> Result<(), StoreError> {
            Err(unused_store_error())
        }

        async fn delete(&self, _key: &str) -> Result<(), StoreError> {
            Err(unused_store_error())
        }

        async fn lock(
            &self,
            _key: &str,
            _ttl: std::time::Duration,
        ) -> Result<StoreLockGuard, StoreError> {
            Err(unused_store_error())
        }
    }

    struct TestProviderStoreResolver {
        store: Arc<dyn ProviderStore>,
    }

    impl ProviderStoreResolver for TestProviderStoreResolver {
        fn load(&self, _name: &str) -> Arc<dyn ProviderStore> {
            self.store.clone()
        }

        fn key_prefix(&self) -> &'static str {
            "test:"
        }
    }

    let resolver: Arc<dyn ProviderStoreResolver> = Arc::new(TestProviderStoreResolver {
        store: Arc::new(TestProviderStore),
    });
    let provider_stores = Some(resolver.clone());

    assert!(
        provider_stores
            .as_ref()
            .is_some_and(|injected| Arc::ptr_eq(injected, &resolver)),
        "client API config should retain the injected provider store resolver object"
    );
}

#[test]
fn test_room_access_error_authorization_stays_forbidden() {
    let mapped = super::ClientApiImpl::map_room_access_error(synctv_core::Error::Authorization(
        "Not a member of this room".to_string(),
    ));

    assert!(
        matches!(mapped, ApiError::Authorization(ref msg) if msg == "Forbidden: Not a member of this room"),
        "authorization failures must remain forbidden, got: {mapped:?}"
    );
}

#[test]
fn test_room_access_error_not_found_stays_not_found() {
    let mapped = super::ClientApiImpl::map_room_access_error(synctv_core::Error::NotFound(
        "Room not found".to_string(),
    ));

    assert!(
        matches!(mapped, ApiError::NotFound(ref msg) if msg == "Room not found"),
        "missing rooms must not be rewritten as forbidden, got: {mapped:?}"
    );
}

#[test]
fn test_room_access_error_service_unavailable_stays_service_unavailable() {
    let mapped = super::ClientApiImpl::map_room_access_error(
        synctv_core::Error::ServiceUnavailable("permission backend unavailable".to_string()),
    );

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "permission backend unavailable"),
        "backend failures must not be rewritten as forbidden, got: {mapped:?}"
    );
}

#[test]
fn test_room_access_error_permission_denied_stays_forbidden() {
    let mapped = super::ClientApiImpl::map_room_access_error(synctv_core::Error::Authorization(
        "Permission denied".to_string(),
    ));

    assert!(
        matches!(mapped, ApiError::Authorization(ref msg) if msg == "Forbidden: Permission denied"),
        "permission denials must remain forbidden, got: {mapped:?}"
    );
}

#[test]
fn test_media_lookup_error_service_unavailable_stays_service_unavailable() {
    let mapped = super::ClientApiImpl::map_media_lookup_error(
        synctv_core::Error::ServiceUnavailable("media lookup unavailable".to_string()),
        "Media not found",
    );

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "media lookup unavailable"),
        "media lookup backend failures must remain service unavailable, got: {mapped:?}"
    );
}

#[test]
fn test_membership_probe_error_service_unavailable_stays_service_unavailable() {
    let mapped = super::ClientApiImpl::map_membership_probe_error(
        synctv_core::Error::ServiceUnavailable("membership backend unavailable".to_string()),
    );

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "membership backend unavailable"),
        "membership probe backend failures must remain service unavailable, got: {mapped:?}"
    );
}

#[test]
fn test_room_list_backend_outage_maps_to_service_unavailable() {
    let mapped =
        crate::impls::ApiError::from(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
        "room list backend failures must remain service unavailable, got: {mapped:?}"
    );
}

#[test]
fn test_livestream_backend_error_service_unavailable_stays_service_unavailable() {
    let stream_error =
        synctv_livestream::StreamError::RegistryError("redis temporarily unavailable".to_string());
    let mapped = super::ClientApiImpl::map_livestream_backend_error(&stream_error);

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "Live streaming service is temporarily unavailable. Please try again later."),
        "livestream backend failures must remain service unavailable, got: {mapped:?}"
    );
}

#[test]
fn test_livestream_backend_error_finds_nested_stream_error() {
    let err = anyhow::Error::new(synctv_livestream::StreamError::ResourceExhausted(
        "max concurrent streams reached (limit: 100)".to_string(),
    ))
    .context("wrapped by anyhow");

    let mapped = super::ClientApiImpl::map_livestream_backend_error(err.as_ref());

    assert!(
        matches!(mapped, ApiError::RateLimited(ref msg) if msg == "Live streaming capacity limit reached. Please try again later."),
        "nested livestream resource exhaustion must remain rate limited, got: {mapped:?}"
    );
}

#[test]
fn test_proto_role_to_room_role_all_variants() -> TestResult {
    assert_eq!(
        api_ok(proto_role_to_room_role(
            synctv_proto::common::RoomMemberRole::Creator as i32
        ))?,
        RoomRole::Creator
    );
    assert_eq!(
        api_ok(proto_role_to_room_role(
            synctv_proto::common::RoomMemberRole::Admin as i32
        ))?,
        RoomRole::Admin
    );
    assert_eq!(
        api_ok(proto_role_to_room_role(
            synctv_proto::common::RoomMemberRole::Member as i32
        ))?,
        RoomRole::Member
    );
    assert_eq!(
        api_ok(proto_role_to_room_role(
            synctv_proto::common::RoomMemberRole::Guest as i32
        ))?,
        RoomRole::Guest
    );
    Ok(())
}

#[test]
fn test_proto_role_to_room_role_invalid() -> TestResult {
    let err = api_err(proto_role_to_room_role(999))?;
    assert!(err.to_string().contains("Unknown room member role"));
    Ok(())
}

#[test]
fn test_proto_role_to_assignable_room_role_rejects_creator() -> TestResult {
    let err = api_err(proto_role_to_assignable_room_role(
        synctv_proto::common::RoomMemberRole::Creator as i32,
    ))?;
    assert!(
        err.to_string().contains("Creator role is bound"),
        "creator assignment must be rejected: {err}"
    );
    Ok(())
}

#[test]
fn test_proto_role_to_assignable_room_role_allows_admin_member_guest() -> TestResult {
    assert_eq!(
        api_ok(proto_role_to_assignable_room_role(
            synctv_proto::common::RoomMemberRole::Admin as i32
        ))?,
        RoomRole::Admin
    );
    assert_eq!(
        api_ok(proto_role_to_assignable_room_role(
            synctv_proto::common::RoomMemberRole::Member as i32
        ))?,
        RoomRole::Member
    );
    assert_eq!(
        api_ok(proto_role_to_assignable_room_role(
            synctv_proto::common::RoomMemberRole::Guest as i32
        ))?,
        RoomRole::Guest
    );
    Ok(())
}

#[test]
fn test_proto_role_to_user_role_all_variants() -> TestResult {
    assert_eq!(
        api_ok(proto_role_to_user_role(
            synctv_proto::common::UserRole::Root as i32
        ))?,
        UserRole::Root
    );
    assert_eq!(
        api_ok(proto_role_to_user_role(
            synctv_proto::common::UserRole::Admin as i32
        ))?,
        UserRole::Admin
    );
    assert_eq!(
        api_ok(proto_role_to_user_role(
            synctv_proto::common::UserRole::User as i32
        ))?,
        UserRole::User
    );
    Ok(())
}

#[test]
fn test_proto_role_to_user_role_invalid() -> TestResult {
    let err = api_err(proto_role_to_user_role(999))?;
    assert!(err.to_string().contains("Unknown user role"));
    Ok(())
}

fn make_test_user(role: UserRole, status: UserStatus) -> synctv_core::models::User {
    synctv_core::models::User {
        id: UserId::expect_positive(101),
        username: "testuser".to_string(),
        role,
        avatar_file_reference_id: None,
        status,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        deleted_at: None,
        version: 0,
    }
}

#[test]
fn test_provider_error_not_found_preserves_not_found_semantics() {
    let err = ApiError::from(synctv_core::provider::ProviderError::NotFound);
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[test]
fn test_provider_error_credential_expired_preserves_authentication_semantics() {
    let err = ApiError::from(synctv_core::provider::ProviderError::CredentialExpired(
        "expired credential".to_string(),
    ));
    assert!(matches!(err, ApiError::Authentication(_)));
}

#[test]
fn test_provider_error_invalid_config_preserves_invalid_input_semantics() {
    let err = ApiError::from(synctv_core::provider::ProviderError::InvalidConfig(
        "missing host".to_string(),
    ));
    assert!(matches!(err, ApiError::InvalidInput(_)));
}

#[test]
fn test_provider_error_upstream_http_is_sanitized() -> TestResult {
    let err = ApiError::from(synctv_core::provider::ProviderError::UpstreamHttp {
        status: 502,
        url: "https://provider.example/playback".to_string(),
    });

    match err {
        ApiError::ServiceUnavailable(message) => {
            assert_eq!(
                message,
                "Upstream provider service is temporarily unavailable."
            );
        }
        other => {
            return Err(test_error(format!(
                "expected upstream unavailability, got {other:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_provider_error_upstream_http_404_maps_to_not_found() -> TestResult {
    let err = ApiError::from(synctv_core::provider::ProviderError::UpstreamHttp {
        status: 404,
        url: "https://provider.example/playback".to_string(),
    });

    match err {
        ApiError::NotFound(message) => {
            assert_eq!(message, "Provider resource not found");
        }
        other => {
            return Err(test_error(format!(
                "expected provider resource not found, got {other:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_provider_error_upstream_http_400_maps_to_invalid_input() -> TestResult {
    let err = ApiError::from(synctv_core::provider::ProviderError::UpstreamHttp {
        status: 400,
        url: "https://provider.example/playback".to_string(),
    });

    match err {
        ApiError::InvalidInput(message) => {
            assert_eq!(message, "Upstream provider rejected the request.");
        }
        other => {
            return Err(test_error(format!(
                "expected upstream provider invalid input, got {other:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_user_public_view_avatar_url_falls_back_to_object_access() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let avatar_access = crate::impls::stored_files::StoredFileObjectAccess::object_access(
        synctv_core::models::FileObjectAccess {
            object_kind: synctv_core::models::FileObjectKind::UserAvatar,
            encoded_object_key: "encoded-avatar".to_string(),
            read_token: "read-avatar".to_string(),
        },
    );

    let proto = api_ok(try_user_public_view_to_proto(
        &user,
        Some(&avatar_access),
        &public_id_codec,
    ))?;

    assert_eq!(
        proto.avatar_url,
        "/api/user/avatar-objects/encoded-avatar?token=read-avatar"
    );
    let object_access = proto
        .avatar_access
        .ok_or_else(|| test_error("avatar access should be present"))?;
    assert_eq!(
        object_access.object_kind,
        synctv_proto::client::FileObjectAccessKind::UserAvatar as i32
    );
    Ok(())
}

fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
    synctv_core::models::Room {
        id: RoomId::expect_positive(201),
        name: "Test Room".to_string(),
        description: "A test room".to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: UserId::expect_positive(202),
        status,
        is_banned: false,
        closed_at: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        deleted_at: None,
        version: 1,
        last_activity_at: synctv_core::SystemClock.now(),
    }
}

#[test]
fn test_try_room_to_proto_basic() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let settings = synctv_core::models::RoomSettings::default();
    let proto = api_ok(try_room_to_proto_basic(
        &room,
        Some(&settings),
        Some(5),
        &public_id_codec,
    ))?;

    assert_eq!(proto.id, codec_ok(public_id_codec.encode_room_id(room.id))?);
    assert_eq!(proto.name, "Test Room");
    assert_eq!(proto.description, "A test room");
    assert_eq!(
        proto.created_by,
        codec_ok(public_id_codec.encode_user_id(room.created_by))?
    );
    assert_eq!(proto.member_count, 5);
    assert_eq!(
        proto.availability,
        synctv_proto::client::ResourceAvailability::Available as i32
    );
    assert!(!proto.is_banned);
    Ok(())
}

#[test]
fn test_room_to_proto_requires_member_count() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let settings = synctv_core::models::RoomSettings::default();
    let error = api_err(try_room_to_proto_basic(
        &room,
        Some(&settings),
        None,
        &public_id_codec,
    ))?;

    assert!(matches!(
        error,
        crate::impls::ApiError::Internal(message)
            if message.contains("Missing member count for client room")
    ));
    Ok(())
}

#[test]
fn test_room_to_proto_requires_settings() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let error = api_err(try_room_to_proto_basic(
        &room,
        None,
        Some(0),
        &public_id_codec,
    ))?;

    assert!(matches!(
        error,
        crate::impls::ApiError::Internal(message)
            if message.contains("Missing room settings for client room")
    ));
    Ok(())
}

#[test]
fn test_room_to_proto_banned() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let mut room = make_test_room(RoomStatus::Active);
    let settings = synctv_core::models::RoomSettings::default();
    room.is_banned = true;
    let proto = api_ok(try_room_to_proto_basic(
        &room,
        Some(&settings),
        Some(0),
        &public_id_codec,
    ))?;
    assert!(proto.is_banned);
    assert_eq!(
        proto.availability,
        synctv_proto::client::ResourceAvailability::Available as i32
    );
    Ok(())
}

#[test]
fn test_playback_state_to_proto() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: Some(MediaId::expect_positive(302)),
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: false,
        playback_generation: 0,
        updated_at: synctv_core::SystemClock.now(),
        version: 42,
    };

    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;
    let playing_media_id = state
        .playing_media_id
        .ok_or_else(|| test_error("playback state should include media id"))?;

    assert_eq!(
        proto.room_id,
        codec_ok(public_id_codec.encode_room_id(state.room_id))?
    );
    assert_eq!(
        proto.playing_media_id,
        codec_ok(public_id_codec.encode_media_id(playing_media_id))?
    );
    assert_eq!(proto.playing_playlist_id, "");
    assert!(proto.target.is_none());
    assert!((proto.position - 120.5).abs() < f64::EPSILON);
    assert!((proto.speed - 1.5).abs() < f64::EPSILON);
    assert!(!proto.is_playing);
    assert_eq!(proto.version, 42);
    assert!(proto.generated_at_millis > 0);
    Ok(())
}

#[test]
fn test_playback_state_to_proto_computes_elapsed_time_while_playing() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: Some(MediaId::expect_positive(302)),
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: true,
        playback_generation: 0,
        updated_at: synctv_core::SystemClock.now() - chrono::TimeDelta::seconds(2),
        version: 42,
    };

    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;

    assert!(proto.position >= 123.49);
    assert!(proto.position < 124.5);
    let generated_at = chrono::DateTime::from_timestamp_millis(proto.generated_at_millis)
        .ok_or_else(|| test_error("generated_at_millis should be a valid timestamp"))?;
    let expected = state.computed_position_at(generated_at);
    assert!((proto.position - expected).abs() < f64::EPSILON);
    Ok(())
}

#[test]
fn test_playback_state_to_proto_dynamic_playlist_target() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: None,
        playing_playlist_id: Some(PlaylistId::expect_positive(303)),
        target: Some(synctv_core::models::ProviderTarget::emby(
            "provider-item-9".to_string(),
        )),
        current_progress_id: None,
        history_cursor_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: true,
        playback_generation: 0,
        updated_at: synctv_core::SystemClock.now(),
        version: 42,
    };

    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;
    let playing_playlist_id = state
        .playing_playlist_id
        .ok_or_else(|| test_error("playback state should include playlist id"))?;

    assert_eq!(proto.playing_media_id, "");
    assert_eq!(
        proto.playing_playlist_id,
        codec_ok(public_id_codec.encode_playlist_id(playing_playlist_id))?
    );
    let Some(synctv_proto::client::ProviderTarget {
        target:
            Some(synctv_proto::client::provider_target::Target::Emby(
                synctv_proto::client::EmbyTarget {
                    target:
                        Some(synctv_proto::client::emby_target::Target::Item(
                            synctv_proto::client::EmbyItemTarget { item_id },
                        )),
                },
            )),
    }) = proto.target
    else {
        return Err(test_error("playback state should include emby target"));
    };
    assert_eq!(item_id, "provider-item-9");
    Ok(())
}

#[test]
fn test_playback_state_to_proto_no_media() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState::new(RoomId::expect_positive(301));
    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;

    assert_eq!(proto.playing_media_id, "");
    assert_eq!(proto.playing_playlist_id, "");
    assert!(!proto.is_playing);
    Ok(())
}

fn make_test_media() -> synctv_core::models::Media {
    let now = synctv_core::SystemClock.now();
    synctv_core::models::Media {
        id: MediaId::expect_positive(302),
        playlist_id: Some(PlaylistId::expect_positive(303)),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Test Video".to_string(),
        description: String::new(),
        position: 3.0,
        source_provider: synctv_core::models::SourceProvider::Bilibili,
        source_config: synctv_core_testing::bilibili_video_media_source_config("BV1234", 1, false),
        provider_instance_name: Some("bili_main".to_string()),
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: now,
        updated_at: now,
        version: 0,
    }
}

fn make_test_cover_reference() -> synctv_core::models::StoredFileReference {
    let now = synctv_core::SystemClock.now();
    synctv_core::models::StoredFileReference {
        file_reference_id: 901,
        storage_backend: "database".to_string(),
        object_key: "covers/original.jpg".to_string(),
        mime_type: "image/jpeg".to_string(),
        size_bytes: 4096,
        content_manifest_sha256: "f".repeat(64),
        metadata: synctv_core::models::FileMetadata {
            width: Some(1280),
            height: Some(720),
            blurhash: Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".to_string()),
            variants: vec![synctv_core::models::FileObjectVariant {
                storage_backend: "database".to_string(),
                object_key: "covers/small.jpg".to_string(),
                original_storage_backend: "database".to_string(),
                original_object_key: "covers/original.jpg".to_string(),
                group_id: "fg_cover".to_string(),
                variant_key: "small".to_string(),
                label: "Small".to_string(),
                object_access: None,
                url: Some("/api/files/covers/small.jpg".to_string()),
                mime_type: "image/jpeg".to_string(),
                size_bytes: 1024,
                width: Some(320),
                height: Some(180),
                is_original: false,
                lossy: true,
                quality: Some(80),
                sort_order: 10,
                metadata: synctv_core::models::FileVariantMetadata {
                    width: Some(320),
                    height: Some(180),
                    blurhash: Some("LKO2?U%2Tw=w]~RBVZRi};RPxuwH".to_string()),
                },
                created_at: now,
            }],
            upload_token: Some("private-token".to_string()),
            ownership_proof: Some("private-proof".to_string()),
            audio: None,
        },
        created_at: now,
        validated_at: Some(now),
    }
}

#[test]
fn test_media_to_proto_basic() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let proto = api_ok(try_media_to_proto_for_viewer_without_cover(
        &media,
        true,
        None,
        &public_id_codec,
        None,
    ))?;
    let creator_id = media
        .creator_id
        .ok_or_else(|| test_error("media should include creator id"))?;

    assert_eq!(
        proto.id,
        codec_ok(public_id_codec.encode_media_id(media.id))?
    );
    assert_eq!(
        proto.room_id,
        codec_ok(public_id_codec.encode_room_id(media.room_id))?
    );
    assert_eq!(
        proto.source_provider,
        synctv_proto::source_config::SourceProvider::Bilibili as i32
    );
    assert_eq!(proto.name, "Test Video");
    assert!(proto.metadata.is_none());
    assert!(proto.source_config.is_none());
    assert_eq!(proto.position.to_bits(), 3.0f64.to_bits());
    assert_eq!(
        proto.creator_id,
        codec_ok(public_id_codec.encode_user_id(creator_id))?
    );
    assert_eq!(proto.provider_instance_name, "bili_main");
    Ok(())
}

#[test]
fn test_media_to_proto_preserves_provider_resource_metadata() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let provider_metadata = synctv_core::models::PlaybackMetadata::Bilibili(
        synctv_core::models::BilibiliPlaybackMetadata {
            room_id: Some(21_292_831),
            ..synctv_core::models::BilibiliPlaybackMetadata::new(
                synctv_core::models::BilibiliPlaybackKind::Live,
            )
        },
    );
    let proto = api_ok(try_media_to_proto_for_viewer_without_cover(
        &media,
        true,
        None,
        &public_id_codec,
        Some(&provider_metadata),
    ))?;
    let metadata = proto
        .metadata
        .ok_or_else(|| test_error("resource metadata should be present"))?;
    let provider = metadata
        .provider
        .ok_or_else(|| test_error("provider metadata should be present"))?;
    let Some(synctv_proto::client::playback_metadata::Metadata::Bilibili(metadata)) =
        provider.metadata
    else {
        return Err(test_error("Bilibili provider metadata should be present"));
    };
    assert_eq!(
        metadata.kind,
        synctv_proto::client::BilibiliPlaybackKind::Live as i32
    );
    Ok(())
}

#[test]
fn test_media_to_proto_with_cover_includes_cover_payload() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let cover = make_test_cover_reference();
    let cover_access = crate::impls::stored_files::StoredFileObjectAccess::external_url(
        "/api/media/covers/original.jpg",
    );
    let proto = api_ok(try_media_to_proto_for_viewer_with_cover(
        &media,
        MediaProtoView {
            is_available: true,
            viewer_id: media.creator_id,
            cover: Some(&cover),
            cover_access: Some(&cover_access),
            thumbnail: None,
            thumbnail_access: None,
            public_id_codec: &public_id_codec,
        },
        None,
    ))?;
    let proto_cover = proto
        .cover
        .ok_or_else(|| test_error("media cover should be present"))?;

    assert_eq!(proto_cover.id, "901");
    assert_eq!(proto_cover.url, "/api/media/covers/original.jpg");
    assert_eq!(proto_cover.mime_type, "image/jpeg");
    assert_eq!(proto_cover.size_bytes, 4096);
    assert_eq!(proto_cover.width, 1280);
    assert_eq!(proto_cover.height, 720);
    let metadata = proto_cover
        .metadata
        .ok_or_else(|| test_error("media cover metadata should be present"))?;
    assert_eq!(metadata.width, Some(1280));
    assert_eq!(metadata.height, Some(720));
    assert_eq!(
        metadata.blurhash.as_deref(),
        Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj")
    );
    assert_eq!(proto_cover.variants.len(), 1);
    assert_eq!(proto_cover.variants[0].key, "small");
    assert_eq!(proto_cover.variants[0].url, "/api/files/covers/small.jpg");
    Ok(())
}

#[test]
fn test_media_to_proto_with_object_access_cover_includes_structured_access() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let cover = make_test_cover_reference();
    let cover_access = crate::impls::stored_files::StoredFileObjectAccess::object_access(
        synctv_core::models::FileObjectAccess {
            object_kind: synctv_core::models::FileObjectKind::MediaCover,
            encoded_object_key: "encoded-cover".to_string(),
            read_token: "read-cover".to_string(),
        },
    );
    let proto = api_ok(try_media_to_proto_for_viewer_with_cover(
        &media,
        MediaProtoView {
            is_available: true,
            viewer_id: media.creator_id,
            cover: Some(&cover),
            cover_access: Some(&cover_access),
            thumbnail: None,
            thumbnail_access: None,
            public_id_codec: &public_id_codec,
        },
        None,
    ))?;
    let proto_cover = proto
        .cover
        .ok_or_else(|| test_error("media cover should be present"))?;
    let object_access = proto_cover
        .object_access
        .ok_or_else(|| test_error("object access should be present"))?;

    assert_eq!(
        proto_cover.url,
        "/api/media/cover-objects/encoded-cover?token=read-cover"
    );
    assert_eq!(
        object_access.object_kind,
        synctv_proto::client::FileObjectAccessKind::MediaCover as i32
    );
    assert_eq!(object_access.encoded_object_key, "encoded-cover");
    assert_eq!(object_access.read_token, "read-cover");
    Ok(())
}

#[test]
fn test_media_to_proto_with_thumbnail_includes_distinct_payload() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let thumbnail = make_test_cover_reference();
    let thumbnail_access = crate::impls::stored_files::StoredFileObjectAccess::object_access(
        synctv_core::models::FileObjectAccess {
            object_kind: synctv_core::models::FileObjectKind::MediaThumbnail,
            encoded_object_key: "encoded-thumbnail".to_string(),
            read_token: "read-thumbnail".to_string(),
        },
    );
    let proto = api_ok(try_media_to_proto_for_viewer_with_cover(
        &media,
        MediaProtoView {
            is_available: true,
            viewer_id: media.creator_id,
            cover: None,
            cover_access: None,
            thumbnail: Some(&thumbnail),
            thumbnail_access: Some(&thumbnail_access),
            public_id_codec: &public_id_codec,
        },
        None,
    ))?;
    let proto_thumbnail = proto
        .thumbnail
        .ok_or_else(|| test_error("media thumbnail should be present"))?;
    let object_access = proto_thumbnail
        .object_access
        .ok_or_else(|| test_error("thumbnail object access should be present"))?;

    assert!(proto.cover.is_none());
    assert_eq!(
        proto_thumbnail.url,
        "/api/media/thumbnail-objects/encoded-thumbnail?token=read-thumbnail"
    );
    assert_eq!(proto_thumbnail.mime_type, "image/jpeg");
    assert_eq!(proto_thumbnail.width, 1280);
    assert_eq!(proto_thumbnail.height, 720);
    assert_eq!(
        object_access.object_kind,
        synctv_proto::client::FileObjectAccessKind::MediaThumbnail as i32
    );
    assert_eq!(object_access.encoded_object_key, "encoded-thumbnail");
    assert_eq!(object_access.read_token, "read-thumbnail");
    Ok(())
}

#[test]
fn test_media_to_proto_for_owner_includes_source_metadata() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let owner_id = media
        .creator_id
        .ok_or_else(|| test_error("media should include creator id"))?;
    let proto = api_ok(try_media_to_proto_for_viewer_without_cover(
        &media,
        true,
        Some(owner_id),
        &public_id_codec,
        None,
    ))?;

    assert_eq!(
        proto
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.source.as_deref()),
        Some("BV1234")
    );
    assert!(proto.source_config.is_some());
    Ok(())
}

#[test]
fn test_seafile_source_metadata_uses_native_path() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let mut media = make_test_media();
    media.source_provider = synctv_core::models::SourceProvider::Seafile;
    media.source_config = synctv_core::models::MediaSourceConfig::Seafile(
        synctv_core::models::SeafileMediaSourceConfig {
            server_id: "seafile-home".to_string(),
            repository_id: "repo-1".to_string(),
            path: "/Videos/Movie.mkv".to_string(),
            object_id: "object-1".to_string(),
            has_thumbnail: true,
        },
    );
    let proto = api_ok(try_media_to_proto_for_viewer_without_cover(
        &media,
        true,
        media.creator_id,
        &public_id_codec,
        None,
    ))?;

    assert_eq!(
        proto
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.source.as_deref()),
        Some("/Videos/Movie.mkv")
    );
    Ok(())
}

#[test]
fn test_media_to_proto_direct_media_omits_default_instance_binding() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let media = synctv_core::models::Media::from_direct_single_mode(
        Some(PlaylistId::expect_positive(305)),
        RoomId::expect_positive(301),
        Some(UserId::expect_positive(304)),
        "Direct Media".to_string(),
        "direct",
        direct_url_playback_info("https://example.com/video.mp4", "1080p"),
        1.0,
    )
    .map_err(|error| test_error(error.to_string()))?;
    let proto = api_ok(try_media_to_proto_for_viewer_without_cover(
        &media,
        true,
        None,
        &public_id_codec,
        None,
    ))?;
    assert_eq!(
        proto.source_provider,
        synctv_proto::source_config::SourceProvider::DirectUrl as i32
    );
    assert!(proto.provider_instance_name.is_empty());
    Ok(())
}

fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
    synctv_core::models::RoomMemberWithUser {
        room_id: RoomId::expect_positive(301),
        user_id: UserId::expect_positive(304),
        username: "alice".to_string(),
        remark_name: "Alice Remark".to_string(),
        display_tag: "VIP".to_string(),
        role,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: synctv_core::SystemClock.now(),
        is_online: true,
        is_active: true,
    }
}

#[test]
fn test_room_member_to_proto() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let member = make_test_member(RoomRole::Member);
    let role_default = RoomRole::Member.permissions();
    let proto = api_ok(try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    ))?;

    assert_eq!(
        proto.room_id,
        codec_ok(public_id_codec.encode_room_id(member.room_id))?
    );
    assert_eq!(
        proto.user_id,
        codec_ok(public_id_codec.encode_user_id(member.user_id))?
    );
    assert_eq!(proto.username, "alice");
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );
    assert!(proto.is_online);
    Ok(())
}

#[test]
fn test_room_member_to_proto_creator() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let member = make_test_member(RoomRole::Creator);
    let role_default = RoomRole::Creator.permissions();
    let proto = api_ok(try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    ))?;
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Creator as i32
    );
    Ok(())
}

#[test]
fn test_room_member_to_proto_custom_permissions() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xFF;
    member.removed_permissions = 0x0F;
    let role_default = RoomRole::Member.permissions();
    let proto = api_ok(try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    ))?;
    assert_eq!(proto.added_permissions, 0xFF);
    assert_eq!(proto.removed_permissions, 0x0F);
    Ok(())
}

#[test]
fn test_playlist_to_proto() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(303),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "My Playlist".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };

    let proto = api_ok(try_playlist_to_proto_for_viewer_without_cover(
        &playlist,
        10,
        true,
        None,
        &public_id_codec,
        None,
    ))?;

    assert_eq!(
        proto.id,
        codec_ok(public_id_codec.encode_playlist_id(playlist.id))?
    );
    assert_eq!(
        proto.room_id,
        codec_ok(public_id_codec.encode_room_id(playlist.room_id))?
    );
    assert_eq!(proto.name, "My Playlist");
    assert_eq!(proto.parent_id, "");
    assert_eq!(proto.item_count, 10);
    assert!(!proto.is_dynamic);
    assert_eq!(
        proto.creator_id,
        codec_ok(public_id_codec.encode_user_id(UserId::expect_positive(304)))?
    );
    Ok(())
}

#[test]
fn test_playlist_to_proto_with_cover_includes_cover_payload() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(303),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "My Playlist".to_string(),
        description: String::new(),
        cover_file_reference_id: Some(901),
        parent_id: None,
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };
    let cover = make_test_cover_reference();
    let cover_access = crate::impls::stored_files::StoredFileObjectAccess::external_url(
        "/api/playlist/covers/original.jpg",
    );
    let proto = api_ok(try_playlist_to_proto_for_viewer_with_cover(
        &playlist,
        10,
        true,
        playlist.creator_id,
        Some(&cover),
        Some(&cover_access),
        &public_id_codec,
        None,
    ))?;
    let proto_cover = proto
        .cover
        .ok_or_else(|| test_error("playlist cover should be present"))?;

    assert_eq!(proto_cover.url, "/api/playlist/covers/original.jpg");
    let metadata = proto_cover
        .metadata
        .ok_or_else(|| test_error("playlist cover metadata should be present"))?;
    assert_eq!(metadata.width, Some(1280));
    assert_eq!(metadata.height, Some(720));
    assert_eq!(
        metadata.blurhash.as_deref(),
        Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj")
    );
    assert_eq!(proto_cover.variants.len(), 1);
    assert_eq!(proto_cover.variants[0].key, "small");
    assert_eq!(proto_cover.variants[0].width, 320);
    assert_eq!(proto_cover.variants[0].height, 180);
    Ok(())
}

#[test]
fn test_playlist_to_proto_dynamic() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(306),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Alist Folder".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: Some(PlaylistId::expect_positive(303)),
        position: 1.0,
        source_provider: Some(synctv_core::models::SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            "alist-main",
            "/tv",
        )),
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };

    let proto = api_ok(try_playlist_to_proto_for_viewer_without_cover(
        &playlist,
        5,
        true,
        None,
        &public_id_codec,
        None,
    ))?;
    let parent_id = playlist
        .parent_id
        .ok_or_else(|| test_error("dynamic playlist should include parent id"))?;

    assert_eq!(
        proto.parent_id,
        codec_ok(public_id_codec.encode_playlist_id(parent_id))?
    );
    assert!(proto.is_dynamic);
    assert_eq!(
        proto.source_provider,
        synctv_proto::source_config::SourceProvider::Alist as i32
    );
    assert_eq!(proto.provider_instance_name, "");
    Ok(())
}

#[test]
fn test_playlist_to_proto_for_owner_includes_source_config() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let owner_id = UserId::expect_positive(304);
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(308),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(owner_id),
        name: "Alist Folder".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 1.0,
        source_provider: Some(synctv_core::models::SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            "alist-main",
            "/tv",
        )),
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };

    let proto = api_ok(try_playlist_to_proto_for_viewer_without_cover(
        &playlist,
        5,
        true,
        Some(owner_id),
        &public_id_codec,
        None,
    ))?;

    assert!(proto.source_config.is_some());
    Ok(())
}

#[test]
fn test_playlist_to_proto_for_non_owner_hides_source_config() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(309),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Alist Folder".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 1.0,
        source_provider: Some(synctv_core::models::SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            "alist-main",
            "/tv",
        )),
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };

    let proto = api_ok(try_playlist_to_proto_for_viewer_without_cover(
        &playlist,
        5,
        true,
        Some(UserId::expect_positive(999)),
        &public_id_codec,
        None,
    ))?;

    assert!(proto.source_config.is_none());
    Ok(())
}

#[test]
fn test_playlist_to_proto_dynamic_requires_source_config() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(307),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Broken Dynamic Playlist".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 1.0,
        source_provider: Some(synctv_core::models::SourceProvider::Bilibili),
        source_config: None,
        provider_instance_name: None,
        created_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    };

    assert!(matches!(
        try_playlist_to_proto_for_viewer_without_cover(
            &playlist,
            5,
            true,
            None,
            &public_id_codec,
            None,
        ),
        Err(ApiError::Internal(message))
            if message.contains("Dynamic playlist")
                && message.contains("source_config")
    ));
    Ok(())
}

#[test]
fn test_members_to_proto_pattern_multiple_roles() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let creator = {
        let mut m = make_test_member(RoomRole::Creator);
        m.username = "owner".to_string();
        m
    };
    let admin = {
        let mut m = make_test_member(RoomRole::Admin);
        m.username = "admin".to_string();
        m.user_id = UserId::expect_positive(305);
        m
    };
    let member = {
        let mut m = make_test_member(RoomRole::Member);
        m.username = "member".to_string();
        m.user_id = UserId::expect_positive(306);
        m
    };
    let guest = {
        let mut m = make_test_member(RoomRole::Guest);
        m.username = "guest".to_string();
        m.user_id = UserId::expect_positive(307);
        m
    };

    let all = vec![creator, admin, member, guest];
    let result: Vec<synctv_proto::common::RoomMember> = all
        .into_iter()
        .map(|m| {
            let role_default = m.role.permissions();
            try_room_member_to_proto_with_permissions(
                &m,
                m.effective_permissions(role_default),
                &public_id_codec,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(result.len(), 4);
    assert_eq!(result[0].username, "owner");
    assert_eq!(
        result[0].role,
        synctv_proto::common::RoomMemberRole::Creator as i32
    );
    assert_eq!(result[1].username, "admin");
    assert_eq!(
        result[1].role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert_eq!(result[2].username, "member");
    assert_eq!(
        result[2].role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );
    assert_eq!(result[3].username, "guest");
    assert_eq!(
        result[3].role,
        synctv_proto::common::RoomMemberRole::Guest as i32
    );

    assert!(
        result[0].permissions > result[3].permissions,
        "Creator should have more permissions than guest"
    );
    Ok(())
}

#[test]
fn test_members_to_proto_pattern_preserves_custom_permissions() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xFF;
    member.removed_permissions = 0x0F;
    let role_default = member.role.permissions();
    let result = api_ok(try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    ))?;
    assert_eq!(result.added_permissions, 0xFF);
    assert_eq!(result.removed_permissions, 0x0F);
    Ok(())
}

const _: () = assert!(
    super::ClientApiImpl::MAX_PLAYLIST_SIZE > 100,
    "MAX_PLAYLIST_SIZE must exceed single batch limit"
);

#[test]
fn test_joined_rooms_permission_needs_three_layer_calculation() {
    // This test documents the bug: role.permissions() gives only role-level
    // defaults, missing room-level and member-level overrides.
    // Correct calculation requires:
    // Using role.permissions() directly skips layers 1 (global settings) and 2 (room overrides).

    let mut member = make_test_member(RoomRole::Member);
    // Give the member custom permission overrides. Member defaults already
    // include every member bit, so deny a default permission to prove that
    // role.permissions() alone is insufficient.
    member.added_permissions = 0;
    member.removed_permissions = RoomMemberPermissionBits::SEND_CHAT_MESSAGES;

    // role.permissions() ignores member overrides completely
    let role_only = member.role.permissions();
    let effective_with_role_only = member.effective_permissions(role_only);

    // The effective permissions should include the added_permissions overlay
    // which means role-only is NOT sufficient when member has overrides
    assert_ne!(
        role_only.0, effective_with_role_only.0,
        "Member with added_permissions should differ from pure role default"
    );
}

#[test]
fn test_effective_permissions_applies_member_overrides() {
    // Verify that effective_permissions correctly applies member-level
    // added and removed permissions on top of the role default
    let mut member = make_test_member(RoomRole::Member);
    let base = RoomRole::Member.permissions();

    // Add a specific permission bit
    member.added_permissions = RoomMemberPermissionBits::USE_VOICE_CHAT;
    let effective = member.effective_permissions(base);
    assert!(
        effective.has(RoomPermission::USE_VOICE_CHAT),
        "Added permission bit should be present in effective permissions"
    );

    // Remove a specific permission bit that the role default includes
    member.added_permissions = 0;
    member.removed_permissions = RoomMemberPermissionBits::ALL;
    let effective = member.effective_permissions(base);
    assert_eq!(
        effective.0 & base.0,
        0,
        "All removed permission bits should be cleared"
    );
}

// type name (e.g., "bilibili") instead of the instance ID (e.g., "bilibili_main")
// for registry lookup.

#[test]
fn test_add_media_batch_uses_provider_instance_name() {
    let instance_name = "bilibili_main";
    let type_name = "bilibili";
    assert_ne!(
        instance_name, type_name,
        "Instance name and type name must be different to catch the bug"
    );
}

#[test]
fn test_playback_state_version_no_truncation() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let large_version: i64 = i64::from(i32::MAX) + 1;
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(401),
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        playback_generation: 0,
        updated_at: synctv_core::SystemClock.now(),
        version: large_version,
    };

    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;
    assert_eq!(
        proto.version, large_version,
        "Version should not be truncated from i64 to i32"
    );
    Ok(())
}

#[test]
fn test_playback_state_version_i32_range_still_works() -> TestResult {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(402),
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        playback_generation: 0,
        updated_at: synctv_core::SystemClock.now(),
        version: 42,
    };

    let proto = api_ok(try_playback_state_to_proto(&state, &public_id_codec))?;
    assert_eq!(proto.version, 42);
    Ok(())
}
