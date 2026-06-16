use super::{
    apply_global_layers, build_app_state, build_cors_layer, optional_header_str,
    register_all_routes, required_header_str, start_proxy_cache_lifecycle, RouterConfig,
};
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::{routing::get, Router};
use bytes::Bytes;
use http_body_util::BodyExt as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use synctv_core::cache::{KeyBuilder, UsernameCache};
use synctv_core::provider::ProviderSet;
use synctv_core::proxy_signature::ProxySigningKey;
use synctv_core::service::{
    AuditService, ContentFilter, InMemoryTokenBlacklistStore, ProvidersManager, RateLimitConfig,
    RateLimiter, RoomService, UserService,
};
use synctv_proxy::slice_cache::{SliceCache, SliceCacheBackend, SliceCacheConfig, StoredEntry};
use tower::ServiceExt;

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn app_ok<T>(result: Result<T, super::AppError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn app_err<T>(result: Result<T, super::AppError>) -> TestResult<super::AppError> {
    match result {
        Ok(_) => Err(test_error("expected HTTP app error")),
        Err(error) => Ok(error),
    }
}

fn test_request(result: Result<Request<Body>, axum::http::Error>) -> TestResult<Request<Body>> {
    result.map_err(|error| test_error(error.to_string()))
}

fn test_response<E>(result: Result<Response<Body>, E>) -> TestResult<Response<Body>>
where
    E: std::fmt::Display,
{
    result.map_err(|error| test_error(error.to_string()))
}

fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
    result.map_err(|error| test_error(error.to_string()))
}

fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn test_fixture<T, E>(result: Result<T, E>) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("test fixture setup failed: {error}")),
    }
}

#[test]
fn optional_file_range_parses_standard_byte_ranges() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=10-19"));
    let range = app_ok(super::optional_file_range(&headers))?;
    assert!(matches!(
        range,
        Some(synctv_core::models::FileRangeRequest::Exact(
            synctv_core::models::FileByteRange {
                start: 10,
                end_inclusive: 19,
            },
        ))
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=10-"));
    assert!(matches!(
        app_ok(super::optional_file_range(&headers))?,
        Some(synctv_core::models::FileRangeRequest::From { start: 10 })
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=-20"));
    assert!(matches!(
        app_ok(super::optional_file_range(&headers))?,
        Some(synctv_core::models::FileRangeRequest::Suffix { length: 20 })
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1,2-3"));
    let error = app_err(super::optional_file_range(&headers))?;
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn file_blob_response_sets_partial_content_headers() -> TestResult {
    let blob = synctv_core::models::FileBlob {
        storage_backend: "database".to_string(),
        object_key: "object".to_string(),
        mime_type: "text/plain".to_string(),
        size_bytes: 4,
        total_size_bytes: 10,
        content_manifest_sha256: "a".repeat(64),
        compression: synctv_core::models::FileBlobCompression::None,
        range: Some(synctv_core::models::FileByteRange {
            start: 2,
            end_inclusive: 5,
        }),
        data: b"cdef".to_vec(),
        metadata: serde_json::Value::Object(Default::default()),
        created_at: chrono::Utc::now(),
    };
    let response = app_ok(super::file_blob_response(blob, Some("private, max-age=1")))?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE),
        Some(&HeaderValue::from_static("bytes 2-5/10"))
    );
    assert_eq!(
        response.headers().get(header::ACCEPT_RANGES),
        Some(&HeaderValue::from_static("bytes"))
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, max-age=1"))
    );
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| test_error(error.to_string()))?
        .to_bytes();
    assert_eq!(body, Bytes::from_static(b"cdef"));
    Ok(())
}

fn http_test_database() -> synctv_core_testing::TestDatabase {
    let handle = std::thread::spawn(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                std::panic::panic_any(format!("test database runtime should build: {error}"))
            }
        };
        runtime.block_on(synctv_core_testing::create_test_database_with_db_and_label(
            "http_test",
            "http_test",
        ))
    });
    match handle.join() {
        Ok(database) => database,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[test]
fn required_header_str_rejects_missing_header() -> TestResult {
    let headers = HeaderMap::new();

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "missing token");
    Ok(())
}

#[test]
fn required_header_str_rejects_blank_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("x-upload-token", HeaderValue::from_static("   "));

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "missing token");
    Ok(())
}

#[test]
fn required_header_str_rejects_non_utf8_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("x-upload-token", HeaderValue::from_bytes(&[0xff])?);

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("x-upload-token"));
    Ok(())
}

#[test]
fn optional_header_str_rejects_non_utf8_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_bytes(&[0xff])?,
    );

    let error = app_err(optional_header_str(
        &headers,
        &axum::http::header::CONTENT_TYPE,
    ))?;

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("content-type"));
    Ok(())
}

#[test]
fn forwarded_proto_is_https_accepts_trusted_proxy_https() -> TestResult {
    let mut server = synctv_core::config::ServerConfig::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let result = app_ok(super::forwarded_proto_is_https(
        &server,
        &headers,
        Some("10.1.2.3".parse()?),
    ))?;

    assert!(result);
    Ok(())
}

#[test]
fn forwarded_proto_is_https_ignores_untrusted_peer() -> TestResult {
    let mut server = synctv_core::config::ServerConfig::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let result = super::forwarded_proto_is_https(&server, &headers, Some("192.168.1.10".parse()?))?;

    assert!(!result);
    Ok(())
}

#[test]
fn forwarded_proto_is_https_rejects_non_utf8_from_trusted_proxy() -> TestResult {
    let mut server = synctv_core::config::ServerConfig::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_bytes(&[0xff])?);

    let error = app_err(super::forwarded_proto_is_https(
        &server,
        &headers,
        Some("10.1.2.3".parse()?),
    ))?;

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("x-forwarded-proto"));
    Ok(())
}

#[test]
fn test_path_injected_json_proto_requests_deserialize_without_injected_fields() -> TestResult {
    let join_room: synctv_proto::client::JoinRoomRequest = serde_json::from_str(r"{}")?;
    assert!(join_room.room_id.is_empty());

    let room_password_login: synctv_proto::client::StartRoomPasswordLoginRequest =
        serde_json::from_str(r#"{"credential_request":"AQID"}"#)?;
    assert!(room_password_login.room_id.is_empty());
    assert_eq!(room_password_login.credential_request, vec![1, 2, 3]);

    let edit_media: synctv_proto::client::EditMediaRequest =
        serde_json::from_str(r#"{"name":"Episode 1"}"#)?;
    assert_eq!(edit_media.name, "Episode 1");
    assert!(edit_media.media_id.is_empty());

    let update_playlist: synctv_proto::client::UpdatePlaylistRequest =
        serde_json::from_str(r#"{"name":"Season 1"}"#)?;
    assert_eq!(update_playlist.name, "Season 1");
    assert!(update_playlist.playlist_id.is_empty());

    let move_playlist: synctv_proto::client::MovePlaylistRequest =
        serde_json::from_str(r#"{"after_playlist_id":"pl_anchor123"}"#)?;
    assert!(move_playlist.playlist_id.is_empty());
    assert!(matches!(
        move_playlist.anchor,
        Some(synctv_proto::client::move_playlist_request::Anchor::AfterPlaylistId(
            ref id
        )) if id == "pl_anchor123"
    ));

    let member_permissions: synctv_proto::client::UpdateMemberPermissionsRequest =
        serde_json::from_str(r#"{"role":2,"added_permissions":1}"#)?;
    assert!(member_permissions.user_id.is_empty());
    assert_eq!(member_permissions.role, 2);
    assert_eq!(member_permissions.added_permissions, 1);

    let delete_passkey: synctv_proto::client::DeletePasskeyRequest =
        serde_json::from_str(r#"{"verification_id":"verify_123"}"#)?;
    assert!(delete_passkey.credential_id.is_empty());
    assert_eq!(delete_passkey.verification_id, "verify_123");
    Ok(())
}

#[test]
fn test_admin_path_injected_json_proto_requests_deserialize_without_injected_fields() -> TestResult
{
    let user_preferences: synctv_proto::admin::UpdateUserPreferencesRequest =
        serde_json::from_str(r#"{"two_factor_enabled":true}"#)?;
    assert!(user_preferences.user_id.is_empty());
    assert_eq!(user_preferences.two_factor_enabled, Some(true));

    let user_role: synctv_proto::admin::UpdateUserRoleRequest =
        serde_json::from_str(r#"{"role":1}"#)?;
    assert!(user_role.user_id.is_empty());
    assert_eq!(user_role.role, 1);

    let user_password: synctv_proto::admin::SetUserPasswordRequest =
        serde_json::from_str(r#"{"password":"NewPassword123!","reason":"support reset"}"#)?;
    assert!(user_password.user_id.is_empty());
    assert_eq!(user_password.password, "NewPassword123!");
    assert_eq!(user_password.reason, "support reset");

    let user_username: synctv_proto::admin::UpdateUserUsernameRequest =
        serde_json::from_str(r#"{"new_username":"new_admin_name"}"#)?;
    assert!(user_username.user_id.is_empty());
    assert_eq!(user_username.new_username, "new_admin_name");

    let ban_user: synctv_proto::admin::BanUserRequest =
        serde_json::from_str(r#"{"reason":"spam"}"#)?;
    assert!(ban_user.user_id.is_empty());
    assert_eq!(ban_user.reason, "spam");

    let room_password: synctv_proto::admin::UpdateRoomPasswordRequest =
        serde_json::from_str(r#"{"new_password":""}"#)?;
    assert!(room_password.room_id.is_empty());
    assert!(room_password.new_password.is_empty());

    let ban_room: synctv_proto::admin::BanRoomRequest =
        serde_json::from_str(r#"{"reason":"abuse"}"#)?;
    assert!(ban_room.room_id.is_empty());
    assert_eq!(ban_room.reason, "abuse");

    let room_settings: synctv_proto::admin::UpdateRoomSettingsRequest =
        serde_json::from_str(r#"{"settings":{"room":"settings"}}"#)?;
    let settings: serde_json::Value = serde_json::from_slice(&room_settings.settings)?;
    assert_eq!(settings, serde_json::json!({"room":"settings"}));
    Ok(())
}

#[test]
fn test_provider_path_injected_json_proto_requests_deserialize_without_injected_fields(
) -> TestResult {
    let update_provider: synctv_proto::providers::common::UpdateProviderInstanceRequest =
        serde_json::from_str(r#"{"endpoint":"https://provider.internal","providers":["alist"]}"#)?;

    assert_eq!(
        update_provider.endpoint.as_deref(),
        Some("https://provider.internal")
    );
    assert_eq!(update_provider.providers, vec!["alist".to_string()]);
    Ok(())
}

pub(crate) fn test_app_state() -> super::AppState {
    test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig::default())
}

fn test_app_state_with_rate_limits(
    request_rate_limits: synctv_core::RequestRateLimitConfig,
) -> super::AppState {
    let database = http_test_database();
    let pool = database.pool.clone();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        test_fixture(synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )),
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
    ));
    let room_service = Arc::new(test_fixture(RoomService::new_for_tests(
        pool.clone(),
        (*user_service).clone(),
    )));
    let provider_instance_manager = synctv_core_testing::create_empty_provider_instance_manager();
    let providers_manager = Arc::new(test_fixture(ProvidersManager::new(
        provider_instance_manager.clone(),
    )));
    let providers = ProviderSet::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    );
    let providers = test_fixture(providers);
    let jwt_service = test_fixture(synctv_core::service::JwtService::new(
        "test-secret-key-for-http-router-tests-minimum-32-chars",
    ));
    let (audit_service, _audit_handle) = AuditService::new(pool.clone());
    let config = synctv_core::Config {
        request_rate_limits,
        ..synctv_core::Config::default()
    };
    let router_config = RouterConfig {
        config: Arc::new(config),
        user_cache: Arc::new(synctv_core::cache::UserCache::local_only(
            128,
            60,
            300,
            "test:user:".to_string(),
        )),
        user_service,
        room_service,
        content_filter: ContentFilter::new(),
        provider_instance_manager,
        user_provider_credential_repository: Arc::new(
            synctv_core::repository::UserProviderCredentialRepository::new(pool),
        ),
        providers,
        event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
        connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        )),
        presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
        jwt_service,
        realtime_fanout_service: crate::realtime_fanout::disabled_realtime_fanout_service(),
        oauth2_service: None,
        passkey_service: None,
        settings_service: None,
        settings_registry: None,
        email_service: None,
        email_token_service: None,
        publish_key_service: None,
        notification_service: None,
        chat_service: None,
        audit_service: Arc::new(audit_service),
        live_streaming_infrastructure: None,
        rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
        ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
        redis_runtime: None,
        shared_provider_stores: Arc::new(
            synctv_core::provider::store::ProviderStoreRegistry::local_only("test:provider:"),
        ),
        shared_proxy_signing_key: Arc::new(
            synctv_core::proxy_signature::ProxySigningKey::try_derive_from(
                b"test-proxy-signing-key-minimum-32-bytes!!",
            )
            .expect("test proxy signing key should derive"),
        ),
        builtin_stun_url: None,
        webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
        credential_encryption: None,
        ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
        proxy_slice_cache: Arc::new(test_fixture(SliceCache::new(SliceCacheConfig::default()))),
        proxy_http_client: test_fixture(synctv_proxy::build_proxy_http_client(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )),
        messaging_rate_limit_config: RateLimitConfig::default(),
        heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
        providers_manager,
    };
    test_fixture(build_app_state(router_config)).with_test_database_leases(vec![database])
}

async fn test_app_state_with_websocket_runtime(
    request_rate_limits: synctv_core::RequestRateLimitConfig,
) -> super::AppState {
    let state = test_app_state_with_rate_limits(request_rate_limits);
    let mut router_config = state.router_config.as_ref().clone();
    let database = http_test_database();
    let pool = database.pool.clone();

    let room_settings_service = synctv_core::service::RoomSettingsService::new(
        synctv_core::repository::RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(synctv_core::service::NotificationService::default()),
        None,
        None,
    );
    let chat_service = synctv_core::service::ChatService::new(
        Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
        synctv_core::service::chat::ChatRuntime {
            rate_limiter: router_config.rate_limiter.clone(),
            rate_limit_config: state
                .shared_api_runtime
                .messaging_rate_limit_config
                .as_ref()
                .clone(),
            content_filter: state.shared_api_runtime.content_filter.as_ref().clone(),
        },
        synctv_core::service::chat::ChatDependencies {
            permission_service: router_config.room_service.permission_service().clone(),
            room_settings_service,
            user_service: router_config.user_service.clone(),
            file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
            audit_service: None,
            notification_service: synctv_core::service::NotificationService::default(),
        },
    );
    router_config.chat_service = Some(Arc::new(chat_service));
    let realtime_manager =
        synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_secs(30),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await;
    let realtime_manager = Arc::new(test_fixture(realtime_manager));
    router_config.event_service = realtime_manager;
    test_fixture(build_app_state(router_config))
        .with_shared_test_database_leases(state.test_database_leases())
        .with_added_test_database_lease(database)
}

async fn test_app_state_with_real_chat_runtime(pool: sqlx::PgPool) -> super::AppState {
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig::default());
    let mut router_config = state.router_config.as_ref().clone();

    let username_cache = UsernameCache::local_only("test:http-chat:username:".to_string(), 128, 60);
    let user_service = UserService::new_for_tests(
        &pool,
        router_config.jwt_service.clone(),
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test:http-chat"),
        synctv_core::service::BruteForceProtection::in_memory("test:http-chat:auth".to_string()),
    );
    let user_service = Arc::new(user_service);

    let room_service = test_fixture(RoomService::new_for_tests(
        pool.clone(),
        (*user_service).clone(),
    ));
    let room_service = Arc::new(room_service);

    let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
    let permission_service = synctv_core::service::PermissionService::new_with_runtime(
        synctv_core::repository::RoomMemberRepository::new(pool.clone()),
        synctv_core::repository::RoomRepository::new(pool.clone()),
        synctv_core::service::permission::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::permission::PermissionServiceRuntime::local_only()
        },
    );
    let permission_service = test_fixture(permission_service);
    let notification_service = synctv_core::service::NotificationService::default();
    let room_settings_service = synctv_core::service::RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
    );
    let chat_service = synctv_core::service::ChatService::new(
        Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
        synctv_core::service::chat::ChatRuntime {
            rate_limiter: Arc::new(RateLimiter::local_only("test:http-chat:".to_string())),
            rate_limit_config: RateLimitConfig::default(),
            content_filter: ContentFilter::new(),
        },
        synctv_core::service::chat::ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: Arc::clone(&user_service),
            file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
            audit_service: None,
            notification_service,
        },
    );

    let realtime_manager =
        synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-http-chat-node".to_string(),
            dedup_window: Duration::from_secs(30),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:http-chat:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await;
    let realtime_manager = Arc::new(test_fixture(realtime_manager));

    router_config.user_service = user_service;
    router_config.room_service = room_service;
    router_config.chat_service = Some(Arc::new(chat_service));
    router_config.event_service = realtime_manager;
    router_config.connection_manager = Arc::new(synctv_realtime::sync::ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));
    router_config.audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    router_config.user_provider_credential_repository =
        Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool));

    test_fixture(build_app_state(router_config))
}

#[tokio::test]
async fn test_start_proxy_cache_lifecycle_evicts_expired_entries_and_stops_on_cancel() -> TestResult
{
    let cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            eviction_interval: Duration::from_millis(20),
            max_cache_size: 1024,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );
    let key = "expired-slice".to_string();
    cache
        .backend()
        .put(
            &key,
            StoredEntry {
                data: Bytes::from_static(b"stale"),
                inserted_at: SystemTime::now() - Duration::from_secs(2),
                ttl: Duration::from_millis(5),
                last_accessed: SystemTime::now() - Duration::from_secs(2),
            },
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let lifecycle = start_proxy_cache_lifecycle(&cache);

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if cache.backend().get(&key).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    lifecycle.cancel.cancel();

    tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
        .await
        .map_err(|error| test_error(error.to_string()))?
        .map_err(|error| test_error(error.to_string()))?;
    Ok(())
}

#[tokio::test]
async fn test_start_proxy_cache_lifecycle_starts_even_when_runtime_toggle_is_off() -> TestResult {
    let cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );

    let lifecycle = start_proxy_cache_lifecycle(&cache);
    lifecycle.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
        .await
        .map_err(|error| test_error(error.to_string()))?
        .map_err(|error| test_error(error.to_string()))?;
    Ok(())
}

#[tokio::test]
async fn test_build_app_state_reuses_injected_proxy_cache() -> TestResult {
    let database = http_test_database();
    let pool = database.pool.clone();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .map_err(|error| test_error(error.to_string()))?,
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
    ));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .map_err(|error| test_error(error.to_string()))?,
    );
    let provider_instance_manager = synctv_core_testing::create_empty_provider_instance_manager();
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager.clone())
            .map_err(|error| test_error(error.to_string()))?,
    );
    let providers = ProviderSet::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    )
    .map_err(|error| test_error(error.to_string()))?;
    let jwt_service = synctv_core::service::JwtService::new(
        "test-secret-key-for-http-router-tests-minimum-32-chars",
    )
    .map_err(|error| test_error(error.to_string()))?;
    let (audit_service, _audit_handle) = AuditService::new(pool.clone());
    let injected_cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );
    let injected_provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver> =
        Arc::new(synctv_core::provider::store::ProviderStoreRegistry::local_only("shared:test:"));
    let injected_proxy_signing_key = Arc::new(
        ProxySigningKey::try_derive_from(b"test-secret-key-for-http-router-tests-minimum-32-chars")
            .map_err(|error| test_error(error.to_string()))?,
    );
    let injected_proxy_http_client =
        synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
            .map_err(|error| test_error(error.to_string()))?;

    let state = build_app_state(RouterConfig {
        config: Arc::new(synctv_core::Config::default()),
        user_service,
        user_cache: Arc::new(synctv_core::cache::UserCache::local_only(
            128,
            60,
            300,
            "test:user:".to_string(),
        )),
        room_service,
        content_filter: ContentFilter::new(),
        provider_instance_manager,
        user_provider_credential_repository: Arc::new(
            synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()),
        ),
        providers,
        event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
        connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        )),
        presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
        jwt_service,
        realtime_fanout_service: crate::realtime_fanout::disabled_realtime_fanout_service(),
        oauth2_service: None,
        passkey_service: None,
        settings_service: None,
        settings_registry: None,
        email_service: None,
        email_token_service: None,
        publish_key_service: None,
        notification_service: None,
        chat_service: None,
        audit_service: Arc::new(audit_service),
        live_streaming_infrastructure: None,
        rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
        ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
        redis_runtime: None,
        shared_provider_stores: injected_provider_stores.clone(),
        shared_proxy_signing_key: injected_proxy_signing_key.clone(),
        builtin_stun_url: None,
        webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
        credential_encryption: None,
        proxy_slice_cache: injected_cache.clone(),
        ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
        proxy_http_client: injected_proxy_http_client,
        messaging_rate_limit_config: RateLimitConfig::default(),
        heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
        providers_manager,
    })?
    .with_test_database_leases(vec![database]);

    assert!(
        Arc::ptr_eq(&state.proxy_slice_cache, &injected_cache),
        "AppState must reuse the injected proxy slice cache instead of creating a hidden default instance"
    );
    assert!(
        !state.proxy_slice_cache.config().enabled,
        "The injected cache configuration must be preserved"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.provider_stores,
            &injected_provider_stores
        ),
        "AppState must reuse the injected provider store registry"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.proxy_signing_key,
            &injected_proxy_signing_key
        ),
        "AppState must reuse the injected proxy signing key"
    );
    assert!(
        state
            .proxy_http_client
            .get("https://example.com")
            .build()
            .is_ok(),
        "The injected proxy HTTP client must remain usable in AppState"
    );
    assert!(
        state.shared_api_runtime.security_pipeline.has_user_cache(),
        "AppState security pipeline should carry the shared user cache"
    );
    Ok(())
}

#[tokio::test]
async fn test_playback_patch_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room_123/playback")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":1}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "PATCH playback route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "PATCH playback route must accept PATCH requests"
    );
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "request should reach the registered route and follow the normal auth path; playback PATCH is not gated by websocket-runtime-only middleware"
    );
    Ok(())
}

#[tokio::test]
async fn test_chat_message_patch_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room_123/chat/messages/msg_456")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"content":"edited","expected_version":"1"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "chat message PATCH route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "chat message PATCH route must accept PATCH requests"
    );
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
        ),
        "request should reach the registered route and be handled by the normal request pipeline"
    );
    Ok(())
}

#[tokio::test]
async fn test_chat_message_delete_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/rooms/room_123/chat/messages/msg_456")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"expected_version":"1","reason":"cleanup"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "chat message DELETE route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "chat message DELETE route must accept DELETE requests"
    );
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
        ),
        "request should reach the registered route and be handled by the normal request pipeline"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_receives_live_send_event() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = chrono::Utc::now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_live_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Live Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());
    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await?
        .ok_or_else(|| test_error("SSE stream ended before initial frame"))?
        .map_err(|error| test_error(error.to_string()))?;
    let mut rendered = String::new();
    if let Some(data) = first_frame.data_ref() {
        rendered.push_str(std::str::from_utf8(data)?);
    }
    assert!(rendered.contains("event: observed\n"));

    let sent = api_ok(
        state
            .shared_api_runtime
            .client_api
            .send_chat_message_for_actor(
                &crate::impls::client::RoomActor::User {
                    room_id: room.id,
                    user_id: owner.id,
                },
                synctv_proto::client::SendChatMessageRequest {
                    client_message_id: "http-sse-live-send-1".to_string(),
                    content: "live push event".to_string(),
                    metadata: br"{}".to_vec(),
                    ..Default::default()
                },
            )
            .await,
    )?
    .event
    .ok_or_else(|| test_error("chat send should return event"))?;

    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await?
            .ok_or_else(|| test_error("SSE stream ended before expected frame"))?
            .map_err(|error| test_error(error.to_string()))?;
        if let Some(data) = frame.data_ref() {
            rendered.push_str(std::str::from_utf8(data)?);
        }
        if rendered.contains("live push event") {
            break;
        }
    }

    assert!(rendered.contains("event: changed\n"));
    assert!(rendered.contains(&format!("id: {}\n", sent.sequence)));
    assert!(rendered.contains("live push event"));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_replays_after_last_event_id_header() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = chrono::Utc::now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Replay Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let chat_service = state
        .chat_service
        .as_ref()
        .ok_or_else(|| test_error("chat service should be present"))?
        .clone();

    let first = core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-1".to_string()),
                content: "first replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;
    core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-2".to_string()),
                content: "second replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;
    core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-3".to_string()),
                content: "third replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;

    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());
    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header("last-event-id", first.sequence.to_string())
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut rendered = String::new();
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await?
            .ok_or_else(|| test_error("SSE stream ended before expected frame"))?
            .map_err(|error| test_error(error.to_string()))?;
        if let Some(data) = frame.data_ref() {
            rendered.push_str(std::str::from_utf8(data)?);
        }
        if rendered.contains("second replay") && rendered.contains("third replay") {
            break;
        }
    }

    assert!(rendered.contains("event: observed\n"));
    assert!(rendered.contains("event: changed\n"));
    assert!(rendered.contains("id: "));
    assert!(rendered.contains("second replay"));
    assert!(rendered.contains("third replay"));
    assert!(
        !rendered.contains("first replay"),
        "Last-Event-ID should replay events strictly after the supplied sequence"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_unknown_last_event_id_returns_bad_request() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = chrono::Utc::now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_bad_cursor_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Bad Cursor Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header("last-event-id", "missing-chat-sequence")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_public_rooms_route_is_reachable_without_auth() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms?page=1&page_size=10")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "public room listing must not require auth"
    );
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "public room listing route must be registered"
    );
    Ok(())
}

#[tokio::test]
async fn test_opaque_login_routes_are_registered() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for uri in [
        "/api/auth/opaque/login/start",
        "/api/auth/opaque/login/finish",
    ] {
        let request = test_request(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from("{")),
        )?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept POST"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_direct_password_and_email_registration_routes_are_registered() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for uri in [
        "/api/auth/direct-password/register",
        "/api/auth/direct-password/login",
        "/api/auth/email/registration/request",
        "/api/auth/email/registration/confirm",
    ] {
        let request = test_request(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from("{")),
        )?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept POST"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_passkey_login_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let start_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkeys/login/start")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r"{}")),
    )?;
    let start_response = test_response(app.clone().oneshot(start_request).await)?;
    assert_eq!(start_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let finish_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkeys/login/finish")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"}}"#,
            )),
    )?;
    let finish_response = test_response(app.oneshot(finish_request).await)?;
    assert_eq!(finish_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_passkey_user_routes_are_registered_and_require_authentication() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for (method, uri, body) in [
        ("GET", "/api/user/preferences", None),
        (
            "PATCH",
            "/api/user/preferences",
            Some(r#"{"two_factor_enabled":true}"#),
        ),
        ("GET", "/api/user/passkeys", None),
        (
            "POST",
            "/api/user/passkeys/bind/start",
            Some(r#"{"name":"Laptop"}"#),
        ),
        (
            "POST",
            "/api/user/passkeys/bind/finish",
            Some(
                r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"},"verification_id":"verification-id"}"#,
            ),
        ),
        (
            "DELETE",
            "/api/user/passkeys/Y3JlZGVudGlhbA",
            Some(r#"{"verification_id":"verification-id"}"#),
        ),
        (
            "PUT",
            "/api/rooms/room_123/chat/messages/42/reactions/like",
            None,
        ),
        (
            "DELETE",
            "/api/rooms/room_123/chat/messages/42/reactions/like",
            None,
        ),
        (
            "GET",
            "/api/rooms/room_123/chat/messages/42/reactions/like/users",
            None,
        ),
    ] {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
        }
        let request = test_request(builder.body(body.map_or_else(Body::empty, Body::from)))?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept {method}"
        );
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} should follow the normal authenticated user route path"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_member_approval_routes_are_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for (method, uri, body) in [
        (
            "POST",
            "/api/rooms/room1234_abx/members",
            Some(r#"{"user_id":"usr_1","role":1,"notify":true}"#),
        ),
        ("GET", "/api/rooms/room1234_abx/reviews/joins", None),
        (
            "POST",
            "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/approve",
            None,
        ),
        (
            "POST",
            "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/reject",
            Some(r#"{"request_id":"usr_1","reason":"no longer eligible"}"#),
        ),
    ] {
        let builder = Request::builder().method(method).uri(uri);
        let builder = if body.is_some() {
            builder.header(axum::http::header::CONTENT_TYPE, "application/json")
        } else {
            builder
        };
        let request = test_request(builder.body(Body::from(
            body.map_or_else(String::new, ToString::to_string),
        )))?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{uri} must be registered in the project router"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept its documented HTTP method"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_oauth2_unlink_route_is_reachable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/oauth2/type/github/unlink")
            .body(Body::empty()),
    )?;
    let new_route_response = test_response(app.clone().oneshot(request).await)?;
    assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_delete_all_read_notifications_route_is_reachable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/notifications/read")
            .body(Body::empty()),
    )?;
    let new_route_response = test_response(app.clone().oneshot(request).await)?;
    assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_main_router_does_not_expose_metrics_endpoint() -> TestResult {
    let mut state = test_app_state();
    Arc::make_mut(&mut state.router_config).config = Arc::new({
        let mut config = (*state.config).clone();
        config.metrics.enabled = true;
        config.metrics.auth.mode = synctv_core::config::MetricsAuthMode::BearerToken;
        config.metrics.auth.bearer_token = "metrics-secret".to_string();
        config
    });
    let app = register_all_routes().with_state(state);

    let request = test_request(Request::builder().uri("/metrics").body(Body::empty()))?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_provider_login_routes_reject_invalid_tokens_before_rate_limiting() -> TestResult {
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
        auth_max_requests: 1,
        auth_window_seconds: 60,
        read_max_requests: 100,
        read_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "provider login routes should consume the auth rate-limit bucket before invalid-token authentication fails"
    );
    Ok(())
}

#[tokio::test]
async fn test_auth_login_malformed_json_still_consumes_auth_rate_limit_bucket() -> TestResult {
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
        auth_max_requests: 1,
        auth_window_seconds: 60,
        read_max_requests: 100,
        read_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/email/confirm")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{invalid json")),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/email/confirm")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{invalid json")),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "malformed auth payloads should still consume the auth rate-limit bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_bilibili_me_route_requires_post() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/bilibili/me")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "Bilibili /me must require POST so provider requests stay consistently structured"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_route_uses_write_rate_limit_tier() -> TestResult {
    let state = test_app_state_with_websocket_runtime(synctv_core::RequestRateLimitConfig {
        write_max_requests: 1,
        write_window_seconds: 60,
        read_max_requests: 100,
        read_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    })
    .await;
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"room_123"}"#)),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(
        first.status(),
        StatusCode::UNAUTHORIZED,
        "first unauthenticated ticket request should reach auth before exhausting the write bucket"
    );

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"room_123"}"#)),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "ticket issuance should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
    );
    Ok(())
}

#[tokio::test]
async fn test_provider_proxy_routes_use_streaming_rate_limit_tier() -> TestResult {
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
        streaming_max_requests: 1,
        streaming_window_seconds: 60,
        read_max_requests: 100,
        read_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/proxy/bilibili/v1/test.m3u8")
            .body(Body::empty()),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/proxy/bilibili/v1/test.m3u8")
            .body(Body::empty()),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "provider proxy endpoints must use the streaming rate-limit bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_transport_layers_preserve_shared_http_metadata_without_global_timeout() -> TestResult
{
    let state = test_app_state();
    let app = apply_global_layers(
        Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "completed"
            }),
        ),
        &state,
    )?;

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/slow")
            .header("x-request-id", "transport-no-timeout-123")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "transport layers should no longer enforce a path-selected unary timeout"
    );
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("transport-no-timeout-123"),
        "request IDs must still be propagated without transport timeout wrapping"
    );
    assert_eq!(
        response
            .headers()
            .get("X-Frame-Options")
            .ok_or_else(|| test_error("missing X-Frame-Options header"))?,
        "DENY",
        "shared security headers must still be applied after removing transport timeout routing"
    );
    Ok(())
}

#[tokio::test]
async fn test_streaming_proxy_routes_preserve_options_preflight() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let rtmp_request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/api/providers/proxy/rtmp/ver1/playlist.m3u8")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let rtmp_preflight = test_response(app.clone().oneshot(rtmp_request).await)?;
    assert_ne!(
        rtmp_preflight.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "RTMP proxy routes must continue handling browser preflight through the generic proxy route"
    );

    let live_proxy_request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/api/providers/proxy/live_proxy/ver1/playlist.m3u8")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let live_proxy_preflight = test_response(app.oneshot(live_proxy_request).await)?;
    assert_ne!(
        live_proxy_preflight.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "live_proxy proxy routes must continue handling browser preflight through the generic proxy route"
    );
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_does_not_advertise_credentials() -> TestResult {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert!(
        response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none(),
        "native-client-oriented CORS policy should not advertise credentialed browser requests by default"
    );
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_allows_request_correlation_headers() -> TestResult {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "x-request-id, traceparent, tracestate",
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);

    let allowed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
        .ok_or_else(|| test_error("preflight should advertise allowed headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(allowed_headers.contains("x-request-id"));
    assert!(allowed_headers.contains("traceparent"));
    assert!(allowed_headers.contains("tracestate"));
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_allows_upload_and_range_headers() -> TestResult {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "PUT")
            .header(
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "content-range, range, x-synctv-file-upload-token",
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let allowed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
        .ok_or_else(|| test_error("preflight should advertise upload headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(allowed_headers.contains("content-range"));
    assert!(allowed_headers.contains("range"));
    assert!(allowed_headers.contains("x-synctv-file-upload-token"));
    Ok(())
}

#[tokio::test]
async fn test_cors_actual_response_exposes_request_id_header() -> TestResult {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let exposed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .ok_or_else(|| {
            test_error("CORS response should expose request correlation response headers")
        })?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(exposed_headers.contains("x-request-id"));
    Ok(())
}

#[tokio::test]
async fn test_cors_actual_response_exposes_upload_and_range_headers() -> TestResult {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let exposed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .ok_or_else(|| test_error("CORS response should expose upload and range headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(exposed_headers.contains("x-synctv-upload-complete"));
    assert!(exposed_headers.contains("x-synctv-uploaded-size-bytes"));
    assert!(exposed_headers.contains("x-synctv-uploaded-parts"));
    assert!(exposed_headers.contains("content-range"));
    assert!(exposed_headers.contains("accept-ranges"));
    assert!(exposed_headers.contains("x-synctv-content-manifest-sha256"));
    Ok(())
}

#[cfg(feature = "openapi")]
#[tokio::test]
async fn test_openapi_json_route_is_available() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .uri("/api-docs/openapi.json")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["openapi"], "3.1.0");
    assert!(json["paths"]["/api/auth/email/confirm"].is_object());
    assert!(json["paths"]["/api/auth/direct-password/register"].is_object());
    assert!(json["paths"]["/api/auth/direct-password/login"].is_object());
    assert!(json["paths"]["/api/auth/email/registration/request"].is_object());
    assert!(json["paths"]["/api/auth/email/registration/confirm"].is_object());
    assert!(json["paths"]["/api/tickets"].is_object());
    assert!(json["paths"]["/api/user"].is_object());
    assert!(json["paths"]["/api/rooms/{room_id}/media"].is_object());
    assert!(json["paths"]["/api/admin/users"].is_object());
    assert!(json["paths"]["/api/rooms/{room_id}/webrtc/ice-servers"].is_object());
    assert!(json["paths"]["/api/oauth2/{provider}/exchange"].is_object());
    assert!(json["paths"]["/api/oauth2/providers"].is_object());
    assert!(json["paths"]["/api/oauth2/{provider}/authorize"].is_object());
    assert!(json["paths"]["/api/notifications"].is_object());
    assert!(json["paths"]["/api/providers/bilibili/parse"].is_object());
    assert!(json["paths"]["/api/providers/alist/login"].is_object());
    assert!(json["paths"]["/api/providers/instances"].is_object());
    assert!(json["paths"]["/api/rooms/{room_id}/streams"].is_object());
    assert!(
        json["paths"]["/api/providers/rtmp/rooms/{room_id}/publish-key/{media_id}"].is_object()
    );
    assert!(json["paths"]["/api/providers/rtmp/rooms/{room_id}/info/{media_id}"].is_object());
    assert_eq!(
        json["paths"]["/api/providers/alist/login"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_alist_LoginResponse"
    );
    assert_eq!(
        json["paths"]["/api/providers/emby/list"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_emby_ListResponse"
    );
    assert_eq!(
        json["paths"]["/api/providers/bilibili/login/qr/check"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_bilibili_QRStatusResponse"
    );
    assert_eq!(
        json["paths"]["/api/user"]["patch"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/synctv_client_SetUsernameResponse"
    );
    assert_eq!(
        json["paths"]["/api/tickets"]["post"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/synctv_client_CreateWebSocketTicketResponse"
    );
    assert_eq!(
        json["paths"]["/api/rooms/{room_id}/webrtc/ice-servers"]["get"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_client_GetIceServersResponse"
    );

    let alist_login_ref = json["paths"]["/api/providers/alist/login"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("alist login schema ref"))?;
    let auth_login_ref = json["paths"]["/api/auth/email/confirm"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("auth login schema ref"))?;
    let emby_login_ref = json["paths"]["/api/providers/emby/login"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("emby login schema ref"))?;
    assert_eq!(
        auth_login_ref,
        "#/components/schemas/synctv_client_LoginResponse"
    );
    assert_ne!(
        auth_login_ref, alist_login_ref,
        "client login and provider login must use distinct OpenAPI components"
    );
    assert_ne!(
        alist_login_ref, emby_login_ref,
        "distinct provider response types must not collapse onto the same OpenAPI component"
    );

    let alist_login_schema_name = alist_login_ref
        .rsplit('/')
        .next()
        .ok_or_else(|| test_error("alist login schema name"))?;
    let emby_login_schema_name = emby_login_ref
        .rsplit('/')
        .next()
        .ok_or_else(|| test_error("emby login schema name"))?;

    let alist_login_properties =
        &json["components"]["schemas"][alist_login_schema_name]["properties"];
    assert!(
        alist_login_properties["token"].is_object(),
        "alist login schema should expose token"
    );
    assert!(
        alist_login_properties["server_id"].is_object(),
        "alist login schema should expose server_id"
    );
    assert!(
        alist_login_properties["user_id"].is_null(),
        "alist login schema must not be overwritten by emby login response"
    );

    let emby_login_properties =
        &json["components"]["schemas"][emby_login_schema_name]["properties"];
    assert!(
        emby_login_properties["user_id"].is_object(),
        "emby login schema should expose user_id"
    );
    assert!(
        emby_login_properties["username"].is_object(),
        "emby login schema should expose username"
    );
    assert!(
        emby_login_properties["is_admin"].is_object(),
        "emby login schema should expose is_admin"
    );
    assert!(
        emby_login_properties["token"].is_null(),
        "emby login schema must not be overwritten by alist login response"
    );
    Ok(())
}

#[cfg(feature = "openapi")]
#[tokio::test]
async fn test_swagger_ui_route_is_available() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(Request::builder().uri("/swagger-ui/").body(Body::empty()))?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[test]
fn test_build_cors_layer_rejects_invalid_configured_origin() {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec![
        "https://example.com".to_string(),
        "not a valid origin".to_string(),
    ];

    let result = build_cors_layer(&config);

    assert!(
        result.is_err(),
        "invalid configured CORS origins must fail fast instead of being silently ignored"
    );
}

#[test]
fn test_build_cors_layer_rejects_configured_origin_with_path() {
    let mut config = synctv_core::Config::default();
    config.server.cors_allowed_origins = vec!["https://example.com/app".to_string()];

    let result = build_cors_layer(&config);

    assert!(
        result.is_err(),
        "configured CORS origins with paths must fail fast during router construction"
    );
}

#[tokio::test]
async fn test_provider_common_routes_rate_limit_invalid_tokens_before_authentication() -> TestResult
{
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
        admin_max_requests: 1,
        admin_window_seconds: 60,
        auth_max_requests: 100,
        auth_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(
        first.status(),
        StatusCode::UNAUTHORIZED,
        "first provider common request should still reach authentication while the admin bucket has capacity"
    );

    let second_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "provider common routes should consume the admin rate-limit bucket before invalid-token authentication fails"
    );
    Ok(())
}

#[tokio::test]
async fn test_provider_management_routes_do_not_consume_outer_read_bucket() -> TestResult {
    let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
        read_max_requests: 1,
        read_window_seconds: 60,
        auth_max_requests: 100,
        auth_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    });
    let app = register_all_routes().with_state(state);

    let management_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let management = test_response(app.clone().oneshot(management_request).await)?;
    assert_eq!(
        management.status(),
        StatusCode::UNAUTHORIZED,
        "provider auth routes should hit their own auth limiter without consuming the outer read bucket"
    );

    let common_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let common = test_response(app.oneshot(common_request).await)?;
    assert_eq!(
        common.status(),
        StatusCode::UNAUTHORIZED,
        "provider management traffic must not drain the provider-common read bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_routes_use_write_rate_limit_tier() -> TestResult {
    let state = test_app_state_with_websocket_runtime(synctv_core::RequestRateLimitConfig {
        write_max_requests: 1,
        write_window_seconds: 60,
        read_max_requests: 100,
        read_window_seconds: 60,
        ..synctv_core::RequestRateLimitConfig::default()
    })
    .await;
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"room_123"}"#)),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"room_123"}"#)),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "ticket creation should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_route_fails_closed_when_websocket_runtime_is_unavailable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"room1234_abx"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "ticket issuance must fail closed with service unavailable when websocket runtime dependencies are unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn test_websocket_route_fails_closed_when_runtime_is_unavailable_before_upgrade_checks(
) -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/ws/rooms/AbC123xYz890")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "websocket runtime checks must fail closed before WebSocketUpgrade extraction would otherwise return 400"
    );
    Ok(())
}

#[tokio::test]
async fn test_websocket_ticket_runtime_gate_does_not_leak_to_other_write_routes() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"new_username":"patched-name"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "write routes unrelated to ticket issuance must keep their normal auth path when websocket runtime dependencies are unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn test_rtmp_publish_key_routes_are_reachable_under_api() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let api_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/rtmp/rooms/AbC123xYz890/publish-key/ZyX098wVu765")
            .body(Body::empty()),
    )?;
    let api_response = test_response(app.clone().oneshot(api_request).await)?;
    assert_eq!(api_response.status(), StatusCode::UNAUTHORIZED);

    let info_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/rtmp/rooms/AbC123xYz890/info/ZyX098wVu765")
            .body(Body::empty()),
    )?;
    let info_api_response = test_response(app.clone().oneshot(info_request).await)?;
    assert_eq!(info_api_response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_oauth2_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/oauth2/providers")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_optional_user_execution_rejects_invalid_authorization_header() -> TestResult {
    let state = test_app_state();
    let request_meta = crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Http)
        .with_authorization(Some("Bearer malformed-token".to_string()))
        .with_client_ip(Some("127.0.0.1".parse()?));

    let err = match state
        .shared_api_runtime
        .request_executor
        .execute_optional_user_with_control(
            &request_meta,
            crate::impls::EndpointRateLimitCategory::Auth,
            |_control, _authenticated| async move { Ok::<_, crate::impls::ApiError>(()) },
        )
        .await
    {
        Ok(()) => return Err(test_error("invalid bearer token must be rejected")),
        Err(error) => error,
    };

    assert!(
        matches!(err.classify(), crate::impls::ErrorKind::Unauthenticated),
        "strict optional-auth execution must reject invalid bearer headers instead of downgrading to anonymous",
    );
    Ok(())
}

#[tokio::test]
async fn test_http_request_metadata_rejects_non_utf8_authorization_header() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/hot")
            .header(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_bytes(&[0xff])?,
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_http_request_metadata_rejects_non_utf8_user_agent_header() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/hot")
            .header(
                axum::http::header::USER_AGENT,
                axum::http::HeaderValue::from_bytes(&[0xff])?,
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_notification_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let read_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/notifications")
            .body(Body::empty()),
    )?;
    let read_response = test_response(app.clone().oneshot(read_request).await)?;
    assert_eq!(read_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let write_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/notifications/read-all")
            .body(Body::empty()),
    )?;
    let write_response = test_response(app.oneshot(write_request).await)?;
    assert_eq!(write_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_live_provider_routes_remain_registered_when_infrastructure_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/room_123/streams")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_websocket_routes_fail_closed_when_dependencies_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/ws/rooms/room1234_abx")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "websocket route must fail closed before auth/query validation when runtime dependencies are unavailable"
    );
    Ok(())
}
