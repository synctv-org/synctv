use super::*;
use crate::impls::admin::response::active_room_stream_media_ids_for_infra;
use crate::impls::admin::rooms::username_from_loaded_user;
use crate::ApiRuntimeSettings;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use synctv_core::models::{
    FromProviderParams, MemberStatus, PlaylistId, ReviewRequestId, RoomId, RoomRole, RoomStatus,
    UserId, UserRole, UserStatus,
};
use synctv_core::service::ProvidersManager;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    repository::{
        MediaRepository, ProviderInstanceRepository, RoomRepository, SettingsRepository,
        UserRepository,
    },
    service::{
        AuditService, BruteForceProtection, EmailService, InMemoryTokenBlacklistStore, JwtService,
        NewRealtimeOutboxEvent, PublishKeyService, RemoteProviderManager,
        RuntimeEmailConfigProvider, RuntimeSettingsStore, SettingsService, UserService,
    },
};
use synctv_core_testing::create_test_pool;
use synctv_livestream::{
    LiveStreamingInfrastructure, StreamError, StreamRegistryTrait, StreamTracker,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager, PublishRequest, RealtimeEvent};
use tokio::sync::mpsc;

type TestResult<T = ()> = anyhow::Result<T>;

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

fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
    result.map_err(|error| test_error(error.to_string()))
}

fn some_value<T>(value: Option<T>, context: &str) -> TestResult<T> {
    match value {
        Some(value) => Ok(value),
        None => Err(test_error(context)),
    }
}

fn admin_room_defaults_settings(
    settings: &synctv_proto::admin::RuntimeSettings,
) -> TestResult<&synctv_proto::admin::RoomDefaultsSettings> {
    settings
        .room_defaults
        .as_ref()
        .ok_or_else(|| test_error("expected room_defaults settings"))
}

fn admin_room_creation_settings(
    settings: &synctv_proto::admin::RuntimeSettings,
) -> TestResult<&synctv_proto::admin::RoomCreationSettings> {
    settings
        .room_creation
        .as_ref()
        .ok_or_else(|| test_error("expected room_creation settings"))
}

fn admin_email_settings(
    settings: &synctv_proto::admin::RuntimeSettings,
) -> TestResult<&synctv_proto::admin::EmailSettings> {
    settings
        .email
        .as_ref()
        .ok_or_else(|| test_error("expected email settings"))
}

fn test_runtime_settings() -> synctv_core::service::RuntimeSettings {
    synctv_core::service::RuntimeSettings {
        server: synctv_core::service::ServerRuntimeSettings {
            name: "SyncTV".to_string(),
        },
        room_defaults: synctv_core::service::RoomDefaultsRuntimeSettings {
            default_max_members: 100,
            default_max_chat_messages: 500,
        },
        permissions: synctv_core::service::PermissionRuntimeSettings {
            admin_default_permissions: synctv_core::service::PermissionSet::admin_default(),
            member_default_permissions: synctv_core::service::PermissionSet::member_default(),
            guest_default_permissions: synctv_core::service::PermissionSet::guest_default(),
        },
        room_creation: synctv_core::service::RoomCreationRuntimeSettings {
            enabled: true,
            approval_required: false,
            password_policy: synctv_core::service::RoomPasswordPolicy::Optional,
            max_rooms_per_user: 10,
        },
        user: synctv_core::service::UserRuntimeSettings {
            enable_password_signup: true,
            password_signup_need_review: false,
            enable_email_signup: true,
            email_signup_need_review: false,
            enable_webauthn_signup: true,
            webauthn_signup_need_review: false,
            enable_guest: true,
        },
        oauth2: synctv_core::service::OAuth2RuntimeSettings {
            providers: synctv_core::service::OAuth2ProviderConfigs::default(),
        },
        proxy: synctv_core::service::ProxyRuntimeSettings {
            movie_proxy: true,
            live_proxy: true,
        },
        rtmp: synctv_core::service::RtmpRuntimeSettings {
            custom_publish_host: None,
            ts_disguised_as_png: false,
        },
        email: synctv_core::service::EmailRuntimeSettings {
            enabled: true,
            smtp_host: Some("smtp.example.com".to_string()),
            smtp_port: 587,
            smtp_credentials: Some(synctv_core::service::SmtpCredentials {
                username: "smtp-user".to_string(),
                password: "smtp-secret".to_string(),
            }),
            smtp_proxy: Some(synctv_core::service::SmtpProxyConfig {
                url: "socks5://proxy.example.com:1080".to_string(),
                credentials: Some(synctv_core::service::SmtpCredentials {
                    username: "proxy-user".to_string(),
                    password: "proxy-secret".to_string(),
                }),
            }),
            use_tls: true,
            from_email: Some("noreply@example.com".to_string()),
            from_name: "SyncTV".to_string(),
            whitelist_enabled: true,
            whitelist_domains: vec!["example.com".to_string()],
        },
        webrtc: synctv_core::service::WebRtcRuntimeSettings {
            external_ice_servers: synctv_core::service::IceServerList(Vec::new()),
            max_voice_participants_per_room: 8,
        },
        chat: synctv_core::service::ChatRuntimeSettings {
            max_messages_per_room: 500,
            max_pinned_messages_per_room: 20,
            message_retention_days: 90,
        },
        playback_history: synctv_core::service::PlaybackHistoryRuntimeSettings {
            retention_days: 90,
            max_entries_per_room: 1_000,
        },
        cors: synctv_core::service::CorsRuntimeSettings {
            allowed_origins: synctv_core::service::CorsAllowedOrigins(vec![
                "https://app.example.com".to_string(),
            ]),
        },
    }
}

async fn current_admin_settings(
    admin_api: &AdminApiImpl,
) -> TestResult<synctv_proto::admin::RuntimeSettings> {
    api_ok(
        admin_api
            .get_settings(&UserId::new(), &RequestContext::default())
            .await,
    )
}

async fn update_admin_settings(
    admin_api: &AdminApiImpl,
    patch: synctv_proto::admin::UpdateSettingsRequest,
) -> TestResult<synctv_proto::admin::RuntimeSettings> {
    api_ok(
        admin_api
            .update_settings(patch, &UserId::new(), &RequestContext::default())
            .await,
    )
}

fn runtime_settings_request(
    settings: synctv_proto::admin::RuntimeSettingsPatch,
    paths: &[&str],
) -> synctv_proto::admin::UpdateSettingsRequest {
    synctv_proto::admin::UpdateSettingsRequest {
        settings: Some(settings),
        update_mask: Some(synctv_proto::FieldMask {
            paths: paths.iter().map(|path| (*path).to_string()).collect(),
        }),
    }
}

fn room_creation_max_rooms_per_user_patch(
    max_rooms_per_user: i64,
) -> synctv_proto::admin::UpdateSettingsRequest {
    runtime_settings_request(
        synctv_proto::admin::RuntimeSettingsPatch {
            room_creation: Some(synctv_proto::admin::RoomCreationSettingsPatch {
                max_rooms_per_user: Some(max_rooms_per_user),
                ..Default::default()
            }),
            ..Default::default()
        },
        &["room_creation.max_rooms_per_user"],
    )
}

fn email_settings_patch(
    enabled: bool,
    smtp_host: &str,
    smtp_port: u32,
    from_email: &str,
) -> synctv_proto::admin::UpdateSettingsRequest {
    runtime_settings_request(
        synctv_proto::admin::RuntimeSettingsPatch {
            email: Some(synctv_proto::admin::EmailSettingsPatch {
                enabled: Some(enabled),
                smtp_host: Some(smtp_host.to_string()),
                smtp_port: Some(smtp_port),
                use_tls: Some(true),
                from_email: Some(from_email.to_string()),
                from_name: Some("SyncTV".to_string()),
                whitelist_enabled: Some(false),
                whitelist_domains: Vec::new(),
                ..Default::default()
            }),
            ..Default::default()
        },
        &[
            "email.enabled",
            "email.smtp_host",
            "email.smtp_port",
            "email.use_tls",
            "email.from_email",
            "email.from_name",
            "email.whitelist_enabled",
            "email.whitelist_domains",
        ],
    )
}

#[test]
fn proto_list_filter_enums_reject_unknown_values() {
    assert!(matches!(
        proto_room_status_filter(99),
        Err(ApiError::InvalidInput(message)) if message.contains("room status")
    ));
    assert!(matches!(
        proto_user_status_filter(99),
        Err(ApiError::InvalidInput(message)) if message.contains("user status")
    ));
    assert!(matches!(
        proto_user_role_filter(99),
        Err(ApiError::InvalidInput(message)) if message.contains("user role")
    ));
}

#[test]
fn proto_list_filter_enums_allow_unspecified_as_empty_filter() -> TestResult {
    assert_eq!(
        api_ok(proto_room_status_filter(
            synctv_proto::common::RoomStatus::Unspecified as i32
        ))?,
        None
    );
    assert_eq!(
        api_ok(proto_user_status_filter(
            synctv_proto::common::UserStatus::Unspecified as i32
        ))?,
        None
    );
    assert_eq!(
        api_ok(proto_user_role_filter(
            synctv_proto::common::UserRole::Unspecified as i32
        ))?,
        None
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MembershipEventFanoutCall {
    PublishPermissionChanged {
        room_id: String,
        target_user_id: String,
        changed_by: String,
    },
    PublishUserLeft {
        room_id: String,
        user_id: String,
    },
}

#[derive(Default, Clone)]
struct RecordingMembershipEventFanout {
    calls: Arc<Mutex<Vec<MembershipEventFanoutCall>>>,
}

impl RecordingMembershipEventFanout {
    fn take_calls(&self) -> Vec<MembershipEventFanoutCall> {
        let mut calls = match self.calls.lock() {
            Ok(calls) => calls,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *calls)
    }

    fn push(&self, call: MembershipEventFanoutCall) {
        let mut calls = match self.calls.lock() {
            Ok(calls) => calls,
            Err(poisoned) => poisoned.into_inner(),
        };
        calls.push(call);
    }
}

fn test_realtime_outbox_event(event: &RealtimeEvent) -> NewRealtimeOutboxEvent {
    NewRealtimeOutboxEvent {
        id: event.event_id().to_string(),
        enqueue_outbox: false,
        aggregate_type: "test".to_string(),
        aggregate_id: event
            .room_id()
            .map_or_else(|| "global".to_string(), std::string::ToString::to_string),
        event_type: event.event_type().to_string(),
        event_version: 1,
        aggregate_version: None,
        payload: event.clone(),
    }
}

#[derive(Debug, Default)]
struct FailingRealtimeFanout {
    publish_attempts: std::sync::atomic::AtomicUsize,
}

impl FailingRealtimeFanout {
    fn publish_attempts(&self) -> usize {
        self.publish_attempts
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RealtimeFanoutService for FailingRealtimeFanout {
    async fn try_publish(&self, _request: PublishRequest) -> bool {
        self.publish_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        false
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        Ok(test_realtime_outbox_event(event))
    }

    fn publish_after_outbox_commit(&self, _event: RealtimeEvent) {}

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

async fn recv_matching_realtime_event(
    receiver: &mut mpsc::Receiver<PublishRequest>,
    description: &str,
    mut predicate: impl FnMut(&RealtimeEvent) -> bool,
) -> RealtimeEvent {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "timed out waiting for {description}");
        let remaining = deadline - now;
        let request = match tokio::time::timeout(remaining, receiver.recv()).await {
            Ok(Some(request)) => request,
            Ok(None) => std::panic::panic_any(format!(
                "cluster publish channel closed while waiting for {description}"
            )),
            Err(_) => std::panic::panic_any(format!("timed out waiting for {description}")),
        };
        if predicate(&request.event) {
            return request.event;
        }
    }
}

#[async_trait]
impl MembershipEventFanoutService for RecordingMembershipEventFanout {
    fn prepare_permission_changed_outbox_fanout(
        &self,
        target_is_online: bool,
        target_connection_count: usize,
    ) -> crate::membership_event_fanout::PreparedPermissionChangedFanout {
        crate::membership_event_fanout::PreparedPermissionChangedFanout::new(
            Arc::new(self.clone()),
            crate::membership_event_fanout::local_realtime_event_publisher(Arc::new(
                LocalNoopRealtimeEventService::new(),
            )),
            target_is_online,
            target_connection_count,
        )
    }

    fn prepare_user_left_outbox_fanout(
        &self,
    ) -> crate::membership_event_fanout::PreparedUserLeftFanout {
        crate::membership_event_fanout::PreparedUserLeftFanout::new(Arc::new(self.clone()))
    }
}

#[async_trait]
impl RealtimeFanoutService for RecordingMembershipEventFanout {
    async fn try_publish(&self, _request: PublishRequest) -> bool {
        true
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        match event {
            RealtimeEvent::PermissionChanged {
                room_id,
                target_user_id,
                changed_by,
                ..
            } => self.push(MembershipEventFanoutCall::PublishPermissionChanged {
                room_id: room_id.to_string(),
                target_user_id: target_user_id.to_string(),
                changed_by: changed_by.to_string(),
            }),
            RealtimeEvent::UserLeft {
                room_id, user_id, ..
            } => self.push(MembershipEventFanoutCall::PublishUserLeft {
                room_id: room_id.to_string(),
                user_id: user_id.to_string(),
            }),
            _ => {}
        }
        Ok(test_realtime_outbox_event(event))
    }

    fn publish_after_outbox_commit(&self, _event: RealtimeEvent) {}

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

#[test]
fn local_management_actor_id_is_reserved_numeric_actor() {
    assert_eq!(LOCAL_MANAGEMENT_ACTOR_USER_ID.as_i64(), i64::MAX);
}

fn fixture_value<T, E>(result: Result<T, E>, context: &str) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn public_room_id(admin_api: &AdminApiImpl, id: RoomId) -> String {
    fixture_value(
        admin_api.public_id_codec.encode_room_id(id),
        "test room id should encode",
    )
}

fn public_user_id(admin_api: &AdminApiImpl, id: UserId) -> String {
    fixture_value(
        admin_api.public_id_codec.encode_user_id(id),
        "test user id should encode",
    )
}

fn public_media_id(admin_api: &AdminApiImpl, id: synctv_core::models::MediaId) -> String {
    fixture_value(
        admin_api.public_id_codec.encode_media_id(id),
        "test media id should encode",
    )
}

fn public_playlist_id(admin_api: &AdminApiImpl, id: PlaylistId) -> String {
    fixture_value(
        admin_api.public_id_codec.encode_playlist_id(id),
        "test playlist id should encode",
    )
}

#[test]
fn test_map_batch_result_error_preserves_business_message() {
    let message = map_batch_result_error(ApiError::Authorization(
        "Only root users can ban admin users".to_string(),
    ));

    assert_eq!(message, "Only root users can ban admin users");
}

#[test]
fn test_map_batch_result_error_sanitizes_internal_error() {
    let message = map_batch_result_error(ApiError::Internal(
        "sqlx error: relation users does not exist".to_string(),
    ));

    assert_eq!(message, "Operation failed due to an internal error");
    assert!(!message.contains("sqlx"));
}

#[test]
fn test_map_batch_result_error_sanitizes_service_unavailable_error() {
    let message = map_batch_result_error(ApiError::ServiceUnavailable(
        "Redis timeout while publishing realtime event".to_string(),
    ));

    assert_eq!(
        message,
        "Operation failed because the service is temporarily unavailable"
    );
    assert!(!message.contains("Redis timeout"));
}

#[test]
fn test_map_send_test_email_result_failure_is_sanitized() {
    let response = AdminApiImpl::map_send_test_email_result(
        "test@example.com",
        Err(synctv_core::Error::Internal(
            "smtp connect ECONNREFUSED 127.0.0.1:587".to_string(),
        )),
    );

    assert!(!response.success);
    assert_eq!(
        response.message,
        "Failed to send test email. Please verify the email configuration and try again."
    );
    assert!(
        !response.message.contains("ECONNREFUSED"),
        "internal transport details must not leak to clients"
    );
}

fn make_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = fixture_value(
        JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars"),
        "jwt service should build",
    );
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400));

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test".to_string()),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_validate_admin_auth_rejects_banned_user() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = make_user_service(&pool);
    let user_repo = UserRepository::new(pool);

    let banned_admin = create_db_user(&user_repo, "banned_admin_auth", UserRole::Admin).await;
    user_repo
        .ban(&banned_admin.id, None, Some("admin auth test".to_string()))
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let err = api_err(
        AdminAuthValidator::new(&user_service)
            .validate(banned_admin.id, 0, 0)
            .await,
    )?;

    assert!(
        matches!(err, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
        "banned admin auth must fail with generic authentication error, got: {err:?}"
    );
    Ok(())
}

async fn make_admin_api_for_delete_user_test(
    pool: sqlx::PgPool,
) -> (
    AdminApiImpl,
    tokio::sync::mpsc::Receiver<synctv_realtime::sync::PublishRequest>,
) {
    let realtime_outbox = Arc::new(
        synctv_core::repository::realtime_outbox::RealtimeOutboxRepository::new(pool.clone()),
    );
    let user_service = Arc::new(synctv_core::service::UserService::new_with_runtime(
        &pool,
        fixture_value(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars"),
            "jwt service should build",
        ),
        UsernameCache::local_only("test:username:".to_string(), 128, 60),
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test".to_string()),
        synctv_core::service::UserServiceRuntimeOptions {
            realtime_outbox: Some(realtime_outbox.clone()),
            ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
        },
    ));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let providers_manager = ProvidersManager::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    );
    let providers_manager = fixture_value(providers_manager, "providers manager should build");
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    fixture_value(
        settings_service
            .initialize()
            .await
            .map_err(|error| error.to_string()),
        "settings initialized",
    );
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service.clone()));
    let room_service = synctv_core::service::RoomService::new_with_providers_and_options(
        pool.clone(),
        (*user_service).clone(),
        Arc::new(providers_manager),
        synctv_core::service::RoomServiceOptions {
            runtime_settings_store: Some(runtime_settings_store.clone()),
            realtime_outbox: Some(realtime_outbox),
            ..synctv_core::service::RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    );
    let room_service = fixture_value(room_service, "room service should build");
    let email_service = EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
        &runtime_settings_store,
    )));
    let email_service = Arc::new(fixture_value(email_service, "email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let room_service = Arc::new(room_service);
    fixture_value(
        room_service
            .media_service()
            .providers_manager()
            .create_builtin_defaults()
            .await
            .map_err(|error| error.to_string()),
        "built-in providers should initialize",
    );
    let audit_service = Arc::new(AuditService::new_unbuffered(pool));
    let config = Arc::new(ApiRuntimeSettings::default());
    let publish_key_service = PublishKeyService::new(
        fixture_value(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars"),
            "jwt",
        ),
        Arc::new(synctv_core::SystemClock),
        24,
    );
    let publish_key_service = Arc::new(fixture_value(
        publish_key_service,
        "publish key service should build",
    ));
    let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
    let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
    );

    (
        AdminApiImpl::new_with_runtime(
            AdminApiOptions {
                room_service,
                read_services: crate::test_support::admin_read_services(user_service.as_ref()),
                user_service,
                settings_service,
                runtime_settings_store: Some(runtime_settings_store),
                email_service,
                connection_service: connection_manager,
                provider_instance_manager,
                live_streaming_infrastructure: None,
                publish_key_service: Some(publish_key_service),
                runtime_settings: config,
                audit_service,
                public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
            },
            AdminApiRuntime {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout: crate::test_support::channel_realtime_fanout_service(
                    redis_publish_tx,
                ),
                realtime_event_service: Arc::new(LocalNoopRealtimeEventService::new()),
                provider_stores,
                provider_access_service: crate::impls::disabled_provider_access_service(),
                signing_key: Arc::new(
                    crate::proxy_signature::ProxySigningKey::try_derive_from(
                        b"test-admin-api-signing-key-32-bytes!!",
                    )
                    .expect("test signing key should derive"),
                ),
                media_swarm_signing_key: crate::test_support::media_swarm_signing_key(
                    b"test-admin-api-media-swarm-signing-key-32-bytes",
                ),
                presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
                request_executor: Arc::new(crate::test_support::local_request_executor()),
            },
        ),
        redis_publish_rx,
    )
}

async fn make_admin_api_with_livestream_for_test(
    pool: sqlx::PgPool,
) -> (
    AdminApiImpl,
    Arc<LiveStreamingInfrastructure>,
    Arc<dyn StreamRegistryTrait>,
    tokio::sync::mpsc::Receiver<synctv_realtime::sync::PublishRequest>,
) {
    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(fixture_value(
        synctv_core::service::RoomService::new_for_tests(pool.clone(), (*user_service).clone()),
        "room service should build",
    ));
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    fixture_value(
        settings_service
            .initialize()
            .await
            .map_err(|error| error.to_string()),
        "settings initialized",
    );
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service.clone()));
    let email_service = EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
        &runtime_settings_store,
    )));
    let email_service = Arc::new(fixture_value(email_service, "email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    fixture_value(
        room_service
            .media_service()
            .providers_manager()
            .create_builtin_defaults()
            .await
            .map_err(|error| error.to_string()),
        "built-in providers should initialize",
    );
    let audit_service = Arc::new(AuditService::new_unbuffered(pool));
    let config = Arc::new(ApiRuntimeSettings::default());
    let publish_key_service = PublishKeyService::new(
        fixture_value(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars"),
            "jwt",
        ),
        Arc::new(synctv_core::SystemClock),
        24,
    );
    let publish_key_service = Arc::new(fixture_value(
        publish_key_service,
        "publish key service should build",
    ));
    let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
    let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
    );

    let tracker = Arc::new(StreamTracker::new());
    let registry = synctv_livestream::local_stream_registry();
    let (event_sender, _event_receiver) = mpsc::channel(64);
    let live_streaming_infrastructure = Arc::new(fixture_value(
        LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker,
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        ),
        "livestream infrastructure should build",
    ));

    (
        AdminApiImpl::new_with_runtime(
            AdminApiOptions {
                room_service,
                read_services: crate::test_support::admin_read_services(user_service.as_ref()),
                user_service,
                settings_service,
                runtime_settings_store: Some(runtime_settings_store),
                email_service,
                connection_service: connection_manager,
                provider_instance_manager,
                live_streaming_infrastructure: Some(live_streaming_infrastructure.clone()),
                publish_key_service: Some(publish_key_service),
                runtime_settings: config,
                audit_service,
                public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
            },
            AdminApiRuntime {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout: crate::test_support::channel_realtime_fanout_service(
                    redis_publish_tx,
                ),
                realtime_event_service: Arc::new(LocalNoopRealtimeEventService::new()),
                provider_stores,
                provider_access_service: crate::impls::disabled_provider_access_service(),
                signing_key: Arc::new(
                    crate::proxy_signature::ProxySigningKey::try_derive_from(
                        b"test-admin-api-signing-key-32-bytes!!",
                    )
                    .expect("test signing key should derive"),
                ),
                media_swarm_signing_key: crate::test_support::media_swarm_signing_key(
                    b"test-admin-api-media-swarm-signing-key-32-bytes",
                ),
                presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
                request_executor: Arc::new(crate::test_support::local_request_executor()),
            },
        ),
        live_streaming_infrastructure,
        registry,
        redis_publish_rx,
    )
}

async fn create_room_with_member(
    admin_api: &AdminApiImpl,
    owner_id: &UserId,
    member_id: &UserId,
) -> synctv_core::models::Room {
    let room = fixture_value(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room for user lifecycle test".to_string(),
                *owner_id,
                None,
                None,
            )
            .await,
        "room should be created",
    )
    .0;

    fixture_value(
        admin_api
            .room_service
            .join_room(room.id, *member_id, None)
            .await,
        "member should join room",
    );

    room
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_add_member_publishes_permission_changed_membership_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (mut admin_api, _redis_publish_rx) =
        make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_add_memberhip_outbox",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_add_memberhip_outbox",
        UserRole::User,
    )
    .await;
    let target = create_db_user(&user_repo, "target_add_memberhip_outbox", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room for add member fanout test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let fanout = Arc::new(RecordingMembershipEventFanout::default());
    admin_api.membership_event_fanout = fanout.clone();

    let response = admin_api
        .add_member(
            synctv_proto::admin::AddMemberRequest {
                room_id: public_room_id(&admin_api, room.id),
                user_id: public_user_id(&admin_api, target.id),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
                remark_name: String::new(),
                display_tag: String::new(),
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

    let member = response;
    assert_eq!(member.user_id, public_user_id(&admin_api, target.id));
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_update_member_permissions_publishes_membership_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (mut admin_api, _redis_publish_rx) =
        make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_update_membership_outbox",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_update_membership_outbox",
        UserRole::User,
    )
    .await;
    let target = create_db_user(
        &user_repo,
        "target_update_membership_outbox",
        UserRole::User,
    )
    .await;

    let room = Box::pin(create_room_with_member(&admin_api, &owner.id, &target.id)).await;

    let fanout = Arc::new(RecordingMembershipEventFanout::default());
    admin_api.membership_event_fanout = fanout.clone();

    let response = admin_api
        .update_member_permissions(
            synctv_proto::admin::UpdateMemberPermissionsRequest {
                room_id: public_room_id(&admin_api, room.id),
                user_id: public_user_id(&admin_api, target.id),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                added_permissions: 0b100,
                removed_permissions: 0b010,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

    let member = response;
    assert_eq!(member.user_id, public_user_id(&admin_api, target.id));
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_member_response_uses_room_permission_overrides() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_member_response_permissions",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "owner_member_response_permissions",
        UserRole::User,
    )
    .await;
    let target = create_db_user(
        &user_repo,
        "target_member_response_permissions",
        UserRole::User,
    )
    .await;
    let room = Box::pin(create_room_with_member(&admin_api, &owner.id, &target.id)).await;

    let mut settings = core_ok(admin_api.room_service.get_room_settings(&room.id).await)?;
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            synctv_core::models::RoomMemberPermissionBits::MANAGE_OWN_MEDIA,
        );
    core_ok(
        admin_api
            .room_service
            .set_room_settings(&room.id, &settings)
            .await,
    )?;

    let response = admin_api
        .update_member_permissions(
            synctv_proto::admin::UpdateMemberPermissionsRequest {
                room_id: public_room_id(&admin_api, room.id),
                user_id: public_user_id(&admin_api, target.id),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                added_permissions: 0,
                removed_permissions:
                    synctv_core::models::RoomMemberPermissionBits::SEND_CHAT_MESSAGES,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

    let member = response;
    assert!(
        synctv_core::models::RoomPermissionSet::default_member()
            .has(synctv_core::models::RoomPermission::MANAGE_OWN_MEDIA),
        "static member defaults include MANAGE_OWN_MEDIA, so the response must prove it used room overrides"
    );
    assert!(
        !synctv_core::models::RoomPermissionSet(member.permissions)
            .has(synctv_core::models::RoomPermission::MANAGE_OWN_MEDIA),
        "admin member response must apply room-level permission removals"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_kick_member_publishes_membership_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (mut admin_api, _redis_publish_rx) =
        make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_kick_membership_outbox",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_kick_membership_outbox",
        UserRole::User,
    )
    .await;
    let target = create_db_user(&user_repo, "target_kick_membership_outbox", UserRole::User).await;

    let room = Box::pin(create_room_with_member(&admin_api, &owner.id, &target.id)).await;

    let fanout = Arc::new(RecordingMembershipEventFanout::default());
    admin_api.membership_event_fanout = fanout.clone();

    let response = admin_api
        .kick_member(
            synctv_proto::admin::KickMemberRequest {
                room_id: public_room_id(&admin_api, room.id),
                user_id: public_user_id(&admin_api, target.id),
                kick_cooldown_seconds: 60,
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert!(response.success);
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
    Ok(())
}

async fn create_room_media(
    pool: &sqlx::PgPool,
    room_id: RoomId,
    creator_id: UserId,
    name: &str,
) -> synctv_core::models::Media {
    let media_repo = MediaRepository::new(pool.clone());
    let mut tx = fixture_value(pool.begin().await, "begin media test transaction");
    let position = fixture_value(
        media_repo
            .get_next_append_position_with_tx(&room_id, None, &mut tx)
            .await,
        "compute next media position",
    );
    let media = fixture_value(
        synctv_core::models::Media::from_direct_single_mode(
            None,
            room_id,
            Some(creator_id),
            name.to_string(),
            "direct",
            synctv_core::models::PlaybackInfo::single_url(
                "https://example.com/video.mp4".to_string(),
                "default".to_string(),
            ),
            position,
        ),
        "direct media should build",
    );
    let created = fixture_value(
        media_repo.create_with_executor(&media, &mut *tx).await,
        "create media",
    );
    fixture_value(tx.commit().await, "commit media test transaction");
    created
}

fn make_test_room_model(created_by: &UserId) -> synctv_core::models::Room {
    let now = synctv_core::SystemClock.now();
    synctv_core::models::Room {
        id: RoomId::new(),
        name: "room-ban-test".to_string(),
        description: "room for admin ban test".to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *created_by,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

#[test]
fn username_from_loaded_user_rejects_empty_username() {
    let mut user = synctv_core::models::User::new(
        "valid-user".to_string(),
        synctv_core::models::SignupMethod::Password,
    );
    user.id = UserId::expect_positive(44);
    user.username = "   ".to_string();

    assert!(matches!(
        username_from_loaded_user(user),
        Err(ApiError::Internal(message))
            if message.contains("empty username")
                && message.contains("44")
    ));
}

#[test]
fn admin_query_enum_mappers_reject_unknown_values_and_preserve_defaults() -> TestResult {
    assert_eq!(
        api_ok(proto_admin_user_list_sort_by(
            synctv_proto::admin::UserListSortBy::Status as i32
        ))?,
        synctv_core::models::UserListSortBy::Status
    );
    assert_eq!(
        api_ok(proto_admin_user_list_sort_by(
            synctv_proto::admin::UserListSortBy::Unspecified as i32
        ))?,
        synctv_core::models::UserListSortBy::CreatedAt
    );
    assert_eq!(
        api_ok(proto_admin_room_list_sort_by(
            synctv_proto::admin::RoomListSortBy::Unspecified as i32
        ))?,
        synctv_core::models::RoomListSortBy::CreatedAt
    );
    assert_eq!(
        api_ok(proto_admin_room_member_list_sort_by(
            synctv_proto::admin::RoomMemberListSortBy::Unspecified as i32
        ))?,
        synctv_core::models::RoomMemberListSortBy::JoinedAt
    );
    assert_eq!(
        api_ok(proto_admin_active_stream_list_sort_by(
            synctv_proto::admin::ActiveStreamListSortBy::Unspecified as i32
        ))?,
        ActiveStreamListSortBy::StartedAt
    );
    assert_eq!(
        api_ok(proto_admin_sort_direction(
            synctv_proto::admin::SortDirection::Unspecified as i32,
            CoreSortDirection::Desc
        ))?,
        CoreSortDirection::Desc
    );
    assert_eq!(
        api_ok(proto_admin_active_stream_sort_direction(
            synctv_proto::admin::SortDirection::Unspecified as i32
        ))?,
        CoreSortDirection::Desc
    );
    assert_eq!(
        api_ok(map_admin_playlist_sort(
            synctv_proto::client::PlaylistListSortBy::Unspecified as i32
        ))?,
        synctv_core::models::PlaylistListSortBy::Position
    );
    assert_eq!(
        api_ok(map_admin_media_sort(
            synctv_proto::client::MediaListSortBy::Unspecified as i32
        ))?,
        synctv_core::models::MediaListSortBy::Position
    );
    assert_eq!(
        api_ok(map_resource_availability_filter(
            synctv_proto::client::ResourceAvailabilityFilter::All as i32
        ))?,
        None
    );

    assert!(matches!(
        proto_admin_user_list_sort_by(99),
        Err(ApiError::InvalidInput(message)) if message.contains("user list sort")
    ));
    assert!(matches!(
        proto_admin_room_list_sort_by(99),
        Err(ApiError::InvalidInput(message)) if message.contains("room list sort")
    ));
    assert!(matches!(
        proto_admin_room_member_list_sort_by(99),
        Err(ApiError::InvalidInput(message)) if message.contains("room member list sort")
    ));
    assert!(matches!(
        proto_admin_active_stream_list_sort_by(99),
        Err(ApiError::InvalidInput(message)) if message.contains("active stream list sort")
    ));
    assert!(matches!(
        proto_admin_sort_direction(99, CoreSortDirection::Desc),
        Err(ApiError::InvalidInput(message)) if message.contains("sort direction")
    ));
    assert!(matches!(
        proto_admin_active_stream_sort_direction(99),
        Err(ApiError::InvalidInput(message)) if message.contains("sort direction")
    ));
    assert!(matches!(
        map_admin_playlist_sort(99),
        Err(ApiError::InvalidInput(message)) if message.contains("playlist list sort")
    ));
    assert!(matches!(
        map_admin_media_sort(99),
        Err(ApiError::InvalidInput(message)) if message.contains("media list sort")
    ));
    assert!(matches!(
        map_resource_availability_filter(99),
        Err(ApiError::InvalidInput(message)) if message.contains("availability")
    ));
    Ok(())
}

#[tokio::test]
async fn test_active_room_stream_media_ids_unions_local_and_registry_streams() -> TestResult {
    let tracker = Arc::new(StreamTracker::new());
    let room_id = RoomId::expect_positive(101);
    let local_media_id = MediaId::expect_positive(201);
    let shared_media_id = MediaId::expect_positive(202);
    let remote_media_id = MediaId::expect_positive(203);
    let other_room_id = RoomId::expect_positive(102);
    let other_media_id = MediaId::expect_positive(204);
    tracker.insert(
        "user-local".to_string(),
        room_id.to_string(),
        local_media_id.to_string(),
    );
    tracker.insert(
        "user-overlap".to_string(),
        room_id.to_string(),
        shared_media_id.to_string(),
    );

    let registry = synctv_livestream::local_stream_registry();
    registry
        .try_register_publisher(
            &room_id.to_string(),
            &shared_media_id.to_string(),
            "node-a",
            "user-overlap",
            "127.0.0.1:50051",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;
    registry
        .try_register_publisher(
            &room_id.to_string(),
            &remote_media_id.to_string(),
            "node-b",
            "user-remote",
            "127.0.0.1:50052",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;
    registry
        .try_register_publisher(
            &other_room_id.to_string(),
            &other_media_id.to_string(),
            "node-c",
            "user-other",
            "127.0.0.1:50053",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let (event_sender, _event_receiver) = mpsc::channel(64);
    let infra = Arc::new(
        LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            tracker,
            String::new(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .map_err(|error| test_error(error.to_string()))?,
    );

    let media_ids = active_room_stream_media_ids_for_infra(Some(&infra), &room_id).await;

    assert_eq!(
        media_ids,
        vec![local_media_id, shared_media_id, remote_media_id]
    );
    Ok(())
}

#[tokio::test]
async fn test_force_disconnect_user_publishes_cluster_kick_event() -> TestResult {
    let connection_service: Arc<dyn ConnectionRuntime> = Arc::new(ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));

    let (publish_tx, mut publish_rx) = mpsc::channel(4);
    let user_id = UserId::expect_positive(110_001);
    let realtime_lifecycle = default_realtime_lifecycle_service(
        connection_service,
        None,
        crate::test_support::channel_realtime_fanout_service(publish_tx),
    );

    realtime_lifecycle
        .disconnect_user(&user_id, "user_deleted")
        .await;

    let published = publish_rx
        .recv()
        .await
        .ok_or_else(|| test_error("force_disconnect_user should publish a kick event"))?;
    match published.event {
        RealtimeEvent::KickUser {
            user_id: published_user_id,
            reason,
            ..
        } => {
            assert_eq!(published_user_id, user_id);
            assert_eq!(reason, "user_deleted");
        }
        other => {
            return Err(test_error(format!(
                "expected KickUser event, got {other:?}"
            )))
        }
    }
    Ok(())
}

fn make_test_user(role: UserRole, status: UserStatus) -> synctv_core::models::User {
    synctv_core::models::User {
        id: UserId::expect_positive(103),
        username: "admin_test".to_string(),
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

fn make_db_user(username: &str, role: UserRole) -> synctv_core::models::User {
    synctv_core::models::User {
        id: UserId::new(),
        username: username.to_string(),
        role,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
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

async fn create_db_user(
    user_repo: &UserRepository,
    username: &str,
    role: UserRole,
) -> synctv_core::models::User {
    fixture_value(
        user_repo.create(&make_db_user(username, role)).await,
        "create test user",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_taxonomy_handles_local_management_actor_labels() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let owner = create_db_user(&user_repo, "taxonomy_management_owner", UserRole::User).await;
    let category = core_ok(
        admin_api
            .room_service
            .upsert_room_category(synctv_core::models::UpsertRoomCategory {
                key: "management_taxonomy".to_string(),
                name: "Management Taxonomy".to_string(),
                description: String::new(),
                sort_order: 1,
                is_enabled: true,
            })
            .await,
    )?;
    let label = core_ok(
        admin_api
            .room_service
            .upsert_room_label(synctv_core::models::UpsertRoomLabel {
                key: "management_taxonomy_label".to_string(),
                name: "Management Label".to_string(),
                description: String::new(),
                color: String::new(),
                category_id: Some(category.id),
                sort_order: 1,
                is_enabled: true,
            })
            .await,
    )?;
    let room = core_ok(
        admin_api
            .room_service
            .create_room_with_taxonomy_outbox(
                synctv_core::service::CreateRoomWithTaxonomyRequest {
                    name: "management taxonomy room".to_string(),
                    description: String::new(),
                    created_by: owner.id,
                    password: None,
                    settings: None,
                    category_id: Some(category.id),
                    label_ids: Vec::new(),
                },
                None,
            )
            .await,
    )?
    .0;

    let response = api_ok(
        admin_api
            .update_room_taxonomy(
                synctv_proto::admin::UpdateRoomTaxonomyRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    category_id: None,
                    clear_category: false,
                    label_ids: vec![fixture_value(
                        admin_api.public_id_codec.encode_room_label_id(label.id),
                        "test room label id should encode",
                    )],
                },
                &LOCAL_MANAGEMENT_ACTOR_USER_ID,
            )
            .await,
    )?;

    let response_room = response;
    assert_eq!(response_room.labels.len(), 1);
    assert_eq!(
        response_room.labels[0].id,
        fixture_value(
            admin_api.public_id_codec.encode_room_label_id(label.id),
            "label id should encode"
        )
    );

    let assigned_by = sqlx::query_scalar!(
        r#"
        SELECT assigned_by AS "assigned_by: UserId"
        FROM room_label_assignments
        WHERE room_id = $1 AND label_id = $2
        "#,
        room.id as RoomId,
        label.id as synctv_core::models::RoomLabelId,
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(assigned_by, None);
    Ok(())
}

#[test]
fn test_admin_user_to_proto_all_roles() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    for (role, expected) in [
        (UserRole::Root, synctv_proto::common::UserRole::Root as i32),
        (
            UserRole::Admin,
            synctv_proto::common::UserRole::Admin as i32,
        ),
        (UserRole::User, synctv_proto::common::UserRole::User as i32),
    ] {
        let user = make_test_user(role, UserStatus::Active);
        let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), None, &public_id_codec)
            .map_err(|error| test_error(format!("{error:?}")))?;
        assert_eq!(proto.role, expected);
    }
    Ok(())
}

#[test]
fn test_admin_user_to_proto_all_statuses() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    for (status, expected) in [
        (
            UserStatus::Active,
            synctv_proto::common::UserStatus::Active as i32,
        ),
        (
            UserStatus::Banned,
            synctv_proto::common::UserStatus::Banned as i32,
        ),
    ] {
        let user = make_test_user(UserRole::User, status);
        let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), None, &public_id_codec)
            .map_err(|error| test_error(format!("{error:?}")))?;
        assert_eq!(proto.status, expected);
    }
    Ok(())
}

#[test]
fn test_admin_user_to_proto_fields() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let user = make_test_user(UserRole::Admin, UserStatus::Active);
    let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), None, &public_id_codec)
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(
        proto.id,
        public_id_codec
            .encode_user_id(user.id)
            .map_err(test_error)?
    );
    assert_eq!(proto.username, "admin_test");
    assert_eq!(proto.email, "admin@test.com");
    Ok(())
}

#[test]
fn test_admin_user_to_proto_preserves_ban_timestamp() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let mut user = make_test_user(UserRole::User, UserStatus::Active);
    let banned_at = synctv_core::SystemClock.now();
    user.is_banned = true;
    user.banned_at = Some(banned_at);

    let proto = try_admin_user_to_proto(&user, Some("banned@test.com"), None, &public_id_codec)
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(proto.banned_at, banned_at.timestamp());
    Ok(())
}

#[test]
fn test_admin_user_to_proto_rejects_banned_user_without_ban_timestamp() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let mut user = make_test_user(UserRole::User, UserStatus::Active);
    user.is_banned = true;

    let error = api_err(try_admin_user_to_proto(
        &user,
        Some("banned@test.com"),
        None,
        &public_id_codec,
    ))?;

    assert!(matches!(
        error,
        ApiError::Internal(message) if message.contains("missing banned_at")
    ));
    Ok(())
}

#[test]
fn test_admin_user_to_proto_no_email() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let proto = try_admin_user_to_proto(&user, None, None, &public_id_codec)
        .map_err(|error| test_error(format!("{error:?}")))?;
    assert_eq!(proto.email, "");
    Ok(())
}

#[test]
fn review_rows_preserve_absent_optional_fields() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let requested_at = synctv_core::SystemClock.now();

    let registration = user_registration_review_row_to_proto(
        &synctv_core::repository::UserRegistrationReviewRecord {
            id: UserId::new(),
            username: "pending_user".to_string(),
            email: "pending@example.com".to_string(),
            signup_method: synctv_core::models::SignupMethod::Email,
            status: synctv_core::models::ReviewStatus::Pending,
            requested_at,
            reviewed_at: None,
            reviewed_by: None,
            rejection_reason: None,
            oauth2_provider: None,
            oauth2_provider_instance_name: None,
            oauth2_provider_issuer: None,
            oauth2_provider_user_id: None,
            oauth2_provider_username: None,
            oauth2_avatar_url: None,
            webauthn_credential_id: None,
            webauthn_credential_name: None,
        },
        &public_id_codec,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(registration.reviewed_by, None);
    assert_eq!(registration.rejection_reason, None);
    assert_eq!(registration.oauth2_provider, None);
    assert_eq!(registration.oauth2_provider_instance_name, None);
    assert_eq!(registration.webauthn_credential_id, None);
    assert_eq!(registration.webauthn_credential_name, None);

    let creation = room_creation_review_row_to_proto(
        &synctv_core::repository::RoomCreationReviewRecord {
            id: RoomId::new(),
            requested_by: UserId::new(),
            requested_by_username: "owner".to_string(),
            name: "room_creation".to_string(),
            description: String::new(),
            status: synctv_core::models::ReviewStatus::Pending,
            requested_at,
            reviewed_at: None,
            reviewed_by: None,
            rejection_reason: None,
            category: None,
            labels: Vec::new(),
        },
        &public_id_codec,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(creation.reviewed_by, None);
    assert_eq!(creation.rejection_reason, None);

    let join = room_join_review_row_to_proto(
        &synctv_core::repository::RoomJoinReviewRecord {
            id: ReviewRequestId::new(),
            room_id: RoomId::new(),
            room_name: "room_creation".to_string(),
            user_id: UserId::new(),
            username: "joiner".to_string(),
            requested_role: synctv_proto::common::RoomMemberRole::Member as i32,
            status: synctv_core::models::ReviewStatus::Pending,
            requested_at,
            reviewed_at: None,
            reviewed_by: None,
            rejection_reason: None,
        },
        &public_id_codec,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(join.reviewed_by, None);
    assert_eq!(join.rejection_reason, None);
    Ok(())
}

#[test]
fn test_set_user_password_user_lookup_backend_failure_stays_service_unavailable() {
    let mapped = AdminApiImpl::map_target_user_lookup_error(
        synctv_core::Error::ServiceUnavailable("user lookup unavailable".to_string()),
    );

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "user lookup unavailable"),
        "user lookup backend failures must not be reported as not found, got: {mapped:?}"
    );
}

#[test]
fn test_set_user_password_user_lookup_not_found_stays_not_found() {
    let mapped = AdminApiImpl::map_target_user_lookup_error(synctv_core::Error::NotFound(
        "missing row".to_string(),
    ));

    assert!(
        matches!(mapped, ApiError::NotFound(ref msg) if msg == "User not found"),
        "true user misses must remain not found, got: {mapped:?}"
    );
}

#[test]
fn test_admin_auth_user_lookup_backend_failure_stays_service_unavailable() {
    let mapped = AdminApiImpl::map_admin_auth_user_lookup_error(
        synctv_core::Error::ServiceUnavailable("user backend unavailable".to_string()),
    );

    assert!(
        matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "user backend unavailable"),
        "admin auth backend failures must not be reported as authentication failures, got: {mapped:?}"
    );
}

#[test]
fn test_admin_auth_user_lookup_not_found_stays_authentication_failed() {
    let mapped = AdminApiImpl::map_admin_auth_user_lookup_error(synctv_core::Error::NotFound(
        "missing row".to_string(),
    ));

    assert!(
        matches!(mapped, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
        "missing admin users should still be treated as authentication failure, got: {mapped:?}"
    );
}

fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
    synctv_core::models::RoomMemberWithUser {
        room_id: RoomId::expect_positive(110_002),
        user_id: UserId::expect_positive(110_003),
        username: "testmember".to_string(),
        remark_name: "Test Member".to_string(),
        display_tag: "VIP".to_string(),
        role,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: synctv_core::SystemClock.now(),
        is_online: false,
        is_active: true,
    }
}

#[test]
fn test_admin_room_member_to_proto() -> TestResult {
    let member = make_test_member(RoomRole::Admin);
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let proto = try_admin_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(member.role.permissions()),
        &public_id_codec,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(
        proto.room_id,
        public_id_codec
            .encode_room_id(member.room_id)
            .map_err(test_error)?
    );
    assert_eq!(
        proto.user_id,
        public_id_codec
            .encode_user_id(member.user_id)
            .map_err(test_error)?
    );
    assert_eq!(proto.username, "testmember");
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert!(!proto.is_online);
    Ok(())
}

#[test]
fn test_admin_room_member_to_proto_with_permissions() -> TestResult {
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xAA;
    member.removed_permissions = 0x55;
    member.admin_added_permissions = 0xCC;
    member.admin_removed_permissions = 0x33;
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let proto = try_admin_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(member.role.permissions()),
        &public_id_codec,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(proto.added_permissions, 0xAA);
    assert_eq!(proto.removed_permissions, 0x55);
    assert_eq!(proto.admin_added_permissions, 0xCC);
    assert_eq!(proto.admin_removed_permissions, 0x33);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_admins_includes_root_and_admin_only() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);
    let now = synctv_core::SystemClock.now();

    for user in [
        synctv_core::models::User {
            id: UserId::new(),
            username: "root-zeta".to_string(),
            role: UserRole::Root,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method: synctv_core::models::SignupMethod::Email,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        },
        synctv_core::models::User {
            id: UserId::new(),
            username: "admin-alpha".to_string(),
            role: UserRole::Admin,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method: synctv_core::models::SignupMethod::Email,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        },
        synctv_core::models::User {
            id: UserId::new(),
            username: "user-ignored".to_string(),
            role: UserRole::User,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method: synctv_core::models::SignupMethod::Email,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        },
    ] {
        core_ok(user_repo.create(&user).await)?;
    }

    let response = api_ok(
        admin_api
            .list_admins(synctv_proto::admin::ListAdminsRequest {
                page: 1,
                page_size: 10,
                search: String::new(),
                sort_by: synctv_proto::admin::UserListSortBy::Username as i32,
                sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
            })
            .await,
    )?;

    let usernames: Vec<String> = response
        .admins
        .into_iter()
        .map(|user| user.username)
        .collect();
    assert_eq!(
        usernames,
        vec!["admin-alpha".to_string(), "root-zeta".to_string()]
    );
    assert_eq!(response.total, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_list_endpoints_reject_invalid_proto_requests() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;

    let list_rooms_error = api_err(
        admin_api
            .list_rooms(synctv_proto::admin::ListRoomsRequest {
                page: -1,
                page_size: 101,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: String::new(),
                creator_id: String::new(),
                is_banned: None,
                sort_by: synctv_proto::admin::RoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
                category_id: String::new(),
                label_ids: Vec::new(),
            })
            .await,
    )?;
    assert!(list_rooms_error.is_invalid_argument());

    let get_room_members_error = api_err(
        admin_api
            .get_room_members(synctv_proto::admin::GetRoomMembersRequest {
                room_id: "room_abc123def456".to_string(),
                page: -1,
                page_size: 101,
                search: "a".repeat(101),
                role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
                sort_by: synctv_proto::admin::RoomMemberListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
            })
            .await,
    )?;
    assert!(get_room_members_error.is_invalid_argument());

    let get_user_rooms_error = api_err(
        admin_api
            .get_user_rooms(synctv_proto::admin::GetUserRoomsRequest {
                user_id: "usr_abc123def456".to_string(),
                page: -1,
                page_size: 101,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "a".repeat(101),
                is_banned: None,
                sort_by: synctv_proto::admin::RoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
            })
            .await,
    )?;
    assert!(get_user_rooms_error.is_invalid_argument());

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_client_list_endpoints_reject_invalid_proto_requests() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);
    let admin_user = create_db_user(&user_repo, "proto_list_admin", UserRole::Root).await;

    let list_playlists_error = api_err(
        admin_api
            .list_playlists(
                "abc123def456",
                synctv_proto::client::ListPlaylistsRequest {
                    parent_id: String::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: 99,
                    provider_instance_name: "bad name".to_string(),
                    dynamic_only: None,
                    sort_by: synctv_proto::client::PlaylistListSortBy::Unspecified as i32,
                    sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
                    availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &admin_user.id,
            )
            .await,
    )?;
    assert!(list_playlists_error.is_invalid_argument());

    let list_media_error = api_err(
        admin_api
            .list_media(
                "abc123def456",
                synctv_proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: None,
                    pagination: Some(
                        synctv_proto::client::list_playlist_items_request::Pagination::Page(
                            synctv_proto::client::PagePagination { page: 1 },
                        ),
                    ),
                    page_size: 20,
                    search: String::new(),
                    source_provider: 99,
                    provider_instance_name: "bad name".to_string(),
                    sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
                    sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
                    availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                    refresh: false,
                    preview_source_config: None,
                },
                &admin_user.id,
            )
            .await,
    )?;
    assert!(list_media_error.is_invalid_argument());
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_admins_respects_search_sort_and_pagination() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);

    for (username, role) in [
        ("zzz-admin", UserRole::Admin),
        ("alpha-admin", UserRole::Admin),
        ("plain-user", UserRole::User),
    ] {
        create_db_user(&user_repo, username, role).await;
    }

    let response = api_ok(
        admin_api
            .list_admins(synctv_proto::admin::ListAdminsRequest {
                page: 1,
                page_size: 1,
                search: "admin".to_string(),
                sort_by: synctv_proto::admin::UserListSortBy::Username as i32,
                sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
            })
            .await,
    )?;

    assert_eq!(response.admins.len(), 1);
    assert_eq!(response.admins[0].username, "alpha-admin");
    assert_eq!(response.total, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_user_rooms_respects_related_room_query_semantics() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let target_user = create_db_user(&user_repo, "target-user-rooms", UserRole::User).await;
    let other_owner = create_db_user(&user_repo, "other-owner-rooms", UserRole::User).await;

    let owned_room = core_ok(
        admin_api
            .room_service
            .create_room(
                "Beta Owned Room".to_string(),
                "owned room".to_string(),
                target_user.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let joined_room = core_ok(
        admin_api
            .room_service
            .create_room(
                "Alpha Joined Room".to_string(),
                "joined room".to_string(),
                other_owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    core_ok(
        admin_api
            .room_service
            .join_room(joined_room.id, target_user.id, None)
            .await,
    )?;

    let response = api_ok(
        admin_api
            .get_user_rooms(synctv_proto::admin::GetUserRoomsRequest {
                user_id: public_user_id(&admin_api, target_user.id),
                page: 1,
                page_size: 1,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "room".to_string(),
                is_banned: Some(false),
                sort_by: synctv_proto::admin::RoomListSortBy::Name as i32,
                sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
            })
            .await,
    )?;

    assert_eq!(response.total, 2);
    assert_eq!(response.rooms.len(), 1);
    assert_eq!(response.rooms[0].name, "Alpha Joined Room");
    assert_eq!(
        response.rooms[0].id,
        public_room_id(&admin_api, joined_room.id)
    );

    let page2 = api_ok(
        admin_api
            .get_user_rooms(synctv_proto::admin::GetUserRoomsRequest {
                user_id: public_user_id(&admin_api, target_user.id),
                page: 2,
                page_size: 1,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "room".to_string(),
                is_banned: Some(false),
                sort_by: synctv_proto::admin::RoomListSortBy::Name as i32,
                sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
            })
            .await,
    )?;

    assert_eq!(page2.total, 2);
    assert_eq!(page2.rooms.len(), 1);
    assert_eq!(page2.rooms[0].name, "Beta Owned Room");
    assert_eq!(page2.rooms[0].id, public_room_id(&admin_api, owned_room.id));
    Ok(())
}

// Role hierarchy rules enforced by set_user_password.

/// Helper for password reset role checks.
fn password_reset_allowed(caller_role: UserRole, target_role: UserRole) -> bool {
    if target_role == UserRole::Root && caller_role != UserRole::Root {
        return false;
    }
    if target_role == UserRole::Admin && caller_role != UserRole::Root {
        return false;
    }
    true
}

#[test]
fn test_root_can_reset_any_password() {
    assert!(password_reset_allowed(UserRole::Root, UserRole::Root));
    assert!(password_reset_allowed(UserRole::Root, UserRole::Admin));
    assert!(password_reset_allowed(UserRole::Root, UserRole::User));
}

#[test]
fn test_admin_cannot_reset_root_password() {
    assert!(!password_reset_allowed(UserRole::Admin, UserRole::Root));
}

#[test]
fn test_admin_cannot_reset_other_admin_password() {
    assert!(!password_reset_allowed(UserRole::Admin, UserRole::Admin));
}

#[test]
fn test_admin_can_reset_user_password() {
    assert!(password_reset_allowed(UserRole::Admin, UserRole::User));
}

#[test]
fn test_check_role_hierarchy_root_can_operate_on_all() {
    assert!(check_role_hierarchy(UserRole::Root, UserRole::Root, "ban").is_ok());
    assert!(check_role_hierarchy(UserRole::Root, UserRole::Admin, "ban").is_ok());
    assert!(check_role_hierarchy(UserRole::Root, UserRole::User, "ban").is_ok());
}

fn assert_authorization_error(result: Result<(), ApiError>, expected: &str) -> TestResult {
    match result {
        Err(ApiError::Authorization(msg)) => {
            assert!(
                msg.contains(expected),
                "authorization error should mention {expected}: {msg}"
            );
            Ok(())
        }
        Ok(()) => Err(test_error("expected authorization error")),
        Err(error) => Err(test_error(format!(
            "expected authorization error: {error:?}"
        ))),
    }
}

#[test]
fn test_check_role_hierarchy_admin_cannot_operate_on_root() -> TestResult {
    assert_authorization_error(
        check_role_hierarchy(UserRole::Admin, UserRole::Root, "ban"),
        "root",
    )
}

#[test]
fn test_check_role_hierarchy_admin_cannot_operate_on_admin() -> TestResult {
    assert_authorization_error(
        check_role_hierarchy(UserRole::Admin, UserRole::Admin, "delete"),
        "root",
    )
}

#[test]
fn test_check_role_hierarchy_admin_cannot_update_admin_preferences() -> TestResult {
    assert_authorization_error(
        check_role_hierarchy(UserRole::Admin, UserRole::Admin, "update preferences"),
        "admin",
    )
}

#[test]
fn test_check_role_hierarchy_admin_can_operate_on_user() {
    assert!(check_role_hierarchy(UserRole::Admin, UserRole::User, "ban").is_ok());
}

/// Verify that proto_role_to_user_role maps Admin role correctly
/// (prerequisite for the role elevation check).
#[test]
fn test_proto_role_to_user_role_admin() -> TestResult {
    let admin_role = api_ok(crate::impls::client::proto_role_to_user_role(
        synctv_proto::common::UserRole::Admin as i32,
    ))?;
    assert_eq!(admin_role, UserRole::Admin);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_settings_projects_registered_defaults() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let settings = current_admin_settings(&admin_api).await?;
    assert_eq!(
        settings
            .server
            .as_ref()
            .ok_or_else(|| test_error("expected server settings"))?
            .name,
        "SyncTV"
    );
    let room_defaults = admin_room_defaults_settings(&settings)?;
    let room_creation = admin_room_creation_settings(&settings)?;
    assert!(room_creation.enabled);
    assert_eq!(room_creation.max_rooms_per_user, 10);
    assert_eq!(room_defaults.default_max_members, 100);
    assert_eq!(room_defaults.default_max_chat_messages, 500);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_runtime_settings_patch_updates_only_present_fields() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (_admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;
    let current = test_runtime_settings();
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                server: Some(synctv_proto::admin::ServerSettingsPatch {
                    name: Some("Family TV".to_string()),
                }),
                room_creation: Some(synctv_proto::admin::RoomCreationSettingsPatch {
                    max_rooms_per_user: Some(42),
                    ..Default::default()
                }),
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    smtp_proxy: Some(synctv_proto::admin::SmtpProxy {
                        url: "socks5://proxy.example.com:1080".to_string(),
                        credentials: Some(synctv_proto::admin::SmtpCredentials {
                            username: "proxy-user".to_string(),
                            password: Some("next-proxy-secret".to_string()),
                        }),
                    }),
                    whitelist_domains: Vec::new(),
                    ..Default::default()
                }),
                cors: Some(synctv_proto::admin::CorsSettingsPatch {
                    allowed_origins: Vec::new(),
                }),
                playback_history: Some(synctv_proto::admin::PlaybackHistorySettingsPatch {
                    retention_days: Some(30),
                    max_entries_per_room: Some(500),
                }),
                ..Default::default()
            },
            &[
                "server.name",
                "room_creation.max_rooms_per_user",
                "email.smtp_credentials",
                "email.smtp_proxy",
                "email.whitelist_domains",
                "cors.allowed_origins",
                "playback_history.retention_days",
                "playback_history.max_entries_per_room",
            ],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let patch_result = AdminApiImpl::apply_runtime_settings_patch(current, patch)
        .map_err(|error| test_error(format!("{error:?}")))?;

    let patched = patch_result.settings;
    assert_eq!(patched.server.name, "Family TV");
    assert_eq!(patched.room_creation.max_rooms_per_user, 42);
    assert!(patched.room_creation.enabled);
    assert_eq!(patched.email.smtp_host.as_deref(), Some("smtp.example.com"));
    assert_eq!(patched.email.smtp_credentials, None);
    assert_eq!(
        patched
            .email
            .smtp_proxy
            .as_ref()
            .and_then(|proxy| proxy.credentials.as_ref())
            .map(|credentials| credentials.password.as_str()),
        Some("next-proxy-secret")
    );
    assert!(patched.email.whitelist_domains.is_empty());
    assert!(patched.cors.allowed_origins.0.is_empty());
    assert_eq!(patched.playback_history.retention_days, 30);
    assert_eq!(patched.playback_history.max_entries_per_room, 500);
    assert!(patch_result.update_mask.server.name);
    assert!(patch_result.update_mask.room_creation.max_rooms_per_user);
    assert!(!patch_result.update_mask.room_creation.enabled);
    assert!(patch_result.update_mask.email.smtp_credentials);
    assert!(patch_result.update_mask.email.smtp_proxy);
    assert!(!patch_result.update_mask.email.smtp_host);
    assert!(patch_result.update_mask.email.whitelist_domains);
    assert!(patch_result.update_mask.cors.allowed_origins);
    assert!(patch_result.update_mask.playback_history.retention_days);
    assert!(
        patch_result
            .update_mask
            .playback_history
            .max_entries_per_room
    );
    Ok(())
}

#[test]
fn test_runtime_settings_field_mask_ignores_values_outside_mask() -> TestResult {
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    enabled: Some(true),
                    smtp_port: Some(2525),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["email.enabled"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    let email = patch.email.ok_or_else(|| test_error("email patch"))?;
    assert_eq!(email.enabled, Some(true));
    assert_eq!(email.smtp_port, None);
    Ok(())
}

#[test]
fn test_server_name_runtime_settings_patch() -> TestResult {
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                server: Some(synctv_proto::admin::ServerSettingsPatch {
                    name: Some("Family TV".to_string()),
                }),
                ..Default::default()
            },
            &["server.name"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    let result = AdminApiImpl::apply_runtime_settings_patch(test_runtime_settings(), patch)
        .map_err(|error| test_error(format!("{error:?}")))?;
    assert_eq!(result.settings.server.name, "Family TV");
    assert!(result.update_mask.server.name);
    assert!(result.update_mask.room_defaults.is_empty());
    Ok(())
}

#[test]
fn test_runtime_settings_field_mask_maps_repeated_fields_directly() -> TestResult {
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                oauth2: Some(synctv_proto::admin::OAuth2SettingsPatch {
                    providers: Vec::new(),
                }),
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    whitelist_domains: vec!["example.com".to_string()],
                    ..Default::default()
                }),
                webrtc: Some(synctv_proto::admin::WebRtcSettingsPatch {
                    external_ice_servers: Vec::new(),
                    max_voice_participants_per_room: Some(12),
                }),
                cors: Some(synctv_proto::admin::CorsSettingsPatch {
                    allowed_origins: vec!["https://app.example.com".to_string()],
                }),
                ..Default::default()
            },
            &[
                "oauth2.providers",
                "email.whitelist_domains",
                "webrtc.external_ice_servers",
                "webrtc.max_voice_participants_per_room",
                "cors.allowed_origins",
            ],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert!(patch
        .oauth2
        .and_then(|section| section.providers)
        .is_some_and(|providers| providers.0.is_empty()));
    assert_eq!(
        patch.email.and_then(|section| section.whitelist_domains),
        Some(vec!["example.com".to_string()])
    );
    let webrtc = patch
        .webrtc
        .ok_or_else(|| test_error("WebRTC patch should be present"))?;
    assert_eq!(webrtc.external_ice_servers, Some(Vec::new()));
    assert_eq!(webrtc.max_voice_participants_per_room, Some(12));
    assert_eq!(
        patch.cors.and_then(|section| section.allowed_origins),
        Some(vec!["https://app.example.com".to_string()])
    );
    Ok(())
}

#[test]
fn test_runtime_settings_field_mask_requires_non_optional_values() {
    let result = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch::default()),
                ..Default::default()
            },
            &["email.smtp_port"],
        ),
    );

    assert!(matches!(
        result,
        Err(ApiError::InvalidInput(message))
            if message.contains("email.smtp_port is required by update_mask")
    ));
}

#[test]
fn test_runtime_settings_field_mask_rejects_invalid_paths() {
    let cases = [
        (Vec::<&str>::new(), "must not be empty"),
        (vec!["email"], "unsupported update_mask path 'email'"),
        (
            vec!["email.unknown_field"],
            "unsupported update_mask path 'email.unknown_field'",
        ),
        (
            vec!["email.enabled", "email.enabled"],
            "duplicate update_mask path 'email.enabled'",
        ),
    ];

    for (paths, expected) in cases {
        let result = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
            runtime_settings_request(
                synctv_proto::admin::RuntimeSettingsPatch {
                    email: Some(synctv_proto::admin::EmailSettingsPatch {
                        enabled: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                &paths,
            ),
        );
        assert!(
            matches!(result, Err(ApiError::InvalidInput(ref message)) if message.contains(expected)),
            "expected FieldMask error containing {expected:?}, got {result:?}"
        );
    }
}

#[test]
fn test_smtp_proxy_runtime_patch_and_projection() -> TestResult {
    let current = test_runtime_settings();
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    smtp_proxy: Some(synctv_proto::admin::SmtpProxy {
                        url: "socks5://next-proxy.example.com:1081".to_string(),
                        credentials: Some(synctv_proto::admin::SmtpCredentials {
                            username: "next-proxy-user".to_string(),
                            password: Some("next-proxy-secret".to_string()),
                        }),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["email.smtp_proxy"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let patch_result = AdminApiImpl::apply_runtime_settings_patch(current, patch)
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert!(patch_result.update_mask.email.smtp_proxy);
    assert!(!patch_result.update_mask.email.smtp_host);

    let projected = AdminApiImpl::project_admin_settings(patch_result.settings)
        .map_err(|error| test_error(format!("{error:?}")))?;
    let email = admin_email_settings(&projected)?;
    let proxy = some_value(email.smtp_proxy.as_ref(), "SMTP proxy")?;
    assert_eq!(proxy.url, "socks5://next-proxy.example.com:1081");
    let credentials = some_value(proxy.credentials.as_ref(), "SMTP proxy credentials")?;
    assert_eq!(credentials.username, "next-proxy-user");
    assert_eq!(credentials.password, None);
    assert_eq!(
        some_value(email.smtp_credentials.as_ref(), "SMTP credentials")?.password,
        None
    );
    Ok(())
}

#[test]
fn test_smtp_credentials_patch_preserves_password_for_unchanged_username() -> TestResult {
    let current = test_runtime_settings();
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    smtp_credentials: Some(synctv_proto::admin::SmtpCredentials {
                        username: "smtp-user".to_string(),
                        password: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["email.smtp_credentials"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let patched = AdminApiImpl::apply_runtime_settings_patch(current, patch)
        .map_err(|error| test_error(format!("{error:?}")))?
        .settings;

    let credentials = some_value(patched.email.smtp_credentials.as_ref(), "SMTP credentials")?;
    assert_eq!(credentials.username, "smtp-user");
    assert_eq!(credentials.password, "smtp-secret");
    Ok(())
}

#[test]
fn test_smtp_credentials_patch_requires_password_for_username_change() -> TestResult {
    let current = test_runtime_settings();
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch {
                    smtp_credentials: Some(synctv_proto::admin::SmtpCredentials {
                        username: "next-user".to_string(),
                        password: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["email.smtp_credentials"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    assert!(matches!(
        AdminApiImpl::apply_runtime_settings_patch(current, patch),
        Err(ApiError::InvalidInput(message)) if message.contains("password is required")
    ));
    Ok(())
}

#[test]
fn test_optional_rtmp_publish_host_supports_set_and_clear() -> TestResult {
    let current = test_runtime_settings();
    let set_patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                rtmp: Some(synctv_proto::admin::RtmpSettingsPatch {
                    custom_publish_host: Some("rtmp://live.example.com".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["rtmp.custom_publish_host"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let set_result = AdminApiImpl::apply_runtime_settings_patch(current, set_patch)
        .map_err(|error| test_error(format!("{error:?}")))?;
    assert_eq!(
        set_result.settings.rtmp.custom_publish_host.as_deref(),
        Some("rtmp://live.example.com")
    );
    assert!(set_result.update_mask.rtmp.custom_publish_host);

    let clear_patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                rtmp: Some(synctv_proto::admin::RtmpSettingsPatch::default()),
                ..Default::default()
            },
            &["rtmp.custom_publish_host"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let clear_result = AdminApiImpl::apply_runtime_settings_patch(set_result.settings, clear_patch)
        .map_err(|error| test_error(format!("{error:?}")))?;
    assert_eq!(clear_result.settings.rtmp.custom_publish_host, None);
    assert!(clear_result.update_mask.rtmp.custom_publish_host);
    Ok(())
}

#[test]
fn test_optional_email_address_fields_support_clear() -> TestResult {
    let current = test_runtime_settings();
    let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                email: Some(synctv_proto::admin::EmailSettingsPatch::default()),
                ..Default::default()
            },
            &["email.smtp_host", "email.from_email"],
        ),
    )
    .map_err(|error| test_error(format!("{error:?}")))?;
    let result = AdminApiImpl::apply_runtime_settings_patch(current, patch)
        .map_err(|error| test_error(format!("{error:?}")))?;

    assert_eq!(result.settings.email.smtp_host, None);
    assert_eq!(result.settings.email.from_email, None);
    assert!(result.update_mask.email.smtp_host);
    assert!(result.update_mask.email.from_email);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_settings_omits_nested_smtp_credential_passwords() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let registry = some_value(
        admin_api.runtime_settings_store.as_ref(),
        "runtime settings store",
    )?;
    let mut runtime_settings = registry
        .runtime_settings()
        .map_err(|error| test_error(error.to_string()))?;
    runtime_settings.email.smtp_credentials = Some(synctv_core::service::SmtpCredentials {
        username: "smtp-user".to_string(),
        password: "smtp-secret".to_string(),
    });
    runtime_settings.email.smtp_proxy = Some(synctv_core::service::SmtpProxyConfig {
        url: "socks5://proxy.example.com:1080".to_string(),
        credentials: Some(synctv_core::service::SmtpCredentials {
            username: "proxy-user".to_string(),
            password: "proxy-secret".to_string(),
        }),
    });
    core_ok(registry.persist_runtime_settings(&runtime_settings).await)?;

    let settings = current_admin_settings(&admin_api).await?;
    let email = admin_email_settings(&settings)?;
    assert!(!email.enabled);
    let credentials = some_value(email.smtp_credentials.as_ref(), "SMTP credentials")?;
    assert_eq!(credentials.username, "smtp-user");
    assert_eq!(credentials.password, None);
    let proxy = some_value(email.smtp_proxy.as_ref(), "SMTP proxy")?;
    assert_eq!(proxy.url, "socks5://proxy.example.com:1080");
    let proxy_credentials = some_value(proxy.credentials.as_ref(), "SMTP proxy credentials")?;
    assert_eq!(proxy_credentials.username, "proxy-user");
    assert_eq!(proxy_credentials.password, None);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_settings_ignores_hidden_registered_settings_without_warning_path() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let runtime_settings_store = some_value(
        admin_api.runtime_settings_store.as_ref(),
        "runtime settings store",
    )?;
    let server_id = core_ok(runtime_settings_store.get_or_initialize_server_id().await)?;
    assert!(server_id.starts_with("srv_"));

    let settings = current_admin_settings(&admin_api).await?;
    let room_creation = admin_room_creation_settings(&settings)?;
    assert_eq!(room_creation.max_rooms_per_user, 10);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_settings_maps_admin_settings_to_flat_keys_and_upserts_missing_rows(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    update_admin_settings(&admin_api, room_creation_max_rooms_per_user_patch(42)).await?;

    let max_rooms_per_user = core_ok(
        admin_api
            .settings_service
            .get(synctv_core::service::MaxRoomsPerUserSetting::KEY)
            .await,
    )?;
    assert_eq!(max_rooms_per_user.group_name, "room_creation");
    assert_eq!(max_rooms_per_user.value, "42");
    assert!(
        admin_api
            .settings_service
            .get(synctv_core::service::RoomCreationEnabledSetting::KEY)
            .await
            .is_err(),
        "partial settings patch should only write present fields"
    );

    let settings = current_admin_settings(&admin_api).await?;
    let room_creation = admin_room_creation_settings(&settings)?;
    assert_eq!(room_creation.max_rooms_per_user, 42);
    assert!(room_creation.enabled);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_settings_persists_when_global_cache_invalidation_fanout_fails() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;
    let failing_fanout = Arc::new(FailingRealtimeFanout::default());
    let admin_api = AdminApiImpl::new_with_runtime(
        AdminApiOptions {
            room_service: admin_api.room_service.clone(),
            user_service: admin_api.user_service.clone(),
            read_services: AdminReadServices {
                system_stats_service: admin_api.system_stats_service.clone(),
                review_service: admin_api.review_service.clone(),
                ban_record_service: admin_api.ban_record_service.clone(),
                content_report_service: admin_api.content_report_service.clone(),
            },
            settings_service: admin_api.settings_service.clone(),
            runtime_settings_store: admin_api.runtime_settings_store.clone(),
            email_service: admin_api.email_service.clone(),
            connection_service: admin_api.connection_service.clone(),
            provider_instance_manager: admin_api.provider_instance_manager.clone(),
            live_streaming_infrastructure: admin_api.live_streaming_infrastructure.clone(),
            publish_key_service: admin_api.publish_key_service.clone(),
            runtime_settings: admin_api.runtime_settings.clone(),
            audit_service: admin_api.audit_service.clone(),
            public_id_codec: admin_api.public_id_codec.clone(),
        },
        AdminApiRuntime {
            clock: Arc::new(synctv_core::SystemClock),
            realtime_fanout: failing_fanout.clone(),
            realtime_event_service: admin_api.realtime_event_service.clone(),
            provider_stores: admin_api.provider_stores.clone(),
            provider_access_service: admin_api.provider_access_service.clone(),
            signing_key: admin_api.signing_key.clone(),
            media_swarm_signing_key: admin_api.media_swarm_signing_key.clone(),
            presence_service: admin_api.presence_service.clone(),
            request_executor: admin_api.request_executor.clone(),
        },
    );

    update_admin_settings(&admin_api, room_creation_max_rooms_per_user_patch(43)).await?;

    let max_rooms_per_user = core_ok(
        admin_api
            .settings_service
            .get(synctv_core::service::MaxRoomsPerUserSetting::KEY)
            .await,
    )?;
    assert_eq!(max_rooms_per_user.value, "43");
    assert_eq!(failing_fanout.publish_attempts(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_email_settings_rejects_enabled_incomplete_config() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let error = api_err(
        admin_api
            .update_settings(
                email_settings_patch(true, "", 587, ""),
                &UserId::new(),
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(
        matches!(error, ApiError::InvalidInput(ref message) if message.contains(synctv_core::service::EmailSmtpHostSetting::KEY)),
        "expected missing smtp_host validation error, got: {error:?}"
    );
    assert!(
        admin_api
            .settings_service
            .get(synctv_core::service::EmailEnabledSetting::KEY)
            .await
            .is_err(),
        "failed email settings update keeps email.enabled absent"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_email_settings_accepts_enabled_complete_config() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    update_admin_settings(
        &admin_api,
        email_settings_patch(true, "smtp.example.com", 587, "noreply@example.com"),
    )
    .await?;

    let enabled = core_ok(
        admin_api
            .settings_service
            .get(synctv_core::service::EmailEnabledSetting::KEY)
            .await,
    )?;
    assert_eq!(enabled.value, "true");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_runtime_settings_persist_rejects_enabled_incomplete_email_config() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;
    let registry = some_value(
        admin_api.runtime_settings_store.as_ref(),
        "runtime settings store",
    )?;
    let mut runtime_settings = registry
        .runtime_settings()
        .map_err(|error| test_error(error.to_string()))?;
    runtime_settings.email.enabled = true;

    let Err(error) = registry.persist_runtime_settings(&runtime_settings).await else {
        return Err(test_error("expected runtime settings persist error"));
    };

    assert!(
        matches!(error, synctv_core::Error::InvalidInput(ref message) if message.contains(synctv_core::service::EmailSmtpHostSetting::KEY)),
        "expected missing smtp_host validation error, got: {error:?}"
    );
    assert!(
        admin_api
            .settings_service
            .get(synctv_core::service::EmailEnabledSetting::KEY)
            .await
            .is_err(),
        "failed core service update keeps email.enabled absent"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_runtime_settings_patch_uses_role_specific_permission_validation() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    update_admin_settings(
        &admin_api,
        runtime_settings_request(
            synctv_proto::admin::RuntimeSettingsPatch {
                permissions: Some(synctv_proto::admin::PermissionSettingsPatch {
                    admin_default_permissions: Some(
                        synctv_core::models::RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE,
                    ),
                    member_default_permissions: Some(
                        synctv_core::models::RoomAdminPermissionBits::SEND_CHAT_MESSAGES,
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &[
                "permissions.admin_default_permissions",
                "permissions.member_default_permissions",
            ],
        ),
    )
    .await?;

    let error = api_err(
        admin_api
            .update_settings(
                runtime_settings_request(
                    synctv_proto::admin::RuntimeSettingsPatch {
                        permissions: Some(synctv_proto::admin::PermissionSettingsPatch {
                            guest_default_permissions: Some(
                                synctv_core::models::RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE,
                            ),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    &["permissions.guest_default_permissions"],
                ),
                &UserId::new(),
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(
        matches!(error, ApiError::InvalidInput(ref message) if message.contains(synctv_core::service::GuestDefaultPermissionsSetting::KEY)),
        "expected guest permission validation error, got: {error:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_publishes_kick_user_realtime_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let admin_api = {
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        (admin_api, redis_publish_rx)
    };
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_admin", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "victim_user", UserRole::User).await;

    let (admin_api, mut redis_publish_rx) = admin_api;
    let ctx = RequestContext::default();

    api_ok(
        admin_api
            .delete_user(
                synctv_proto::admin::DeleteUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                },
                &admin_user.id,
                &ctx,
            )
            .await,
    )?;

    let publish = tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
        .await
        .map_err(|error| test_error(format!("expected cluster publish: {error}")))?;
    let publish = some_value(publish, "publish request")?;

    match publish.event {
        RealtimeEvent::KickUser {
            user_id, reason, ..
        } => {
            assert_eq!(user_id, target_user.id);
            assert_eq!(reason, "user_deleted");
        }
        other => {
            return Err(test_error(format!(
                "expected KickUser event, got {other:?}"
            )))
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_allows_missing_email_and_explicit_status() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let admin_user = create_db_user(&user_repo, "root_create_user_attrs", UserRole::Root).await;

    let response = api_ok(
        admin_api
            .create_user(
                synctv_proto::admin::CreateUserRequest {
                    username: "attr_user".to_string(),
                    email: String::new(),
                    role: synctv_proto::common::UserRole::Admin as i32,
                    status: synctv_proto::common::UserStatus::Active as i32,
                    password: String::new(),
                },
                UserRole::Root,
                &admin_user.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let created = response;
    assert_eq!(created.username, "attr_user");
    assert_eq!(created.email, "");
    assert_eq!(created.role, synctv_proto::common::UserRole::Admin as i32);
    assert_eq!(
        created.status,
        synctv_proto::common::UserStatus::Active as i32
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_user_username_preserves_missing_email() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_update_username", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "target_update_username", UserRole::User).await;

    let response = api_ok(
        admin_api
            .update_user_username(
                synctv_proto::admin::UpdateUserUsernameRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                    new_username: "target_update_renamed".to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let updated = response;
    assert_eq!(updated.username, "target_update_renamed");
    assert_eq!(updated.email, "");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_cleans_memberships_and_preserves_kick_user_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_delete_membership", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "victim_membership", UserRole::User).await;
    let owner_one = create_db_user(&user_repo, "room_owner_one", UserRole::User).await;
    let owner_two = create_db_user(&user_repo, "room_owner_two", UserRole::User).await;

    let room_one = Box::pin(create_room_with_member(
        &admin_api,
        &owner_one.id,
        &target_user.id,
    ))
    .await;
    let room_two = Box::pin(create_room_with_member(
        &admin_api,
        &owner_two.id,
        &target_user.id,
    ))
    .await;

    api_ok(
        admin_api
            .delete_user(
                synctv_proto::admin::DeleteUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let room_one_member = core_ok(
        admin_api
            .room_service
            .get_member(&room_one.id, &target_user.id)
            .await,
    )?;
    assert!(
        room_one_member.is_none(),
        "deleted user must no longer appear as an active room member"
    );

    let room_two_member = core_ok(
        admin_api
            .room_service
            .get_member(&room_two.id, &target_user.id)
            .await,
    )?;
    assert!(
        room_two_member.is_none(),
        "deleted user must no longer appear as an active room member"
    );

    let mut saw_kick_user = false;
    while let Ok(Some(publish)) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        redis_publish_rx.recv(),
    )
    .await
    {
        if let RealtimeEvent::KickUser {
            user_id, reason, ..
        } = publish.event
        {
            assert_eq!(user_id, target_user.id);
            assert_eq!(reason, "user_deleted");
            saw_kick_user = true;
        }
    }

    assert!(saw_kick_user, "delete_user must still publish KickUser");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_deletes_owned_rooms_and_publishes_room_deleted() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_delete_owned_room", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "owned_room_victim", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                "victim owned room".to_string(),
                "will be deleted with owner".to_string(),
                target_user.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    api_ok(
        admin_api
            .delete_user(
                synctv_proto::admin::DeleteUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(
        core_ok(room_repo.get_by_id(&room.id).await)?.is_none(),
        "owned room must be deleted with the user"
    );

    let mut saw_room_deleted = false;
    let mut saw_kick_user = false;
    while let Ok(Some(publish)) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        redis_publish_rx.recv(),
    )
    .await
    {
        match publish.event {
            RealtimeEvent::RoomDeleted {
                room_id,
                deleted_by,
                ..
            } => {
                assert_eq!(room_id, room.id);
                assert_eq!(deleted_by, admin_user.id);
                saw_room_deleted = true;
            }
            RealtimeEvent::KickUser {
                user_id, reason, ..
            } => {
                assert_eq!(user_id, target_user.id);
                assert_eq!(reason, "user_deleted");
                saw_kick_user = true;
            }
            _ => {}
        }
    }

    assert!(
        saw_room_deleted,
        "delete_user must publish RoomDeleted for owned rooms"
    );
    assert!(saw_kick_user, "delete_user must still publish KickUser");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_cleans_memberships_and_preserves_kick_user_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_user_ban_cleanup", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_membership", UserRole::User).await;
    let owner = create_db_user(&user_repo, "room_owner_ban", UserRole::User).await;

    let room = Box::pin(create_room_with_member(
        &admin_api,
        &owner.id,
        &target_user.id,
    ))
    .await;

    let response = api_ok(
        admin_api
            .ban_user(
                synctv_proto::admin::BanUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    let banned_user = response;
    assert_eq!(
        banned_user.status,
        synctv_proto::common::UserStatus::Banned as i32
    );
    assert_eq!(
        banned_user.banned_by,
        public_user_id(&admin_api, admin_user.id)
    );
    assert_eq!(banned_user.banned_reason, "policy");

    let room_member = core_ok(
        admin_api
            .room_service
            .get_member(&room.id, &target_user.id)
            .await,
    )?;
    assert!(
        room_member.is_none(),
        "banned user must no longer appear as an active room member"
    );

    let mut saw_kick_user = false;
    while let Ok(Some(publish)) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        redis_publish_rx.recv(),
    )
    .await
    {
        if let RealtimeEvent::KickUser {
            user_id, reason, ..
        } = publish.event
        {
            assert_eq!(user_id, target_user.id);
            assert_eq!(reason, "user_banned");
            saw_kick_user = true;
        }
    }

    assert!(saw_kick_user, "ban_user must still publish KickUser");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_resets_playback_for_media_created_by_target() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_playback", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_playback_creator", UserRole::User).await;
    let room_owner = create_db_user(&user_repo, "playback_room_owner", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                "ban-playback-room".to_string(),
                String::new(),
                room_owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, target_user.id, "banned-media").await;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, room_owner.id, Some(media.id), None, None)
            .await,
    )?;

    api_ok(
        admin_api
            .ban_user(
                synctv_proto::admin::BanUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    let state = core_ok(
        admin_api
            .room_service
            .playback_service()
            .get_state(&room.id)
            .await,
    )?;

    assert!(
        state.playing_media_id.is_none(),
        "playback media must be cleared after banning its creator"
    );
    assert!(
        state.playing_playlist_id.is_none(),
        "playback playlist must be cleared after banning the media creator"
    );
    assert!(
        !state.is_playing,
        "playback must be stopped after banning the media creator"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_disconnects_owned_room_connections() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_owned_room", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_owned_room_creator", UserRole::User).await;
    let member_user = create_db_user(&user_repo, "owned_room_member", UserRole::User).await;

    let room = Box::pin(create_room_with_member(
        &admin_api,
        &target_user.id,
        &member_user.id,
    ))
    .await;

    let mut disconnect_rx = admin_api.connection_service.subscribe_disconnect();
    admin_api
        .connection_service
        .register("owned-room-conn".to_string(), member_user.id)
        .await
        .map_err(|error| test_error(error.clone()))?;
    admin_api
        .connection_service
        .join_room("owned-room-conn", room.id)
        .await
        .map_err(|error| test_error(error.clone()))?;

    api_ok(
        admin_api
            .ban_user(
                synctv_proto::admin::BanUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut saw_room_disconnect = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let signal = tokio::time::timeout(remaining, disconnect_rx.recv())
            .await
            .map_err(|error| test_error(format!("disconnect signal timeout: {error}")))?;
        let signal = signal.map_err(|error| test_error(format!("disconnect channel: {error}")))?;

        if let synctv_realtime::sync::DisconnectSignal::Room(room_id) = signal {
            assert_eq!(room_id, room.id, "owned room must be disconnected");
            saw_room_disconnect = true;
            break;
        }
    }

    assert!(
        saw_room_disconnect,
        "banning a room owner must emit a room-scoped disconnect signal"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_publishes_room_owner_inactive_event_for_owned_rooms() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_owned_room_event", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "owned_room_event_creator", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                "owned-room-event".to_string(),
                String::new(),
                target_user.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    api_ok(
        admin_api
            .ban_user(
                synctv_proto::admin::BanUserRequest {
                    user_id: public_user_id(&admin_api, target_user.id),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    let mut saw_owner_inactive = false;
    while let Ok(Some(publish)) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        redis_publish_rx.recv(),
    )
    .await
    {
        if let RealtimeEvent::RoomOwnerInactive {
            room_id,
            owner_id,
            triggered_by,
            ..
        } = publish.event
        {
            assert_eq!(room_id, room.id);
            assert_eq!(owner_id, target_user.id);
            assert_eq!(triggered_by, admin_user.id);
            saw_owner_inactive = true;
        }
    }

    assert!(
        saw_owner_inactive,
        "banning a room owner must publish RoomOwnerInactive for owned rooms"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_batch_ban_users_resets_playback_for_media_created_by_target() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_batch_ban_playback", UserRole::Root).await;
    let target_user =
        create_db_user(&user_repo, "batch_banned_media_creator", UserRole::User).await;
    let room_owner = create_db_user(&user_repo, "batch_ban_playback_owner", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                "batch-ban-playback-room".to_string(),
                String::new(),
                room_owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, target_user.id, "batch-banned-media").await;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, room_owner.id, Some(media.id), None, None)
            .await,
    )?;

    let response = api_ok(
        admin_api
            .batch_ban_users(
                synctv_proto::admin::BatchBanUsersRequest {
                    user_ids: vec![public_user_id(&admin_api, target_user.id)],
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert_eq!(response.succeeded, 1);
    assert_eq!(response.failed, 0);

    let state = core_ok(
        admin_api
            .room_service
            .playback_service()
            .get_state(&room.id)
            .await,
    )?;

    assert!(
        state.playing_media_id.is_none(),
        "batch ban must clear playback media created by the banned user"
    );
    assert!(
        state.playing_playlist_id.is_none(),
        "batch ban must clear playlist context created by the banned user"
    );
    assert!(
        !state.is_playing,
        "batch ban must stop playback for media created by the banned user"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_batch_ban_users_publishes_room_owner_inactive_event_for_owned_rooms() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(
        &user_repo,
        "root_batch_ban_owned_room_event",
        UserRole::Root,
    )
    .await;
    let target_user = create_db_user(&user_repo, "batch_owned_room_creator", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                "batch-owned-room-event".to_string(),
                String::new(),
                target_user.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let response = api_ok(
        admin_api
            .batch_ban_users(
                synctv_proto::admin::BatchBanUsersRequest {
                    user_ids: vec![public_user_id(&admin_api, target_user.id)],
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert_eq!(response.succeeded, 1);
    assert_eq!(response.failed, 0);

    let mut saw_owner_inactive = false;
    while let Ok(Some(publish)) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        redis_publish_rx.recv(),
    )
    .await
    {
        if let RealtimeEvent::RoomOwnerInactive {
            room_id,
            owner_id,
            triggered_by,
            ..
        } = publish.event
        {
            assert_eq!(room_id, room.id);
            assert_eq!(owner_id, target_user.id);
            assert_eq!(triggered_by, admin_user.id);
            saw_owner_inactive = true;
        }
    }

    assert!(
        saw_owner_inactive,
        "batch ban must publish RoomOwnerInactive for rooms owned by banned users"
    );
    Ok(())
}

#[test]
fn test_parse_batch_user_ids_trims_and_preserves_order() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let first = UserId::expect_positive(901);
    let second = UserId::expect_positive(902);
    let parsed = parse_batch_user_ids(
        &[
            format!(
                "  {}  ",
                public_id_codec.encode_user_id(first).map_err(test_error)?
            ),
            public_id_codec.encode_user_id(second).map_err(test_error)?,
        ],
        &public_id_codec,
    )
    .map_err(test_error)?;

    assert_eq!(parsed, vec![first, second]);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_member_permissions_bypasses_room_creator_constraint_for_global_admin(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_member_update", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_member_update", UserRole::User).await;
    let target = create_db_user(&user_repo, "target_member_update", UserRole::User).await;

    let room = Box::pin(create_room_with_member(&admin_api, &owner.id, &target.id)).await;

    let response = api_ok(
        admin_api
            .update_member_permissions(
                synctv_proto::admin::UpdateMemberPermissionsRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    user_id: public_user_id(&admin_api, target.id),
                    role: synctv_proto::common::RoomMemberRole::Admin as i32,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0b1010,
                    admin_removed_permissions: 0b0101,
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let member = response;
    assert_eq!(member.user_id, public_user_id(&admin_api, target.id));
    assert_eq!(
        member.role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert_eq!(member.admin_added_permissions, 0b1010);
    assert_eq!(member.admin_removed_permissions, 0b0101);

    let persisted = core_ok(
        admin_api
            .room_service
            .get_member(&room.id, &target.id)
            .await,
    )?;
    let persisted = some_value(persisted, "target should remain a member")?;
    assert_eq!(persisted.role, RoomRole::Admin);
    assert_eq!(persisted.admin_added_permissions, 0b1010);
    assert_eq!(persisted.admin_removed_permissions, 0b0101);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_member_kick", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_member_kick", UserRole::User).await;
    let target = create_db_user(&user_repo, "target_member_kick", UserRole::User).await;

    let room = Box::pin(create_room_with_member(&admin_api, &owner.id, &target.id)).await;

    let response = api_ok(
        admin_api
            .kick_member(
                synctv_proto::admin::KickMemberRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    user_id: public_user_id(&admin_api, target.id),
                    kick_cooldown_seconds: 60,
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;
    assert!(response.success);

    let persisted = core_ok(
        admin_api
            .room_service
            .get_member(&room.id, &target.id)
            .await,
    )?;
    assert!(
        persisted.is_none(),
        "kicked member should no longer appear as an active room member"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_stream_info_bypasses_room_membership_requirement_for_global_admin() -> TestResult
{
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, registry, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let _global_admin =
        create_db_user(&user_repo, "global_admin_stream_info", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_stream_info", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room stream info test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    registry
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-local",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let response = api_ok(
        admin_api
            .get_stream_info(
                &public_room_id(&admin_api, room.id),
                &public_media_id(&admin_api, media.id),
            )
            .await,
    )?;
    assert!(response.active);
    let publisher = some_value(response.publisher, "publisher info")?;
    assert_eq!(publisher.user_id, public_user_id(&admin_api, owner.id));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_reports_local_unpublish_enqueue_failure() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, registry, mut redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_stream_kick_failure",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_stream_kick_failure", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room stream kick failure test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    registry
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-local",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let err = api_err(
        admin_api
            .kick_stream(
                synctv_proto::admin::KickStreamRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    media_id: public_media_id(&admin_api, media.id),
                    reason: "test failure".to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(
        matches!(err, ApiError::Internal(_) | ApiError::ServiceUnavailable(_)),
        "unexpected kick_stream error: {err:?}"
    );
    assert!(
        registry
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .map_err(|error| test_error(error.to_string()))?
            .is_some(),
        "failed kick must not unregister the publisher"
    );
    assert!(
        redis_publish_rx.try_recv().is_err(),
        "failed local kick must not publish replica-wide success event"
    );
    let stream_error = StreamError::StreamHubError("send failed".to_string());
    assert!(matches!(
        crate::impls::map_livestream_stream_error(&stream_error),
        ApiError::ServiceUnavailable(_)
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_publishes_cluster_event_for_remote_publisher() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, registry, mut redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_remote_stream_kick",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_remote_stream_kick", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "remote stream kick test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "remote-stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    registry
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-remote",
            &registry_owner_id,
            "127.0.0.1:50052",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    api_ok(
        admin_api
            .kick_stream(
                synctv_proto::admin::KickStreamRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    media_id: public_media_id(&admin_api, media.id),
                    reason: "remote owner".to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let request = some_value(redis_publish_rx.recv().await, "remote kick publish event")?;
    assert!(matches!(
        request.event,
        RealtimeEvent::KickPublisher { room_id, media_id, ref reason, .. }
            if room_id == room.id && media_id == media.id && reason == "remote owner"
    ));
    assert!(
        registry
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .map_err(|error| test_error(error.to_string()))?
            .is_some(),
        "remote publisher remains registered on non-owner replica"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_reports_remote_fanout_failure() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, registry, redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    drop(redis_publish_rx);
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_remote_stream_fanout_failure",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_remote_stream_fanout_failure",
        UserRole::User,
    )
    .await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "remote stream fanout failure test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "remote-stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    registry
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-remote",
            &registry_owner_id,
            "127.0.0.1:50052",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let err = api_err(
        admin_api
            .kick_stream(
                synctv_proto::admin::KickStreamRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    media_id: public_media_id(&admin_api, media.id),
                    reason: "remote fanout failure".to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(
        matches!(err, ApiError::Internal(_) | ApiError::ServiceUnavailable(_)),
        "unexpected kick_stream error: {err:?}"
    );
    assert!(
        registry
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .map_err(|error| test_error(error.to_string()))?
            .is_some(),
        "remote fanout failure keeps publisher registered"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_room_streams_bypasses_room_membership_requirement_for_global_admin() -> TestResult
{
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, registry, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let _global_admin =
        create_db_user(&user_repo, "global_admin_stream_list", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_stream_list", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room stream list test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();
    let encoded_room_id = public_room_id(&admin_api, room.id);
    let encoded_media_id = public_media_id(&admin_api, media.id);

    registry
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-a",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let response = api_ok(
        admin_api
            .list_room_streams(
                &encoded_room_id,
                synctv_proto::client::ListRoomStreamsRequest {
                    page: 1,
                    page_size: 50,
                    search: String::new(),
                    sort_by: synctv_proto::client::RoomStreamListSortBy::Unspecified as i32,
                    sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                },
            )
            .await,
    )?;
    assert_eq!(response.total, 1);
    assert_eq!(response.streams.len(), 1);
    assert_eq!(response.streams[0].media_id, encoded_media_id);
    assert!(response.streams[0].active);
    Ok(())
}

#[test]
fn build_room_stream_list_response_applies_search_sort_and_pagination() -> TestResult {
    let public_id_codec = synctv_adapter::PublicIdCodec::plain();
    let media_ids = vec![
        MediaId::expect_positive(301),
        MediaId::expect_positive(302),
        MediaId::expect_positive(303),
    ];
    let mut expected_ids = media_ids
        .iter()
        .map(|media_id| {
            public_id_codec
                .encode_media_id(*media_id)
                .map_err(test_error)
        })
        .collect::<TestResult<Vec<_>>>()?;
    expected_ids.sort_unstable();
    expected_ids.reverse();
    let response = crate::impls::client::stream::build_room_streams_response(
        media_ids,
        &synctv_proto::client::ListRoomStreamsRequest {
            page: 2,
            page_size: 1,
            search: String::new(),
            sort_by: synctv_proto::client::RoomStreamListSortBy::MediaId as i32,
            sort_direction: synctv_proto::client::SortDirection::Desc as i32,
        },
        &public_id_codec,
    )
    .map_err(test_error)?;

    assert_eq!(response.total, 3);
    assert_eq!(response.streams.len(), 1);
    assert_eq!(response.streams[0].media_id, expected_ids[1]);
    assert!(response.streams[0].active);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_publish_key_bypasses_room_membership_requirement_for_global_admin(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, _registry, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_publish_key", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_publish_key", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room publish key test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let public_room_id = public_room_id(&admin_api, room.id);
    let public_media_id = public_media_id(&admin_api, media.id);

    let response = api_ok(
        admin_api
            .create_publish_key_for_actor(
                &public_room_id,
                &public_media_id,
                &owner.id,
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let publish_key_service = some_value(
        admin_api.publish_key_service.as_ref(),
        "publish key service should be configured",
    )?;
    let claims = core_ok(
        publish_key_service
            .validate_publish_key_for_stream_claims(&response.publish_key, &room.id, &media.id)
            .await,
    )?;

    assert_eq!(claims.user_id, owner.id.to_string());
    assert!(!response.publish_key.is_empty());
    assert!(response.rtmp_url.contains(&public_room_id));
    assert!(response.stream_key.contains(&public_media_id));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_start_playback_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_start", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_start", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback start test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    api_ok(
        admin_api
            .start_playback(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::StartPlaybackRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    playlist_id: String::new(),
                    target: None,
                    client_operation_id: None,
                },
                Some(global_admin.id),
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let state = core_ok(admin_api.room_service.get_playback_state(&room.id).await)?;
    assert_eq!(state.playing_media_id.as_ref(), Some(&media.id));
    assert!(state.is_playing);
    let selected_by_user_id = core_ok(
        sqlx::query_scalar!(
            r#"
            SELECT selected_by_user_id AS "selected_by_user_id: UserId"
            FROM room_playback_history
            WHERE room_id = $1
            ORDER BY sequence DESC
            LIMIT 1
            "#,
            room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await
        .map_err(synctv_core::Error::from),
    )?;
    assert_eq!(selected_by_user_id, Some(global_admin.id));
    let playback_metadata_has_actor_username = core_ok(
        sqlx::query_scalar!(
            r#"
            SELECT metadata ? 'actorUsername' AS "has_actor_username!"
            FROM chat_messages
            WHERE room_id = $1 AND message_type = $2
            ORDER BY created_at DESC, id DESC
            LIMIT 1
            "#,
            room.id.as_i64(),
            i16::from(synctv_core::models::ChatMessageType::SystemPlaybackChanged),
        )
        .fetch_one(&pool)
        .await
        .map_err(synctv_core::Error::from),
    )?;
    assert!(!playback_metadata_has_actor_username);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stop_playback_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_stop", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_stop", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback stop test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await,
    )?;

    api_ok(
        admin_api
            .stop_playback(
                &public_room_id(&admin_api, room.id),
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let state = core_ok(admin_api.room_service.get_playback_state(&room.id).await)?;
    assert!(state.playing_media_id.is_none());
    assert!(!state.is_playing);
    assert!((state.position - 0.0).abs() < f64::EPSILON);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_get", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_get", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback get test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await,
    )?;

    let response = api_ok(
        admin_api
            .get_playback(&public_room_id(&admin_api, room.id), &global_admin.id, None)
            .await,
    )?;

    let state = some_value(response.playback_state, "playback state should be present")?;
    assert!(state.is_playing);
    assert_eq!(
        state.playing_media_id,
        public_media_id(&admin_api, media.id)
    );

    let result = some_value(response.playback, "playback should be present")?;
    assert_eq!(result.media_id, public_media_id(&admin_api, media.id));
    assert_eq!(result.room_id, public_room_id(&admin_api, room.id));
    assert_eq!(result.name, media.name);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_start_playback_returns_error_for_invalid_provider_config_for_global_admin(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_playback_invalid_config",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_playback_invalid_config",
        UserRole::User,
    )
    .await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback invalid config test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Invalid Playback Provider".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::live_proxy_pull_live_media_source_config("not-a-url"),
        source_provider: synctv_core::models::SourceProvider::LiveProxy,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = core_ok(media_repo.create(&media).await)?;

    let error = api_err(
        admin_api
            .start_playback(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::StartPlaybackRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    playlist_id: String::new(),
                    target: None,
                    client_operation_id: None,
                },
                Some(global_admin.id),
                &global_admin.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(matches!(
        error,
        ApiError::InvalidInput(message)
            if message.contains("Invalid LiveProxy source URL")
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_for_provider_media_signs_proxy_urls_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_playback_get_signed",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_playback_get_signed", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "playback provider playback get test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "provider-playback-media".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::direct_url_media_source_config_with_headers(
            "https://example.com/video.mp4",
            HashMap::from([(
                "Authorization".to_string(),
                "Bearer admin-provider-token".to_string(),
            )]),
        ),
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = core_ok(media_repo.create(&media).await)?;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await,
    )?;

    let response = api_ok(
        admin_api
            .get_playback(&public_room_id(&admin_api, room.id), &global_admin.id, None)
            .await,
    )?;

    let result = some_value(response.playback, "playback should be present")?;
    let direct = result
        .playback_infos
        .get("proxy_direct")
        .ok_or_else(|| test_error("proxy_direct mode should be present"))?;
    assert_eq!(direct.medias.len(), 1);
    assert!(
        direct.medias[0]
            .url
            .starts_with("/api/playback-providers/direct-url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.medias[0].url
    );
    assert!(
        direct.medias[0].url.contains("/streams/direct/0?"),
        "signed direct-url playback should use stream proxy contract, got {}",
        direct.medias[0].url
    );
    assert!(
        direct.medias[0].headers.is_empty(),
        "proxy-backed playback keeps client headers empty"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_for_provider_media_signs_proxy_urls_for_local_management_actor(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = create_db_user(
        &user_repo,
        "room_owner_local_mgmt_playback_get_signed",
        UserRole::User,
    )
    .await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "playback provider playback get local management test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "provider-playback-media".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::direct_url_media_source_config_with_headers(
            "https://example.com/video.mp4",
            HashMap::from([(
                "Authorization".to_string(),
                "Bearer admin-provider-token".to_string(),
            )]),
        ),
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = core_ok(media_repo.create(&media).await)?;

    core_ok(
        admin_api
            .room_service
            .playback_service()
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await,
    )?;

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = api_ok(
        admin_api
            .get_playback(
                &public_room_id(&admin_api, room.id),
                &management_actor,
                None,
            )
            .await,
    )?;

    let result = some_value(response.playback, "playback should be present")?;
    let direct = result
        .playback_infos
        .get("proxy_direct")
        .ok_or_else(|| test_error("proxy_direct mode should be present"))?;
    assert_eq!(direct.medias.len(), 1);
    assert!(
        direct.medias[0]
            .url
            .starts_with("/api/playback-providers/direct-url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.medias[0].url
    );
    assert!(
        direct.medias[0]
            .url
            .contains(&format!("uid={}", public_user_id(&admin_api, owner.id))),
        "local management playback must sign proxy URLs with a real room member, got {}",
        direct.medias[0].url
    );
    assert!(
        !direct.medias[0]
            .url
            .contains(&LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string()),
        "local management playback signs proxy URLs with the resolved room member"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playlists_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_list_playlists", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_list_playlists", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list playlists test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-a".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;

    let response = api_ok(
        admin_api
            .list_playlists(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::ListPlaylistsRequest {
                    parent_id: String::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: synctv_proto::source_config::SourceProvider::Unspecified
                        as i32,
                    provider_instance_name: String::new(),
                    dynamic_only: None,
                    sort_by: synctv_proto::client::PlaylistListSortBy::Position as i32,
                    sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                    availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert_eq!(response.total, 1);
    assert_eq!(response.playlists.len(), 1);
    assert_eq!(
        response.playlists[0].id,
        public_playlist_id(&admin_api, playlist.id)
    );
    assert_eq!(response.playlists[0].name, "playlist-a");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playlist_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_get_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_get_playlist", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room get playlist test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-b".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;

    let response = api_ok(
        admin_api
            .get_playlist(
                &public_room_id(&admin_api, room.id),
                &public_playlist_id(&admin_api, playlist.id),
                &global_admin.id,
            )
            .await,
    )?;

    let response_playlist = some_value(response.playlist, "playlist should be returned")?;
    assert_eq!(
        response_playlist.id,
        public_playlist_id(&admin_api, playlist.id)
    );
    assert_eq!(response_playlist.name, "playlist-b");
    assert_eq!(response.media_count, 0);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_playlist_bypasses_room_membership_requirement_for_global_admin() -> TestResult
{
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_update_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_update_playlist", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room update playlist test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-before".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;

    let response = api_ok(
        admin_api
            .update_playlist(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::UpdatePlaylistRequest {
                    playlist_id: public_playlist_id(&admin_api, playlist.id),
                    name: "playlist-after".to_string(),
                    description: String::new(),
                },
                &global_admin.id,
            )
            .await,
    )?;

    let updated = response;
    assert_eq!(updated.id, public_playlist_id(&admin_api, playlist.id));
    assert_eq!(updated.name, "playlist-after");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_playlist_bypasses_room_membership_requirement_for_global_admin() -> TestResult
{
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_delete_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_delete_playlist", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete playlist test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-delete".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;

    let response = api_ok(
        admin_api
            .delete_playlist(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::DeletePlaylistRequest {
                    playlist_id: public_playlist_id(&admin_api, playlist.id),
                    force: true,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert!(response.success);
    let playlist_after = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .get_playlist(&playlist.id)
            .await,
    )?;
    assert!(playlist_after.is_none());
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_playlist_publishes_cascaded_playlist_and_media_events_for_global_admin(
) -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_delete_playlist_cascade",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_delete_playlist_cascade",
        UserRole::User,
    )
    .await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete playlist cascade test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let parent_playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-delete-parent".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;
    let child_playlist = core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "playlist-delete-child".to_string(),
                    description: String::new(),
                    parent_id: Some(parent_playlist.id),
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;
    let nested_media = core_ok(
        admin_api
            .room_service
            .media_service()
            .add_media(
                room.id,
                owner.id,
                synctv_core::service::AddMediaRequest {
                    playlist_id: Some(child_playlist.id),
                    name: "playlist-delete-cascade-media".to_string(),
                    description: String::new(),
                    source_provider: synctv_core::models::SourceProvider::DirectUrl,
                    provider_instance_name: None,
                    source_config: synctv_core_testing::direct_url_media_source_config(
                        "https://example.com/admin-playlist-delete-cascade.mp4",
                    ),
                },
            )
            .await,
    )?;

    let response = api_ok(
        admin_api
            .delete_playlist(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::DeletePlaylistRequest {
                    playlist_id: public_playlist_id(&admin_api, parent_playlist.id),
                    force: true,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert!(response.success);

    let mut deleted_playlist_ids = Vec::new();
    let mut deleted_media_ids = Vec::new();
    let mut kicked_media_ids = Vec::new();

    while let Ok(request) = redis_publish_rx.try_recv() {
        match request.event {
            RealtimeEvent::PlaylistDeleted { playlist_id, .. } => {
                deleted_playlist_ids.push(playlist_id.to_string());
            }
            RealtimeEvent::MediaRemoved { media_id, .. } => {
                deleted_media_ids.push(media_id.to_string());
            }
            RealtimeEvent::KickPublisher { media_id, .. } => {
                kicked_media_ids.push(media_id.to_string());
            }
            RealtimeEvent::CacheInvalidate { .. } => {}
            other => {
                return Err(test_error(format!(
                    "unexpected admin delete_playlist cascade event: {other:?}"
                )))
            }
        }
    }

    deleted_playlist_ids.sort_unstable();
    deleted_media_ids.sort_unstable();
    kicked_media_ids.sort_unstable();
    let mut expected_playlist_ids = vec![
        child_playlist.id.to_string(),
        parent_playlist.id.to_string(),
    ];
    expected_playlist_ids.sort_unstable();

    assert_eq!(
        deleted_playlist_ids, expected_playlist_ids,
        "admin delete_playlist must publish PlaylistDeleted for every playlist removed by cascade"
    );
    assert_eq!(
        deleted_media_ids,
        vec![nested_media.id.to_string()],
        "admin delete_playlist must publish MediaRemoved for media deleted through playlist cascade"
    );
    assert_eq!(
        kicked_media_ids,
        vec![nested_media.id.to_string()],
        "admin delete_playlist must kick publishers for media deleted through playlist cascade"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_media_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_list_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_list_media", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list media test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-a").await;

    let response = api_ok(
        admin_api
            .list_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: None,
                    pagination: Some(
                        synctv_proto::client::list_playlist_items_request::Pagination::Page(
                            synctv_proto::client::PagePagination { page: 1 },
                        ),
                    ),
                    page_size: 20,
                    search: String::new(),
                    source_provider: synctv_proto::source_config::SourceProvider::Unspecified
                        as i32,
                    provider_instance_name: String::new(),
                    sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                    sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                    availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                    refresh: false,
                    preview_source_config: None,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].id, public_media_id(&admin_api, media.id));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_room_settings_bypasses_room_membership_for_local_management_actor() -> TestResult
{
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let owner = create_db_user(&user_repo, "room_owner_reset_room_settings", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings reset test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let customized = synctv_core::models::RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..synctv_core::models::RoomSettings::default()
    };
    core_ok(
        admin_api
            .room_service
            .set_room_settings(&room.id, &customized)
            .await,
    )?;

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = api_ok(
        admin_api
            .reset_room_settings(
                synctv_proto::admin::ResetRoomSettingsRequest {
                    room_id: public_room_id(&admin_api, room.id),
                },
                &management_actor,
            )
            .await,
    )?;

    let response_room = response;
    let room_id = admin_api
        .public_id_codec
        .decode_room_id(&response_room.id)
        .map_err(test_error)?;
    let settings = core_ok(admin_api.room_service.get_room_settings(&room_id).await)?;
    assert!(settings.chat_enabled.0);
    assert!(!settings.allow_guest_join.0);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_room_bypasses_room_membership_for_local_management_actor() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = create_db_user(&user_repo, "room_owner_delete_room", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = api_ok(
        admin_api
            .delete_room(
                synctv_proto::admin::DeleteRoomRequest {
                    room_id: public_room_id(&admin_api, room.id),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await,
    )?;

    assert!(response.success);
    assert!(
        core_ok(room_repo.get_by_id(&room.id).await)?.is_none(),
        "room should be deleted by local management actor"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_media_respects_search_filters_and_sort_for_static_root() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_list_media_filters",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_list_media_filters", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list media filter test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    core_ok(
        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id,
                owner.id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id: room.id,
                    name: "Alpha Folder".to_string(),
                    description: String::new(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await,
    )?;

    create_room_media(&pool, room.id, owner.id, "Alpha Media").await;
    create_room_media(&pool, room.id, owner.id, "Beta Media").await;

    let response = api_ok(
        admin_api
            .list_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: None,
                    pagination: Some(
                        synctv_proto::client::list_playlist_items_request::Pagination::Page(
                            synctv_proto::client::PagePagination { page: 1 },
                        ),
                    ),
                    page_size: 10,
                    search: "alpha".to_string(),
                    source_provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                    provider_instance_name: String::new(),
                    sort_by: synctv_proto::client::MediaListSortBy::Name as i32,
                    sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                    availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                    refresh: false,
                    preview_source_config: None,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert_eq!(response.total, Some(1));
    assert_eq!(response.folder_count, 0);
    assert_eq!(response.file_count, 1);
    assert!(response.playlists.is_empty());
    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].name, "Alpha Media");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_media_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_edit_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_edit_media", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room edit media test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-edit").await;

    let response = api_ok(
        admin_api
            .edit_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::EditMediaRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    name: "media-edited".to_string(),
                    description: String::new(),
                },
                &global_admin.id,
            )
            .await,
    )?;

    let updated = response;
    assert_eq!(updated.id, public_media_id(&admin_api, media.id));
    assert_eq!(updated.name, "media-edited");

    let event = recv_matching_realtime_event(
        &mut redis_publish_rx,
        "admin edit_media MediaUpdated realtime event",
        |event| matches!(event, RealtimeEvent::MediaUpdated { .. }),
    )
    .await;
    match event {
        RealtimeEvent::MediaUpdated {
            media_id,
            media_title,
            ..
        } => {
            assert_eq!(media_id.to_string(), media.id.to_string());
            assert_eq!(media_title, "media-edited");
        }
        other => {
            return Err(test_error(format!(
                "expected MediaUpdated event, got {other:?}"
            )))
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_local_management_actor_preserves_username_in_media_notifications() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let owner = create_db_user(
        &user_repo,
        "room_owner_management_media_notifications",
        UserRole::User,
    )
    .await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room management media notification test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, owner.id, "management-media").await;
    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let mut notification_rx = admin_api.room_service.notification_service().subscribe();

    api_ok(
        admin_api
            .edit_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::EditMediaRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    name: "management-media-updated".to_string(),
                    description: String::new(),
                },
                &management_actor,
            )
            .await,
    )?;

    let updated_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (_, event) = notification_rx
                .recv()
                .await
                .map_err(|error| test_error(format!("notification should arrive: {error}")))?;
            match event {
                synctv_core::service::RoomEvent::MediaUpdated {
                    media_id, username, ..
                } if media_id == media.id => break Ok::<_, anyhow::Error>(username),
                _ => {}
            }
        }
    })
    .await
    .map_err(|error| test_error(format!("media updated notification timeout: {error}")))??;
    assert_eq!(updated_event, "local-management");

    api_ok(
        admin_api
            .delete_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::DeleteMediaRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    force: false,
                },
                &management_actor,
            )
            .await,
    )?;

    let removed_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (_, event) = notification_rx
                .recv()
                .await
                .map_err(|error| test_error(format!("notification should arrive: {error}")))?;
            match event {
                synctv_core::service::RoomEvent::MediaRemoved {
                    media_id,
                    username,
                    user_id,
                } if media_id == media.id => break Ok::<_, anyhow::Error>((username, user_id)),
                _ => {}
            }
        }
    })
    .await
    .map_err(|error| test_error(format!("media removed notification timeout: {error}")))??;
    assert_eq!(removed_event.0, "local-management");
    assert_eq!(removed_event.1, Some(management_actor));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_media_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_delete_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_delete_media", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete media test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-delete").await;

    let response = api_ok(
        admin_api
            .delete_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::DeleteMediaRequest {
                    media_id: public_media_id(&admin_api, media.id),
                    force: true,
                },
                &global_admin.id,
            )
            .await,
    )?;

    assert!(response.success);
    let media_after = core_ok(
        admin_api
            .room_service
            .media_service()
            .get_media(&media.id)
            .await,
    )?;
    assert!(media_after.is_none());
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_bypasses_room_membership_requirement_for_global_admin() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_move_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_move_media", UserRole::User).await;

    let room = core_ok(
        admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room move media test".to_string(),
                owner.id,
                None,
                None,
            )
            .await,
    )?
    .0;

    let media_a = create_room_media(&pool, room.id, owner.id, "media-move-a").await;
    let media_b = create_room_media(&pool, room.id, owner.id, "media-move-b").await;

    api_ok(
        admin_api
            .move_media(
                &public_room_id(&admin_api, room.id),
                synctv_proto::client::MoveMediaRequest {
                    media_ids: vec![public_media_id(&admin_api, media_b.id)],
                    source_playlist_id: None,
                    target_playlist_id: None,
                    all_from_scope: false,
                    before_media_id: Some(public_media_id(&admin_api, media_a.id)),
                    after_media_id: None,
                },
                &global_admin.id,
            )
            .await,
    )?;

    let media_a_after = core_ok(
        admin_api
            .room_service
            .media_service()
            .get_media(&media_a.id)
            .await,
    )?;
    let media_a_after = some_value(media_a_after, "media_a should exist")?;
    let media_b_after = core_ok(
        admin_api
            .room_service
            .media_service()
            .get_media(&media_b.id)
            .await,
    )?;
    let media_b_after = some_value(media_b_after, "media_b should exist")?;
    assert!(media_b_after.position < media_a_after.position);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_room_publishes_room_banned_realtime_event() -> TestResult {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "room_admin", UserRole::Root).await;

    let room = make_test_room_model(&admin_user.id);
    let room = core_ok(room_repo.create(&room).await)?;

    api_ok(
        admin_api
            .ban_room(
                synctv_proto::admin::BanRoomRequest {
                    room_id: public_room_id(&admin_api, room.id),
                    reason: "moderation".to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await,
    )?;

    let publish = tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
        .await
        .map_err(|error| test_error(format!("expected cluster publish: {error}")))?;
    let publish = some_value(publish, "publish request")?;

    match publish.event {
        RealtimeEvent::RoomBanned {
            room_id, banned_by, ..
        } => {
            assert_eq!(room_id, room.id);
            assert_eq!(banned_by, admin_user.id);
        }
        other => {
            return Err(test_error(format!(
                "expected RoomBanned event, got {other:?}"
            )))
        }
    }
    Ok(())
}
