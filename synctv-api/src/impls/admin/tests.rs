use super::*;
use crate::impls::ErrorKind;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use synctv_core::models::{
    FromProviderParams, MemberStatus, PlaylistId, ReviewRequestId, RoomId, RoomRole, RoomStatus,
    UserId, UserRole, UserStatus,
};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::Config,
    repository::{
        MediaRepository, ProviderInstanceRepository, RoomRepository, SettingsRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        AuditService, EmailService, InMemoryTokenBlacklistStore, PublishKeyService,
        RemoteProviderManager, RuntimeEmailConfigProvider, SettingsRegistry, SettingsService,
        UserService,
    },
};
use synctv_core::{
    provider::{MediaProvider, ProviderStoreExt},
    service::ProvidersManager,
};
use synctv_core_testing::create_test_pool;
use synctv_livestream::{
    api::{LiveStreamingInfrastructure, StreamTracker},
    error::StreamError,
    livestream::{external_publish_manager::ExternalPublishManager, PullStreamManager},
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager, PublishRequest, RealtimeEvent};
use tokio::sync::mpsc;

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
fn proto_list_filter_enums_allow_unspecified_as_empty_filter() {
    assert_eq!(
        proto_room_status_filter(synctv_proto::common::RoomStatus::Unspecified as i32)
            .expect("unspecified room status should be accepted"),
        None
    );
    assert_eq!(
        proto_user_status_filter(synctv_proto::common::UserStatus::Unspecified as i32)
            .expect("unspecified user status should be accepted"),
        None
    );
    assert_eq!(
        proto_user_role_filter(synctv_proto::common::UserRole::Unspecified as i32)
            .expect("unspecified user role should be accepted"),
        None
    );
}

#[derive(Debug, Default)]
struct AdminLifecycleTestProvider {
    progress_calls: Arc<Mutex<Vec<(String, f64, bool)>>>,
}

#[async_trait]
impl MediaProvider for AdminLifecycleTestProvider {
    fn name(&self) -> &'static str {
        "direct_url"
    }

    async fn generate_playback(
        &self,
        _ctx: &synctv_core::provider::ProviderContext<'_>,
        _source_config: &serde_json::Value,
    ) -> Result<synctv_core::provider::PlaybackResult, synctv_core::provider::ProviderError> {
        Ok(admin_lifecycle_playback_result("admin-session"))
    }

    async fn on_playback_progress(
        &self,
        _ctx: &synctv_core::provider::ProviderContext<'_>,
        session_id: &str,
        _source_config: &serde_json::Value,
        position: f64,
        is_paused: bool,
    ) -> Result<(), synctv_core::provider::ProviderError> {
        self.progress_calls
            .lock()
            .expect("progress calls lock")
            .push((session_id.to_string(), position, is_paused));
        Ok(())
    }

    fn playback_lifecycle_session_id(
        &self,
        result: &synctv_core::provider::PlaybackResult,
    ) -> Option<String> {
        result
            .metadata
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
    }
}

fn admin_lifecycle_playback_result(session_id: &str) -> synctv_core::provider::PlaybackResult {
    let mut playback_infos = std::collections::HashMap::new();
    playback_infos.insert(
        "direct".to_string(),
        synctv_core::provider::PlaybackInfo {
            urls: vec!["https://example.com/video.mp4".to_string()],
            format: "mp4".to_string(),
            headers: std::collections::HashMap::new(),
            subtitles: Vec::new(),
            expires_at: None,
            cors_proxy_required: false,
        },
    );

    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );

    synctv_core::provider::PlaybackResult {
        playback_infos,
        default_mode: "direct".to_string(),
        metadata,
    }
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
        let mut calls = self.calls.lock().expect("membership fanout calls lock");
        std::mem::take(&mut *calls)
    }

    fn push(&self, call: MembershipEventFanoutCall) {
        self.calls
            .lock()
            .expect("membership fanout calls lock")
            .push(call);
    }
}

fn test_realtime_outbox_event(
    event: &RealtimeEvent,
) -> synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent {
    synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent {
        id: event.event_id().to_string(),
        enqueue_outbox: false,
        aggregate_type: "test".to_string(),
        aggregate_id: event
            .room_id()
            .map_or_else(|| "global".to_string(), std::string::ToString::to_string),
        event_type: event.event_type().to_string(),
        event_version: 1,
        aggregate_version: None,
        payload: serde_json::to_value(event).expect("test realtime event should serialize"),
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

    fn outbox_event(
        &self,
        event: &RealtimeEvent,
    ) -> Result<synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent, String> {
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
        let request = tokio::time::timeout(remaining, receiver.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
            .unwrap_or_else(|| {
                panic!("cluster publish channel closed while waiting for {description}")
            });
        if predicate(&request.event) {
            return request.event;
        }
    }
}

#[async_trait]
impl MembershipEventFanoutService for RecordingMembershipEventFanout {
    fn prepare_permission_changed_outbox_fanout(
        &self,
        target_user_id: UserId,
        changed_by: UserId,
    ) -> crate::membership_event_fanout::PreparedPermissionChangedFanout {
        crate::membership_event_fanout::PreparedPermissionChangedFanout::new(
            Arc::new(self.clone()),
            Arc::new(LocalNoopRealtimeEventService::new()),
            target_user_id,
            changed_by,
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

    fn outbox_event(
        &self,
        event: &RealtimeEvent,
    ) -> Result<synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent, String> {
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

fn public_room_id(admin_api: &AdminApiImpl, id: RoomId) -> String {
    admin_api
        .public_id_codec
        .encode_room_id(id)
        .expect("test room id should encode")
}

fn public_user_id(admin_api: &AdminApiImpl, id: UserId) -> String {
    admin_api
        .public_id_codec
        .encode_user_id(id)
        .expect("test user id should encode")
}

fn public_media_id(admin_api: &AdminApiImpl, id: synctv_core::models::MediaId) -> String {
    admin_api
        .public_id_codec
        .encode_media_id(id)
        .expect("test media id should encode")
}

fn public_playlist_id(admin_api: &AdminApiImpl, id: PlaylistId) -> String {
    admin_api
        .public_id_codec
        .encode_playlist_id(id)
        .expect("test playlist id should encode")
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
fn test_live_streaming_unavailable_error_is_service_unavailable() {
    let err = live_streaming_unavailable_error();
    assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
    assert_eq!(
        err.message(),
        "Live streaming is not available on this server."
    );
}

#[test]
fn test_publish_key_service_unavailable_error_is_service_unavailable() {
    let err = crate::impls::providers::rtmp::publish_key_service_unavailable_error();
    assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
    assert_eq!(
        err.message(),
        "Publish key service is not available on this server."
    );
}

#[test]
fn test_map_send_test_email_result_success_is_human_readable() {
    let response = AdminApiImpl::map_send_test_email_result("test@example.com", Ok(()));

    assert!(response.success);
    assert_eq!(
        response.message,
        "Test email sent successfully to test@example.com"
    );
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
    let jwt_service =
        JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt");
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
async fn test_validate_admin_auth_rejects_banned_user() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = make_user_service(&pool);
    let user_repo = UserRepository::new(pool);

    let banned_admin = create_db_user(&user_repo, "banned_admin_auth", UserRole::Admin).await;
    user_repo
        .ban(&banned_admin.id, None, Some("admin auth test".to_string()))
        .await
        .expect("ban admin");

    let err = validate_admin_auth(&user_service, banned_admin.id, 0, 0)
        .await
        .err()
        .expect("banned admin must not pass admin auth");

    assert!(
        matches!(err, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
        "banned admin auth must fail with generic authentication error, got: {err:?}"
    );
}

async fn make_admin_api_for_delete_user_test(
    pool: sqlx::PgPool,
) -> (
    AdminApiImpl,
    tokio::sync::mpsc::Receiver<synctv_realtime::sync::PublishRequest>,
) {
    let user_service = Arc::new(make_user_service(&pool));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let providers_manager = ProvidersManager::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    )
    .expect("providers manager should build");
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    let room_service = synctv_core::service::RoomService::new_with_providers_and_options(
        pool.clone(),
        (*user_service).clone(),
        Arc::new(providers_manager),
        synctv_core::service::room::RoomServiceOptions {
            settings_registry: Some(settings_registry.clone()),
            ..synctv_core::service::room::RoomServiceOptions::test_defaults()
        },
    )
    .expect("room service should build");
    let email_service = Arc::new(
        EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
            &settings_registry,
        )))
        .expect("email service"),
    );
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();
    let room_service = Arc::new(room_service);
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .expect("built-in providers should initialize");
    let audit_service = Arc::new(AuditService::new_unbuffered(pool));
    let config = Arc::new(Config::default());
    let publish_key_service = Arc::new(
        PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt"),
            24,
        )
        .expect("publish key service should build"),
    );
    let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
    let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
    );

    (
        AdminApiImpl::new_with_runtime(
            AdminApiConfig {
                room_service,
                user_service,
                settings_service,
                settings_registry: Some(settings_registry),
                email_service,
                connection_service: connection_manager,
                provider_instance_manager,
                live_streaming_infrastructure: None,
                publish_key_service: Some(publish_key_service),
                config,
                audit_service,
                public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
            },
            AdminApiRuntime {
                realtime_fanout: crate::test_support::channel_realtime_fanout_service(
                    redis_publish_tx,
                ),
                realtime_event_service: Arc::new(LocalNoopRealtimeEventService::new()),
                provider_stores: Some(provider_stores),
                provider_access_service: None,
                request_executor: None,
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
    tokio::sync::mpsc::Receiver<synctv_realtime::sync::PublishRequest>,
) {
    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        synctv_core::service::RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    let email_service = Arc::new(
        EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
            &settings_registry,
        )))
        .expect("email service"),
    );
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .expect("built-in providers should initialize");
    let audit_service = Arc::new(AuditService::new_unbuffered(pool));
    let config = Arc::new(Config::default());
    let publish_key_service = Arc::new(
        PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt"),
            24,
        )
        .expect("publish key service should build"),
    );
    let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
    let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
    );

    let tracker = Arc::new(StreamTracker::new());
    let registry = synctv_livestream::relay::local_stream_registry();
    let (event_sender, _event_receiver) = mpsc::channel(64);
    let pull_manager = Arc::new(PullStreamManager::new(
        registry.clone(),
        event_sender.clone(),
    ));
    let external_publish_manager = Arc::new(
        ExternalPublishManager::new(
            registry.clone(),
            "node-local".to_string(),
            event_sender.clone(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .expect("external publish manager should build"),
    );
    let live_streaming_infrastructure = Arc::new(
        LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker,
        )
        .with_local_node_id("node-local".to_string()),
    );

    (
        AdminApiImpl::new_with_runtime(
            AdminApiConfig {
                room_service,
                user_service,
                settings_service,
                settings_registry: Some(settings_registry),
                email_service,
                connection_service: connection_manager,
                provider_instance_manager,
                live_streaming_infrastructure: Some(live_streaming_infrastructure.clone()),
                publish_key_service: Some(publish_key_service),
                config,
                audit_service,
                public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
            },
            AdminApiRuntime {
                realtime_fanout: crate::test_support::channel_realtime_fanout_service(
                    redis_publish_tx,
                ),
                realtime_event_service: Arc::new(LocalNoopRealtimeEventService::new()),
                provider_stores: Some(provider_stores),
                provider_access_service: None,
                request_executor: None,
            },
        ),
        live_streaming_infrastructure,
        redis_publish_rx,
    )
}

async fn create_room_with_member(
    admin_api: &AdminApiImpl,
    owner_id: &UserId,
    member_id: &UserId,
) -> synctv_core::models::Room {
    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room for user lifecycle test".to_string(),
            *owner_id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    admin_api
        .room_service
        .join_room(room.id, *member_id, None)
        .await
        .expect("member should join room");

    room
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_add_member_publishes_permission_changed_membership_event() {
    let (_postgres, pool) = create_test_pool().await;
    let (mut admin_api, _redis_publish_rx) =
        make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_add_membership_outbox",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(
        &user_repo,
        "room_owner_add_membership_outbox",
        UserRole::User,
    )
    .await;
    let target = create_db_user(&user_repo, "target_add_membership_outbox", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room for add member fanout test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
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
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("admin add member should succeed");

    assert_eq!(
        response.member.expect("member should be returned").user_id,
        public_user_id(&admin_api, target.id)
    );
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_update_member_permissions_publishes_membership_event() {
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

    let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

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
        .expect("admin update member permissions should succeed");

    assert_eq!(
        response.member.expect("member should be returned").user_id,
        public_user_id(&admin_api, target.id)
    );
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_member_response_uses_room_permission_overrides() {
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
    let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

    let mut settings = admin_api
        .room_service
        .get_room_settings(&room.id)
        .await
        .expect("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            synctv_core::models::RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE,
        );
    admin_api
        .room_service
        .set_room_settings(&room.id, &settings)
        .await
        .expect("room settings should update");

    let response = admin_api
        .update_member_permissions(
            synctv_proto::admin::UpdateMemberPermissionsRequest {
                room_id: public_room_id(&admin_api, room.id),
                user_id: public_user_id(&admin_api, target.id),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                added_permissions: 0,
                removed_permissions: synctv_core::models::RoomMemberPermissionBits::CHAT,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("admin update member permissions should succeed");

    let member = response.member.expect("member should be returned");
    assert!(
        synctv_core::models::RoomPermissionSet::default_member()
            .has(synctv_core::models::RoomPermission::CREATE_MEDIA_RESOURCE),
        "static member defaults include CREATE_MEDIA_RESOURCE, so the response must prove it used room overrides"
    );
    assert!(
        !synctv_core::models::RoomPermissionSet(member.permissions)
            .has(synctv_core::models::RoomPermission::CREATE_MEDIA_RESOURCE),
        "admin member response must apply room-level permission removals"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_kick_member_publishes_membership_event() {
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

    let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

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
        .expect("admin kick member should succeed");

    assert!(response.success);
    assert_eq!(
        fanout.take_calls(),
        vec![MembershipEventFanoutCall::PublishPermissionChanged {
            room_id: room.id.to_string(),
            target_user_id: target.id.to_string(),
            changed_by: global_admin.id.to_string(),
        }]
    );
}

async fn create_room_media(
    pool: &sqlx::PgPool,
    room_id: RoomId,
    creator_id: UserId,
    name: &str,
) -> synctv_core::models::Media {
    let media_repo = MediaRepository::new(pool.clone());
    let mut tx = pool.begin().await.expect("begin media test transaction");
    let position = media_repo
        .get_next_append_position_with_tx(&room_id, None, &mut tx)
        .await
        .expect("compute next media position");
    let media = synctv_core::models::Media::from_direct_single_mode(
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
    )
    .expect("direct media should build");
    let created = media_repo
        .create_with_executor(&media, &mut *tx)
        .await
        .expect("create media");
    tx.commit().await.expect("commit media test transaction");
    created
}

fn make_test_room_model(created_by: &UserId) -> synctv_core::models::Room {
    let now = chrono::Utc::now();
    synctv_core::models::Room {
        id: RoomId::new(),
        name: "room-ban-test".to_string(),
        description: "room for admin ban test".to_string(),
        cover_file_reference_id: None,
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

fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
    let now = chrono::Utc::now();
    synctv_core::models::Room {
        id: RoomId::expect_positive(101),
        name: "Admin Test Room".to_string(),
        description: "Room for admin tests".to_string(),
        cover_file_reference_id: None,
        created_by: UserId::expect_positive(102),
        status,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 1,
        last_activity_at: now,
    }
}

#[test]
fn test_admin_room_to_proto_basic() {
    let room = make_test_room(RoomStatus::Active);
    let public_id_codec = crate::PublicIdCodec::plain();
    let settings = synctv_core::models::RoomSettings::default();
    let proto = try_admin_room_to_proto(
        &room,
        Some(&settings),
        Some(10),
        Some("creator_user"),
        UserStatus::Active,
        &public_id_codec,
    )
    .expect("admin room proto should encode");

    assert_eq!(proto.id, public_id_codec.encode_room_id(room.id).unwrap());
    assert_eq!(proto.name, "Admin Test Room");
    assert_eq!(proto.description, "Room for admin tests");
    assert_eq!(
        proto.creator_id,
        public_id_codec.encode_user_id(room.created_by).unwrap()
    );
    assert_eq!(proto.creator_username, "creator_user");
    assert_eq!(
        proto.creator_status,
        synctv_proto::common::UserStatus::Active as i32
    );
    assert_eq!(proto.member_count, 10);
    assert!(!proto.is_banned);
}

#[test]
fn test_admin_room_to_proto_banned() {
    let mut room = make_test_room(RoomStatus::Active);
    let public_id_codec = crate::PublicIdCodec::plain();
    let settings = synctv_core::models::RoomSettings::default();
    room.is_banned = true;
    let proto = try_admin_room_to_proto(
        &room,
        Some(&settings),
        None,
        Some("creator_user"),
        UserStatus::Banned,
        &public_id_codec,
    )
    .expect("admin room proto should encode");
    assert!(proto.is_banned);
    assert_eq!(proto.member_count, 0);
    assert_eq!(
        proto.creator_status,
        synctv_proto::common::UserStatus::Banned as i32
    );
}

#[test]
fn test_admin_room_to_proto_uses_supplied_settings() {
    let room = make_test_room(RoomStatus::Active);
    let public_id_codec = crate::PublicIdCodec::plain();
    let settings = synctv_core::models::RoomSettings {
        allow_auto_join: synctv_core::models::room_settings::AllowAutoJoin(false),
        ..Default::default()
    };

    let proto = try_admin_room_to_proto(
        &room,
        Some(&settings),
        None,
        Some("creator_user"),
        UserStatus::Active,
        &public_id_codec,
    )
    .expect("admin room proto should encode");
    let rendered: serde_json::Value =
        serde_json::from_slice(&proto.settings).expect("settings should be valid json");

    assert_eq!(rendered["allow_auto_join"], false);
}

#[test]
fn test_admin_room_to_proto_different_statuses() {
    for status in [RoomStatus::Active, RoomStatus::Closed] {
        let room = make_test_room(status);
        let public_id_codec = crate::PublicIdCodec::plain();
        let settings = synctv_core::models::RoomSettings::default();
        let proto = try_admin_room_to_proto(
            &room,
            Some(&settings),
            None,
            Some("creator_user"),
            UserStatus::Active,
            &public_id_codec,
        )
        .expect("admin room proto should encode");
        assert_eq!(
            proto.status,
            synctv_proto::common::RoomStatus::from(status) as i32
        );
    }
}

#[test]
fn admin_query_enum_mappers_reject_unknown_values_and_preserve_defaults() {
    assert_eq!(
        proto_admin_user_list_sort_by(synctv_proto::admin::UserListSortBy::Status as i32)
            .expect("status sort should be accepted"),
        synctv_core::models::UserListSortBy::Status
    );
    assert_eq!(
        proto_admin_user_list_sort_by(synctv_proto::admin::UserListSortBy::Unspecified as i32)
            .expect("unspecified user sort should be accepted"),
        synctv_core::models::UserListSortBy::CreatedAt
    );
    assert_eq!(
        proto_admin_room_list_sort_by(synctv_proto::admin::RoomListSortBy::Unspecified as i32)
            .expect("unspecified room sort should be accepted"),
        synctv_core::models::RoomListSortBy::CreatedAt
    );
    assert_eq!(
        proto_admin_room_member_list_sort_by(
            synctv_proto::admin::RoomMemberListSortBy::Unspecified as i32
        )
        .expect("unspecified room member sort should be accepted"),
        synctv_core::models::RoomMemberListSortBy::JoinedAt
    );
    assert_eq!(
        proto_admin_active_stream_list_sort_by(
            synctv_proto::admin::ActiveStreamListSortBy::Unspecified as i32
        )
        .expect("unspecified active stream sort should be accepted"),
        synctv_proto::admin::ActiveStreamListSortBy::StartedAt
    );
    assert_eq!(
        proto_admin_sort_direction(
            synctv_proto::admin::SortDirection::Unspecified as i32,
            CoreSortDirection::Desc
        )
        .expect("unspecified sort direction should be accepted"),
        CoreSortDirection::Desc
    );
    assert_eq!(
        proto_admin_active_stream_sort_direction(
            synctv_proto::admin::SortDirection::Unspecified as i32
        )
        .expect("unspecified active stream sort direction should be accepted"),
        synctv_proto::admin::SortDirection::Desc
    );
    assert_eq!(
        map_admin_playlist_sort(synctv_proto::client::PlaylistListSortBy::Unspecified as i32)
            .expect("unspecified playlist sort should be accepted"),
        synctv_core::models::PlaylistListSortBy::Position
    );
    assert_eq!(
        map_admin_media_sort(synctv_proto::client::MediaListSortBy::Unspecified as i32)
            .expect("unspecified media sort should be accepted"),
        synctv_core::models::MediaListSortBy::Position
    );
    assert_eq!(
        map_resource_availability_filter(
            synctv_proto::client::ResourceAvailabilityFilter::All as i32
        )
        .expect("all availability filter should be accepted"),
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_load_room_creator_status_maps_missing_creator_to_banned() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = make_user_service(&pool);
    let room = make_test_room(RoomStatus::Active);

    let status = load_room_creator_status(&user_service, &room)
        .await
        .expect("missing creator should map to unavailable status");

    assert_eq!(status, UserStatus::Banned);
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_load_room_creator_status_propagates_backend_failures() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = make_user_service(&pool);
    let room = make_test_room(RoomStatus::Active);

    pool.close().await;

    let error = load_room_creator_status(&user_service, &room)
        .await
        .expect_err("creator lookup backend failures must propagate");

    assert!(
        matches!(
            error,
            ApiError::Internal(_) | ApiError::ServiceUnavailable(_)
        ),
        "unexpected error kind: {error:?}"
    );
}

#[tokio::test]
async fn test_active_room_stream_media_ids_unions_local_and_registry_streams() {
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
        "rtmp-room",
        "rtmp-stream",
    );
    tracker.insert(
        "user-overlap".to_string(),
        room_id.to_string(),
        shared_media_id.to_string(),
        "rtmp-room-2",
        "rtmp-stream-2",
    );

    let registry = synctv_livestream::relay::local_stream_registry();
    registry
        .try_register_publisher(
            &room_id.to_string(),
            &shared_media_id.to_string(),
            "node-a",
            "user-overlap",
            "127.0.0.1:50051",
        )
        .await
        .expect("shared publisher should register");
    registry
        .try_register_publisher(
            &room_id.to_string(),
            &remote_media_id.to_string(),
            "node-b",
            "user-remote",
            "127.0.0.1:50052",
        )
        .await
        .expect("remote publisher should register");
    registry
        .try_register_publisher(
            &other_room_id.to_string(),
            &other_media_id.to_string(),
            "node-c",
            "user-other",
            "127.0.0.1:50053",
        )
        .await
        .expect("other room publisher should register");

    let (event_sender, _event_receiver) = mpsc::channel(64);
    let pull_manager = Arc::new(PullStreamManager::new(
        registry.clone(),
        event_sender.clone(),
    ));
    let external_publish_manager = Arc::new(
        ExternalPublishManager::new(
            registry.clone(),
            "node-local".to_string(),
            event_sender.clone(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .expect("external publish manager should build"),
    );
    let infra = Arc::new(LiveStreamingInfrastructure::new(
        registry,
        event_sender,
        pull_manager,
        external_publish_manager,
        tracker,
    ));

    let media_ids = active_room_stream_media_ids_for_infra(Some(&infra), &room_id).await;

    assert_eq!(
        media_ids,
        vec![local_media_id, shared_media_id, remote_media_id]
    );
}

#[tokio::test]
async fn test_force_disconnect_user_publishes_cluster_kick_event() {
    let connection_service: Arc<dyn RealtimeConnectionService> = Arc::new(ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));
    connection_service.start();

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
        .expect("force_disconnect_user should publish a kick event");
    match published.event {
        RealtimeEvent::KickUser {
            user_id: published_user_id,
            reason,
            ..
        } => {
            assert_eq!(published_user_id, user_id);
            assert_eq!(reason, "user_deleted");
        }
        other => panic!("expected KickUser event, got {other:?}"),
    }
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
    }
}

async fn create_db_user(
    user_repo: &UserRepository,
    username: &str,
    role: UserRole,
) -> synctv_core::models::User {
    user_repo
        .create(&make_db_user(username, role))
        .await
        .expect("create test user")
}

#[test]
fn test_admin_user_to_proto_all_roles() {
    let public_id_codec = crate::PublicIdCodec::plain();
    for (role, expected) in [
        (UserRole::Root, synctv_proto::common::UserRole::Root as i32),
        (
            UserRole::Admin,
            synctv_proto::common::UserRole::Admin as i32,
        ),
        (UserRole::User, synctv_proto::common::UserRole::User as i32),
    ] {
        let user = make_test_user(role, UserStatus::Active);
        let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), &public_id_codec)
            .expect("admin user proto should encode");
        assert_eq!(proto.role, expected);
    }
}

#[test]
fn test_admin_user_to_proto_all_statuses() {
    let public_id_codec = crate::PublicIdCodec::plain();
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
        let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), &public_id_codec)
            .expect("admin user proto should encode");
        assert_eq!(proto.status, expected);
    }
}

#[test]
fn test_admin_user_to_proto_fields() {
    let public_id_codec = crate::PublicIdCodec::plain();
    let user = make_test_user(UserRole::Admin, UserStatus::Active);
    let proto = try_admin_user_to_proto(&user, Some("admin@test.com"), &public_id_codec)
        .expect("admin user proto should encode");

    assert_eq!(proto.id, public_id_codec.encode_user_id(user.id).unwrap());
    assert_eq!(proto.username, "admin_test");
    assert_eq!(proto.email, "admin@test.com");
}

#[test]
fn test_admin_user_to_proto_no_email() {
    let public_id_codec = crate::PublicIdCodec::plain();
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let proto = try_admin_user_to_proto(&user, None, &public_id_codec)
        .expect("admin user proto should encode");
    assert_eq!(proto.email, "");
}

#[test]
fn review_rows_preserve_absent_optional_fields() {
    let public_id_codec = crate::PublicIdCodec::plain();
    let requested_at = chrono::Utc::now();

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
            oauth2_email_trusted: false,
            webauthn_credential_id: None,
            webauthn_credential_name: None,
        },
        &public_id_codec,
    )
    .expect("registration review should encode");

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
            name: "room".to_string(),
            description: String::new(),
            status: synctv_core::models::ReviewStatus::Pending,
            requested_at,
            reviewed_at: None,
            reviewed_by: None,
            rejection_reason: None,
        },
        &public_id_codec,
    )
    .expect("room creation review should encode");

    assert_eq!(creation.reviewed_by, None);
    assert_eq!(creation.rejection_reason, None);

    let join = room_join_review_row_to_proto(
        &synctv_core::repository::RoomJoinReviewRecord {
            id: ReviewRequestId::new(),
            room_id: RoomId::new(),
            room_name: "room".to_string(),
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
    .expect("room join review should encode");

    assert_eq!(join.reviewed_by, None);
    assert_eq!(join.rejection_reason, None);
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
        role,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: chrono::Utc::now(),
        is_online: false,
        is_active: true,
    }
}

#[test]
fn test_admin_room_member_to_proto() {
    let member = make_test_member(RoomRole::Admin);
    let public_id_codec = crate::PublicIdCodec::plain();
    let proto = try_admin_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(member.role.permissions()),
        &public_id_codec,
    )
    .expect("admin room member proto should encode");

    assert_eq!(
        proto.room_id,
        public_id_codec.encode_room_id(member.room_id).unwrap()
    );
    assert_eq!(
        proto.user_id,
        public_id_codec.encode_user_id(member.user_id).unwrap()
    );
    assert_eq!(proto.username, "testmember");
    assert_eq!(
        proto.role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert!(!proto.is_online);
}

#[test]
fn test_admin_room_member_to_proto_with_permissions() {
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xAA;
    member.removed_permissions = 0x55;
    member.admin_added_permissions = 0xCC;
    member.admin_removed_permissions = 0x33;
    let public_id_codec = crate::PublicIdCodec::plain();
    let proto = try_admin_room_member_to_proto_with_permissions(
        &member,
        member.effective_permissions(member.role.permissions()),
        &public_id_codec,
    )
    .expect("admin room member proto should encode");

    assert_eq!(proto.added_permissions, 0xAA);
    assert_eq!(proto.removed_permissions, 0x55);
    assert_eq!(proto.admin_added_permissions, 0xCC);
    assert_eq!(proto.admin_removed_permissions, 0x33);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_admins_includes_root_and_admin_only() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);
    let now = chrono::Utc::now();

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
        user_repo
            .create(&user)
            .await
            .expect("user should be created");
    }

    let response = admin_api
        .list_admins(synctv_proto::admin::ListAdminsRequest {
            page: 1,
            page_size: 10,
            search: String::new(),
            sort_by: synctv_proto::admin::UserListSortBy::Username as i32,
            sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
        })
        .await
        .expect("list admins should succeed");

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_list_endpoints_reject_invalid_proto_requests() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;

    let list_rooms_error = admin_api
        .list_rooms(synctv_proto::admin::ListRoomsRequest {
            page: -1,
            page_size: 101,
            status: synctv_proto::common::RoomStatus::Unspecified as i32,
            search: String::new(),
            creator_id: String::new(),
            is_banned: None,
            sort_by: synctv_proto::admin::RoomListSortBy::Unspecified as i32,
            sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
        })
        .await
        .expect_err("invalid list_rooms request must be rejected");
    assert!(matches!(list_rooms_error, ApiError::InvalidInput(_)));

    let get_room_members_error = admin_api
        .get_room_members(synctv_proto::admin::GetRoomMembersRequest {
            room_id: "room_abc123def456".to_string(),
            page: -1,
            page_size: 101,
            search: "a".repeat(101),
            role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
            sort_by: synctv_proto::admin::RoomMemberListSortBy::Unspecified as i32,
            sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
        })
        .await
        .expect_err("invalid get_room_members request must be rejected");
    assert!(matches!(get_room_members_error, ApiError::InvalidInput(_)));

    let list_users_error = admin_api
        .list_users(synctv_proto::admin::ListUsersRequest {
            page: -1,
            page_size: 101,
            status: synctv_proto::common::UserStatus::Unspecified as i32,
            role: synctv_proto::common::UserRole::Unspecified as i32,
            search: "a".repeat(101),
            is_banned: None,
            sort_by: synctv_proto::admin::UserListSortBy::Unspecified as i32,
            sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
        })
        .await
        .expect_err("invalid list_users request must be rejected");
    assert!(matches!(list_users_error, ApiError::InvalidInput(_)));

    let get_user_rooms_error = admin_api
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
        .await
        .expect_err("invalid get_user_rooms request must be rejected");
    assert!(matches!(get_user_rooms_error, ApiError::InvalidInput(_)));

    let list_admins_error = admin_api
        .list_admins(synctv_proto::admin::ListAdminsRequest {
            page: -1,
            page_size: 101,
            search: "a".repeat(101),
            sort_by: synctv_proto::admin::UserListSortBy::Unspecified as i32,
            sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
        })
        .await
        .expect_err("invalid list_admins request must be rejected");
    assert!(matches!(list_admins_error, ApiError::InvalidInput(_)));

    let list_active_streams_error = admin_api
        .list_active_streams(synctv_proto::admin::ListActiveStreamsRequest {
            page: -1,
            page_size: 101,
            room_id: String::new(),
            user_id: String::new(),
            node_id: String::new(),
            search: "a".repeat(101),
            sort_by: synctv_proto::admin::ActiveStreamListSortBy::Unspecified as i32,
            sort_direction: synctv_proto::admin::SortDirection::Unspecified as i32,
        })
        .await
        .expect_err("invalid list_active_streams request must be rejected");
    assert!(matches!(
        list_active_streams_error,
        ApiError::InvalidInput(_)
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_client_list_endpoints_reject_invalid_proto_requests() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool);
    let admin_user = create_db_user(&user_repo, "proto_list_admin", UserRole::Root).await;

    let list_playlists_error = admin_api
        .list_playlists(
            "abc123def456",
            synctv_proto::client::ListPlaylistsRequest {
                parent_id: String::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: "Bad Provider".to_string(),
                provider_instance_name: "bad name".to_string(),
                dynamic_only: None,
                sort_by: synctv_proto::client::PlaylistListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
            },
            &admin_user.id,
        )
        .await
        .expect_err("invalid list_playlists request must be rejected");
    assert!(matches!(list_playlists_error, ApiError::InvalidInput(_)));

    let list_media_error = admin_api
        .list_media(
            "abc123def456",
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: "Bad Provider".to_string(),
                provider_instance_name: "bad name".to_string(),
                sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
            &admin_user.id,
        )
        .await
        .expect_err("invalid list_media request must be rejected");
    assert!(matches!(list_media_error, ApiError::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_admins_respects_search_sort_and_pagination() {
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

    let response = admin_api
        .list_admins(synctv_proto::admin::ListAdminsRequest {
            page: 1,
            page_size: 1,
            search: "admin".to_string(),
            sort_by: synctv_proto::admin::UserListSortBy::Username as i32,
            sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
        })
        .await
        .expect("admin list should succeed");

    assert_eq!(response.admins.len(), 1);
    assert_eq!(response.admins[0].username, "alpha-admin");
    assert_eq!(response.total, 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_user_rooms_respects_related_room_query_semantics() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let target_user = create_db_user(&user_repo, "target-user-rooms", UserRole::User).await;
    let other_owner = create_db_user(&user_repo, "other-owner-rooms", UserRole::User).await;

    let owned_room = admin_api
        .room_service
        .create_room(
            "Beta Owned Room".to_string(),
            "owned room".to_string(),
            target_user.id,
            None,
            None,
        )
        .await
        .expect("owned room should be created")
        .0;
    let joined_room = admin_api
        .room_service
        .create_room(
            "Alpha Joined Room".to_string(),
            "joined room".to_string(),
            other_owner.id,
            None,
            None,
        )
        .await
        .expect("joined room should be created")
        .0;
    admin_api
        .room_service
        .join_room(joined_room.id, target_user.id, None)
        .await
        .expect("target user should join related room");

    let response = admin_api
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
        .await
        .expect("related room list should succeed");

    assert_eq!(response.total, 2);
    assert_eq!(response.rooms.len(), 1);
    assert_eq!(response.rooms[0].name, "Alpha Joined Room");
    assert_eq!(
        response.rooms[0].id,
        public_room_id(&admin_api, joined_room.id)
    );

    let page2 = admin_api
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
        .await
        .expect("second page should succeed");

    assert_eq!(page2.total, 2);
    assert_eq!(page2.rooms.len(), 1);
    assert_eq!(page2.rooms[0].name, "Beta Owned Room");
    assert_eq!(page2.rooms[0].id, public_room_id(&admin_api, owned_room.id));
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

#[test]
fn test_check_role_hierarchy_admin_cannot_operate_on_root() {
    let result = check_role_hierarchy(UserRole::Admin, UserRole::Root, "ban");
    assert!(
        result.is_err(),
        "Admin should not be able to ban Root users"
    );
    match result {
        Err(ApiError::Authorization(msg)) => {
            assert!(msg.contains("root"), "Error should mention root: {msg}");
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[test]
fn test_check_role_hierarchy_admin_cannot_operate_on_admin() {
    let result = check_role_hierarchy(UserRole::Admin, UserRole::Admin, "delete");
    assert!(
        result.is_err(),
        "Admin should not be able to delete Admin users"
    );
    match result {
        Err(ApiError::Authorization(msg)) => {
            assert!(msg.contains("root"), "Error should mention root: {msg}");
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[test]
fn test_check_role_hierarchy_admin_cannot_update_admin_preferences() {
    let result = check_role_hierarchy(UserRole::Admin, UserRole::Admin, "update preferences");
    assert!(
        result.is_err(),
        "Admin should not be able to update Admin user preferences"
    );
    match result {
        Err(ApiError::Authorization(msg)) => {
            assert!(msg.contains("admin"), "Error should mention admin: {msg}");
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[test]
fn test_check_role_hierarchy_admin_can_operate_on_user() {
    assert!(check_role_hierarchy(UserRole::Admin, UserRole::User, "ban").is_ok());
}

/// Verify that proto_role_to_user_role maps Admin role correctly
/// (prerequisite for the role elevation check).
#[test]
fn test_proto_role_to_user_role_admin() {
    let admin_role =
        crate::impls::client::proto_role_to_user_role(synctv_proto::common::UserRole::Admin as i32)
            .expect("should parse admin role");
    assert_eq!(admin_role, UserRole::Admin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_settings_group_projects_registered_defaults() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let response = admin_api
        .get_settings_group(
            synctv_proto::admin::GetSettingsGroupRequest {
                group: "server".to_string(),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("get_settings_group should project registered defaults");

    let group = response.group.expect("settings group response");
    assert_eq!(group.name, "server");

    let payload: serde_json::Value =
        serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
    assert_eq!(payload["allow_room_creation"], true);
    assert_eq!(payload["max_rooms_per_user"], 10);
    assert_eq!(payload["max_members_per_room"], 100);
    assert_eq!(payload["max_chat_messages"], 500);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_email_settings_group_does_not_project_smtp_password() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    admin_api
        .settings_service
        .update("email.smtp_password", "smtp-secret".to_string())
        .await
        .expect("smtp password should be persisted internally");

    let response = admin_api
        .get_settings_group(
            synctv_proto::admin::GetSettingsGroupRequest {
                group: "email".to_string(),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("get_settings_group should project email settings");

    let group = response.group.expect("settings group response");
    let payload: serde_json::Value =
        serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
    assert_eq!(payload["enabled"], false);
    assert!(
        payload.get("smtp_password").is_none(),
        "smtp password must not be returned in settings projection: {payload}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_settings_ignores_hidden_registered_settings_without_warning_path() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let server_id = admin_api
        .settings_registry
        .as_ref()
        .expect("settings registry")
        .get_or_initialize_server_id()
        .await
        .expect("server id should initialize");
    assert!(server_id.starts_with("srv_"));

    let response = admin_api
        .get_settings_group(
            synctv_proto::admin::GetSettingsGroupRequest {
                group: "server".to_string(),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("hidden registered settings should not break projection");

    let group = response.group.expect("settings group response");
    let payload: serde_json::Value =
        serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
    assert!(
        payload.get("identity_id").is_none(),
        "server identity must stay hidden from admin settings projection: {payload}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_settings_maps_group_entries_to_flat_keys_and_upserts_missing_rows() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    admin_api
        .update_settings(
            synctv_proto::admin::UpdateSettingsRequest {
                group: "server".to_string(),
                settings: std::collections::HashMap::from([(
                    "max_rooms_per_user".to_string(),
                    "42".to_string(),
                )]),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("update_settings should upsert missing flat settings");

    let max_rooms_per_user = admin_api
        .settings_service
        .get("server.max_rooms_per_user")
        .await
        .expect("max_rooms_per_user should be persisted");
    assert_eq!(max_rooms_per_user.group_name, "server");
    assert_eq!(max_rooms_per_user.value, "42");

    let response = admin_api
        .get_settings_group(
            synctv_proto::admin::GetSettingsGroupRequest {
                group: "server".to_string(),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("get_settings_group should reflect persisted overrides");
    let group = response.group.expect("settings group response");
    let payload: serde_json::Value =
        serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
    assert_eq!(payload["max_rooms_per_user"], 42);
    assert_eq!(payload["allow_room_creation"], true);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_settings_persists_when_global_cache_invalidation_fanout_fails() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;
    let failing_fanout = Arc::new(FailingRealtimeFanout::default());
    let admin_api = AdminApiImpl::new_with_runtime(
        AdminApiConfig {
            room_service: admin_api.room_service.clone(),
            user_service: admin_api.user_service.clone(),
            settings_service: admin_api.settings_service.clone(),
            settings_registry: admin_api.settings_registry.clone(),
            email_service: admin_api.email_service.clone(),
            connection_service: admin_api.connection_service.clone(),
            provider_instance_manager: admin_api.provider_instance_manager.clone(),
            live_streaming_infrastructure: admin_api.live_streaming_infrastructure.clone(),
            publish_key_service: admin_api.publish_key_service.clone(),
            config: admin_api.config.clone(),
            audit_service: admin_api.audit_service.clone(),
            public_id_codec: admin_api.public_id_codec.clone(),
        },
        AdminApiRuntime {
            realtime_fanout: failing_fanout.clone(),
            realtime_event_service: admin_api.realtime_event_service.clone(),
            provider_stores: admin_api.provider_stores.clone(),
            provider_access_service: admin_api.provider_access_service.clone(),
            request_executor: admin_api.request_executor.clone(),
        },
    );

    admin_api
        .update_settings(
            synctv_proto::admin::UpdateSettingsRequest {
                group: "server".to_string(),
                settings: std::collections::HashMap::from([(
                    "max_rooms_per_user".to_string(),
                    "43".to_string(),
                )]),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("settings update should persist even when cache fanout fails");

    let max_rooms_per_user = admin_api
        .settings_service
        .get("server.max_rooms_per_user")
        .await
        .expect("settings update should be persisted");
    assert_eq!(max_rooms_per_user.value, "43");
    assert_eq!(failing_fanout.publish_attempts(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_email_settings_rejects_enabled_incomplete_config() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    let error = admin_api
        .update_settings(
            synctv_proto::admin::UpdateSettingsRequest {
                group: "email".to_string(),
                settings: std::collections::HashMap::from([(
                    "enabled".to_string(),
                    "true".to_string(),
                )]),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect_err("enabling incomplete email settings should fail");

    assert!(
        matches!(error, ApiError::InvalidInput(ref message) if message.contains("email.smtp_host")),
        "expected missing smtp_host validation error, got: {error:?}"
    );
    assert!(
        admin_api
            .settings_service
            .get("email.enabled")
            .await
            .is_err(),
        "failed email settings update must not persist email.enabled"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_email_settings_accepts_enabled_complete_config() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

    admin_api
        .update_settings(
            synctv_proto::admin::UpdateSettingsRequest {
                group: "email".to_string(),
                settings: std::collections::HashMap::from([
                    ("enabled".to_string(), "true".to_string()),
                    ("smtp_host".to_string(), "smtp.example.com".to_string()),
                    ("smtp_port".to_string(), "587".to_string()),
                    ("from_email".to_string(), "noreply@example.com".to_string()),
                ]),
            },
            &UserId::new(),
            &RequestContext::default(),
        )
        .await
        .expect("complete enabled email settings should update");

    let enabled = admin_api
        .settings_service
        .get("email.enabled")
        .await
        .expect("email.enabled should be persisted");
    assert_eq!(enabled.value, "true");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_publishes_kick_user_realtime_event() {
    let (_postgres, pool) = create_test_pool().await;
    let admin_api = {
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        (admin_api, redis_publish_rx)
    };
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_admin", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "victim_user", UserRole::User).await;

    let (admin_api, mut redis_publish_rx) = admin_api;
    let request = synctv_proto::admin::DeleteUserRequest {
        user_id: public_user_id(&admin_api, target_user.id),
    };
    let ctx = RequestContext::default();

    admin_api
        .delete_user(request, &admin_user.id, &ctx)
        .await
        .expect("delete user should succeed");

    let publish = tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
        .await
        .expect("expected cluster publish")
        .expect("publish request");

    match publish.event {
        RealtimeEvent::KickUser {
            user_id, reason, ..
        } => {
            assert_eq!(user_id, target_user.id);
            assert_eq!(reason, "user_deleted");
        }
        other => panic!("expected KickUser event, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_allows_missing_email_and_explicit_status() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let admin_user = create_db_user(&user_repo, "root_create_user_attrs", UserRole::Root).await;

    let response = admin_api
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
        .await
        .expect("create_user should accept optional email and explicit status");

    let created = response.user.expect("created user");
    assert_eq!(created.username, "attr_user");
    assert_eq!(created.email, "");
    assert_eq!(created.role, synctv_proto::common::UserRole::Admin as i32);
    assert_eq!(
        created.status,
        synctv_proto::common::UserStatus::Active as i32
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_user_username_preserves_missing_email() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_update_username", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "target_update_username", UserRole::User).await;

    let response = admin_api
        .update_user_username(
            synctv_proto::admin::UpdateUserUsernameRequest {
                user_id: public_user_id(&admin_api, target_user.id),
                new_username: "target_update_renamed".to_string(),
            },
            &admin_user.id,
            &RequestContext::default(),
        )
        .await
        .expect("username-only update should not require binding email");

    let updated = response.user.expect("updated user");
    assert_eq!(updated.username, "target_update_renamed");
    assert_eq!(updated.email, "");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_cleans_memberships_and_preserves_kick_user_event() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_delete_membership", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "victim_membership", UserRole::User).await;
    let owner_one = create_db_user(&user_repo, "room_owner_one", UserRole::User).await;
    let owner_two = create_db_user(&user_repo, "room_owner_two", UserRole::User).await;

    let room_one = create_room_with_member(&admin_api, &owner_one.id, &target_user.id).await;
    let room_two = create_room_with_member(&admin_api, &owner_two.id, &target_user.id).await;

    admin_api
        .delete_user(
            synctv_proto::admin::DeleteUserRequest {
                user_id: public_user_id(&admin_api, target_user.id),
            },
            &admin_user.id,
            &RequestContext::default(),
        )
        .await
        .expect("delete user should succeed");

    let room_one_member = admin_api
        .room_service
        .get_member(&room_one.id, &target_user.id)
        .await
        .expect("room one membership query should succeed");
    assert!(
        room_one_member.is_none(),
        "deleted user must no longer appear as an active room member"
    );

    let room_two_member = admin_api
        .room_service
        .get_member(&room_two.id, &target_user.id)
        .await
        .expect("room two membership query should succeed");
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_deletes_owned_rooms_and_publishes_room_deleted() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_delete_owned_room", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "owned_room_victim", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            "victim owned room".to_string(),
            "will be deleted with owner".to_string(),
            target_user.id,
            None,
            None,
        )
        .await
        .expect("create owned room")
        .0;

    admin_api
        .delete_user(
            synctv_proto::admin::DeleteUserRequest {
                user_id: public_user_id(&admin_api, target_user.id),
            },
            &admin_user.id,
            &RequestContext::default(),
        )
        .await
        .expect("delete user should succeed");

    assert!(
        room_repo
            .get_by_id(&room.id)
            .await
            .expect("query room")
            .is_none(),
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_cleans_memberships_and_preserves_kick_user_event() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_user_ban_cleanup", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_membership", UserRole::User).await;
    let owner = create_db_user(&user_repo, "room_owner_ban", UserRole::User).await;

    let room = create_room_with_member(&admin_api, &owner.id, &target_user.id).await;

    let response = admin_api
        .ban_user(
            synctv_proto::admin::BanUserRequest {
                user_id: public_user_id(&admin_api, target_user.id),
                reason: "policy".to_string(),
            },
            &admin_user.id,
            UserRole::Root,
            &RequestContext::default(),
        )
        .await
        .expect("ban user should succeed");

    let banned_user = response
        .user
        .expect("ban_user must return the updated user");
    assert_eq!(
        banned_user.status,
        synctv_proto::common::UserStatus::Banned as i32
    );
    assert_eq!(
        banned_user.banned_by,
        public_user_id(&admin_api, admin_user.id)
    );
    assert_eq!(banned_user.banned_reason, "policy");

    let room_member = admin_api
        .room_service
        .get_member(&room.id, &target_user.id)
        .await
        .expect("room membership query should succeed");
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_resets_playback_for_media_created_by_target() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_playback", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_playback_creator", UserRole::User).await;
    let room_owner = create_db_user(&user_repo, "playback_room_owner", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            "ban-playback-room".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .expect("create room")
        .0;

    let media = create_room_media(&pool, room.id, target_user.id, "banned-media").await;

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, room_owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("start playback");

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
        .await
        .expect("ban user should succeed");

    let state = admin_api
        .room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .expect("load playback state after ban");

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_disconnects_owned_room_connections() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_owned_room", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "banned_owned_room_creator", UserRole::User).await;
    let member_user = create_db_user(&user_repo, "owned_room_member", UserRole::User).await;

    let room = create_room_with_member(&admin_api, &target_user.id, &member_user.id).await;

    let mut disconnect_rx = admin_api.connection_service.subscribe_disconnect();
    admin_api
        .connection_service
        .register("owned-room-conn".to_string(), member_user.id)
        .await
        .expect("register connection");
    admin_api
        .connection_service
        .join_room("owned-room-conn", room.id)
        .await
        .expect("join room connection");

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
        .await
        .expect("ban user should succeed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut saw_room_disconnect = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let signal = tokio::time::timeout(remaining, disconnect_rx.recv())
            .await
            .expect("disconnect signal must arrive before timeout")
            .expect("disconnect channel must stay open");

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_publishes_room_owner_inactive_event_for_owned_rooms() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_ban_owned_room_event", UserRole::Root).await;
    let target_user = create_db_user(&user_repo, "owned_room_event_creator", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            "owned-room-event".to_string(),
            String::new(),
            target_user.id,
            None,
            None,
        )
        .await
        .expect("create room")
        .0;

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
        .await
        .expect("ban user should succeed");

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_batch_ban_users_resets_playback_for_media_created_by_target() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "root_batch_ban_playback", UserRole::Root).await;
    let target_user =
        create_db_user(&user_repo, "batch_banned_media_creator", UserRole::User).await;
    let room_owner = create_db_user(&user_repo, "batch_ban_playback_owner", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            "batch-ban-playback-room".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .expect("create room")
        .0;

    let media = create_room_media(&pool, room.id, target_user.id, "batch-banned-media").await;

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, room_owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("start playback");

    let response = admin_api
        .batch_ban_users(
            synctv_proto::admin::BatchBanUsersRequest {
                user_ids: vec![public_user_id(&admin_api, target_user.id)],
                reason: "policy".to_string(),
            },
            &admin_user.id,
            UserRole::Root,
            &RequestContext::default(),
        )
        .await
        .expect("batch ban should succeed");

    assert_eq!(response.succeeded, 1);
    assert_eq!(response.failed, 0);

    let state = admin_api
        .room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .expect("load playback state after batch ban");

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_batch_ban_users_publishes_room_owner_inactive_event_for_owned_rooms() {
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

    let room = admin_api
        .room_service
        .create_room(
            "batch-owned-room-event".to_string(),
            String::new(),
            target_user.id,
            None,
            None,
        )
        .await
        .expect("create room")
        .0;

    let response = admin_api
        .batch_ban_users(
            synctv_proto::admin::BatchBanUsersRequest {
                user_ids: vec![public_user_id(&admin_api, target_user.id)],
                reason: "policy".to_string(),
            },
            &admin_user.id,
            UserRole::Root,
            &RequestContext::default(),
        )
        .await
        .expect("batch ban should succeed");

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
}

#[test]
fn test_parse_batch_user_ids_trims_and_preserves_order() {
    let public_id_codec = crate::PublicIdCodec::plain();
    let first = UserId::expect_positive(901);
    let second = UserId::expect_positive(902);
    let parsed = parse_batch_user_ids(
        &[
            format!("  {}  ", public_id_codec.encode_user_id(first).unwrap()),
            public_id_codec.encode_user_id(second).unwrap(),
        ],
        &public_id_codec,
    )
    .expect("batch ids should parse");

    assert_eq!(parsed, vec![first, second]);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_member_permissions_bypasses_room_creator_constraint_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_member_update", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_member_update", UserRole::User).await;
    let target = create_db_user(&user_repo, "target_member_update", UserRole::User).await;

    let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

    let response = admin_api
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
        .await
        .expect("global admin should update member permissions without room creator identity");

    let member = response.member.expect("updated member response");
    assert_eq!(member.user_id, public_user_id(&admin_api, target.id));
    assert_eq!(
        member.role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert_eq!(member.admin_added_permissions, 0b1010);
    assert_eq!(member.admin_removed_permissions, 0b0101);

    let persisted = admin_api
        .room_service
        .get_member(&room.id, &target.id)
        .await
        .expect("persisted member query should succeed")
        .expect("target should remain a member");
    assert_eq!(persisted.role, RoomRole::Admin);
    assert_eq!(persisted.admin_added_permissions, 0b1010);
    assert_eq!(persisted.admin_removed_permissions, 0b0101);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_member_kick", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_member_kick", UserRole::User).await;
    let target = create_db_user(&user_repo, "target_member_kick", UserRole::User).await;

    let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

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
        .expect("global admin should kick member without being in the room");
    assert!(response.success);

    let persisted = admin_api
        .room_service
        .get_member(&room.id, &target.id)
        .await
        .expect("persisted member query should succeed");
    assert!(
        persisted.is_none(),
        "kicked member should no longer appear as an active room member"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_stream_info_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, infra, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let _global_admin =
        create_db_user(&user_repo, "global_admin_stream_info", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_stream_info", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room stream info test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    infra
        .registry()
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-local",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .expect("publisher should register");

    let response = admin_api
        .get_stream_info(
            &public_room_id(&admin_api, room.id),
            &public_media_id(&admin_api, media.id),
        )
        .await
        .expect("global admin should inspect stream info without room membership");
    assert!(response.active);
    let publisher = response.publisher.expect("publisher info");
    assert_eq!(publisher.user_id, public_user_id(&admin_api, owner.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_reports_local_unpublish_enqueue_failure() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, infra, mut redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_stream_kick_failure",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_stream_kick_failure", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room stream kick failure test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    infra
        .registry()
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-local",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .expect("publisher should register");

    let err = admin_api
        .kick_stream(
            synctv_proto::admin::KickStreamRequest {
                room_id: public_room_id(&admin_api, room.id),
                media_id: public_media_id(&admin_api, media.id),
                reason: "test failure".to_string(),
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect_err("closed StreamHub event receiver must surface kick failure");

    assert!(
        matches!(err, ApiError::Internal(_) | ApiError::ServiceUnavailable(_)),
        "unexpected kick_stream error: {err:?}"
    );
    assert!(
        infra
            .registry()
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .expect("publisher lookup should succeed")
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_publishes_cluster_event_for_remote_publisher() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, infra, mut redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_remote_stream_kick",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_remote_stream_kick", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "remote stream kick test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "remote-stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    infra
        .registry()
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-remote",
            &registry_owner_id,
            "127.0.0.1:50052",
        )
        .await
        .expect("remote publisher should register");

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
        .await
        .expect("remote stream kick should publish cluster event");

    let request = redis_publish_rx
        .recv()
        .await
        .expect("remote kick should publish a replica-wide event");
    assert!(matches!(
        request.event,
        RealtimeEvent::KickPublisher { room_id, media_id, ref reason, .. }
            if room_id == room.id && media_id == media.id && reason == "remote owner"
    ));
    assert!(
        infra
            .registry()
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .expect("publisher lookup should succeed")
            .is_some(),
        "non-owner replica must not unregister remote publisher"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_stream_reports_remote_fanout_failure() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, infra, redis_publish_rx) =
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

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "remote stream fanout failure test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "remote-stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();

    infra
        .registry()
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-remote",
            &registry_owner_id,
            "127.0.0.1:50052",
        )
        .await
        .expect("remote publisher should register");

    let err = admin_api
        .kick_stream(
            synctv_proto::admin::KickStreamRequest {
                room_id: public_room_id(&admin_api, room.id),
                media_id: public_media_id(&admin_api, media.id),
                reason: "remote fanout failure".to_string(),
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect_err("remote stream kick must fail if fanout fails");

    assert!(
        matches!(err, ApiError::Internal(_) | ApiError::ServiceUnavailable(_)),
        "unexpected kick_stream error: {err:?}"
    );
    assert!(
        infra
            .registry()
            .get_publisher(&registry_room_id, &registry_media_id)
            .await
            .expect("publisher lookup should succeed")
            .is_some(),
        "failed remote kick must not unregister remote publisher"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_room_streams_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, infra, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let _global_admin =
        create_db_user(&user_repo, "global_admin_stream_list", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_stream_list", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room stream list test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let registry_room_id = room.id.to_string();
    let registry_media_id = media.id.to_string();
    let registry_owner_id = owner.id.to_string();
    let encoded_room_id = admin_api
        .public_id_codec
        .encode_room_id(room.id)
        .expect("room id should encode");
    let encoded_media_id = admin_api
        .public_id_codec
        .encode_media_id(media.id)
        .expect("media id should encode");

    infra
        .registry()
        .try_register_publisher(
            &registry_room_id,
            &registry_media_id,
            "node-a",
            &registry_owner_id,
            "127.0.0.1:50051",
        )
        .await
        .expect("publisher should register");

    let response = admin_api
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
        .await
        .expect("global admin should list room streams without room membership");
    assert_eq!(response.total, 1);
    assert_eq!(response.streams.len(), 1);
    assert_eq!(response.streams[0].media_id, encoded_media_id);
    assert!(response.streams[0].active);
}

#[test]
fn build_room_stream_list_response_applies_search_sort_and_pagination() {
    let public_id_codec = crate::PublicIdCodec::plain();
    let media_ids = vec![
        MediaId::expect_positive(301),
        MediaId::expect_positive(302),
        MediaId::expect_positive(303),
    ];
    let mut expected_ids = media_ids
        .iter()
        .map(|media_id| public_id_codec.encode_media_id(*media_id).unwrap())
        .collect::<Vec<_>>();
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
    .expect("valid stream ids should encode");

    assert_eq!(response.total, 3);
    assert_eq!(response.streams.len(), 1);
    assert_eq!(response.streams[0].media_id, expected_ids[1]);
    assert!(response.streams[0].active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_publish_key_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _infra, _redis_publish_rx) =
        make_admin_api_with_livestream_for_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_publish_key", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_publish_key", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room publish key test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "stream-media").await;
    let public_room_id = admin_api
        .public_id_codec
        .encode_room_id(room.id)
        .expect("room id should encode");
    let public_media_id = admin_api
        .public_id_codec
        .encode_media_id(media.id)
        .expect("media id should encode");

    let response = admin_api
        .create_publish_key_for_actor(
            &public_room_id,
            &public_media_id,
            &owner.id,
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("global admin should create publish key without room membership");

    let claims = admin_api
        .publish_key_service
        .as_ref()
        .expect("publish key service should be configured")
        .validate_publish_key_for_stream_claims(&response.publish_key, &room.id, &media.id)
        .await
        .expect("generated publish key should be valid for the target stream");

    assert_eq!(claims.user_id, owner.id.to_string());
    assert!(!response.publish_key.is_empty());
    assert!(response.rtmp_url.contains(&public_room_id));
    assert!(response.stream_key.contains(&public_media_id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_start_playback_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_start", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_start", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room playback start test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    admin_api
        .start_playback(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::StartPlaybackRequest {
                media_id: public_media_id(&admin_api, media.id),
                playlist_id: String::new(),
                target: Vec::new(),
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("global admin should start playback without room membership");

    let state = admin_api
        .room_service
        .get_playback_state(&room.id)
        .await
        .expect("playback state query should succeed");
    assert_eq!(state.playing_media_id.as_ref(), Some(&media.id));
    assert!(state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stop_playback_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_stop", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_stop", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room playback stop test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("owner should be able to seed playback state");

    admin_api
        .stop_playback(
            &public_room_id(&admin_api, room.id),
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("global admin should stop playback without room membership");

    let state = admin_api
        .room_service
        .get_playback_state(&room.id)
        .await
        .expect("playback state query should succeed");
    assert!(state.playing_media_id.is_none());
    assert!(!state.is_playing);
    assert!((state.position - 0.0).abs() < f64::EPSILON);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_update_playback_runs_provider_lifecycle_transition() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(make_user_service(&pool));
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let progress_calls = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn MediaProvider> = Arc::new(AdminLifecycleTestProvider {
        progress_calls: progress_calls.clone(),
    });
    let provider_for_factory = provider.clone();
    let mut providers_manager = ProvidersManager::new(provider_instance_manager.clone())
        .expect("providers manager should build");
    providers_manager.register_factory(
        "direct_url",
        Box::new(move |_instance_id, _config, _instance_manager| Ok(provider_for_factory.clone())),
    );
    providers_manager
        .create_provider("direct_url", "direct_url", &serde_json::Value::Null)
        .await
        .expect("create lifecycle test provider");
    let room_service = Arc::new(
        synctv_core::service::RoomService::new_with_providers_for_tests(
            pool.clone(),
            (*user_service).clone(),
            Arc::new(providers_manager),
        )
        .expect("room service should build"),
    );
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    let email_service = Arc::new(
        EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
            &settings_registry,
        )))
        .expect("email service"),
    );
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();
    let audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> =
        Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
            "admin-lifecycle-test:provider:".to_string(),
        ));
    let admin_api = AdminApiImpl::new_with_runtime(
        AdminApiConfig {
            room_service,
            user_service,
            settings_service,
            settings_registry: Some(settings_registry),
            email_service,
            connection_service: connection_manager,
            provider_instance_manager,
            live_streaming_infrastructure: None,
            publish_key_service: None,
            config: Arc::new(Config::default()),
            audit_service,
            public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        },
        AdminApiRuntime {
            realtime_fanout: crate::realtime_fanout::disabled_realtime_fanout_service(),
            realtime_event_service: Arc::new(LocalNoopRealtimeEventService::new()),
            provider_stores: Some(provider_stores.clone()),
            provider_access_service: None,
            request_executor: None,
        },
    );

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_playback_lifecycle",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_playback_lifecycle", UserRole::User).await;
    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room playback lifecycle test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media_repo = MediaRepository::new(pool.clone());
    let media = media_repo
        .create(&synctv_core::models::Media::from_provider_with_params(
            FromProviderParams {
                playlist_id: None,
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "provider lifecycle media".to_string(),
                description: String::new(),
                source_config: serde_json::json!({"item_id": "admin-lifecycle"}),
                provider_name: "direct_url".to_string(),
                provider_instance_name: None,
                position: 0.0,
            },
        ))
        .await
        .expect("create provider media");
    let state = admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("seed provider playback state");
    let lifecycle_store = provider_stores.load("playback_lifecycle");
    lifecycle_store
        .set(
            &format!("room:{}:sessions", room.id),
            &serde_json::json!({
                "sessions": [{
                    "provider": "direct_url",
                    "provider_instance_name": null,
                    "actor_user_id": owner.id.as_i64(),
                    "credential_owner_id": owner.id.as_i64(),
                    "source_config": {"item_id": "admin-lifecycle"},
                    "room_target_key": format!("media:{}", media.id),
                    "provider_session_id": "admin-session",
                    "started": true,
                    "started_at_millis": chrono::Utc::now().timestamp_millis(),
                    "last_progress_position": 0.0,
                    "last_progress_at_millis": chrono::Utc::now().timestamp_millis(),
                    "last_paused": false
                }]
            }),
            std::time::Duration::from_mins(1),
        )
        .await
        .expect("seed lifecycle session");

    admin_api
        .update_playback(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::UpdatePlaybackRequest {
                r#type: synctv_proto::client::PlaybackUpdateType::Pause as i32,
                playing: None,
                position: Some(12.5),
                speed: None,
                version: Some(state.version),
                expected_media_id: Some(
                    admin_api.public_id_codec.encode_media_id(media.id).unwrap(),
                ),
                expected_playlist_id: Some(String::new()),
                expected_target_hash: Some(state.target_hash()),
            },
            &global_admin.id,
            &RequestContext::default(),
        )
        .await
        .expect("admin update playback should succeed");

    assert_eq!(
        progress_calls
            .lock()
            .expect("progress calls lock")
            .as_slice(),
        [("admin-session".to_string(), 12.5, true)],
        "admin playback updates must trigger provider progress lifecycle hooks"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_playback_get", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_playback_get", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room playback get test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;
    let media = create_room_media(&pool, room.id, owner.id, "playback-media").await;

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("owner should be able to seed playback state");

    let response = admin_api
        .get_playback(&public_room_id(&admin_api, room.id), &global_admin.id, None)
        .await
        .expect("global admin should get playback without room membership");

    let state = response
        .playback_state
        .expect("playback state should be present");
    assert!(state.is_playing);
    assert_eq!(
        state.playing_media_id,
        public_media_id(&admin_api, media.id)
    );

    let result = response.playback.expect("playback should be present");
    assert_eq!(result.media_id, public_media_id(&admin_api, media.id));
    assert_eq!(result.room_id, public_room_id(&admin_api, room.id));
    assert_eq!(result.name, media.name);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_returns_state_when_playback_info_generation_fails_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let global_admin = create_db_user(
        &user_repo,
        "global_admin_playback_state_only",
        UserRole::Root,
    )
    .await;
    let owner = create_db_user(&user_repo, "room_owner_playback_state_only", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room playback degrade test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Broken Playback Provider".to_string(),
        description: String::new(),
        source_config: serde_json::json!({ "opaque": true }),
        provider_name: "live_proxy".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo.create(&media).await.expect("create media");

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("owner should seed playback state");

    let response = admin_api
        .get_playback(&public_room_id(&admin_api, room.id), &global_admin.id, None)
        .await
        .expect("global admin should get playback state even if playback info generation fails");

    let state = response
        .playback_state
        .expect("playback state should be present");
    assert!(state.is_playing);
    assert_eq!(
        state.playing_media_id,
        public_media_id(&admin_api, media.id)
    );
    assert!(
        response.playback.is_none(),
        "admin playback queries should degrade to state-only responses on playback info failures"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_for_provider_media_signs_proxy_urls_for_global_admin() {
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

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room provider playback get test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "provider-playback-media".to_string(),
        description: String::new(),
        source_config: serde_json::json!({
            "url": "https://example.com/video.mp4",
            "headers": {
                "Authorization": "Bearer admin-provider-token"
            }
        }),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo
        .create(&media)
        .await
        .expect("create provider media");

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("owner should be able to seed playback state");

    let response = admin_api
        .get_playback(&public_room_id(&admin_api, room.id), &global_admin.id, None)
        .await
        .expect("global admin should get signed provider playback");

    let result = response.playback.expect("playback should be present");
    let direct = result
        .playback_infos
        .get("direct")
        .expect("direct mode should be present");
    assert_eq!(direct.urls.len(), 1);
    assert!(
        direct.urls[0]
            .url
            .starts_with("/api/providers/proxy/direct_url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.urls[0].url
    );
    assert!(
        direct.urls[0].url.contains("/stream?"),
        "signed direct-url playback should use stream proxy contract, got {}",
        direct.urls[0].url
    );
    assert!(
        direct.urls[0].headers.is_empty(),
        "proxy-backed playback should not require client-side secret headers"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_for_provider_media_signs_proxy_urls_for_local_management_actor() {
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

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room provider playback get local management test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = synctv_core::models::Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "provider-playback-media".to_string(),
        description: String::new(),
        source_config: serde_json::json!({
            "url": "https://example.com/video.mp4",
            "headers": {
                "Authorization": "Bearer admin-provider-token"
            }
        }),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo
        .create(&media)
        .await
        .expect("create provider media");

    admin_api
        .room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .expect("owner should be able to seed playback state");

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = admin_api
        .get_playback(
            &public_room_id(&admin_api, room.id),
            &management_actor,
            None,
        )
        .await
        .expect("local management actor should get signed provider playback");

    let result = response.playback.expect("playback should be present");
    let direct = result
        .playback_infos
        .get("direct")
        .expect("direct mode should be present");
    assert_eq!(direct.urls.len(), 1);
    assert!(
        direct.urls[0]
            .url
            .starts_with("/api/providers/proxy/direct_url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.urls[0].url
    );
    assert!(
        direct.urls[0]
            .url
            .contains(&format!("uid={}", public_user_id(&admin_api, owner.id))),
        "local management playback must sign proxy URLs with a real room member, got {}",
        direct.urls[0].url
    );
    assert!(
        !direct.urls[0]
            .url
            .contains(&LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string()),
        "local management playback must not sign proxy URLs with the synthetic management actor"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playlists_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_list_playlists", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_list_playlists", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room list playlists test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-a".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create playlist");

    let response = admin_api
        .list_playlists(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::ListPlaylistsRequest {
                parent_id: String::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                dynamic_only: None,
                sort_by: synctv_proto::client::PlaylistListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should list playlists without room membership");

    assert_eq!(response.total, 1);
    assert_eq!(response.playlists.len(), 1);
    assert_eq!(
        response.playlists[0].id,
        public_playlist_id(&admin_api, playlist.id)
    );
    assert_eq!(response.playlists[0].name, "playlist-a");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playlist_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_get_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_get_playlist", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room get playlist test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-b".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create playlist");

    let response = admin_api
        .get_playlist(
            &public_room_id(&admin_api, room.id),
            &public_playlist_id(&admin_api, playlist.id),
            &global_admin.id,
        )
        .await
        .expect("global admin should get playlist without room membership");

    let response_playlist = response.playlist.expect("playlist should be returned");
    assert_eq!(
        response_playlist.id,
        public_playlist_id(&admin_api, playlist.id)
    );
    assert_eq!(response_playlist.name, "playlist-b");
    assert_eq!(response.media_count, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_playlist_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_update_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_update_playlist", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room update playlist test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-before".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create playlist");

    let response = admin_api
        .update_playlist(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::UpdatePlaylistRequest {
                playlist_id: public_playlist_id(&admin_api, playlist.id),
                name: "playlist-after".to_string(),
                description: String::new(),
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should update playlist without room membership");

    let updated = response.playlist.expect("playlist should be returned");
    assert_eq!(updated.id, public_playlist_id(&admin_api, playlist.id));
    assert_eq!(updated.name, "playlist-after");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_playlist_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_delete_playlist", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_delete_playlist", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room delete playlist test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-delete".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create playlist");

    let response = admin_api
        .delete_playlist(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::DeletePlaylistRequest {
                playlist_id: public_playlist_id(&admin_api, playlist.id),
                force: true,
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should delete playlist without room membership");

    assert!(response.success);
    let playlist_after = admin_api
        .room_service
        .playlist_service()
        .get_playlist(&playlist.id)
        .await
        .expect("playlist lookup should succeed");
    assert!(playlist_after.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_playlist_publishes_cascaded_playlist_and_media_events_for_global_admin() {
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

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room delete playlist cascade test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let parent_playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-delete-parent".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create parent playlist");
    let child_playlist = admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "playlist-delete-child".to_string(),
                description: String::new(),
                parent_id: Some(parent_playlist.id),
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("owner should create child playlist");
    let nested_media = admin_api
        .room_service
        .media_service()
        .add_media(
            room.id,
            owner.id,
            synctv_core::service::media::AddMediaRequest {
                playlist_id: Some(child_playlist.id),
                name: "playlist-delete-cascade-media".to_string(),
                description: String::new(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/admin-playlist-delete-cascade.mp4"
                }),
            },
        )
        .await
        .expect("owner should create nested media");

    let response = admin_api
        .delete_playlist(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::DeletePlaylistRequest {
                playlist_id: public_playlist_id(&admin_api, parent_playlist.id),
                force: true,
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should delete playlist without room membership");

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
            other => panic!("unexpected admin delete_playlist cascade event: {other:?}"),
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_media_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_list_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_list_media", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room list media test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-a").await;

    let response = admin_api
        .list_media(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should list media without room membership");

    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].id, public_media_id(&admin_api, media.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_room_settings_bypasses_room_membership_for_local_management_actor() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let owner = create_db_user(&user_repo, "room_owner_reset_room_settings", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room settings reset test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let customized = synctv_core::models::RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..synctv_core::models::RoomSettings::default()
    };
    admin_api
        .room_service
        .set_room_settings(&room.id, &customized)
        .await
        .expect("room settings should be updated");

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = admin_api
        .reset_room_settings(
            synctv_proto::admin::ResetRoomSettingsRequest {
                room_id: public_room_id(&admin_api, room.id),
            },
            &management_actor,
        )
        .await
        .expect("local management actor should reset room settings without membership");

    let response_room = response.room.expect("response should include room");
    let room_id = admin_api
        .public_id_codec
        .decode_room_id(&response_room.id)
        .expect("response room id should decode");
    let settings = admin_api
        .room_service
        .get_room_settings(&room_id)
        .await
        .expect("room settings should be readable");
    assert!(settings.chat_enabled.0);
    assert!(!settings.allow_guest_join.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_room_bypasses_room_membership_for_local_management_actor() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = create_db_user(&user_repo, "room_owner_delete_room", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room delete test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let response = admin_api
        .delete_room(
            synctv_proto::admin::DeleteRoomRequest {
                room_id: public_room_id(&admin_api, room.id),
            },
            &management_actor,
            &RequestContext::default(),
        )
        .await
        .expect("local management actor should delete room without membership");

    assert!(response.success);
    assert!(
        room_repo
            .get_by_id(&room.id)
            .await
            .expect("room lookup should succeed")
            .is_none(),
        "room should be deleted by local management actor"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_media_respects_search_filters_and_sort_for_static_root() {
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

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room list media filter test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    admin_api
        .room_service
        .playlist_service()
        .create_playlist(
            room.id,
            owner.id,
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id,
                name: "Alpha Folder".to_string(),
                description: String::new(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .expect("playlist should be created");

    create_room_media(&pool, room.id, owner.id, "Alpha Media").await;
    create_room_media(&pool, room.id, owner.id, "Beta Media").await;

    let response = admin_api
        .list_media(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 10,
                search: "alpha".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
            &global_admin.id,
        )
        .await
        .expect("list media should succeed");

    assert_eq!(response.total, 1);
    assert_eq!(response.folder_count, 0);
    assert_eq!(response.file_count, 1);
    assert!(response.playlists.is_empty());
    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].name, "Alpha Media");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_media_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_edit_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_edit_media", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room edit media test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-edit").await;

    let response = admin_api
        .edit_media(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::EditMediaRequest {
                media_id: public_media_id(&admin_api, media.id),
                name: "media-edited".to_string(),
                description: String::new(),
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should edit media without room membership");

    let updated = response.media.expect("media should be returned");
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
        other => panic!("expected MediaUpdated event, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_local_management_actor_preserves_username_in_media_notifications() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let owner = create_db_user(
        &user_repo,
        "room_owner_management_media_notifications",
        UserRole::User,
    )
    .await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room management media notification test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = create_room_media(&pool, room.id, owner.id, "management-media").await;
    let management_actor = LOCAL_MANAGEMENT_ACTOR_USER_ID;
    let mut notification_rx = admin_api.room_service.notification_service().subscribe();

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
        .await
        .expect("local management actor should edit media");

    let updated_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (_, event) = notification_rx
                .recv()
                .await
                .expect("notification should arrive");
            match event {
                synctv_core::service::notification::RoomEvent::MediaUpdated {
                    media_id,
                    username,
                    ..
                } if media_id == media.id => break username,
                _ => {}
            }
        }
    })
    .await
    .expect("media updated notification should arrive");
    assert_eq!(updated_event, "local-management");

    admin_api
        .delete_media(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::DeleteMediaRequest {
                media_id: public_media_id(&admin_api, media.id),
                force: false,
            },
            &management_actor,
        )
        .await
        .expect("local management actor should delete media");

    let removed_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (_, event) = notification_rx
                .recv()
                .await
                .expect("notification should arrive");
            match event {
                synctv_core::service::notification::RoomEvent::MediaRemoved {
                    media_id,
                    username,
                    user_id,
                } if media_id == media.id => break (username, user_id),
                _ => {}
            }
        }
    })
    .await
    .expect("media removed notification should arrive");
    assert_eq!(removed_event.0, "local-management");
    assert_eq!(removed_event.1, Some(management_actor));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_media_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin =
        create_db_user(&user_repo, "global_admin_delete_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_delete_media", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room delete media test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media = create_room_media(&pool, room.id, owner.id, "media-delete").await;

    let response = admin_api
        .delete_media(
            &public_room_id(&admin_api, room.id),
            synctv_proto::client::DeleteMediaRequest {
                media_id: public_media_id(&admin_api, media.id),
                force: true,
            },
            &global_admin.id,
        )
        .await
        .expect("global admin should delete media without room membership");

    assert!(response.success);
    let media_after = admin_api
        .room_service
        .media_service()
        .get_media(&media.id)
        .await
        .expect("media lookup should succeed");
    assert!(media_after.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_bypasses_room_membership_requirement_for_global_admin() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());

    let global_admin = create_db_user(&user_repo, "global_admin_move_media", UserRole::Root).await;
    let owner = create_db_user(&user_repo, "room_owner_move_media", UserRole::User).await;

    let room = admin_api
        .room_service
        .create_room(
            format!("room-{}", synctv_common::snanoid!(6)),
            "room move media test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created")
        .0;

    let media_a = create_room_media(&pool, room.id, owner.id, "media-move-a").await;
    let media_b = create_room_media(&pool, room.id, owner.id, "media-move-b").await;

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
        .await
        .expect("global admin should move media without room membership");

    let media_a_after = admin_api
        .room_service
        .media_service()
        .get_media(&media_a.id)
        .await
        .expect("media lookup should succeed")
        .expect("media_a should exist");
    let media_b_after = admin_api
        .room_service
        .media_service()
        .get_media(&media_b.id)
        .await
        .expect("media lookup should succeed")
        .expect("media_b should exist");
    assert!(media_b_after.position < media_a_after.position);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_room_publishes_room_banned_realtime_event() {
    let (_postgres, pool) = create_test_pool().await;
    let (admin_api, mut redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let admin_user = create_db_user(&user_repo, "room_admin", UserRole::Root).await;

    let room = make_test_room_model(&admin_user.id);
    let room = room_repo.create(&room).await.expect("create room");

    admin_api
        .ban_room(
            synctv_proto::admin::BanRoomRequest {
                room_id: public_room_id(&admin_api, room.id),
                reason: "moderation".to_string(),
            },
            &admin_user.id,
            &RequestContext::default(),
        )
        .await
        .expect("ban room should succeed");

    let publish = tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
        .await
        .expect("expected cluster publish")
        .expect("publish request");

    match publish.event {
        RealtimeEvent::RoomBanned {
            room_id, banned_by, ..
        } => {
            assert_eq!(room_id, room.id);
            assert_eq!(banned_by, admin_user.id);
        }
        other => panic!("expected RoomBanned event, got {other:?}"),
    }
}
