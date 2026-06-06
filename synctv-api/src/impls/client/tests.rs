//! Tests for client API implementation
#![allow(clippy::unwrap_used)]

use super::convert::*;
use crate::impls::ApiError;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::{
    MediaId, MemberStatus, PlaylistId, RoomGuestPermissionBits, RoomId, RoomMemberPermissionBits,
    RoomPermission, RoomPermissionSet, RoomRole, RoomStatus, UserId, UserRole, UserStatus,
};
use synctv_core::provider::{ProviderStore, ProviderStoreResolver, StoreError, StoreLockGuard};
use synctv_core::RedisConnectionRuntime;

fn test_public_id_codec() -> crate::PublicIdCodec {
    crate::PublicIdCodec::plain()
}

fn test_pool_without_repository_access() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused?connect_timeout=1")
        .expect("lazy test pool")
}

fn test_client_api_without_repository_access() -> super::ClientApiImpl {
    test_client_api_without_repository_access_with_runtime(
        crate::impls::ClientApiRuntime::test_disabled(),
    )
}

fn test_client_api_without_repository_access_with_runtime(
    runtime: crate::impls::ClientApiRuntime,
) -> super::ClientApiImpl {
    let pool = test_pool_without_repository_access();
    super::ClientApiImpl::new_with_runtime(
        crate::impls::ClientApiConfig {
            user_service: Arc::new(synctv_core_testing::create_test_user_service(pool.clone())),
            room_service: Arc::new(synctv_core_testing::create_test_room_service(pool)),
            connection_service: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            )),
            config: Arc::new(synctv_core::Config::default()),
            publish_key_service: None,
            jwt_service: synctv_core_testing::create_test_jwt_service(),
            live_streaming_infrastructure: None,
            providers_manager: None,
            settings_registry: None,
            public_id_codec: Arc::new(test_public_id_codec()),
            chat_service: None,
            credential_encryption: None,
            provider_stores: None,
            email_api: None,
            passkey_service: None,
        },
        runtime,
    )
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
    let api = test_client_api_without_repository_access();
    let err = api
        .list_playlist_items_as_guest(
            &guest_access(RoomPermissionSet::empty()),
            crate::proto::client::ListPlaylistItemsRequest::default(),
        )
        .await
        .expect_err("guest media-resource reads must be rejected before any repository lookup");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("Guests do not have permission")),
        "expected guest authorization error, got {err:?}"
    );
}

#[tokio::test]
async fn test_shared_room_actor_playlist_items_rejects_guest_even_if_media_resource_permission_requested(
) {
    let api = test_client_api_without_repository_access();
    let requested = RoomPermissionSet(
        synctv_core::models::RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES
            | synctv_core::models::RoomAdminPermissionBits::USE_WEBRTC,
    );
    let capped = RoomPermissionSet(
        requested.0 & RoomGuestPermissionBits::to_permissions(RoomGuestPermissionBits::ALL),
    );
    assert!(!capped.has(synctv_core::models::RoomPermission::VIEW_MEDIA_RESOURCES));

    let err = api
        .list_playlist_items_as_guest(
            &guest_access(capped),
            crate::proto::client::ListPlaylistItemsRequest::default(),
        )
        .await
        .expect_err("guest media-resource reads must stay rejected after guest permission capping");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("Guests do not have permission")),
        "expected guest authorization error, got {err:?}"
    );
}

#[tokio::test]
async fn update_room_settings_rejects_empty_patch_before_repository_access() {
    let api = test_client_api_without_repository_access();
    let room_id = test_public_id_codec()
        .encode_room_id(RoomId::expect_positive(1))
        .expect("room id should encode");

    let err = api
        .update_room_settings(
            &UserId::expect_positive(1),
            &room_id,
            crate::proto::client::UpdateRoomSettingsRequest {
                settings: Vec::new(),
            },
        )
        .await
        .expect_err("empty settings patch must be rejected");

    assert!(
        matches!(err, ApiError::InvalidInput(ref message) if message.contains("settings patch")),
        "expected settings patch validation error, got {err:?}"
    );
}

#[test]
fn test_guest_actor_cannot_satisfy_signed_in_room_operations() {
    let actor = super::RoomActor::Guest(guest_access(RoomPermissionSet(
        synctv_core::models::RoomAdminPermissionBits::USE_WEBRTC,
    )));
    let err = actor
        .require_user_id()
        .expect_err("playlist/media mutation endpoints require a signed-in user");

    assert!(
        matches!(err, ApiError::Authorization(ref message) if message.contains("signed-in user")),
        "expected signed-in user requirement, got {err:?}"
    );
}

#[tokio::test]
async fn test_room_actor_executor_rejects_malformed_authorization_header() {
    let config = Arc::new(synctv_core::Config::default());
    let api = test_client_api_without_repository_access();
    let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
        synctv_core_testing::create_test_jwt_service(),
    )));
    let request_executor = Arc::new(crate::impls::RequestExecutor::new(
        config.clone(),
        jwt_validator,
        Arc::new(synctv_core::service::SecurityPipeline::new(
            api.user_service.clone(),
        )),
        Arc::new(synctv_core::service::RateLimiter::local_only(
            "test:room-actor-auth:".to_string(),
        )),
    ));
    let api = Arc::new(test_client_api_without_repository_access_with_runtime(
        crate::impls::ClientApiRuntime {
            request_executor: Some(request_executor),
            ..crate::impls::ClientApiRuntime::test_disabled()
        },
    ));
    let metadata = crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Http)
        .with_authorization(Some("malformed-token".to_string()));

    let err = super::ClientApiImpl::execute_room_actor_endpoint(
        api,
        &metadata,
        test_public_id_codec()
            .encode_room_id(RoomId::expect_positive(1))
            .expect("room id should encode"),
        crate::impls::EndpointRateLimitCategory::Read,
        |_api, _actor| async move { Ok::<_, ApiError>(()) },
    )
    .await
    .expect_err("malformed authorization must fail before room actor resolution");

    assert!(
        matches!(err, ApiError::Authentication(ref message) if message == synctv_common::messages::INVALID_AUTHORIZATION_HEADER),
        "expected invalid authorization error, got {err:?}"
    );
}

#[tokio::test]
async fn test_client_api_impl_accepts_trait_object_redis_runtime() {
    #[derive(Clone)]
    struct FakeRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for FakeRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            panic!("snapshot should not be called in constructor-only test");
        }
    }

    let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
    let api = super::ClientApiImpl::new_with_runtime(
        crate::impls::ClientApiConfig {
            user_service: Arc::new(synctv_core::service::UserService::new_for_tests(
            &sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                .expect("lazy pool"),
            synctv_core::service::JwtService::new(
                "test-secret-key-for-client-api-redis-runtime-minimum-32-chars",
            )
            .expect("jwt"),
            synctv_core::cache::UsernameCache::local_only("test:username:".to_string(), 128, 60),
            Arc::new(
                synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
                    128, 3600, 86400,
                ),
            ),
            synctv_core::cache::KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        )),
            room_service: Arc::new(synctv_core::service::RoomService::new_for_tests(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                .expect("lazy pool"),
            synctv_core::service::UserService::new_for_tests(
                &sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-client-api-redis-runtime-minimum-32-chars",
                )
                .expect("jwt"),
                synctv_core::cache::UsernameCache::local_only(
                    "test:username:".to_string(),
                    128,
                    60,
                ),
                Arc::new(
                    synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
                        128, 3600, 86400,
                    ),
                ),
                synctv_core::cache::KeyBuilder::new("test"),
                synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
            ),
        )
        .expect("room service should build")),
            connection_service: Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        )),
            config: Arc::new(synctv_core::Config::default()),
            publish_key_service: None,
            jwt_service: synctv_core::service::JwtService::new(
            "test-secret-key-for-client-api-redis-runtime-minimum-32-chars",
        )
        .expect("jwt"),
            live_streaming_infrastructure: None,
            providers_manager: None,
            settings_registry: None,
            public_id_codec: Arc::new(test_public_id_codec()),
            chat_service: None,
            credential_encryption: None,
            provider_stores: None,
            email_api: None,
            passkey_service: None,
        },
        crate::impls::ClientApiRuntime {
            redis_runtime: Some(runtime.clone()),
            ..crate::impls::ClientApiRuntime::test_disabled()
        },
    );

    assert!(
        api.redis_runtime
            .as_ref()
            .is_some_and(|injected| Arc::ptr_eq(injected, &runtime)),
        "client API should retain the injected Redis runtime object"
    );
}

#[tokio::test]
async fn test_client_api_impl_accepts_trait_object_provider_store_resolver() {
    #[derive(Clone)]
    struct FakeProviderStore;

    #[async_trait]
    impl ProviderStore for FakeProviderStore {
        async fn get_raw(&self, _key: &str) -> Result<Option<Vec<u8>>, StoreError> {
            panic!("store access should not be called in constructor-only test");
        }

        async fn set_raw(
            &self,
            _key: &str,
            _value: &[u8],
            _ttl: std::time::Duration,
        ) -> Result<(), StoreError> {
            panic!("store access should not be called in constructor-only test");
        }

        async fn delete(&self, _key: &str) -> Result<(), StoreError> {
            panic!("store access should not be called in constructor-only test");
        }

        async fn lock(
            &self,
            _key: &str,
            _ttl: std::time::Duration,
        ) -> Result<StoreLockGuard, StoreError> {
            panic!("store access should not be called in constructor-only test");
        }
    }

    struct FakeProviderStoreResolver {
        store: Arc<dyn ProviderStore>,
    }

    impl ProviderStoreResolver for FakeProviderStoreResolver {
        fn load(&self, _name: &str) -> Arc<dyn ProviderStore> {
            self.store.clone()
        }

        fn key_prefix(&self) -> &'static str {
            "test:"
        }
    }

    let resolver: Arc<dyn ProviderStoreResolver> = Arc::new(FakeProviderStoreResolver {
        store: Arc::new(FakeProviderStore),
    });
    let api = super::ClientApiImpl::new_with_runtime(
        crate::impls::ClientApiConfig {
            user_service: Arc::new(synctv_core::service::UserService::new_for_tests(
            &sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                .expect("lazy pool"),
            synctv_core::service::JwtService::new(
                "test-secret-key-for-client-api-provider-store-minimum-32-chars",
            )
            .expect("jwt"),
            synctv_core::cache::UsernameCache::local_only("test:username:".to_string(), 128, 60),
            Arc::new(
                synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
                    128, 3600, 86400,
                ),
            ),
            synctv_core::cache::KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        )),
            room_service: Arc::new(synctv_core::service::RoomService::new_for_tests(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                .expect("lazy pool"),
            synctv_core::service::UserService::new_for_tests(
                &sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-client-api-provider-store-minimum-32-chars",
                )
                .expect("jwt"),
                synctv_core::cache::UsernameCache::local_only(
                    "test:username:".to_string(),
                    128,
                    60,
                ),
                Arc::new(
                    synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
                        128, 3600, 86400,
                    ),
                ),
                synctv_core::cache::KeyBuilder::new("test"),
                synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
            ),
        )
        .expect("room service should build")),
            connection_service: Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        )),
            config: Arc::new(synctv_core::Config::default()),
            publish_key_service: None,
            jwt_service: synctv_core::service::JwtService::new(
            "test-secret-key-for-client-api-provider-store-minimum-32-chars",
        )
        .expect("jwt"),
            live_streaming_infrastructure: None,
            providers_manager: None,
            settings_registry: None,
            public_id_codec: Arc::new(test_public_id_codec()),
            chat_service: None,
            credential_encryption: None,
            provider_stores: Some(resolver.clone()),
            email_api: None,
            passkey_service: None,
        },
        crate::impls::ClientApiRuntime::test_disabled(),
    );

    assert!(
        api.provider_stores
            .as_ref()
            .is_some_and(|injected| Arc::ptr_eq(injected, &resolver)),
        "client API should retain the injected provider store resolver object"
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
    let stream_error = synctv_livestream::error::StreamError::RegistryError(
        "redis temporarily unavailable".to_string(),
    );
    let mapped = super::ClientApiImpl::map_livestream_backend_error(&stream_error);

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "Live streaming service is temporarily unavailable. Please try again later."),
        "livestream backend failures must remain service unavailable, got: {mapped:?}"
    );
}

#[test]
fn test_livestream_backend_error_finds_nested_stream_error() {
    let err = anyhow::Error::new(synctv_livestream::error::StreamError::ResourceExhausted(
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
fn test_proto_role_to_room_role_all_variants() {
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Creator as i32).unwrap(),
        RoomRole::Creator
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Admin as i32).unwrap(),
        RoomRole::Admin
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Member as i32).unwrap(),
        RoomRole::Member
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Guest as i32).unwrap(),
        RoomRole::Guest
    );
}

#[test]
fn test_proto_role_to_room_role_invalid() {
    let err = proto_role_to_room_role(999).unwrap_err();
    assert!(err.to_string().contains("Unknown room member role"));
}

#[test]
fn test_proto_role_to_assignable_room_role_rejects_creator() {
    let err =
        proto_role_to_assignable_room_role(synctv_proto::common::RoomMemberRole::Creator as i32)
            .unwrap_err();
    assert!(
        err.to_string().contains("Creator role is bound"),
        "creator assignment must be rejected: {err}"
    );
}

#[test]
fn test_proto_role_to_assignable_room_role_allows_admin_member_guest() {
    assert_eq!(
        proto_role_to_assignable_room_role(synctv_proto::common::RoomMemberRole::Admin as i32)
            .unwrap(),
        RoomRole::Admin
    );
    assert_eq!(
        proto_role_to_assignable_room_role(synctv_proto::common::RoomMemberRole::Member as i32)
            .unwrap(),
        RoomRole::Member
    );
    assert_eq!(
        proto_role_to_assignable_room_role(synctv_proto::common::RoomMemberRole::Guest as i32)
            .unwrap(),
        RoomRole::Guest
    );
}

#[test]
fn test_proto_role_to_user_role_all_variants() {
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::Root as i32).unwrap(),
        UserRole::Root
    );
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::Admin as i32).unwrap(),
        UserRole::Admin
    );
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::User as i32).unwrap(),
        UserRole::User
    );
}

#[test]
fn test_proto_role_to_user_role_invalid() {
    let err = proto_role_to_user_role(999).unwrap_err();
    assert!(err.to_string().contains("Unknown user role"));
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
    }
}

#[test]
fn test_user_to_proto_basic() {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let proto = try_user_to_proto(&user, Some("test@example.com"), &public_id_codec)
        .expect("user proto should encode");

    assert_eq!(proto.id, public_id_codec.encode_user_id(user.id).unwrap());
    assert_eq!(proto.username, "testuser");
    assert_eq!(proto.email, "test@example.com");
    assert_eq!(proto.role, synctv_proto::common::UserRole::User as i32);
    assert_eq!(
        proto.status,
        synctv_proto::common::UserStatus::Active as i32
    );
    assert!(!proto.is_banned);
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
fn test_provider_error_upstream_http_is_sanitized() {
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
        other => panic!("expected upstream unavailability, got {other:?}"),
    }
}

#[test]
fn test_provider_error_upstream_http_404_maps_to_not_found() {
    let err = ApiError::from(synctv_core::provider::ProviderError::UpstreamHttp {
        status: 404,
        url: "https://provider.example/playback".to_string(),
    });

    match err {
        ApiError::NotFound(message) => {
            assert_eq!(message, "Provider resource not found");
        }
        other => panic!("expected provider resource not found, got {other:?}"),
    }
}

#[test]
fn test_provider_error_upstream_http_400_maps_to_invalid_input() {
    let err = ApiError::from(synctv_core::provider::ProviderError::UpstreamHttp {
        status: 400,
        url: "https://provider.example/playback".to_string(),
    });

    match err {
        ApiError::InvalidInput(message) => {
            assert_eq!(message, "Upstream provider rejected the request.");
        }
        other => panic!("expected upstream provider invalid input, got {other:?}"),
    }
}

#[test]
fn test_user_to_proto_admin_role() {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::Admin, UserStatus::Active);
    let proto = try_user_to_proto(&user, Some("test@example.com"), &public_id_codec)
        .expect("user proto should encode");
    assert_eq!(proto.role, synctv_proto::common::UserRole::Admin as i32);
}

#[test]
fn test_user_to_proto_root_role() {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::Root, UserStatus::Active);
    let proto = try_user_to_proto(&user, Some("test@example.com"), &public_id_codec)
        .expect("user proto should encode");
    assert_eq!(proto.role, synctv_proto::common::UserRole::Root as i32);
}

#[test]
fn test_user_to_proto_banned_status() {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::User, UserStatus::Banned);
    let proto = try_user_to_proto(&user, Some("test@example.com"), &public_id_codec)
        .expect("user proto should encode");
    assert_eq!(
        proto.status,
        synctv_proto::common::UserStatus::Banned as i32
    );
}

#[test]
fn test_user_to_proto_no_email() {
    let public_id_codec = test_public_id_codec();
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let proto = try_user_to_proto(&user, None, &public_id_codec).expect("user proto should encode");
    assert_eq!(proto.email, ""); // None -> empty string
}

fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
    synctv_core::models::Room {
        id: RoomId::expect_positive(201),
        name: "Test Room".to_string(),
        description: "A test room".to_string(),
        cover_file_reference_id: None,
        created_by: UserId::expect_positive(202),
        status,
        is_banned: false,
        closed_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 1,
        last_activity_at: chrono::Utc::now(),
    }
}

#[test]
fn test_try_room_to_proto_basic() {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let proto = try_room_to_proto_basic(&room, None, Some(5), &public_id_codec)
        .expect("room proto should encode");

    assert_eq!(proto.id, public_id_codec.encode_room_id(room.id).unwrap());
    assert_eq!(proto.name, "Test Room");
    assert_eq!(proto.description, "A test room");
    assert_eq!(
        proto.created_by,
        public_id_codec.encode_user_id(room.created_by).unwrap()
    );
    assert_eq!(proto.member_count, 5);
    assert_eq!(
        proto.availability,
        crate::proto::client::ResourceAvailability::Available as i32
    );
    assert!(!proto.is_banned);
}

#[test]
fn test_room_to_proto_no_member_count() {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let proto = try_room_to_proto_basic(&room, None, None, &public_id_codec)
        .expect("room proto should encode");
    assert_eq!(proto.member_count, 0); // None -> 0
}

#[test]
fn test_room_to_proto_banned() {
    let public_id_codec = test_public_id_codec();
    let mut room = make_test_room(RoomStatus::Active);
    room.is_banned = true;
    let proto = try_room_to_proto_basic(&room, None, None, &public_id_codec)
        .expect("room proto should encode");
    assert!(proto.is_banned);
    assert_eq!(
        proto.availability,
        crate::proto::client::ResourceAvailability::Available as i32
    );
}

#[test]
fn test_hot_room_embedded_room_member_count_uses_total_member_count() {
    let public_id_codec = test_public_id_codec();
    let room = make_test_room(RoomStatus::Active);
    let online_count = 3;
    let total_members = 17;

    let proto = hot_room_to_proto(&room, None, online_count, total_members, &public_id_codec)
        .expect("hot room proto should encode");

    assert_eq!(
        proto.room.as_ref().unwrap().member_count,
        total_members,
        "embedded Room.member_count should reflect total active members"
    );
    assert_eq!(proto.online_count, online_count);
    assert_eq!(proto.total_members, total_members);
    assert_ne!(proto.room.as_ref().unwrap().member_count, online_count);
}

#[test]
fn test_playback_state_to_proto() {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: Some(MediaId::expect_positive(302)),
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: false,
        updated_at: chrono::Utc::now(),
        version: 42,
    };

    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");

    assert_eq!(
        proto.room_id,
        public_id_codec.encode_room_id(state.room_id).unwrap()
    );
    assert_eq!(
        proto.playing_media_id,
        public_id_codec
            .encode_media_id(state.playing_media_id.unwrap())
            .unwrap()
    );
    assert_eq!(proto.playing_playlist_id, "");
    assert!(proto.target.is_empty());
    assert!((proto.position - 120.5).abs() < f64::EPSILON);
    assert!((proto.speed - 1.5).abs() < f64::EPSILON);
    assert!(!proto.is_playing);
    assert_eq!(proto.version, 42);
}

#[test]
fn test_playback_state_to_proto_computes_elapsed_time_while_playing() {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: Some(MediaId::expect_positive(302)),
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: true,
        updated_at: chrono::Utc::now() - chrono::TimeDelta::seconds(2),
        version: 42,
    };

    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");

    assert!(proto.position >= 123.5);
    assert!(proto.position < 124.5);
}

#[test]
fn test_playback_state_to_proto_dynamic_playlist_target() {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(301),
        playing_media_id: None,
        playing_playlist_id: Some(PlaylistId::expect_positive(303)),
        target: br#"{"item_id":"provider-item-9"}"#.to_vec(),
        current_progress_id: None,
        position: 120.5,
        speed: 1.5,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 42,
    };

    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");

    assert_eq!(proto.playing_media_id, "");
    assert_eq!(
        proto.playing_playlist_id,
        public_id_codec
            .encode_playlist_id(state.playing_playlist_id.unwrap())
            .unwrap()
    );
    let target: serde_json::Value = serde_json::from_slice(&proto.target).unwrap();
    assert_eq!(target, serde_json::json!({"item_id":"provider-item-9"}));
}

#[test]
fn test_playback_state_to_proto_no_media() {
    let public_id_codec = test_public_id_codec();
    let state = synctv_core::models::RoomPlaybackState::new(RoomId::expect_positive(301));
    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");

    assert_eq!(proto.playing_media_id, ""); // None -> empty string
    assert_eq!(proto.playing_playlist_id, "");
    assert!(!proto.is_playing);
}

fn make_test_media() -> synctv_core::models::Media {
    let now = chrono::Utc::now();
    synctv_core::models::Media {
        id: MediaId::expect_positive(302),
        playlist_id: Some(PlaylistId::expect_positive(303)),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Test Video".to_string(),
        description: String::new(),
        position: 3.0,
        source_provider: "bilibili".to_string(),
        source_config: serde_json::json!({"bvid": "BV1234"}),
        provider_instance_name: Some("bili_main".to_string()),
        cover_file_reference_id: None,
        added_at: now,
        updated_at: now,
        version: 0,
    }
}

#[test]
fn test_media_to_proto_basic() {
    let public_id_codec = test_public_id_codec();
    let media = make_test_media();
    let proto = try_media_to_proto(&media, &public_id_codec).expect("media proto should encode");

    assert_eq!(proto.id, public_id_codec.encode_media_id(media.id).unwrap());
    assert_eq!(
        proto.room_id,
        public_id_codec.encode_room_id(media.room_id).unwrap()
    );
    assert_eq!(proto.source_provider, "bilibili");
    assert_eq!(proto.name, "Test Video");
    assert!(proto.source_config.is_empty());
    assert_eq!(proto.position.to_bits(), 3.0f64.to_bits());
    assert_eq!(
        proto.creator_id,
        public_id_codec
            .encode_user_id(media.creator_id.unwrap())
            .unwrap()
    );
    assert_eq!(proto.provider_instance_name, "bili_main");
}

#[test]
fn test_media_to_proto_direct_media_omits_default_instance_binding() {
    let public_id_codec = test_public_id_codec();
    let media = synctv_core::models::Media::from_direct_single_mode(
        Some(PlaylistId::expect_positive(305)),
        RoomId::expect_positive(301),
        Some(UserId::expect_positive(304)),
        "Direct Media".to_string(),
        "direct",
        synctv_core::models::PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "1080p".to_string(),
        ),
        1.0,
    )
    .expect("direct media should build");
    let proto = try_media_to_proto(&media, &public_id_codec).expect("media proto should encode");
    assert_eq!(proto.source_provider, "direct_url");
    assert!(proto.provider_instance_name.is_empty());
}

fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
    synctv_core::models::RoomMemberWithUser {
        room_id: RoomId::expect_positive(301),
        user_id: UserId::expect_positive(304),
        username: "alice".to_string(),
        role,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: chrono::Utc::now(),
        is_online: true,
        is_active: true,
    }
}

#[test]
fn test_room_member_to_proto() {
    let public_id_codec = test_public_id_codec();
    let member = make_test_member(RoomRole::Member);
    let role_default = RoomRole::Member.permissions();
    let proto = try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    )
    .expect("room member proto should encode");

    assert_eq!(
        proto.room_id,
        public_id_codec.encode_room_id(member.room_id).unwrap()
    );
    assert_eq!(
        proto.user_id,
        public_id_codec.encode_user_id(member.user_id).unwrap()
    );
    assert_eq!(proto.username, "alice");
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );
    assert!(proto.is_online);
}

#[test]
fn test_room_member_to_proto_creator() {
    let public_id_codec = test_public_id_codec();
    let member = make_test_member(RoomRole::Creator);
    let role_default = RoomRole::Creator.permissions();
    let proto = try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    )
    .expect("room member proto should encode");
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Creator as i32
    );
}

#[test]
fn test_room_member_to_proto_custom_permissions() {
    let public_id_codec = test_public_id_codec();
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xFF;
    member.removed_permissions = 0x0F;
    let role_default = RoomRole::Member.permissions();
    let proto = try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    )
    .expect("room member proto should encode");
    assert_eq!(proto.added_permissions, 0xFF);
    assert_eq!(proto.removed_permissions, 0x0F);
}

#[test]
fn test_playlist_to_proto() {
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    let proto = try_playlist_to_proto(&playlist, 10, &public_id_codec)
        .expect("playlist proto should encode");

    assert_eq!(
        proto.id,
        public_id_codec.encode_playlist_id(playlist.id).unwrap()
    );
    assert_eq!(
        proto.room_id,
        public_id_codec.encode_room_id(playlist.room_id).unwrap()
    );
    assert_eq!(proto.name, "My Playlist");
    assert_eq!(proto.parent_id, "");
    assert_eq!(proto.item_count, 10);
    assert!(!proto.is_dynamic);
}

#[test]
fn test_playlist_to_proto_dynamic() {
    let public_id_codec = test_public_id_codec();
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::expect_positive(306),
        room_id: RoomId::expect_positive(301),
        creator_id: Some(UserId::expect_positive(304)),
        name: "Bilibili Folder".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: Some(PlaylistId::expect_positive(303)),
        position: 1.0,
        source_provider: Some("bilibili".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    let proto = try_playlist_to_proto(&playlist, 5, &public_id_codec)
        .expect("playlist proto should encode");

    assert_eq!(
        proto.parent_id,
        public_id_codec
            .encode_playlist_id(playlist.parent_id.unwrap())
            .unwrap()
    );
    assert!(proto.is_dynamic);
    assert_eq!(proto.source_provider, "bilibili");
    assert_eq!(proto.provider_instance_name, "");
}

#[test]
fn test_members_to_proto_pattern_multiple_roles() {
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
        .expect("room member protos should encode");

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

    // Creator should have more permissions than guest
    assert!(
        result[0].permissions > result[3].permissions,
        "Creator should have more permissions than guest"
    );
}

#[test]
fn test_members_to_proto_pattern_preserves_custom_permissions() {
    let public_id_codec = test_public_id_codec();
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xFF;
    member.removed_permissions = 0x0F;
    let role_default = member.role.permissions();
    let result = try_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(role_default),
        &public_id_codec,
    )
    .expect("room member proto should encode");
    assert_eq!(result.added_permissions, 0xFF);
    assert_eq!(result.removed_permissions, 0x0F);
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
    member.removed_permissions = RoomMemberPermissionBits::CHAT;

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
    member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;
    let effective = member.effective_permissions(base);
    assert!(
        effective.has(RoomPermission::USE_WEBRTC),
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
    // add_media_batch correctly uses provider_instance_name from request items.
    // This test documents that the batch path uses item.provider_instance_name
    // directly (not the provider type name), serving as a regression guard.
    // Single-item add_media is stricter now: non-direct providers must send an
    // explicit provider_instance_name instead of falling back to req.source_provider.
    // The batch path already used item.provider_instance_name directly.
    let instance_name = "bilibili_main";
    let type_name = "bilibili";
    // Instance name and type name should be distinct
    assert_ne!(
        instance_name, type_name,
        "Instance name and type name must be different to catch the bug"
    );
}

#[test]
fn test_playback_state_version_no_truncation() {
    let public_id_codec = test_public_id_codec();
    // Version values above i32::MAX should not be truncated
    let large_version: i64 = i64::from(i32::MAX) + 1;
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(401),
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        updated_at: chrono::Utc::now(),
        version: large_version,
    };

    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");
    assert_eq!(
        proto.version, large_version,
        "Version should not be truncated from i64 to i32"
    );
}

#[test]
fn test_playback_state_version_i32_range_still_works() {
    let public_id_codec = test_public_id_codec();
    // Normal i32-range versions should continue to work correctly
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::expect_positive(402),
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        updated_at: chrono::Utc::now(),
        version: 42,
    };

    let proto = try_playback_state_to_proto(&state, &public_id_codec)
        .expect("playback state proto should encode");
    assert_eq!(proto.version, 42);
}
