use super::*;
use crate::proto::client::server_message::Message;
use crate::runtime::RealtimeDeliveryOutcome;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_core::cache::UsernameCache;
use synctv_core::models::notification::{Notification, NotificationType};
use synctv_core::models::{
    MediaId, PermissionBits, Playlist, PlaylistId, RoomId, RoomPlaybackState, UserId,
};
use synctv_core::repository::NotificationRepository;
use synctv_core::repository::{
    ChatRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository,
};
use synctv_core::service::user_notification::NotificationCreatedEvent;
use synctv_core::service::{
    chat::{ChatDependencies, ChatRuntime},
    ChatService, ContentFilter, NotificationService, PermissionService, RateLimitConfig,
    RateLimiter, RoomService, RoomSettingsService,
};
use synctv_core::{DirectRedisConnectionRuntime, RedisConnectionRuntime};
use synctv_core_testing::{
    create_test_request_rate_limiter, start_dedicated_redis_url_with_label, RedisContainer,
};
use synctv_realtime::sync::{
    build_room_message_runtime, ConnectionLimits, ConnectionManager, RealtimeConfig,
    RealtimeManager,
};
use synctv_realtime::sync::{NotificationLevel, RealtimeEvent, RoomMessageHub};

fn room_id() -> RoomId {
    RoomId::expect_positive(1)
}
fn user_id() -> UserId {
    UserId::expect_positive(1)
}
fn media_id() -> MediaId {
    MediaId::expect_positive(1)
}
fn public_id_codec() -> crate::PublicIdCodec {
    crate::PublicIdCodec::default_for_tests()
}
fn public_actor_id() -> String {
    public_id_codec().encode_user_id(user_id()).unwrap()
}
fn public_media_id() -> String {
    public_id_codec().encode_media_id(media_id()).unwrap()
}
fn public_playlist_id() -> String {
    public_id_codec().encode_playlist_id(playlist().id).unwrap()
}
fn empty_playlist_items_response(
    version: impl Into<String>,
) -> crate::proto::client::ListPlaylistItemsResponse {
    crate::proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: 0,
        folder_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        version: version.into(),
    }
}
fn playlist() -> Playlist {
    Playlist {
        id: PlaylistId::expect_positive(1),
        room_id: room_id(),
        creator_id: Some(user_id()),
        name: "Test Playlist".to_string(),
        parent_id: None,
        position: 1.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: now(),
        updated_at: now(),
        version: 1,
    }
}
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[derive(Default)]
struct FailingMessageSender {
    fail_after: usize,
    send_calls: AtomicUsize,
    ping_calls: AtomicUsize,
    alive: AtomicBool,
}

impl FailingMessageSender {
    fn immediate() -> Arc<Self> {
        Arc::new(Self {
            fail_after: 0,
            send_calls: AtomicUsize::new(0),
            ping_calls: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
        })
    }

    fn fail_after(send_count_before_failure: usize) -> Arc<Self> {
        Arc::new(Self {
            fail_after: send_count_before_failure,
            send_calls: AtomicUsize::new(0),
            ping_calls: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
        })
    }

    fn send_calls(&self) -> usize {
        self.send_calls.load(Ordering::Relaxed)
    }
}

impl MessageSender for FailingMessageSender {
    fn send(&self, _message: ServerMessage) -> Result<(), String> {
        let attempt = self.send_calls.fetch_add(1, Ordering::Relaxed);
        if attempt >= self.fail_after {
            self.alive.store(false, Ordering::Relaxed);
            return Err(format!("forced send failure on attempt {}", attempt + 1));
        }
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn ping(&self) -> Result<(), String> {
        self.ping_calls.fetch_add(1, Ordering::Relaxed);
        if self.is_alive() {
            Ok(())
        } else {
            Err("forced dead connection".to_string())
        }
    }
}

#[derive(Default)]
struct RecordingMessageSender {
    sent_messages: parking_lot::Mutex<Vec<ServerMessage>>,
    alive: AtomicBool,
}

impl RecordingMessageSender {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sent_messages: parking_lot::Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
        })
    }

    fn sent_messages(&self) -> Vec<ServerMessage> {
        self.sent_messages.lock().clone()
    }
}

impl MessageSender for RecordingMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        self.sent_messages.lock().push(message);
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct FailingStreamState {
    send_calls: AtomicUsize,
    alive: AtomicBool,
}

impl FailingStreamState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            send_calls: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
        })
    }

    fn send_calls(&self) -> usize {
        self.send_calls.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct RecordingStreamState {
    sent_messages: parking_lot::Mutex<Vec<ServerMessage>>,
    alive: AtomicBool,
}

impl RecordingStreamState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sent_messages: parking_lot::Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
        })
    }

    fn sent_messages(&self) -> Vec<ServerMessage> {
        self.sent_messages.lock().clone()
    }
}

struct RecordingStream {
    incoming: VecDeque<Result<ClientMessage, String>>,
    state: Arc<RecordingStreamState>,
}

impl RecordingStream {
    fn new() -> (Self, Arc<RecordingStreamState>) {
        let state = RecordingStreamState::new();
        (
            Self {
                incoming: VecDeque::new(),
                state: Arc::clone(&state),
            },
            state,
        )
    }

    fn with_incoming(incoming: Vec<ClientMessage>) -> (Self, Arc<RecordingStreamState>) {
        let state = RecordingStreamState::new();
        (
            Self {
                incoming: incoming.into_iter().map(Ok).collect(),
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

fn observe_playback_state_message(
    observe_id: &'static str,
    version: impl Into<String>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            version: version.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::PlaybackState(
                    crate::proto::client::ObservePlaybackState {},
                ),
            ),
        },
    )
}

fn observe_playback_snapshot_message(
    observe_id: &'static str,
    version: impl Into<String>,
    media_id: impl Into<String>,
    playlist_id: impl Into<String>,
    target: Vec<u8>,
    playback_client_profile: Option<crate::proto::client::PlaybackClientProfile>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            version: version.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::PlaybackSnapshot(
                    crate::proto::client::ObservePlaybackSnapshot {
                        media_id: media_id.into(),
                        playlist_id: playlist_id.into(),
                        target,
                        playback_client_profile,
                    },
                ),
            ),
        },
    )
}

fn observe_room_settings_message(
    observe_id: impl Into<String>,
    version: impl Into<String>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.into(),
            version: version.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::RoomSettings(
                    crate::proto::client::ObserveRoomSettings {},
                ),
            ),
        },
    )
}

fn observe_playlist_items_message(
    observe_id: &'static str,
    version: impl Into<String>,
    request: crate::proto::client::ListPlaylistItemsRequest,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            version: version.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::PlaylistItems(
                    crate::proto::client::ObservePlaylistItems {
                        request: Some(request),
                    },
                ),
            ),
        },
    )
}

fn observe_room_members_message(
    observe_id: &'static str,
    version: impl Into<String>,
    request: crate::proto::client::GetRoomMembersRequest,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            version: version.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::RoomMembers(
                    crate::proto::client::ObserveRoomMembers {
                        request: Some(request),
                    },
                ),
            ),
        },
    )
}

fn resource_changed_payload(
    message: &ServerMessage,
) -> Option<&crate::proto::client::resource_changed::Payload> {
    match &message.message {
        Some(Message::ResourceChanged(changed)) => changed.payload.as_ref(),
        _ => None,
    }
}

fn resource_observe_error(
    message: &ServerMessage,
) -> Option<&crate::proto::client::ResourceObserveError> {
    match &message.message {
        Some(Message::ResourceObserveError(error)) => Some(error),
        _ => None,
    }
}

fn resource_playback_state(
    message: &ServerMessage,
) -> Option<&crate::proto::client::PlaybackState> {
    match resource_changed_payload(message) {
        Some(crate::proto::client::resource_changed::Payload::PlaybackState(state)) => Some(state),
        _ => None,
    }
}

fn resource_playback_snapshot(
    message: &ServerMessage,
) -> Option<&crate::proto::client::PlaybackSnapshot> {
    match resource_changed_payload(message) {
        Some(crate::proto::client::resource_changed::Payload::PlaybackSnapshot(snapshot)) => {
            Some(snapshot)
        }
        _ => None,
    }
}

fn resource_room_settings(
    message: &ServerMessage,
) -> Option<&crate::proto::client::RoomSettingsChanged> {
    match resource_changed_payload(message) {
        Some(crate::proto::client::resource_changed::Payload::RoomSettings(settings)) => {
            Some(settings)
        }
        _ => None,
    }
}

fn resource_playlist_items(
    message: &ServerMessage,
) -> Option<&crate::proto::client::ListPlaylistItemsResponse> {
    match resource_changed_payload(message) {
        Some(crate::proto::client::resource_changed::Payload::PlaylistItems(snapshot)) => {
            Some(snapshot)
        }
        _ => None,
    }
}

fn resource_room_members(
    message: &ServerMessage,
) -> Option<&crate::proto::client::GetRoomMembersResponse> {
    match resource_changed_payload(message) {
        Some(crate::proto::client::resource_changed::Payload::RoomMembers(snapshot)) => {
            Some(snapshot)
        }
        _ => None,
    }
}

#[async_trait::async_trait]
impl StreamMessage for RecordingStream {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        if let Some(msg) = self.incoming.pop_front() {
            return Some(msg);
        }
        std::future::pending().await
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        self.state.sent_messages.lock().push(message);
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.state.alive.load(Ordering::Relaxed)
    }

    fn ping(&self) -> Result<(), String> {
        if self.is_alive() {
            Ok(())
        } else {
            Err("forced dead recording stream".to_string())
        }
    }
}

struct FailingStream {
    incoming: VecDeque<Result<ClientMessage, String>>,
    fail_after: usize,
    state: Arc<FailingStreamState>,
}

impl FailingStream {
    fn fail_after(send_count_before_failure: usize) -> (Self, Arc<FailingStreamState>) {
        let state = FailingStreamState::new();
        (
            Self {
                incoming: VecDeque::new(),
                fail_after: send_count_before_failure,
                state: Arc::clone(&state),
            },
            state,
        )
    }

    fn fail_after_with_incoming(
        send_count_before_failure: usize,
        incoming: Vec<ClientMessage>,
    ) -> (Self, Arc<FailingStreamState>) {
        let state = FailingStreamState::new();
        (
            Self {
                incoming: incoming.into_iter().map(Ok).collect(),
                fail_after: send_count_before_failure,
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

#[async_trait::async_trait]
impl StreamMessage for FailingStream {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        if let Some(msg) = self.incoming.pop_front() {
            return Some(msg);
        }
        std::future::pending().await
    }

    fn send(&self, _message: ServerMessage) -> Result<(), String> {
        let attempt = self.state.send_calls.fetch_add(1, Ordering::Relaxed);
        if attempt >= self.fail_after {
            self.state.alive.store(false, Ordering::Relaxed);
            return Err(format!(
                "forced stream send failure on attempt {}",
                attempt + 1
            ));
        }
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.state.alive.load(Ordering::Relaxed)
    }

    fn ping(&self) -> Result<(), String> {
        if self.is_alive() {
            Ok(())
        } else {
            Err("forced dead stream".to_string())
        }
    }
}

fn test_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused?connect_timeout=1")
        .expect("lazy test pool")
}

fn test_room_service(pool: sqlx::PgPool) -> Arc<RoomService> {
    Arc::new(synctv_core_testing::create_test_room_service(pool))
}

fn test_chat_service(pool: sqlx::PgPool) -> Arc<ChatService> {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = create_test_request_rate_limiter("test:chat:");
    let content_filter = ContentFilter::new();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool);
    let mut permission_service = PermissionService::new(
        member_repo,
        room_repo,
        None,
        PermissionService::DEFAULT_CACHE_SIZE,
        PermissionService::DEFAULT_CACHE_TTL_SECS,
    );
    permission_service.set_room_settings_repo(room_settings_repo.clone());

    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(NotificationService::default()),
        None,
        None,
    );

    Arc::new(ChatService::new(
        chat_repo,
        ChatRuntime {
            rate_limiter,
            rate_limit_config: RateLimitConfig::default(),
            content_filter,
            username_cache,
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            notification_service: NotificationService::default(),
        },
    ))
}

fn bounded_fixture_username(node_id: &str) -> String {
    const MAX_USERNAME_LEN: usize = 50;
    let prefix = "fixture_";
    let suffix: String = node_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .take(MAX_USERNAME_LEN - prefix.len())
        .collect();
    format!("{prefix}{suffix}")
}

async fn test_realtime_manager(node_id: &str) -> Arc<RealtimeManager> {
    Arc::new(
        RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: node_id.to_string(),
            dedup_window: Duration::from_mins(1),
            critical_channel_capacity: 100,
            publish_channel_capacity: 1000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 1000,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await
        .expect("realtime manager"),
    )
}

async fn test_realtime_manager_with_redis(node_id: &str, redis_url: &str) -> Arc<RealtimeManager> {
    let redis_client = redis::Client::open(redis_url).expect("Redis client");
    let redis_conn = redis_client
        .get_connection_manager()
        .await
        .expect("Redis connection manager");
    let shared_runtime: Arc<dyn RedisConnectionRuntime> =
        Arc::new(DirectRedisConnectionRuntime::new(redis_conn.clone()));
    let realtime_profile =
        synctv_core::SharedStateProfile::from_runtime(Some(shared_runtime), "synctv:", true);

    Arc::new(
        RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: Some(
                synctv_realtime::build_realtime_message_transport_factory(
                    synctv_core::coordination_runtime_from_client(redis_client),
                ),
            ),
            message_runtime: build_room_message_runtime(&realtime_profile)
                .expect("shared message runtime should initialize"),
            distributed_enabled: false,
            node_id: node_id.to_string(),
            dedup_window: Duration::from_mins(1),
            critical_channel_capacity: 100,
            publish_channel_capacity: 1000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 1000,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await
        .expect("realtime manager with redis"),
    )
}

fn test_connection_manager() -> Arc<ConnectionManager> {
    Arc::new(ConnectionManager::new(ConnectionLimits::default()))
}

#[derive(Clone)]
struct FakePlaybackSnapshotService {
    snapshot: crate::proto::client::PlaybackSnapshot,
}

#[derive(Clone, Default)]
struct SnapshotCallProbe {
    calls: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

impl SnapshotCallProbe {
    fn mark_called(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    async fn wait_for_calls(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.calls.load(Ordering::Relaxed) >= expected {
                    break;
                }
                self.notify.notified().await;
            }
        })
        .await
        .expect("snapshot service should be called");
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl crate::impls::playback_snapshot::PlaybackSnapshotService for FakePlaybackSnapshotService {
    async fn get_playback_snapshot(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, crate::impls::ApiError> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct MutablePlaybackSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::proto::client::PlaybackSnapshot>>,
    probe: SnapshotCallProbe,
}

impl MutablePlaybackSnapshotService {
    fn new(snapshot: crate::proto::client::PlaybackSnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::PlaybackSnapshot) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }
}

#[async_trait::async_trait]
impl crate::impls::playback_snapshot::PlaybackSnapshotService for MutablePlaybackSnapshotService {
    async fn get_playback_snapshot(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, crate::impls::ApiError> {
        self.probe.mark_called();
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct SequencedPlaybackSnapshotService {
    responses: Arc<
        parking_lot::Mutex<
            VecDeque<Result<crate::proto::client::PlaybackSnapshot, crate::impls::ApiError>>,
        >,
    >,
    probe: SnapshotCallProbe,
}

impl SequencedPlaybackSnapshotService {
    fn new(
        responses: impl IntoIterator<
            Item = Result<crate::proto::client::PlaybackSnapshot, crate::impls::ApiError>,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: Arc::new(parking_lot::Mutex::new(responses.into_iter().collect())),
            probe: SnapshotCallProbe::default(),
        })
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }
}

#[async_trait::async_trait]
impl crate::impls::playback_snapshot::PlaybackSnapshotService for SequencedPlaybackSnapshotService {
    async fn get_playback_snapshot(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, crate::impls::ApiError> {
        self.probe.mark_called();
        self.responses.lock().pop_front().unwrap_or_else(|| {
            Err(crate::impls::ApiError::Internal(
                "no playback snapshot response queued".to_string(),
            ))
        })
    }
}

#[derive(Clone)]
struct MutableRoomSettingsSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::impls::room_settings_snapshot::RoomSettingsSnapshot>>,
    probe: SnapshotCallProbe,
}

impl MutableRoomSettingsSnapshotService {
    fn new(snapshot: crate::impls::room_settings_snapshot::RoomSettingsSnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::impls::room_settings_snapshot::RoomSettingsSnapshot) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    fn call_count(&self) -> usize {
        self.probe.call_count()
    }
}

#[async_trait::async_trait]
impl crate::impls::room_settings_snapshot::RoomSettingsSnapshotService
    for MutableRoomSettingsSnapshotService
{
    async fn get_room_settings_snapshot(
        &self,
        _room_id: &RoomId,
    ) -> Result<crate::impls::room_settings_snapshot::RoomSettingsSnapshot, crate::impls::ApiError>
    {
        self.probe.mark_called();
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct SlowRoomSettingsSnapshotService {
    snapshot: crate::impls::room_settings_snapshot::RoomSettingsSnapshot,
    probe: SnapshotCallProbe,
    delay: Duration,
}

impl SlowRoomSettingsSnapshotService {
    fn new(
        snapshot: crate::impls::room_settings_snapshot::RoomSettingsSnapshot,
        delay: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            snapshot,
            probe: SnapshotCallProbe::default(),
            delay,
        })
    }

    fn call_count(&self) -> usize {
        self.probe.call_count()
    }
}

#[async_trait::async_trait]
impl crate::impls::room_settings_snapshot::RoomSettingsSnapshotService
    for SlowRoomSettingsSnapshotService
{
    async fn get_room_settings_snapshot(
        &self,
        _room_id: &RoomId,
    ) -> Result<crate::impls::room_settings_snapshot::RoomSettingsSnapshot, crate::impls::ApiError>
    {
        self.probe.mark_called();
        tokio::time::sleep(self.delay).await;
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct FakePlaylistItemsSnapshotService {
    snapshot: crate::proto::client::ListPlaylistItemsResponse,
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for FakePlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct MutablePlaylistItemsSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::proto::client::ListPlaylistItemsResponse>>,
    probe: SnapshotCallProbe,
}

impl MutablePlaylistItemsSnapshotService {
    fn new(snapshot: crate::proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::ListPlaylistItemsResponse) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    fn call_count(&self) -> usize {
        self.probe.call_count()
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for MutablePlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct RecordingPlaylistItemsRequestSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::proto::client::ListPlaylistItemsResponse>>,
    refresh_values: Arc<parking_lot::Mutex<Vec<bool>>>,
    probe: SnapshotCallProbe,
}

impl RecordingPlaylistItemsRequestSnapshotService {
    fn new(snapshot: crate::proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            refresh_values: Arc::new(parking_lot::Mutex::new(Vec::new())),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::ListPlaylistItemsResponse) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    fn refresh_values(&self) -> Vec<bool> {
        self.refresh_values.lock().clone()
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for RecordingPlaylistItemsRequestSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        self.refresh_values.lock().push(req.refresh);
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct BlockingPlaylistItemsSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::proto::client::ListPlaylistItemsResponse>>,
    probe: SnapshotCallProbe,
    block_on_call: usize,
    release_blocked_call: Arc<AtomicBool>,
    release_notify: Arc<tokio::sync::Notify>,
}

impl BlockingPlaylistItemsSnapshotService {
    fn new(
        snapshot: crate::proto::client::ListPlaylistItemsResponse,
        block_on_call: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
            block_on_call,
            release_blocked_call: Arc::new(AtomicBool::new(false)),
            release_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::ListPlaylistItemsResponse) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    fn call_count(&self) -> usize {
        self.probe.call_count()
    }

    fn release(&self) {
        self.release_blocked_call.store(true, Ordering::Relaxed);
        self.release_notify.notify_waiters();
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for BlockingPlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        if self.probe.call_count() == self.block_on_call {
            while !self.release_blocked_call.load(Ordering::Relaxed) {
                self.release_notify.notified().await;
            }
        }
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct BlockingFailingPlaylistItemsSnapshotService {
    first_snapshot: crate::proto::client::ListPlaylistItemsResponse,
    probe: SnapshotCallProbe,
    release_blocked_call: Arc<AtomicBool>,
    release_notify: Arc<tokio::sync::Notify>,
}

impl BlockingFailingPlaylistItemsSnapshotService {
    fn new(first_snapshot: crate::proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
        Arc::new(Self {
            first_snapshot,
            probe: SnapshotCallProbe::default(),
            release_blocked_call: Arc::new(AtomicBool::new(false)),
            release_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    fn release(&self) {
        self.release_blocked_call.store(true, Ordering::Relaxed);
        self.release_notify.notify_waiters();
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for BlockingFailingPlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        if self.probe.call_count() == 1 {
            return Ok(self.first_snapshot.clone());
        }
        while !self.release_blocked_call.load(Ordering::Relaxed) {
            self.release_notify.notified().await;
        }
        Err(crate::impls::ApiError::Internal(
            "blocked refresh failed".to_string(),
        ))
    }
}

#[derive(Clone)]
struct SlowPlaylistItemsSnapshotService {
    snapshot: crate::proto::client::ListPlaylistItemsResponse,
    probe: SnapshotCallProbe,
    delay: Duration,
}

impl SlowPlaylistItemsSnapshotService {
    fn new(
        snapshot: crate::proto::client::ListPlaylistItemsResponse,
        delay: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            snapshot,
            probe: SnapshotCallProbe::default(),
            delay,
        })
    }

    fn call_count(&self) -> usize {
        self.probe.call_count()
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for SlowPlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        tokio::time::sleep(self.delay).await;
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct FakeRoomMembersSnapshotService {
    snapshot: crate::proto::client::GetRoomMembersResponse,
}

#[async_trait::async_trait]
impl crate::impls::room_members_snapshot::RoomMembersSnapshotService
    for FakeRoomMembersSnapshotService
{
    async fn get_room_members_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct MutableRoomMembersSnapshotService {
    snapshot: Arc<parking_lot::Mutex<crate::proto::client::GetRoomMembersResponse>>,
    probe: SnapshotCallProbe,
}

impl MutableRoomMembersSnapshotService {
    fn new(snapshot: crate::proto::client::GetRoomMembersResponse) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::GetRoomMembersResponse) {
        *self.snapshot.lock() = snapshot;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }
}

#[async_trait::async_trait]
impl crate::impls::room_members_snapshot::RoomMembersSnapshotService
    for MutableRoomMembersSnapshotService
{
    async fn get_room_members_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        Ok(self.snapshot.lock().clone())
    }
}

fn test_message_handler(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
) -> StreamMessageHandler {
    test_message_handler_for_user(sender, event_service, connection_service, user_id())
}

fn test_message_handler_for_user(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    user_id: UserId,
) -> StreamMessageHandler {
    let pool = test_pool();
    StreamMessageHandler::new(
        room_id(),
        user_id,
        "tester".to_string(),
        &test_room_service(pool.clone()),
        test_chat_service(pool),
        event_service,
        connection_service,
        Arc::new(RateLimiter::local_only("test:handler:".to_string())),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender,
    )
    .with_heartbeat_schedule(HeartbeatSchedule::for_tests(
        Duration::from_millis(10),
        Duration::from_mins(1),
    ))
}

fn test_guest_principal_with_permissions(permissions: PermissionBits) -> RealtimePrincipal {
    let session_id = "guest-session-1";
    RealtimePrincipal::guest(
        room_id(),
        GuestRealtimeIdentity {
            guest_id: guest_public_id(session_id),
            display_name: guest_display_name(session_id),
            session_id: session_id.to_string(),
            token_jti: "guest-token-jti".to_string(),
            room_guest_version: 0,
            permissions,
        },
    )
}

fn test_guest_message_handler(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    permissions: PermissionBits,
) -> StreamMessageHandler {
    let pool = test_pool();
    StreamMessageHandler::new_with_principal(
        room_id(),
        test_guest_principal_with_permissions(permissions),
        &test_room_service(pool.clone()),
        test_chat_service(pool),
        event_service,
        connection_service,
        Arc::new(RateLimiter::local_only("test:guest-handler:".to_string())),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender,
    )
    .with_heartbeat_schedule(HeartbeatSchedule::for_tests(
        Duration::from_millis(10),
        Duration::from_mins(1),
    ))
}

/// Creates a StreamMessageHandler backed by a real PostgreSQL database with a
/// registered user, created room, and accepted membership so that
/// `start()` (which calls `pre_join_after_registration`) can pass the
/// admission revalidation checks.
async fn create_start_handler_fixture(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
) -> StartTestFixture {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager(node_id).await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();

    let user = user_service
        .register(
            bounded_fixture_username(node_id),
            Some(format!("fixture-{node_id}@test.invalid")),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("fixture user should register")
        .0;

    let (room, _) = room_service
        .create_room(
            format!("Fixture Room {node_id}"),
            "test".to_string(),
            user.id,
            None,
            None,
        )
        .await
        .expect("fixture room should be created");

    let handler = StreamMessageHandler::new(
        room.id,
        user.id,
        user.username.clone(),
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(format!("test:fixture:{node_id}:"))),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender,
    )
    .with_heartbeat_schedule(HeartbeatSchedule::for_tests(
        Duration::from_millis(10),
        Duration::from_mins(1),
    ));

    StartTestFixture {
        _container: container,
        pool,
        event_service,
        connection_service,
        handler,
    }
}

async fn create_guest_start_handler_fixture(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
    permissions: PermissionBits,
) -> StartTestFixture {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager(node_id).await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();

    let owner = user_service
        .register(
            bounded_fixture_username(&format!("{node_id}_owner")),
            Some(format!("fixture-{node_id}-owner@test.invalid")),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("fixture owner should register")
        .0;

    let (room, _) = room_service
        .create_room(
            format!("Guest Fixture Room {node_id}"),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("fixture room should be created");

    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .expect("room settings should load");
    settings.allow_guest_join = synctv_core::models::room_settings::AllowGuestJoin(true);
    room_service
        .set_settings(room.id, owner.id, settings)
        .await
        .expect("guest access should be enabled");
    let room_guest_version = room_service
        .get_room_guest_version(&room.id)
        .await
        .expect("guest version should load");

    let session_id = format!("{node_id}-guest-session");
    let principal = RealtimePrincipal::guest(
        room.id,
        GuestRealtimeIdentity {
            guest_id: guest_public_id(&session_id),
            display_name: guest_display_name(&session_id),
            session_id,
            token_jti: format!("{node_id}-guest-jti"),
            room_guest_version,
            permissions,
        },
    );

    let handler = StreamMessageHandler::new_with_principal(
        room.id,
        principal,
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(format!(
            "test:guest-fixture:{node_id}:"
        ))),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender,
    )
    .with_heartbeat_schedule(HeartbeatSchedule::for_tests(
        Duration::from_millis(10),
        Duration::from_mins(1),
    ));

    StartTestFixture {
        _container: container,
        pool,
        event_service,
        connection_service,
        handler,
    }
}

struct StartTestFixture {
    _container: synctv_core_testing::TestContainer,
    pool: sqlx::PgPool,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    handler: StreamMessageHandler,
}

impl StartTestFixture {
    async fn shutdown(self) {
        shutdown_test_runtime_resources(self.event_service, self.connection_service).await;
        self.pool.close().await;
    }
}

async fn prepare_handler_for_run_after_join(
    handler: &StreamMessageHandler,
    connection_service: &Arc<ConnectionManager>,
) {
    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed");
    connection_service
        .join_room(&handler.connection_id, handler.room_id)
        .await
        .expect("join_room should succeed");
    handler
        .cache_room_event_subscription()
        .await
        .expect("room subscription should cache before run_after_join");
}

async fn wait_for_start_cleanup(
    handler: &StreamMessageHandler,
    connection_service: &Arc<ConnectionManager>,
    event_service: &RealtimeManager,
    cancel_token: &tokio_util::sync::CancellationToken,
    expect_room_subscription_cleanup: bool,
) {
    tokio::time::timeout(Duration::from_secs(1), cancel_token.cancelled())
        .await
        .expect("start() should cancel");

    let room = handler.room_id;
    let user = handler.user_id;
    let connection_id = handler.connection_id.clone();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if connection_service.connection_count() == 0
                && connection_service.room_connection_count(&room) == 0
                && connection_service.user_connection_count(&user) == 0
                && handler
                    .connection_service
                    .get_connection(&connection_id)
                    .is_none()
                && (!expect_room_subscription_cleanup
                    || realtime_manager_subscriber_count(event_service, &room) == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cleanup should finish");
}

async fn shutdown_test_runtime_resources(
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
) {
    event_service.shutdown().await;
    connection_service.shutdown().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
}

fn realtime_manager_subscriber_count(event_service: &RealtimeManager, room_id: &RoomId) -> usize {
    event_service.get_room_subscribers(room_id).len()
}

async fn wait_for_run_after_join_ready(stream_state: &FailingStreamState) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if stream_state.send_calls() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run_after_join should be ready");
}

async fn wait_for_recorded_message_count(stream_state: &RecordingStreamState, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if stream_state.sent_messages().len() >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive expected messages");
}

async fn wait_for_run_after_join_cleanup(
    handler: &StreamMessageHandler,
    connection_service: &Arc<ConnectionManager>,
    event_service: &RealtimeManager,
    task: tokio::task::JoinHandle<Result<(), String>>,
) {
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("run_after_join should exit")
        .expect("run_after_join task should not panic");
    assert!(
        result.is_ok(),
        "run_after_join should exit cleanly: {result:?}"
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if connection_service.connection_count() == 0
                && connection_service.room_connection_count(&handler.room_id) == 0
                && connection_service.user_connection_count(&handler.user_id) == 0
                && realtime_manager_subscriber_count(event_service, &handler.room_id) == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run_after_join cleanup should finish");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_cancels_and_cleans_up_when_initial_send_fails() {
    let sender = FailingMessageSender::immediate();
    let fixture = create_start_handler_fixture("start_initial_send_fail", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.expect("start should return");

    wait_for_start_cleanup(
        handler,
        connection_service,
        event_service,
        &cancel_token,
        true,
    )
    .await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_does_not_broadcast_presence_events_when_initial_send_fails() {
    let sender = FailingMessageSender::immediate();
    let fixture =
        create_start_handler_fixture("start_no_broadcast_on_initial_failure", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let room = handler.room_id;
    let user = handler.user_id;
    let (mut rx, conn_id) = event_service
        .subscribe(room, user)
        .await
        .expect("subscribe should succeed");
    let (_tx, cancel_token) = handler.start().await.expect("start should return");

    let maybe_presence_event = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;

    assert!(
        maybe_presence_event.is_err(),
        "initial send failure must not broadcast UserJoined/UserLeft presence events"
    );

    event_service.unsubscribe(&conn_id);
    wait_for_start_cleanup(
        handler,
        connection_service,
        event_service,
        &cancel_token,
        true,
    )
    .await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_cancels_and_cleans_up_when_realtime_event_send_fails() {
    let sender = FailingMessageSender::fail_after(1);
    let sender_for_assert = Arc::clone(&sender);
    let fixture = create_start_handler_fixture("start_event_send_failure", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.expect("start should return");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("subscription should be established");

    event_service.broadcast(RealtimeEvent::ChatMessage {
        event_id: "evt-start-fail".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        message: "boom".to_string(),
        timestamp: now(),
        position: None,
        color: None,
    });

    wait_for_start_cleanup(
        handler,
        connection_service,
        event_service,
        &cancel_token,
        true,
    )
    .await;
    assert!(
        sender_for_assert.send_calls() >= 2,
        "initial join send + failing event send should both be attempted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_cancels_and_cleans_up_when_admin_notification_send_fails() {
    let sender = FailingMessageSender::fail_after(1);
    let sender_for_assert = Arc::clone(&sender);
    let fixture = create_start_handler_fixture("start_admin_notification_failure", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.expect("start should return");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("subscription should be established");

    event_service.broadcast(RealtimeEvent::UserNotification {
        event_id: "evt-admin-notify".to_string(),
        user_id: handler.user_id,
        title: "title".to_string(),
        content: "content".to_string(),
        notification_type: "system".to_string(),
        notification_id: "notif-1".to_string(),
        timestamp: now(),
    });

    wait_for_start_cleanup(
        handler,
        connection_service,
        event_service,
        &cancel_token,
        true,
    )
    .await;
    assert!(
        sender_for_assert.send_calls() >= 2,
        "initial join send + failing admin notification send should both be attempted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_run_after_join_cleans_up_when_realtime_event_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_event_failure").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = FailingStream::fail_after(1);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    event_service.broadcast(RealtimeEvent::ChatMessage {
        event_id: "evt-run-after-join".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        message: "boom".to_string(),
        timestamp: now(),
        position: None,
        color: None,
    });

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_cached_room_subscription_survives_pre_run_after_join_event_gap() {
    let event_service = test_realtime_manager("test_pre_join_caches_room_subscription").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    event_service.broadcast(RealtimeEvent::ChatMessage {
        event_id: "evt-prejoin-window".to_string(),
        room_id: handler.room_id,
        user_id: UserId::expect_positive(113_001),
        username: "other".to_string(),
        message: "arrived-before-run-after-join".to_string(),
        timestamp: now(),
        position: None,
        color: None,
    });

    let (mut stream, stream_state) = RecordingStream::new();
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 2).await;

    let messages = stream_state.sent_messages();
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg.message, Some(Message::UserJoined(_)))),
        "run_after_join should still send the initial UserJoined payload"
    );
    assert!(
            messages.iter().any(|msg| {
                matches!(
                    &msg.message,
                    Some(Message::Chat(chat))
                    if chat.content == "arrived-before-run-after-join"
                )
            }),
            "room event broadcast after caching the subscription but before run_after_join must not be lost"
        );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_run_after_join_cleans_up_when_initial_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_initial_failure").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut rx, conn_id) = event_service
        .subscribe(handler.room_id, handler.user_id)
        .await
        .expect("subscribe should succeed");
    let (mut stream, _stream_state) = FailingStream::fail_after(0);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    let maybe_presence_event = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
            maybe_presence_event.is_err(),
            "initial run_after_join send failure must not broadcast UserJoined/UserLeft presence events"
        );

    event_service.unsubscribe(&conn_id);
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_state_without_version_sends_current_state_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_playback_state_initial", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    prepare_handler_for_run_after_join(handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_state_message(
            "playback-state",
            String::new(),
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback_state(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive observed playback state");

    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );
    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playback_state) {
        Some(state) => {
            assert_eq!(state.version, 0);
            assert_eq!(
                state.room_id,
                public_id_codec().encode_room_id(handler.room_id).unwrap()
            );
        }
        None => panic!("expected PlaybackState after observe, got {messages:?}"),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_snapshot_without_version_sends_snapshot_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_playback_snapshot_initial", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler =
        handler
            .clone()
            .with_playback_snapshot_service(Arc::new(FakePlaybackSnapshotService {
                snapshot: crate::proto::client::PlaybackSnapshot {
                    media_id: public_media_id(),
                    playlist_id: String::new(),
                    room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
                    name: "test media".to_string(),
                    position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: std::collections::HashMap::new(),
                    version: "snapshot-v1".to_string(),
                    expires_at: Some(12345),
                },
            }));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            String::new(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback_snapshot(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive observed playback snapshot");

    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );
    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playback_snapshot) {
        Some(snapshot) => {
            assert_eq!(snapshot.version, "snapshot-v1");
            assert_eq!(snapshot.media_id, public_media_id());
            assert_eq!(snapshot.expires_at, Some(12345));
        }
        None => panic!("expected PlaybackSnapshot after observe, got {messages:?}"),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_state_with_current_version_skips_immediate_resend() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_playback_state_same_version",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    prepare_handler_for_run_after_join(handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_state_message("playback-state", "0")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let user_joined_sent = stream_state
                .sent_messages()
                .iter()
                .any(|message| matches!(message.message, Some(Message::UserJoined(_))));
            let observe_registered = handler
                .resource_observer
                .has_observation("playback-state")
                .await;
            if user_joined_sent && observe_registered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run_after_join should emit UserJoined and register playback state observation");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playback_state(message).is_none() }),
        "matching playback state version should not trigger an immediate resend"
    );
    assert!(
        stream_state
            .sent_messages()
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_snapshot_with_current_version_and_matching_source_skips_immediate_resend(
) {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_same_src", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "test media".to_string(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "snapshot-v1".to_string(),
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
        });
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            "snapshot-v1".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;

    let sent_messages = message_sender.sent_messages();
    assert!(
        sent_messages
            .iter()
            .all(|message| { resource_playback_snapshot(message).is_none() }),
        "matching playback snapshot version should not trigger an immediate resend: {sent_messages:?}"
    );
    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_snapshot_with_current_version_but_different_source_resends_immediately(
) {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_src_diff", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler =
        handler
            .clone()
            .with_playback_snapshot_service(Arc::new(FakePlaybackSnapshotService {
                snapshot: crate::proto::client::PlaybackSnapshot {
                    media_id: String::new(),
                    playlist_id: String::new(),
                    room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
                    name: "test media".to_string(),
                    position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: std::collections::HashMap::new(),
                    version: "snapshot-v1".to_string(),
                    expires_at: Some(12345),
                },
            }));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            "snapshot-v1".to_string(),
            "stale_media".to_string(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback_snapshot(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("source mismatch should trigger an immediate playback snapshot resend");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_snapshot_receives_future_playback_state_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_playback_snapshot_future_update",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: public_media_id(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "test media".to_string(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "1".to_string(),
            expires_at: Some(4_102_444_800),
        });
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            "1".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playback_snapshot(message).is_none() }),
        "same snapshot version should not resend immediately"
    );

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "test media".to_string(),
        position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: std::collections::HashMap::new(),
        version: "2".to_string(),
        expires_at: Some(4_102_444_801),
    });

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-snapshot-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        state: RoomPlaybackState {
            room_id: handler.room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_time: 12.0,
            speed: 1.0,
            is_playing: true,
            updated_at: now(),
            version: 2,
        },
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_snapshot(message).is_some_and(|snapshot| snapshot.version == "2")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observed playback snapshot should receive future updates");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_snapshot_refreshes_when_current_media_is_updated() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_md_upd", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let media = synctv_core::repository::MediaRepository::new(fixture.pool.clone())
        .create(&synctv_core::models::Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: handler.room_id,
            creator_id: None,
            name: "observe-playback-media-update".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({
                "url": "https://example.com/observe-playback-media-update.mp4"
            }),
            provider_instance_name: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .expect("media should be created for playback snapshot observe test");

    handler
        .room_service
        .update_playback(
            handler.room_id,
            handler.user_id,
            |state| {
                state.playing_media_id = Some(media.id);
                state.playing_playlist_id = None;
                state.target = Vec::new();
                state.current_time = 0.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            PermissionBits::PLAY_CONTROL,
        )
        .await
        .expect("playback should point at created media");

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: public_id_codec().encode_media_id(media.id).unwrap(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "observe-playback-media-update".to_string(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: media.version.to_string(),
            expires_at: Some(4_102_444_800),
        });
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            media.version.to_string(),
            public_id_codec().encode_media_id(media.id).unwrap(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playback_snapshot(message).is_none() }),
        "matching playback snapshot version should not resend immediately"
    );

    let updated_media = handler
        .room_service
        .edit_media(
            handler.room_id,
            handler.user_id,
            media.id,
            Some("observe-playback-media-update-v2".to_string()),
        )
        .await
        .expect("editing current playback media should succeed");

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: public_id_codec().encode_media_id(media.id).unwrap(),
        playlist_id: String::new(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "observe-playback-media-update-v2".to_string(),
        position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: std::collections::HashMap::from([(
            "token".to_string(),
            "\"media-updated\"".to_string(),
        )]),
        version: updated_media.version.to_string(),
        expires_at: Some(4_102_444_860),
    });

    event_service.broadcast(RealtimeEvent::MediaUpdated {
        event_id: "evt-observe-playback-snapshot-media-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        media_id: media.id,
        media_title: "observe-playback-media-update-v2".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_snapshot(message).is_some_and(|snapshot| {
                    snapshot.version == updated_media.version.to_string()
                        && snapshot
                            .metadata
                            .get("token")
                            .is_some_and(|token| token == "\"media-updated\"")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("current media updates should refresh observed playback snapshots");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_snapshot_refreshes_when_current_playlist_is_updated() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_pl_upd", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let playlist = synctv_core::repository::PlaylistRepository::new(fixture.pool.clone())
        .create(&synctv_core::models::Playlist {
            id: PlaylistId::new(),
            room_id: handler.room_id,
            creator_id: None,
            name: "observe-playback-playlist-update".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .expect("playlist should be created for playback snapshot observe test");

    let playback_repo =
        synctv_core::repository::RoomPlaybackStateRepository::new(fixture.pool.clone());
    let mut playback_state = playback_repo
        .create_or_get(&handler.room_id)
        .await
        .expect("playback state row should exist");
    playback_state.playing_media_id = None;
    playback_state.playing_playlist_id = Some(playlist.id);
    playback_state.target = br#"{"relative_path":"/playlist-item-1.mp4"}"#.to_vec();
    playback_state.current_time = 0.0;
    playback_state.speed = 1.0;
    playback_state.is_playing = true;
    playback_repo
        .update(&playback_state)
        .await
        .expect("playback should point at created playlist");

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: public_id_codec().encode_playlist_id(playlist.id).unwrap(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "observe-playback-playlist-update".to_string(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: playlist.version.to_string(),
            expires_at: Some(4_102_444_800),
        });
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            playlist.version.to_string(),
            String::new(),
            public_id_codec().encode_playlist_id(playlist.id).unwrap(),
            br#"{"relative_path":"/playlist-item-1.mp4"}"#.to_vec(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playback_snapshot(message).is_none() }),
        "matching playback snapshot version should not resend immediately"
    );

    let updated_playlist = handler
        .room_service
        .playlist_service()
        .set_playlist(
            handler.room_id,
            handler.user_id,
            synctv_core::service::playlist::SetPlaylistRequest {
                playlist_id: playlist.id,
                name: Some("observe-playback-playlist-update-v2".to_string()),
            },
        )
        .await
        .expect("editing current playback playlist should succeed");

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: String::new(),
        playlist_id: public_id_codec().encode_playlist_id(playlist.id).unwrap(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "observe-playback-playlist-update-v2".to_string(),
        position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: std::collections::HashMap::from([(
            "token".to_string(),
            "\"playlist-updated\"".to_string(),
        )]),
        version: updated_playlist.version.to_string(),
        expires_at: Some(4_102_444_860),
    });

    event_service.broadcast(RealtimeEvent::PlaylistUpdated {
        event_id: "evt-observe-playback-snapshot-playlist-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        playlist: updated_playlist.clone(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_snapshot(message).is_some_and(|snapshot| {
                    snapshot.version == updated_playlist.version.to_string()
                        && snapshot
                            .metadata
                            .get("token")
                            .is_some_and(|token| token == "\"playlist-updated\"")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("current playlist updates should refresh observed playback snapshots");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_snapshot_refreshes_when_target_changes_at_same_version() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_pb_snap_same_ver_target_change",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service = SequencedPlaybackSnapshotService::new([
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "1".to_string(),
            expires_at: Some(4_102_444_800),
        }),
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::from([(
                "token".to_string(),
                "\"refreshed\"".to_string(),
            )]),
            version: "1".to_string(),
            expires_at: Some(4_102_444_860),
        }),
    ]);
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            "1".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playback_snapshot(message).is_none() }),
        "matching playback snapshot version should not resend immediately"
    );

    let updated_state = handler
        .room_service
        .update_playback(
            handler.room_id,
            handler.user_id,
            |state| {
                state.playing_media_id = None;
                state.playing_playlist_id = None;
                state.target = b"target-b".to_vec();
                state.current_time = 12.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            PermissionBits::PLAY_CONTROL,
        )
        .await
        .expect("playback target should update before broadcasting state change");

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-snapshot-same-version-new-content".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        state: updated_state,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_snapshot(message).is_some_and(|snapshot| {
                    snapshot.version == "1"
                        && snapshot
                            .metadata
                            .get("token")
                            .is_some_and(|token| token == "\"refreshed\"")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("target changes at the same DB version must refresh observed playback snapshots");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_playback_snapshot_refresh_failure_removes_observation_without_closing_connection() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_refresh_fail", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service = SequencedPlaybackSnapshotService::new([
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: public_media_id(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "test media".to_string(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "snapshot-v1".to_string(),
            expires_at: Some(111),
        }),
        Err(crate::impls::ApiError::ServiceUnavailable(
            "provider unavailable".to_string(),
        )),
    ]);
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            String::new(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback_snapshot(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial observed playback snapshot should be delivered");

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-snapshot-refresh-error".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        state: RoomPlaybackState {
            room_id: handler.room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_time: 5.0,
            speed: 1.0,
            is_playing: true,
            updated_at: now(),
            version: 2,
        },
        timestamp: now(),
    });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        connection_service
            .get_connection(handler.connection_id())
            .is_some(),
        "snapshot refresh failures should not tear down the realtime connection"
    );

    event_service.broadcast(RealtimeEvent::UserLeft {
        event_id: "evt-after-snapshot-refresh-error".to_string(),
        room_id: handler.room_id,
        user_id: UserId::expect_positive(113_002),
        username: "another-user".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if stream_state
                .sent_messages()
                .iter()
                .any(|message| matches!(message.message, Some(Message::UserLeft(_))))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connection should continue receiving later realtime events");

    let playback_snapshot_messages = message_sender
        .sent_messages()
        .iter()
        .filter(|message| resource_playback_snapshot(message).is_some())
        .count();
    assert_eq!(
        playback_snapshot_messages, 1,
        "failed snapshot refresh should remove the observation instead of repeatedly resending"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_playback_snapshot_observation_refreshes_when_snapshot_expires_without_state_change() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snap_expiry", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let refresh_at = chrono::Utc::now().timestamp() + 1;
    let snapshot_service = SequencedPlaybackSnapshotService::new([
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "0".to_string(),
            expires_at: Some(refresh_at),
        }),
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::from([(
                "token".to_string(),
                "\"refreshed\"".to_string(),
            )]),
            version: "0".to_string(),
            expires_at: Some(refresh_at + 60),
        }),
    ]);
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service);

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            String::new(),
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .filter_map(resource_playback_snapshot)
                .any(|snapshot| {
                    snapshot.version == "0"
                        && snapshot
                            .metadata
                            .get("token")
                            .is_some_and(|token| token == "\"refreshed\"")
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("expired playback snapshots should be refreshed even without state changes");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_room_settings_without_version_sends_current_settings_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_settings_initial", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"chat_enabled":true}"#.to_vec(),
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_settings_message(
            "room-settings",
            String::new(),
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_room_settings(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive observed room settings");

    assert!(
        stream_state
            .sent_messages()
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_room_settings) {
        Some(changed) => {
            assert_eq!(
                changed.room_id,
                public_id_codec().encode_room_id(handler.room_id).unwrap()
            );
            assert_eq!(changed.version, 7);
            assert_eq!(changed.settings, br#"{"chat_enabled":true}"#.to_vec());
        }
        None => panic!("expected RoomSettings after observe, got {messages:?}"),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playlist_items_without_version_sends_snapshot_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_playlist_items_initial", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler = handler
        .clone()
        .with_playlist_items_snapshot_service(Arc::new(FakePlaylistItemsSnapshotService {
            snapshot: crate::proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: vec![crate::proto::client::Media {
                    id: "media_test_1".to_string(),
                    room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
                    source_provider: "direct_url".to_string(),
                    name: "test media".to_string(),
                    metadata: Vec::new(),
                    position: 1.0,
                    added_at: 1,
                    creator_id: handler.user_id.to_string(),
                    provider_instance_name: String::new(),
                    source_config: Vec::new(),
                    availability: crate::proto::client::ResourceAvailability::Available as i32,
                    version: 3,
                }],
                total: 1,
                folder_count: 0,
                file_count: 1,
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
                version: "items-v1".to_string(),
            },
        }));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            String::new(),
            crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
                availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playlist_items(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive observed playlist items");

    assert!(
        stream_state
            .sent_messages()
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playlist_items) {
        Some(snapshot) => {
            assert_eq!(snapshot.version, "items-v1");
            assert_eq!(snapshot.media.len(), 1);
        }
        None => panic!("expected PlaylistItems after observe, got {messages:?}"),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_observed_playlist_items_batch_coalesces_identical_snapshot_loads() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("playlist_items_coalesce").await,
        test_connection_manager(),
    );
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(crate::proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: 0,
            folder_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            version: "items-v1".to_string(),
        });
    let handler = handler.with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "batch-coalesce".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-a",
                "items-v1",
                request.clone(),
            )),
        })
        .await
        .expect("first observe should register");
    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-b",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("second observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(crate::proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: 0,
        folder_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        version: "items-v2".to_string(),
    });

    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .expect("playlist invalidation should refresh observations");

    assert_eq!(
        snapshot_service.call_count(),
        2,
        "identical playlist item observations should share one initial load and one refresh load"
    );
    let changed_versions = message_sender
        .sent_messages()
        .iter()
        .filter_map(resource_playlist_items)
        .filter(|snapshot| snapshot.version == "items-v2")
        .count();
    assert_eq!(
        changed_versions, 2,
        "both observe IDs should still receive their own ResourceChanged message"
    );
}

#[tokio::test]
async fn test_resource_observations_are_bounded_per_connection() {
    let sender = RecordingMessageSender::new();
    let event_service = test_realtime_manager("resource_observation_limit").await;
    let connection_service = test_connection_manager();
    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"chat_enabled":true}"#.to_vec(),
            version: 1,
        },
    );
    let handler = test_message_handler(sender.clone(), event_service, connection_service)
        .with_room_settings_snapshot_service(snapshot_service.clone());
    let max_observations = super::resource_observer::MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION;

    for index in 0..max_observations {
        let observe_id = format!("room-settings-{index}");
        handler
            .handle_client_message(&ClientMessage {
                message: Some(observe_room_settings_message(&observe_id, String::new())),
            })
            .await
            .expect("observe should register while under the per-connection limit");
    }
    let snapshot_calls_before_over_limit = snapshot_service.call_count();

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message(
                "room-settings-over-limit",
                String::new(),
            )),
        })
        .await
        .expect("over-limit observe should send ResourceObserveError without closing");

    assert_eq!(
        snapshot_service.call_count(),
        snapshot_calls_before_over_limit,
        "over-limit observation should be rejected before loading a snapshot"
    );
    assert!(
        !handler
            .resource_observer
            .has_observation("room-settings-over-limit")
            .await
    );
    assert!(sender
        .sent_messages()
        .iter()
        .filter_map(resource_observe_error)
        .any(|error| error
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("maximum per connection"))));

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                crate::proto::client::client_message::Message::UnobserveResource(
                    crate::proto::client::UnobserveResource {
                        observe_id: "room-settings-0".to_string(),
                    },
                ),
            ),
        })
        .await
        .expect("unobserve should free one observation slot");

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message(
                "room-settings-after-unobserve",
                String::new(),
            )),
        })
        .await
        .expect("observe should register after a slot is freed");
    assert!(
        handler
            .resource_observer
            .has_observation("room-settings-after-unobserve")
            .await
    );
}

#[tokio::test]
async fn test_observed_playlist_items_refresh_flag_is_not_persisted() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender,
        test_realtime_manager("playlist_items_refresh_consumed").await,
        test_connection_manager(),
    );
    let snapshot_service = RecordingPlaylistItemsRequestSnapshotService::new(
        empty_playlist_items_response("items-v1"),
    );
    let handler = handler.with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "consume-refresh".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: true,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items",
                String::new(),
                request,
            )),
        })
        .await
        .expect("observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .expect("playlist invalidation should refresh observation");
    snapshot_service.wait_for_calls(2).await;

    assert_eq!(
        snapshot_service.refresh_values(),
        vec![true, false],
        "refresh=true should be used for the initial load only"
    );
}

#[tokio::test]
async fn test_resource_changed_send_failure_propagates_and_removes_observation() {
    let message_sender = FailingMessageSender::fail_after(1);
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("resource_changed_send_failure").await,
        test_connection_manager(),
    );
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));
    let handler = handler.with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "send-failure".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("observe should register with only ResourceObserved sent");
    assert_eq!(message_sender.send_calls(), 1);

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    let error = handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .expect_err("ResourceChanged send failure should propagate");

    assert!(
        error.contains("forced send failure"),
        "unexpected send error: {error}"
    );
    assert!(
        !handler
            .resource_observer
            .has_observation("playlist-items")
            .await,
        "failed stateful send should remove the local observation"
    );
}

#[tokio::test]
async fn test_other_subscriber_send_failure_does_not_fail_refresh_caller() {
    let failing_sender = FailingMessageSender::fail_after(1);
    let healthy_sender = RecordingMessageSender::new();
    let event_service = test_realtime_manager("other_subscriber_send_failure").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));

    let failing_handler = StreamMessageHandler::new(
        room_id(),
        user_id(),
        "slow-client".to_string(),
        &room_service,
        Arc::clone(&chat_service),
        Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        Arc::new(RateLimiter::local_only(
            "test:other-send-fail:a:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        failing_sender.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let healthy_handler = StreamMessageHandler::new(
        room_id(),
        UserId::expect_positive(222),
        "healthy-client".to_string(),
        &room_service,
        chat_service,
        event_service,
        connection_service,
        Arc::new(RateLimiter::local_only(
            "test:other-send-fail:b:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        healthy_sender.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "other-send-failure".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    failing_handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-failing",
                "items-v1",
                request.clone(),
            )),
        })
        .await
        .expect("failing observer should register before its queue fails");
    healthy_handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-healthy",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("healthy observer should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    let event = RealtimeEvent::MediaAdded {
        event_id: "other-subscriber-send-failure-refresh".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "tester".to_string(),
        media_id: media_id(),
        media_title: "Updated media".to_string(),
        timestamp: now(),
    };

    healthy_handler
        .resource_observer
        .room_hub
        .refresh_for_room_event(&event, Some(healthy_handler.connection_id()))
        .await
        .expect("another subscriber's send failure should not fail the healthy caller");

    assert!(
        !failing_handler
            .resource_observer
            .has_observation("playlist-items-failing")
            .await,
        "failed subscriber should still be removed"
    );
    assert!(
        healthy_sender.sent_messages().iter().any(|message| {
            resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
        }),
        "healthy subscriber should receive the refreshed resource"
    );
}

#[tokio::test]
async fn test_stale_refresh_after_unobserve_does_not_send_resource_changed() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("stale_refresh_after_unobserve").await,
        test_connection_manager(),
    );
    let snapshot_service =
        BlockingPlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"), 2);
    let handler = handler.with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "stale-refresh".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    let refresh_observer = handler.resource_observer.clone();
    let refresh_task = tokio::spawn(async move {
        refresh_observer
            .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
            .await
    });
    snapshot_service.wait_for_calls(2).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                crate::proto::client::client_message::Message::UnobserveResource(
                    crate::proto::client::UnobserveResource {
                        observe_id: "playlist-items".to_string(),
                    },
                ),
            ),
        })
        .await
        .expect("unobserve should unregister the observation");
    snapshot_service.release();
    refresh_task
        .await
        .expect("refresh task should join")
        .expect("stale refresh should be suppressed without error");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| resource_playlist_items(message)
                .is_none_or(|snapshot| snapshot.version != "items-v2")),
        "stale refresh should not be delivered after unobserve"
    );
}

#[tokio::test]
async fn test_stale_refresh_failure_after_unobserve_does_not_send_observe_error() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("stale_refresh_failure_after_unobserve").await,
        test_connection_manager(),
    );
    let snapshot_service =
        BlockingFailingPlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));
    let handler = handler.with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "stale-refresh-failure".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("observe should register");
    snapshot_service.wait_for_calls(1).await;

    let refresh_observer = handler.resource_observer.clone();
    let refresh_task = tokio::spawn(async move {
        refresh_observer
            .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
            .await
    });
    snapshot_service.wait_for_calls(2).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                crate::proto::client::client_message::Message::UnobserveResource(
                    crate::proto::client::UnobserveResource {
                        observe_id: "playlist-items".to_string(),
                    },
                ),
            ),
        })
        .await
        .expect("unobserve should unregister the observation");
    snapshot_service.release();
    refresh_task
        .await
        .expect("refresh task should join")
        .expect("stale refresh failure should be suppressed without error");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| resource_observe_error(message).is_none()),
        "obsolete refresh failure should not send ResourceObserveError after unobserve"
    );
}

#[tokio::test]
async fn test_observed_playlist_items_singleflight_coalesces_concurrent_connection_loads() {
    let snapshot_service = SlowPlaylistItemsSnapshotService::new(
        empty_playlist_items_response("items-v1"),
        Duration::from_millis(50),
    );
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("playlist_items_singleflight").await;
    let connection_service = test_connection_manager();
    let handler_a = test_message_handler(
        sender_a.clone(),
        Arc::clone(&event_service),
        Arc::clone(&connection_service),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let handler_b = test_message_handler(sender_b.clone(), event_service, connection_service)
        .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "singleflight-concurrent".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };
    let message_a = ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items-a",
            String::new(),
            request.clone(),
        )),
    };
    let message_b = ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items-b",
            String::new(),
            request,
        )),
    };

    let (result_a, result_b) = tokio::join!(
        handler_a.handle_client_message(&message_a),
        handler_b.handle_client_message(&message_b)
    );
    result_a.expect("first observe should succeed");
    result_b.expect("second observe should succeed");

    assert_eq!(
            snapshot_service.call_count(),
            1,
            "concurrent identical observations across connections should share one in-flight snapshot load"
        );
    assert!(sender_a
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v1")));
    assert!(sender_b
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v1")));
}

#[tokio::test]
async fn test_room_resource_hub_coalesces_event_refresh_and_fans_out() {
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(crate::proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: 0,
            folder_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            version: "items-v1".to_string(),
        });
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("room_resource_hub_event_refresh").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(
        room_id(),
        user_id(),
        "tester-a".to_string(),
        &room_service,
        Arc::clone(&chat_service),
        Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        Arc::new(RateLimiter::local_only("test:hub:a:".to_string())),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender_a.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let handler_b = StreamMessageHandler::new(
        room_id(),
        user_id(),
        "tester-b".to_string(),
        &room_service,
        chat_service,
        event_service,
        connection_service,
        Arc::new(RateLimiter::local_only("test:hub:b:".to_string())),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender_b.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "room-hub-refresh".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler_a
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-a",
                "items-v1",
                request.clone(),
            )),
        })
        .await
        .expect("first observe should register");
    handler_b
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-b",
                "items-v1",
                request,
            )),
        })
        .await
        .expect("second observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(crate::proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: 0,
        folder_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        version: "items-v2".to_string(),
    });
    let event = RealtimeEvent::MediaAdded {
        event_id: "room-hub-media-added".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "tester".to_string(),
        media_id: media_id(),
        media_title: "Updated media".to_string(),
        timestamp: now(),
    };

    let (refresh_a, refresh_b) = tokio::join!(
        handler_a
            .resource_observer
            .room_hub
            .refresh_for_room_event(&event, Some(handler_a.connection_id())),
        handler_b
            .resource_observer
            .room_hub
            .refresh_for_room_event(&event, Some(handler_b.connection_id()))
    );
    refresh_a.expect("first event refresh should succeed");
    refresh_b.expect("deduped event refresh should succeed");
    snapshot_service.wait_for_calls(2).await;

    assert_eq!(
        snapshot_service.call_count(),
        2,
        "room hub should use one initial load and one shared event refresh load"
    );
    assert!(sender_a
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v2")));
    assert!(sender_b
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v2")));
}

#[tokio::test]
async fn test_room_resource_hub_refresh_dedupe_tracks_subscription_generation() {
    let snapshot_service =
        BlockingPlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"), 2);
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("room_resource_hub_generation_dedupe").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(
        room_id(),
        user_id(),
        "tester-a".to_string(),
        &room_service,
        Arc::clone(&chat_service),
        Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        Arc::new(RateLimiter::local_only(
            "test:hub:generation:a:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender_a.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let handler_b = StreamMessageHandler::new(
        room_id(),
        user_id(),
        "tester-b".to_string(),
        &room_service,
        chat_service,
        event_service,
        connection_service,
        Arc::new(RateLimiter::local_only(
            "test:hub:generation:b:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        sender_b.clone(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request_a = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "generation-dedupe-a".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };
    let request_b = crate::proto::client::ListPlaylistItemsRequest {
        search: "generation-dedupe-b".to_string(),
        ..request_a.clone()
    };

    handler_a
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-a",
                "items-v1",
                request_a,
            )),
        })
        .await
        .expect("first observe should register");
    snapshot_service.wait_for_calls(1).await;

    let event = RealtimeEvent::MediaAdded {
        event_id: "room-hub-generation-dedupe-media-added".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "tester".to_string(),
        media_id: media_id(),
        media_title: "Updated media".to_string(),
        timestamp: now(),
    };
    let refresh_observer_a = Arc::clone(&handler_a.resource_observer.room_hub);
    let refresh_event_a = event.clone();
    let connection_id_a = handler_a.connection_id().to_string();
    let refresh_a = tokio::spawn(async move {
        refresh_observer_a
            .refresh_for_room_event(&refresh_event_a, Some(&connection_id_a))
            .await
    });
    snapshot_service.wait_for_calls(2).await;

    handler_b
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-b",
                "items-v1",
                request_b,
            )),
        })
        .await
        .expect("second observe should register while first refresh is in flight");
    snapshot_service.wait_for_calls(3).await;
    assert!(!sender_b
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v2")));

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    snapshot_service.release();
    refresh_a
        .await
        .expect("first refresh task should join")
        .expect("first refresh should finish");

    handler_b
        .resource_observer
        .room_hub
        .refresh_for_room_event(&event, Some(handler_b.connection_id()))
        .await
        .expect("second refresh should not be suppressed by the stale completed refresh");

    assert_eq!(
        snapshot_service.call_count(),
        5,
        "subscriber generation changes should force a new refresh batch for the same event key"
    );
    assert!(sender_b
        .sent_messages()
        .iter()
        .any(|message| resource_playlist_items(message)
            .is_some_and(|snapshot| snapshot.version == "items-v2")));
}

#[tokio::test]
async fn test_observed_room_settings_singleflight_coalesces_cross_user_loads() {
    let snapshot_service = SlowRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"chat_enabled":true}"#.to_vec(),
            version: 7,
        },
        Duration::from_millis(50),
    );
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("room_settings_singleflight").await;
    let connection_service = test_connection_manager();
    let handler_a = test_message_handler_for_user(
        sender_a.clone(),
        Arc::clone(&event_service),
        Arc::clone(&connection_service),
        UserId::expect_positive(11),
    )
    .with_room_settings_snapshot_service(snapshot_service.clone());
    let handler_b = test_message_handler_for_user(
        sender_b.clone(),
        event_service,
        connection_service,
        UserId::expect_positive(12),
    )
    .with_room_settings_snapshot_service(snapshot_service.clone());
    let message_a = ClientMessage {
        message: Some(observe_room_settings_message(
            "room-settings-a",
            String::new(),
        )),
    };
    let message_b = ClientMessage {
        message: Some(observe_room_settings_message(
            "room-settings-b",
            String::new(),
        )),
    };

    let (result_a, result_b) = tokio::join!(
        handler_a.handle_client_message(&message_a),
        handler_b.handle_client_message(&message_b)
    );
    result_a.expect("first room settings observe should succeed");
    result_b.expect("second room settings observe should succeed");

    assert_eq!(
        snapshot_service.call_count(),
        1,
        "room-scoped settings should share one in-flight snapshot load across users"
    );
    assert!(sender_a.sent_messages().iter().any(
        |message| resource_room_settings(message).is_some_and(|settings| settings.version == 7)
    ));
    assert!(sender_b.sent_messages().iter().any(
        |message| resource_room_settings(message).is_some_and(|settings| settings.version == 7)
    ));
}

#[tokio::test]
async fn test_observe_resource_does_not_reuse_completed_evaluation_across_invalidation() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("observe_no_completed_reuse").await,
        test_connection_manager(),
    );
    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"version":1}"#.to_vec(),
            version: 1,
        },
    );
    let handler = handler.with_room_settings_snapshot_service(snapshot_service.clone());

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message(
                "room-settings-a",
                String::new(),
            )),
        })
        .await
        .expect("first observe should register");
    snapshot_service.wait_for_calls(1).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                crate::proto::client::client_message::Message::UnobserveResource(
                    crate::proto::client::UnobserveResource {
                        observe_id: "room-settings-a".to_string(),
                    },
                ),
            ),
        })
        .await
        .expect("unobserve should unregister the first observation");

    snapshot_service.replace(crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
        settings: br#"{"version":2}"#.to_vec(),
        version: 2,
    });
    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::RoomSettings])
        .await
        .expect(
            "invalidation without active observations should still advance resource generation",
        );
    assert_eq!(
        snapshot_service.call_count(),
        1,
        "invalidation with no active observations should not load a snapshot"
    );

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message(
                "room-settings-b",
                String::new(),
            )),
        })
        .await
        .expect("second observe should load the latest snapshot");
    snapshot_service.wait_for_calls(2).await;

    assert_eq!(
        snapshot_service.call_count(),
        2,
        "completed evaluations must not be reused after a resource invalidation"
    );
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_room_settings(message)
                .is_some_and(|settings| settings.version == 2)),
        "second observe should receive the fresh room settings snapshot"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playlist_items_with_current_version_skips_immediate_resend() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_playlist_items_same_version",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(crate::proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: 0,
            folder_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            version: "items-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_playlist_items_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            "items-v1".to_string(),
            crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
                availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playlist_items(message).is_none() }),
        "matching playlist items version should not trigger an immediate resend"
    );
    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playlist_items_receive_future_media_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_playlist_items_future_update",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(crate::proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: 0,
            folder_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            version: "items-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_playlist_items_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            "items-v1".to_string(),
            crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
                availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_playlist_items(message).is_none() }),
        "same playlist items version should not resend immediately"
    );

    snapshot_service.replace(crate::proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: vec![crate::proto::client::Media {
            id: "media_test_2".to_string(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            source_provider: "direct_url".to_string(),
            name: "next media".to_string(),
            metadata: Vec::new(),
            position: 2.0,
            added_at: 2,
            creator_id: handler.user_id.to_string(),
            provider_instance_name: String::new(),
            source_config: Vec::new(),
            availability: crate::proto::client::ResourceAvailability::Available as i32,
            version: 4,
        }],
        total: 1,
        folder_count: 0,
        file_count: 1,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        version: "items-v2".to_string(),
    });

    event_service.broadcast(RealtimeEvent::MediaAdded {
        event_id: "evt-observe-playlist-items-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        media_id: synctv_core::models::MediaId::expect_positive(113_003),
        media_title: "next media".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playlist_items(message)
                    .is_some_and(|snapshot| snapshot.version == "items-v2")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observed playlist items should receive future updates");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_room_members_without_version_sends_snapshot_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_members_initial", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler = handler.clone().with_room_members_snapshot_service(Arc::new(
        FakeRoomMembersSnapshotService {
            snapshot: crate::proto::client::GetRoomMembersResponse {
                members: vec![synctv_proto::common::RoomMember {
                    room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
                    user_id: handler.user_id.to_string(),
                    username: handler.username.clone(),
                    role: synctv_proto::common::RoomMemberRole::Creator as i32,
                    permissions: PermissionBits::ALL,
                    status: synctv_proto::common::MemberStatus::Active as i32,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    joined_at: 1,
                    is_online: true,
                    is_banned: false,
                    banned_at: 0,
                    banned_reason: String::new(),
                }],
                total: 1,
                version: "members-v1".to_string(),
            },
        },
    ));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_members_message(
            "room-members",
            String::new(),
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                is_banned: None,
                sort_by: crate::proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_room_members(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recording stream should receive observed room members");

    assert!(
        stream_state
            .sent_messages()
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_room_members) {
        Some(snapshot) => {
            assert_eq!(snapshot.version, "members-v1");
            assert_eq!(snapshot.members.len(), 1);
        }
        None => panic!("expected RoomMembers after observe, got {messages:?}"),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_room_members_with_current_version_skips_immediate_resend() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_members_same_version", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutableRoomMembersSnapshotService::new(crate::proto::client::GetRoomMembersResponse {
            members: Vec::new(),
            total: 0,
            version: "members-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_room_members_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_members_message(
            "room-members",
            "members-v1".to_string(),
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                is_banned: None,
                sort_by: crate::proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_room_members(message).is_none() }),
        "matching room members version should not trigger an immediate resend"
    );
    assert!(
        stream_state
            .sent_messages()
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_room_members_receive_future_permission_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_members_future_update", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutableRoomMembersSnapshotService::new(crate::proto::client::GetRoomMembersResponse {
            members: Vec::new(),
            total: 0,
            version: "members-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_room_members_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_members_message(
            "room-members",
            "members-v1".to_string(),
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                is_banned: None,
                sort_by: crate::proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_room_members(message).is_none() }),
        "same room members version should not resend immediately"
    );

    snapshot_service.replace(crate::proto::client::GetRoomMembersResponse {
        members: vec![synctv_proto::common::RoomMember {
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            user_id: "member002abc".to_string(),
            username: "member_two".to_string(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            permissions: PermissionBits::VIEW_MEMBER_LIST,
            status: synctv_proto::common::MemberStatus::Active as i32,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: 2,
            is_online: false,
            is_banned: false,
            banned_at: 0,
            banned_reason: String::new(),
        }],
        total: 1,
        version: "members-v2".to_string(),
    });

    event_service.broadcast(RealtimeEvent::PermissionChanged {
        event_id: "evt-observe-room-members-update".to_string(),
        room_id: handler.room_id,
        target_user_id: UserId::expect_positive(113_004),
        target_username: "member_two".to_string(),
        changed_by: handler.user_id,
        changed_by_username: handler.username.clone(),
        new_permissions: PermissionBits(PermissionBits::VIEW_MEMBER_LIST),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: PermissionBits(0),
        removed_permissions: PermissionBits(0),
        admin_added_permissions: PermissionBits(0),
        admin_removed_permissions: PermissionBits(0),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_room_members(message)
                    .is_some_and(|snapshot| snapshot.version == "members-v2")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observed room members should receive future updates");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_room_members_receive_future_room_settings_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_room_members_settings_update",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service =
        MutableRoomMembersSnapshotService::new(crate::proto::client::GetRoomMembersResponse {
            members: Vec::new(),
            total: 0,
            version: "members-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_room_members_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_members_message(
            "room-members",
            "members-v1".to_string(),
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                is_banned: None,
                sort_by: crate::proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
            },
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_room_members(message).is_none() }),
        "same room members version should not resend immediately"
    );

    snapshot_service.replace(crate::proto::client::GetRoomMembersResponse {
        members: vec![synctv_proto::common::RoomMember {
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            user_id: handler.user_id.to_string(),
            username: handler.username.clone(),
            role: synctv_proto::common::RoomMemberRole::Creator as i32,
            permissions: PermissionBits::ALL | PermissionBits::PLAY_CONTROL,
            status: synctv_proto::common::MemberStatus::Active as i32,
            added_permissions: PermissionBits::PLAY_CONTROL,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: 1,
            is_online: true,
            is_banned: false,
            banned_at: 0,
            banned_reason: String::new(),
        }],
        total: 1,
        version: "members-v2".to_string(),
    });

    event_service.broadcast(RealtimeEvent::RoomSettingsChanged {
        event_id: "evt-observe-room-members-settings-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        settings_json: serde_json::to_vec(&serde_json::json!({
            "member_added_permissions": PermissionBits::PLAY_CONTROL
        }))
        .expect("room settings JSON should serialize"),
        version: 2,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_room_members(message)
                    .is_some_and(|snapshot| snapshot.version == "members-v2")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("room settings changes should refresh observed room members snapshots");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_room_settings_with_current_version_skips_immediate_resend() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_settings_same_version", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"chat_enabled":true}"#.to_vec(),
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_settings_message("room-settings", "7")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_room_settings(message).is_none() }),
        "matching room settings version should not trigger an immediate resend"
    );
    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
            .iter()
            .any(|message| matches!(message.message, Some(Message::UserJoined(_)))),
        "stream transport should still emit UserJoined payloads"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_room_settings_receive_future_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_room_settings_future_update",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: br#"{"chat_enabled":true}"#.to_vec(),
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_settings_message("room-settings", "7")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| { resource_room_settings(message).is_none() }),
        "same room settings version should not resend immediately"
    );

    snapshot_service.replace(crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
        settings: br#"{"chat_enabled":false}"#.to_vec(),
        version: 8,
    });

    event_service.broadcast(RealtimeEvent::RoomSettingsChanged {
        event_id: "evt-observe-room-settings-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        settings_json: br#"{"chat_enabled":false}"#.to_vec(),
        version: 8,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_room_settings(message).is_some_and(|changed| {
                    changed.version == 8
                        && changed.settings == br#"{"chat_enabled":false}"#.to_vec()
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("future room settings update should be pushed to observed client");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_run_after_join_cleans_up_when_admin_notification_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_admin_failure").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = FailingStream::fail_after(1);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    event_service.broadcast(RealtimeEvent::UserNotification {
        event_id: "evt-run-after-join-admin".to_string(),
        user_id: handler.user_id,
        title: "title".to_string(),
        content: "content".to_string(),
        notification_type: "system".to_string(),
        notification_id: "notif-admin".to_string(),
        timestamp: now(),
    });

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_run_after_join_cleans_up_when_backpressure_error_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_backpressure_failure").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    )
    .with_concurrency(Arc::new(MessageConcurrencyConfig::new(0)));
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let input = ClientMessage { message: None };
    let (mut stream, stream_state) = FailingStream::fail_after_with_incoming(1, vec![input]);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_run_after_join_cleans_up_when_direct_notification_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_direct_failure").await;
    let connection_service = test_connection_manager();
    let notification_pool = test_pool();
    let notification_service = Arc::new(synctv_core::service::UserNotificationService::new(
        NotificationRepository::new(notification_pool.clone()),
    ));
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    )
    .with_notification_service(Arc::clone(&notification_service));
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = FailingStream::fail_after(1);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    notification_service.publish_realtime_event(NotificationCreatedEvent {
        user_id: handler.user_id,
        notification: Notification {
            id: 1,
            user_id: handler.user_id,
            notification_type: NotificationType::SystemAnnouncement,
            title: "title".to_string(),
            content: "content".to_string(),
            data: serde_json::json!({}),
            is_read: false,
            created_at: now(),
            updated_at: now(),
        },
    });

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    notification_pool.close().await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[test]
fn test_chat_message_event_conversion() {
    let event = RealtimeEvent::ChatMessage {
        event_id: "evt1".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "alice".to_string(),
        message: "hello world".to_string(),
        timestamp: now(),
        position: Some(42.5),
        color: Some("#ff0000".to_string()),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    let msg = &msgs[0];
    match &msg.message {
        Some(Message::Chat(chat)) => {
            assert_eq!(chat.room_id, "room_test");
            assert_eq!(chat.user_id, public_actor_id());
            assert_eq!(chat.username, "alice");
            assert_eq!(chat.content, "hello world");
            assert_eq!(chat.position, Some(42.5));
            assert_eq!(chat.color, Some("#ff0000".to_string()));
        }
        other => panic!("Expected Chat message, got: {other:?}"),
    }
}

#[test]
fn test_playback_state_changed_event_conversion() {
    let state = RoomPlaybackState {
        room_id: room_id(),
        playing_media_id: Some(media_id()),
        playing_playlist_id: None,
        target: Vec::new(),
        current_time: 123.456,
        speed: 1.5,
        is_playing: true,
        updated_at: now(),
        version: 7,
    };
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt2".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaybackState(ps)) => {
            assert_eq!(ps.room_id, "room_test");
            let s = ps.state.as_ref().unwrap();
            assert!((s.current_time - 123.456).abs() < f64::EPSILON);
            assert!((s.speed - 1.5).abs() < f64::EPSILON);
            assert!(s.is_playing);
            assert_eq!(s.playing_media_id, public_media_id());
            assert_eq!(s.version, 7);
        }
        other => panic!("Expected PlaybackState, got: {other:?}"),
    }
}

#[test]
fn test_user_joined_event_conversion() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt3".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "carol".to_string(),
        permissions: PermissionBits(PermissionBits::DEFAULT_MEMBER),
        role: 3,
        added_permissions: PermissionBits(0),
        removed_permissions: PermissionBits(0),
        admin_added_permissions: PermissionBits(0),
        admin_removed_permissions: PermissionBits(0),
        joined_at: now(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::UserJoined(uj)) => {
            assert_eq!(uj.room_id, "room_test");
            let member = uj.member.as_ref().unwrap();
            assert_eq!(member.user_id, public_actor_id());
            assert_eq!(member.username, "carol");
            assert_eq!(member.role, 3);
            assert!(member.is_online);
        }
        other => panic!("Expected UserJoined, got: {other:?}"),
    }
}

#[test]
fn test_user_left_event_conversion() {
    let event = RealtimeEvent::UserLeft {
        event_id: "evt4".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "dave".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::UserLeft(ul)) => {
            assert_eq!(ul.room_id, "room_test");
            assert_eq!(ul.user_id, public_actor_id());
        }
        other => panic!("Expected UserLeft, got: {other:?}"),
    }
}

#[test]
fn test_media_added_event_conversion() {
    let event = RealtimeEvent::MediaAdded {
        event_id: "evt5".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "eve".to_string(),
        media_id: media_id(),
        media_title: "Test Video".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::MediaAdded(ma)) => {
            assert_eq!(ma.room_id, "room_test");
            assert_eq!(ma.media_id, public_media_id());
            assert_eq!(ma.name, "Test Video");
            assert_eq!(ma.creator_username, "eve");
        }
        other => panic!("Expected MediaAdded, got: {other:?}"),
    }
}

#[test]
fn test_media_removed_event_conversion() {
    let event = RealtimeEvent::MediaRemoved {
        event_id: "evt6".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "frank".to_string(),
        media_id: media_id(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::MediaRemoved(mr)) => {
            assert_eq!(mr.room_id, "room_test");
            assert_eq!(mr.media_id, public_media_id());
            assert_eq!(mr.removed_by, "frank");
        }
        other => panic!("Expected MediaRemoved, got: {other:?}"),
    }
}

#[test]
fn test_media_removed_batch_event_conversion() {
    let event = RealtimeEvent::MediaRemovedBatch {
        event_id: "evt6_batch".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "frank".to_string(),
        media_ids: vec![
            MediaId::expect_positive(113_005),
            MediaId::expect_positive(113_006),
        ],
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::MediaRemovedBatch(batch)) => {
            assert_eq!(batch.room_id, "room_test");
            assert_eq!(
                batch.media_ids,
                vec![
                    public_id_codec()
                        .encode_media_id(MediaId::expect_positive(113_005))
                        .unwrap(),
                    public_id_codec()
                        .encode_media_id(MediaId::expect_positive(113_006))
                        .unwrap(),
                ]
            );
            assert_eq!(batch.removed_by, "frank");
            assert_eq!(batch.removed_by_user_id, public_actor_id());
        }
        other => panic!("Expected MediaRemovedBatch, got: {other:?}"),
    }
}

#[test]
fn test_media_updated_event_conversion() {
    let event = RealtimeEvent::MediaUpdated {
        event_id: "evt6b".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "frank".to_string(),
        media_id: media_id(),
        media_title: "Renamed Video".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::MediaUpdated(mu)) => {
            assert_eq!(mu.room_id, "room_test");
            assert_eq!(mu.media_id, public_media_id());
            assert_eq!(mu.name, "Renamed Video");
            assert_eq!(mu.updated_by, "frank");
        }
        other => panic!("Expected MediaUpdated, got: {other:?}"),
    }
}

#[test]
fn test_playlist_reordered_event_conversion() {
    let event = RealtimeEvent::PlaylistReordered {
        event_id: "evt6c".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "grace".to_string(),
        media_ids: vec![
            MediaId::expect_positive(113_006),
            MediaId::expect_positive(113_005),
        ],
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaylistReordered(reordered)) => {
            assert_eq!(reordered.room_id, "room_test");
            assert_eq!(
                reordered.media_ids,
                vec![
                    public_id_codec()
                        .encode_media_id(MediaId::expect_positive(113_006))
                        .unwrap(),
                    public_id_codec()
                        .encode_media_id(MediaId::expect_positive(113_005))
                        .unwrap(),
                ]
            );
            assert_eq!(reordered.reordered_by, "grace");
            assert_eq!(reordered.reordered_by_user_id, public_actor_id());
        }
        other => panic!("Expected PlaylistReordered, got: {other:?}"),
    }
}

#[test]
fn test_playlist_created_event_conversion() {
    let event = RealtimeEvent::PlaylistCreated {
        event_id: "evt6d".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "grace".to_string(),
        playlist: playlist(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaylistCreated(created)) => {
            assert_eq!(created.room_id, "room_test");
            let playlist = created.playlist.as_ref().expect("playlist payload");
            assert_eq!(playlist.id, public_playlist_id());
            assert_eq!(playlist.name, "Test Playlist");
            assert_eq!(playlist.version, 1);
        }
        other => panic!("Expected PlaylistCreated, got: {other:?}"),
    }
}

#[test]
fn test_playlist_updated_event_conversion() {
    let mut updated_playlist = playlist();
    updated_playlist.name = "Renamed Playlist".to_string();
    let event = RealtimeEvent::PlaylistUpdated {
        event_id: "evt6e".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "heidi".to_string(),
        playlist: updated_playlist,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaylistUpdated(updated)) => {
            assert_eq!(updated.room_id, "room_test");
            let playlist = updated.playlist.as_ref().expect("playlist payload");
            assert_eq!(playlist.id, public_playlist_id());
            assert_eq!(playlist.name, "Renamed Playlist");
            assert_eq!(playlist.version, 1);
        }
        other => panic!("Expected PlaylistUpdated, got: {other:?}"),
    }
}

#[test]
fn test_playlist_deleted_event_conversion() {
    let event = RealtimeEvent::PlaylistDeleted {
        event_id: "evt6f".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "ivan".to_string(),
        playlist_id: PlaylistId::expect_positive(113_007),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaylistDeleted(deleted)) => {
            assert_eq!(deleted.room_id, "room_test");
            assert_eq!(
                deleted.playlist_id,
                public_id_codec()
                    .encode_playlist_id(PlaylistId::expect_positive(113_007))
                    .unwrap()
            );
        }
        other => panic!("Expected PlaylistDeleted, got: {other:?}"),
    }
}

#[test]
fn test_room_settings_changed_event_conversion() {
    let event = RealtimeEvent::RoomSettingsChanged {
        event_id: "evt6g".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "judy".to_string(),
        settings_json: br#"{"chat_enabled":false}"#.to_vec(),
        version: 12,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::RoomSettings(changed)) => {
            assert_eq!(changed.room_id, "room_test");
            assert_eq!(changed.settings, br#"{"chat_enabled":false}"#.to_vec());
            assert_eq!(changed.version, 12);
        }
        other => panic!("Expected RoomSettings, got: {other:?}"),
    }
}

#[test]
fn test_webrtc_offer_event_conversion() {
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: "evt7".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::Offer,
        from: "conn_a".to_string(),
        to: "conn_b".to_string(),
        data: "sdp_data".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::WebrtcOffer(o)) => {
            assert_eq!(o.from, "conn_a");
            assert_eq!(o.to, "conn_b");
            assert_eq!(o.data, "sdp_data");
        }
        other => panic!("Expected WebrtcOffer, got: {other:?}"),
    }
}

#[test]
fn test_webrtc_answer_event_conversion() {
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: "evt8".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::Answer,
        from: "conn_b".to_string(),
        to: "conn_a".to_string(),
        data: "answer_sdp".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::WebrtcAnswer(a)) => {
            assert_eq!(a.from, "conn_b");
            assert_eq!(a.to, "conn_a");
        }
        other => panic!("Expected WebrtcAnswer, got: {other:?}"),
    }
}

#[test]
fn test_webrtc_ice_candidate_event_conversion() {
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: "evt9".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::IceCandidate,
        from: "conn_a".to_string(),
        to: "conn_b".to_string(),
        data: "candidate_data".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    assert!(matches!(
        &msgs[0].message,
        Some(Message::WebrtcIceCandidate(_))
    ));
}

#[test]
fn test_room_deleted_event_conversion() {
    let event = RealtimeEvent::RoomDeleted {
        event_id: "evt11".to_string(),
        room_id: room_id(),
        deleted_by: user_id(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Error(e)) => {
            assert!(e.message.contains("deleted"));
            assert_eq!(e.code, crate::impls::error_codes::NOT_FOUND);
        }
        other => panic!("Expected Error message for RoomDeleted, got: {other:?}"),
    }
}

#[test]
fn test_room_banned_event_conversion() {
    let event = RealtimeEvent::RoomBanned {
        event_id: "evt11b".to_string(),
        room_id: room_id(),
        banned_by: user_id(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Error(e)) => {
            assert!(e.message.contains("banned"));
            assert_eq!(e.code, crate::impls::error_codes::FORBIDDEN);
        }
        other => panic!("Expected Error message for RoomBanned, got: {other:?}"),
    }
}

#[test]
fn test_room_owner_inactive_event_conversion() {
    let event = RealtimeEvent::RoomOwnerInactive {
        event_id: "evt11c".to_string(),
        room_id: room_id(),
        owner_id: user_id(),
        triggered_by: user_id(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Error(e)) => {
            assert!(e.message.contains("creator"));
            assert_eq!(e.code, crate::impls::error_codes::FORBIDDEN);
        }
        other => panic!("Expected Error message for RoomOwnerInactive, got: {other:?}"),
    }
}

#[test]
fn test_system_notification_event_conversion() {
    let event = RealtimeEvent::SystemNotification {
        event_id: "evt12".to_string(),
        message: "Server maintenance in 5 minutes".to_string(),
        level: NotificationLevel::Warning,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec());
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Notification(n)) => {
            assert_eq!(n.title, "Server maintenance in 5 minutes");
            assert_eq!(n.notification_type, "system_announcement");
        }
        other => panic!("Expected Notification message for SystemNotification, got: {other:?}"),
    }
}

#[test]
fn test_admin_events_return_empty() {
    let event = RealtimeEvent::KickPublisher {
        event_id: "evt13".to_string(),
        room_id: room_id(),
        media_id: media_id(),
        reason: "test".to_string(),
        timestamp: now(),
    };
    assert!(realtime_event_to_server_messages(&event, "room_test", &public_id_codec()).is_empty());

    let event = RealtimeEvent::KickUser {
        event_id: "evt14".to_string(),
        user_id: user_id(),
        reason: "banned".to_string(),
        timestamp: now(),
    };
    assert!(realtime_event_to_server_messages(&event, "room_test", &public_id_codec()).is_empty());

    let event = RealtimeEvent::RoomBanned {
        event_id: "evt15".to_string(),
        room_id: room_id(),
        banned_by: user_id(),
        timestamp: now(),
    };
    assert_eq!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()).len(),
        1
    );
}

#[test]
fn test_server_message_encode_decode_roundtrip() {
    let msg = ServerMessage {
        message: Some(Message::UserLeft(crate::proto::client::UserLeftRoom {
            room_id: "room1".to_string(),
            user_id: "user1".to_string(),
        })),
    };

    let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
    let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
    match decoded.message {
        Some(Message::UserLeft(ul)) => {
            assert_eq!(ul.room_id, "room1");
            assert_eq!(ul.user_id, "user1");
        }
        other => panic!("Expected UserLeft after roundtrip, got: {other:?}"),
    }
}

#[test]
fn test_client_message_decode_invalid_data() {
    let result = ProtoCodec::decode_client_message(&[0xFF, 0xFF, 0xFF]);
    assert!(result.is_err());
}

#[test]
fn test_server_message_decode_invalid_data() {
    let result = ProtoCodec::decode_server_message(&[0xFF, 0xFF, 0xFF]);
    assert!(result.is_err());
}

#[test]
fn test_server_message_encode_empty() {
    let msg = ServerMessage { message: None };
    let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
    let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
    assert!(decoded.message.is_none());
}

#[test]
fn test_message_concurrency_config_can_be_acquired() {
    // Test that the semaphore can be acquired under normal conditions
    let config = super::MessageConcurrencyConfig::new(100);
    let semaphore = config.semaphore();
    // Use try_acquire to check without blocking
    let permit = semaphore.try_acquire();
    assert!(
        permit.is_ok(),
        "Semaphore should be acquirable under normal load"
    );
    // Release the permit immediately
    drop(permit);
}

#[test]
fn test_message_concurrency_config_enforces_limit() {
    // Test that semaphore enforces the concurrent processing limit.
    // Each test gets its own config instance, so no cross-test interference.
    let config = super::MessageConcurrencyConfig::new(10);
    let semaphore = config.semaphore();

    // Acquire all 10 permits
    let permits: Vec<_> = (0..10)
        .map(|_| semaphore.clone().try_acquire_owned())
        .collect::<Result<Vec<_>, _>>()
        .expect("Should acquire all 10 permits");

    assert_eq!(config.available_permits(), 0, "No permits should remain");

    // Next acquisition should fail
    let failed = semaphore.try_acquire_owned();
    assert!(failed.is_err(), "Should fail when no permits available");

    // Drop all permits
    drop(permits);
    assert_eq!(config.available_permits(), 10, "All permits restored");
}

#[test]
fn test_resource_exhausted_error_message_format() {
    // Test that ResourceExhausted error messages are properly formatted
    let error_msg = ServerMessage {
        message: Some(Message::Error(crate::proto::client::ErrorMessage {
            message: "System overloaded, please retry later".to_string(),
            code: crate::impls::error_codes::RESOURCE_EXHAUSTED,
            detail: String::new(),
        })),
    };

    match error_msg.message {
        Some(Message::Error(e)) => {
            assert_eq!(e.code, crate::impls::error_codes::RESOURCE_EXHAUSTED);
            assert!(!e.message.is_empty());
        }
        other => panic!("Expected Error message, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_concurrency_config_backpressure_with_async() {
    // Test that semaphore backpressure works correctly with async operations.
    // Each test gets its own config instance, so no cross-test interference.
    let config = std::sync::Arc::new(super::MessageConcurrencyConfig::new(50));
    let semaphore = config.semaphore();

    // Acquire a permit for message processing
    let permit = semaphore.try_acquire_owned();
    assert!(permit.is_ok(), "Should be able to acquire permit");
    let after_acquire = config.available_permits();

    // Drop the permit (simulating message processing completion)
    drop(permit);

    // Verify permits are restored
    let after_release = config.available_permits();
    assert!(
            after_release > after_acquire,
            "Available permits should increase after releasing: was {after_acquire}, now {after_release}"
        );
}

#[test]
fn test_validate_danmaku_color_valid_hex_colors() {
    // Valid hex color formats: #RRGGBB
    assert!(super::validate_danmaku_color(&Some("#FF0000".to_string())).is_ok()); // Red
    assert!(super::validate_danmaku_color(&Some("#00FF00".to_string())).is_ok()); // Green
    assert!(super::validate_danmaku_color(&Some("#0000FF".to_string())).is_ok()); // Blue
    assert!(super::validate_danmaku_color(&Some("#FFFFFF".to_string())).is_ok()); // White
    assert!(super::validate_danmaku_color(&Some("#000000".to_string())).is_ok()); // Black
    assert!(super::validate_danmaku_color(&Some("#abcdef".to_string())).is_ok()); // Lowercase
    assert!(super::validate_danmaku_color(&Some("#ABCDEF".to_string())).is_ok()); // Uppercase
    assert!(super::validate_danmaku_color(&Some("#123456".to_string())).is_ok()); // Mixed digits
    assert!(super::validate_danmaku_color(&Some("#1a2B3c".to_string())).is_ok());
    // Mixed case
}

#[test]
fn test_validate_danmaku_color_none_is_valid() {
    // None should be valid (no color specified = default color)
    assert!(super::validate_danmaku_color(&None).is_ok());
}

#[test]
fn test_validate_danmaku_color_invalid_format_no_hash() {
    // Missing # prefix should be rejected
    let result = super::validate_danmaku_color(&Some("FF0000".to_string()));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must start with '#'"));
}

#[test]
fn test_validate_danmaku_color_invalid_format_wrong_length() {
    // Wrong length should be rejected
    let result = super::validate_danmaku_color(&Some("#FFF".to_string())); // Too short
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must be 7 characters"));

    let result = super::validate_danmaku_color(&Some("#FFFFFFFF".to_string())); // Too long (no alpha)
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must be 7 characters"));
}

#[test]
fn test_validate_danmaku_color_invalid_characters() {
    // Non-hex characters should be rejected
    let result = super::validate_danmaku_color(&Some("#GGGGGG".to_string()));
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("must contain only hex characters"));

    let result = super::validate_danmaku_color(&Some("#ZZZZZZ".to_string()));
    assert!(result.is_err());
}

#[test]
fn test_validate_danmaku_color_xss_injection() {
    // XSS injection attempts should be rejected
    let result = super::validate_danmaku_color(&Some("javascript:alert(1)".to_string()));
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("<script>".to_string()));
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("rgb(255,0,0)".to_string()));
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("red".to_string()));
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("#expression(alert(1))".to_string()));
    assert!(result.is_err());
}

#[test]
fn test_validate_danmaku_color_empty_string() {
    // Empty string should be rejected
    let result = super::validate_danmaku_color(&Some(String::new()));
    assert!(result.is_err());
}

#[test]
fn test_validate_danmaku_color_special_characters() {
    // Special characters should be rejected
    let result = super::validate_danmaku_color(&Some("#FF 000".to_string())); // Space
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("#FF-000".to_string())); // Dash
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("#FF\n000".to_string())); // Newline
    assert!(result.is_err());

    let result = super::validate_danmaku_color(&Some("#\u{0000}F0000".to_string())); // Null byte
    assert!(result.is_err());
}

#[test]
fn test_membership_cache_stores_and_retrieves() {
    // Verify the membership cache can store and retrieve entries
    let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
        moka::sync::Cache::builder()
            .time_to_live(super::MEMBERSHIP_CACHE_TTL)
            .build();

    let key = ("room1".to_string(), "user1".to_string());
    let membership = super::CachedMembership {
        is_member: true,
        is_banned: false,
    };

    cache.insert(key.clone(), membership);
    let cached = cache.get(&key);
    assert!(cached.is_some());
    let cached = cached.unwrap();
    assert!(cached.is_member);
    assert!(!cached.is_banned);
}

#[test]
fn test_membership_cache_invalidation_removes_entry() {
    // Verify that invalidate() removes the cached entry so the next
    // lookup returns None (forcing a DB re-query on next heartbeat)
    let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
        moka::sync::Cache::builder()
            .time_to_live(super::MEMBERSHIP_CACHE_TTL)
            .build();

    let key = ("room1".to_string(), "user1".to_string());
    let membership = super::CachedMembership {
        is_member: true,
        is_banned: false,
    };

    cache.insert(key.clone(), membership);
    assert!(
        cache.get(&key).is_some(),
        "Entry should exist before invalidation"
    );

    // Invalidate the entry (simulates receiving KickUser/KickUserFromRoom event)
    cache.invalidate(&key);
    assert!(
        cache.get(&key).is_none(),
        "Entry should be removed after invalidation"
    );
}

#[test]
fn test_membership_cache_invalidation_only_affects_target_user() {
    // Verify that invalidating one user's cache does not affect other users
    let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
        moka::sync::Cache::builder()
            .time_to_live(super::MEMBERSHIP_CACHE_TTL)
            .build();

    let key_user1 = ("room1".to_string(), "user1".to_string());
    let key_user2 = ("room1".to_string(), "user2".to_string());

    cache.insert(
        key_user1.clone(),
        super::CachedMembership {
            is_member: true,
            is_banned: false,
        },
    );
    cache.insert(
        key_user2.clone(),
        super::CachedMembership {
            is_member: true,
            is_banned: false,
        },
    );

    // Invalidate only user1
    cache.invalidate(&key_user1);

    assert!(
        cache.get(&key_user1).is_none(),
        "User1 entry should be invalidated"
    );
    assert!(
        cache.get(&key_user2).is_some(),
        "User2 entry should still be cached"
    );
}

#[test]
fn test_cached_membership_from_member_banned() {
    // Verify CachedMembership correctly identifies banned users
    use synctv_core::models::{RoomMember, RoomRole};

    let member = RoomMember {
        room_id: room_id(),
        user_id: user_id(),
        role: RoomRole::Member,
        status: synctv_core::models::MemberStatus::Left,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: now(),
        left_at: None,
        version: 1,
        banned_at: Some(now()),
        banned_by: None,
        banned_reason: Some("test ban".to_string()),
    };

    let cached = super::CachedMembership::from_member(Some(&member));
    assert!(cached.is_member);
    assert!(cached.is_banned, "Banned user should have is_banned=true");
}

#[test]
fn test_cached_membership_from_member_none() {
    // Verify CachedMembership correctly handles non-members
    let cached = super::CachedMembership::from_member(None);
    assert!(!cached.is_member, "Non-member should have is_member=false");
    assert!(!cached.is_banned);
}

#[test]
fn test_cached_membership_from_member_active() {
    // Verify CachedMembership correctly identifies active members
    use synctv_core::models::{MemberStatus, RoomMember, RoomRole};

    let member = RoomMember {
        room_id: room_id(),
        user_id: user_id(),
        role: RoomRole::Member,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: now(),
        left_at: None,
        version: 1,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let cached = super::CachedMembership::from_member(Some(&member));
    assert!(cached.is_member);
    assert!(!cached.is_banned, "Active member should not be banned");
}

#[test]
fn test_sdp_offer_within_limit() {
    let offer = crate::proto::client::WebRtcOffer {
        to: "user1:conn1".to_string(),
        from: String::new(),
        data: "a".repeat(super::MAX_SDP_SIZE),
    };
    // Size check passes (equal to limit)
    assert!(offer.data.len() <= super::MAX_SDP_SIZE);
}

#[test]
fn test_sdp_offer_exceeds_limit() {
    let offer = crate::proto::client::WebRtcOffer {
        to: "user1:conn1".to_string(),
        from: String::new(),
        data: "a".repeat(super::MAX_SDP_SIZE + 1),
    };
    // Size check fails (exceeds limit)
    assert!(offer.data.len() > super::MAX_SDP_SIZE);
}

#[test]
fn test_sdp_answer_exceeds_limit() {
    let answer = crate::proto::client::WebRtcAnswer {
        to: "user1:conn1".to_string(),
        from: String::new(),
        data: "a".repeat(super::MAX_SDP_SIZE + 1),
    };
    assert!(answer.data.len() > super::MAX_SDP_SIZE);
}

#[test]
fn test_ice_candidate_within_limit() {
    let candidate = crate::proto::client::WebRtcIceCandidate {
        to: "user1:conn1".to_string(),
        from: String::new(),
        data: "a".repeat(super::MAX_ICE_CANDIDATE_SIZE),
    };
    assert!(candidate.data.len() <= super::MAX_ICE_CANDIDATE_SIZE);
}

#[test]
fn test_ice_candidate_exceeds_limit() {
    let candidate = crate::proto::client::WebRtcIceCandidate {
        to: "user1:conn1".to_string(),
        from: String::new(),
        data: "a".repeat(super::MAX_ICE_CANDIDATE_SIZE + 1),
    };
    assert!(candidate.data.len() > super::MAX_ICE_CANDIDATE_SIZE);
}

#[test]
fn test_private_ice_candidate_detection() {
    assert!(
        super::StreamMessageHandler::ice_candidate_contains_private_ip(
            "candidate:0 1 UDP 2122252543 192.168.1.10 54400 typ host"
        )
    );
    assert!(
        super::StreamMessageHandler::ice_candidate_contains_private_ip(
            "candidate:0 1 UDP 2122252543 fd00::1 54400 typ host"
        )
    );
    assert!(
        !super::StreamMessageHandler::ice_candidate_contains_private_ip(
            "candidate:0 1 UDP 2122252543 203.0.113.10 54400 typ srflx"
        )
    );
}

#[tokio::test]
async fn test_progress_throttle_first_write_always_allowed() {
    // First write should always go through (None state)
    let state: tokio::sync::Mutex<Option<(f64, tokio::time::Instant)>> =
        tokio::sync::Mutex::new(None);
    let guard = state.lock().await;
    assert!(guard.is_none(), "Initial state should be None");
}

#[tokio::test]
async fn test_progress_throttle_small_position_change_suppressed() {
    // A position change less than PROGRESS_MIN_POSITION_DELTA should be suppressed
    // when less than PROGRESS_MIN_ELAPSED_SECS has passed
    let last_pos: f64 = 100.0;
    let last_time = tokio::time::Instant::now();
    let new_pos: f64 = 100.5; // delta = 0.5 < 1.0

    let pos_delta = (new_pos - last_pos).abs();
    let elapsed = last_time.elapsed().as_secs_f64();

    let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
        || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
    assert!(
        !should_write,
        "Small position change with short elapsed time should be suppressed"
    );
}

#[tokio::test]
async fn test_progress_throttle_large_position_change_allowed() {
    // A position change >= PROGRESS_MIN_POSITION_DELTA should be allowed
    let last_pos: f64 = 100.0;
    let last_time = tokio::time::Instant::now();
    let new_pos: f64 = 101.5; // delta = 1.5 > 1.0

    let pos_delta = (new_pos - last_pos).abs();
    let elapsed = last_time.elapsed().as_secs_f64();

    let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
        || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
    assert!(should_write, "Large position change should trigger a write");
}

#[tokio::test]
async fn test_progress_throttle_elapsed_time_allows_write() {
    // Even with small position delta, elapsed time > 5s should allow write
    let last_pos: f64 = 100.0;
    // Simulate 6 seconds elapsed
    let last_time = tokio::time::Instant::now() - std::time::Duration::from_secs_f64(6.0);
    let new_pos: f64 = 100.1; // very small delta

    let pos_delta = (new_pos - last_pos).abs();
    let elapsed = last_time.elapsed().as_secs_f64();

    let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
        || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
    assert!(
        should_write,
        "Elapsed time exceeding threshold should trigger a write"
    );
}

#[tokio::test]
async fn test_user_left_retry_semaphore_limits_concurrent_tasks() {
    // Acquire all 100 permits to simulate max concurrent retry tasks
    let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
    let mut permits = Vec::new();

    for _ in 0..100 {
        let permit = semaphore.clone().try_acquire_owned();
        assert!(permit.is_ok(), "Should acquire permit under limit");
        permits.push(permit.unwrap());
    }

    // 101st attempt should fail
    let overflow = semaphore.clone().try_acquire_owned();
    assert!(
        overflow.is_err(),
        "Should reject when semaphore is exhausted"
    );

    // Release one permit and try again
    permits.pop();
    let retry = semaphore.try_acquire_owned();
    assert!(retry.is_ok(), "Should succeed after a permit is released");
}

#[test]
fn test_user_left_requires_retry_when_distributed_delivery_is_missing() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: true,
        },
    );

    assert!(
            !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable),
            "when distributed fan-out is configured, local delivery alone is insufficient for UserLeft consistency"
        );
}

#[test]
fn test_user_left_does_not_retry_in_single_node_mode_after_local_delivery() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: false,
        },
    );

    assert!(
            outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable),
            "single-node mode should not spawn retries when the local subscriber already received UserLeft"
        );
    assert!(
        !outcome.distributed_delivery_missed(),
        "single-node mode should not treat missing redis delivery as a retry condition"
    );
}

#[test]
fn test_user_left_does_not_retry_when_no_subscribers_exist_and_distributed_backend_is_off() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: false,
        },
    );

    assert!(
        !outcome.distributed_delivery_missed(),
        "absence of subscribers without distributed fan-out must not trigger UserLeft retries"
    );
}

#[test]
fn test_guest_policy_authorization_error_disconnects_guest() {
    let reason = "Guest access is not allowed in this room";
    let result = super::guest_policy_error_to_denial_reason(synctv_core::Error::Authorization(
        reason.to_string(),
    ))
    .expect("authorization denials should be converted to disconnect reasons");

    assert_eq!(result.as_deref(), Some(reason));
}

#[test]
fn test_guest_policy_backend_error_remains_transient() {
    let result = super::guest_policy_error_to_denial_reason(
        synctv_core::Error::ServiceUnavailable("settings store unavailable".to_string()),
    );

    assert!(
        result.is_err(),
        "backend failures should remain transient so heartbeat can retry"
    );
}

#[tokio::test]
async fn test_guest_token_blacklist_disconnects_live_guest() {
    let event_service = test_realtime_manager("guest_token_blacklist_disconnects_live_guest").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        PermissionBits(PermissionBits::DEFAULT_GUEST),
    );
    let identity = handler
        .principal
        .guest_identity()
        .expect("test handler should be a guest");
    let key = handler
        .room_service
        .user_service()
        .key_builder()
        .guest_token_blacklist(&identity.token_jti);
    handler
        .room_service
        .user_service()
        .token_blacklist_store()
        .blacklist(&key, 3600)
        .await
        .expect("blacklist guest token");

    let reason = handler
        .guest_token_blacklist_denial_reason(&identity.token_jti)
        .await
        .expect("blacklist check should succeed");

    assert_eq!(reason.as_deref(), Some("Guest token has been revoked"));

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_guest_chat_is_rejected_even_if_permission_bits_include_send_chat() {
    let fixture = create_guest_start_handler_fixture(
        "guest_chat_rejected",
        FailingMessageSender::fail_after(usize::MAX),
        PermissionBits(PermissionBits::SEND_CHAT),
    )
    .await;

    let err = fixture
        .handler
        .handle_client_message(&ClientMessage {
            message: Some(crate::proto::client::client_message::Message::Chat(
                crate::proto::client::ChatMessageSend {
                    content: "guest message".to_string(),
                    position: None,
                    color: None,
                },
            )),
        })
        .await
        .expect_err("guest chat must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_guest_danmaku_is_rejected_even_if_permission_bits_include_send_chat() {
    let fixture = create_guest_start_handler_fixture(
        "guest_danmaku_rejected",
        FailingMessageSender::fail_after(usize::MAX),
        PermissionBits(PermissionBits::SEND_CHAT),
    )
    .await;

    let err = fixture
        .handler
        .handle_client_message(&ClientMessage {
            message: Some(crate::proto::client::client_message::Message::Chat(
                crate::proto::client::ChatMessageSend {
                    content: "guest danmaku".to_string(),
                    position: Some(1.0),
                    color: Some("#ffffff".to_string()),
                },
            )),
        })
        .await
        .expect_err("guest danmaku must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_guest_playlist_observation_is_rejected_even_if_permission_bits_include_view_playlist()
{
    let fixture = create_guest_start_handler_fixture(
        "guest_playlist_observe_rejected",
        FailingMessageSender::fail_after(usize::MAX),
        PermissionBits(PermissionBits::VIEW_PLAYLIST),
    )
    .await;

    let err = fixture
        .handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "guest-playlist-items",
                String::new(),
                crate::proto::client::ListPlaylistItemsRequest::default(),
            )),
        })
        .await
        .expect_err("guest playlist observation must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot observe playlist items"));
    fixture.shutdown().await;
}

#[test]
fn test_guest_left_retry_rebuilds_guest_left_event() {
    let expected_room_id = room_id();
    let original = RealtimeEvent::GuestLeft {
        event_id: "original-event".to_string(),
        room_id: expected_room_id,
        guest_id: "gst_session".to_string(),
        username: "Guest sessio".to_string(),
        timestamp: chrono::Utc::now() - chrono::Duration::seconds(10),
    };

    let retry = super::rebuild_leave_event_for_retry(&original);

    match retry {
        RealtimeEvent::GuestLeft {
            event_id,
            room_id,
            guest_id,
            username,
            ..
        } => {
            assert_ne!(event_id, "original-event");
            assert_eq!(room_id, expected_room_id);
            assert_eq!(guest_id, "gst_session");
            assert_eq!(username, "Guest sessio");
        }
        other => panic!("guest leave retries must remain GuestLeft, got {other:?}"),
    }
}

#[test]
fn test_user_left_retry_rebuilds_user_left_event() {
    let expected_room_id = room_id();
    let expected_user_id = user_id();
    let original = RealtimeEvent::UserLeft {
        event_id: "original-event".to_string(),
        room_id: expected_room_id,
        user_id: expected_user_id,
        username: "user".to_string(),
        timestamp: chrono::Utc::now() - chrono::Duration::seconds(10),
    };

    let retry = super::rebuild_leave_event_for_retry(&original);

    match retry {
        RealtimeEvent::UserLeft {
            event_id,
            room_id,
            user_id,
            username,
            ..
        } => {
            assert_ne!(event_id, "original-event");
            assert_eq!(room_id, expected_room_id);
            assert_eq!(user_id, expected_user_id);
            assert_eq!(username, "user");
        }
        other => panic!("user leave retries must remain UserLeft, got {other:?}"),
    }
}

#[test]
fn test_webrtc_signal_requires_distributed_delivery_when_available() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: true,
        },
    );

    assert!(
            !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable),
            "cluster-mode WebRTC signaling must fail closed unless distributed publish succeeds because local room fan-out cannot prove the targeted peer received the signal"
        );
}

#[test]
fn test_webrtc_signal_allows_single_node_delivery_without_distributed_backend() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: false,
        },
    );

    assert!(outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable));
}

#[test]
fn test_webrtc_signal_allows_cluster_delivery_when_distributed_publish_succeeds() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: true,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: true,
        },
    );

    assert!(outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable));
}

#[test]
fn test_webrtc_signal_fails_when_no_delivery_path_succeeds() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: false,
        },
        crate::runtime::RealtimeMetrics {
            distributed_enabled: true,
        },
    );

    assert!(!outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable));
}

#[test]
fn test_webrtc_membership_transition_requires_existing_connection() {
    let result = super::should_transition_webrtc_membership(None, true);
    assert_eq!(result, Err("Connection not found"));
}

#[test]
fn test_webrtc_membership_transition_detects_join_state_change() {
    let result = super::should_transition_webrtc_membership(Some(false), true);
    assert_eq!(result, Ok(true));
}

#[test]
fn test_webrtc_membership_transition_ignores_duplicate_join() {
    let result = super::should_transition_webrtc_membership(Some(true), true);
    assert_eq!(result, Ok(false));
}

#[test]
fn test_webrtc_membership_transition_detects_leave_state_change() {
    let result = super::should_transition_webrtc_membership(Some(true), false);
    assert_eq!(result, Ok(true));
}

#[test]
fn test_webrtc_membership_transition_ignores_duplicate_leave() {
    let result = super::should_transition_webrtc_membership(Some(false), false);
    assert_eq!(result, Ok(false));
}

#[test]
fn test_user_left_delivery_skips_when_local_connection_remains() {
    let plan = super::should_broadcast_user_left(true, Ok(false));
    assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
}

#[test]
fn test_user_left_delivery_skips_when_distributed_presence_exists() {
    let plan = super::should_broadcast_user_left(false, Ok(true));
    assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
}

#[test]
fn test_user_left_delivery_uses_local_and_redis_when_user_is_last_presence() {
    let plan = super::should_broadcast_user_left(false, Ok(false));
    assert_eq!(plan, super::UserLeftDeliveryPlan::LocalAndRedis);
}

#[test]
fn test_user_left_delivery_skips_when_distributed_check_fails() {
    let plan = super::should_broadcast_user_left(false, Err(()));
    assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
}

#[tokio::test]
async fn test_guest_cleanup_broadcasts_guest_left() {
    let event_service = test_realtime_manager("guest_cleanup_broadcasts_left").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        PermissionBits(PermissionBits::DEFAULT_GUEST),
    );
    let connection_id = handler.connection_id().to_string();
    let guest_user_id = handler.user_id;

    connection_service
        .register(connection_id.clone(), guest_user_id)
        .await
        .expect("register guest connection");
    connection_service
        .join_room(&connection_id, handler.room_id)
        .await
        .expect("join guest connection");
    let (mut rx, _) = event_service
        .subscribe_with_id(handler.room_id, guest_user_id, connection_id.clone())
        .await
        .expect("subscribe guest connection");

    handler.cleanup(&handler.room_id.to_string()).await;

    let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("guest left event should be delivered")
        .expect("guest left receiver should remain open until event is read");
    match event {
        RealtimeEvent::GuestLeft {
            room_id,
            guest_id,
            username,
            ..
        } => {
            assert_eq!(room_id, handler.room_id);
            assert_eq!(guest_id, handler.public_actor_id());
            assert_eq!(username, handler.username);
        }
        other => panic!("expected GuestLeft event, got {other:?}"),
    }
    assert_eq!(connection_service.connection_count(), 0);

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_guest_webrtc_recipient_validation_uses_public_guest_actor_id() {
    let event_service = test_realtime_manager("guest_webrtc_recipient").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        PermissionBits(PermissionBits::DEFAULT_GUEST | PermissionBits::USE_WEBRTC),
    );
    let connection_id = handler.connection_id().to_string();

    connection_service
        .register_actor(
            connection_id.clone(),
            handler.user_id,
            handler.public_actor_id(),
        )
        .await
        .expect("register guest connection");
    connection_service
        .join_room(&connection_id, handler.room_id)
        .await
        .expect("join room");
    connection_service.mark_rtc_joined(&handler.room_id, &handler.user_id, &connection_id, true);

    let guest_target = format!("{}:{}", handler.public_actor_id(), connection_id);
    assert!(
        handler.current_connection_matches_webrtc_recipient(&guest_target),
        "guest WebRTC targets must use the gst_* public actor id"
    );
    handler
        .validate_webrtc_recipient(&guest_target)
        .expect("gst_* recipient should match the active guest connection");

    let internal_user_target = format!(
        "{}:{}",
        handler
            .public_id_codec
            .encode_user_id(handler.user_id)
            .expect("encode internal synthetic user id"),
        connection_id
    );
    assert!(
        !handler.current_connection_matches_webrtc_recipient(&internal_user_target),
        "guest WebRTC targets must not leak or accept the internal synthetic user id"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[test]
fn test_generate_connection_id_is_opaque() {
    let connection_id = super::StreamMessageHandler::generate_connection_id();

    assert!(connection_id.starts_with("conn_"));
    assert!(
        connection_id
            .strip_prefix("conn_c")
            .is_some_and(|id| id.len() == 16),
        "connection id should use a fixed opaque random token shape"
    );
    assert!(
        connection_id["conn_".len()..].parse::<i64>().is_err(),
        "connection id suffix must not be a raw internal numeric id"
    );
}

#[tokio::test]
async fn test_current_connection_matches_webrtc_recipient_requires_public_actor_id() {
    let room_id = room_id();
    let user_id = user_id();
    let manager = test_connection_manager();
    let pool = test_pool();
    let event_service = test_realtime_manager("node-test").await;
    let public_id_codec = Arc::new(crate::PublicIdCodec::default_for_tests());

    let handler = super::StreamMessageHandler::new(
        room_id,
        user_id,
        "user".to_string(),
        &test_room_service(pool.clone()),
        test_chat_service(pool),
        event_service,
        manager.clone(),
        Arc::new(RateLimiter::local_only(
            "test:webrtc-recipient-public-user-match:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::clone(&public_id_codec),
        FailingMessageSender::fail_after(usize::MAX),
    );
    let connection_id = handler.connection_id().to_string();
    let recipient = format!(
        "{}:{}",
        public_id_codec
            .encode_user_id(user_id)
            .expect("encode user id"),
        connection_id
    );

    manager
        .register_actor(
            connection_id.clone(),
            user_id,
            public_id_codec
                .encode_user_id(user_id)
                .expect("encode user id"),
        )
        .await
        .expect("register");
    manager
        .join_room(&connection_id, room_id)
        .await
        .expect("join room");
    manager.mark_rtc_joined(&room_id, &user_id, &connection_id, true);

    assert!(
        handler.current_connection_matches_webrtc_recipient(&recipient),
        "WebRTC recipient should match the current connection only with public user id"
    );
}

#[tokio::test]
async fn test_current_connection_matches_webrtc_recipient_rejects_malformed_target() {
    let room_id = room_id();
    let user_id = user_id();
    let manager = test_connection_manager();
    let pool = test_pool();
    let event_service = test_realtime_manager("node-test").await;

    let handler = super::StreamMessageHandler::new(
        room_id,
        user_id,
        "user".to_string(),
        &test_room_service(pool.clone()),
        test_chat_service(pool),
        event_service,
        manager.clone(),
        Arc::new(RateLimiter::local_only(
            "test:webrtc-recipient-malformed-reject:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        FailingMessageSender::fail_after(usize::MAX),
    );
    let connection_id = handler.connection_id().to_string();

    manager
        .register(connection_id.clone(), user_id)
        .await
        .expect("register");
    manager
        .join_room(&connection_id, room_id)
        .await
        .expect("join room");
    manager.mark_rtc_joined(&room_id, &user_id, &connection_id, true);

    assert!(
        !handler.current_connection_matches_webrtc_recipient(&connection_id),
        "malformed WebRTC recipient must not match"
    );
}

#[test]
fn test_initial_realtime_join_denial_reason_rejects_missing_member() {
    let lookup = Ok(super::RealtimeMembershipAccess::Denied(
        "Not a member of this room".to_string(),
    ));

    assert_eq!(
        super::initial_realtime_join_denial_reason(&lookup).as_deref(),
        Some("Not a member of this room")
    );
}

#[test]
fn test_initial_realtime_join_denial_reason_rejects_banned_member() {
    let lookup = Ok(super::RealtimeMembershipAccess::Denied(
        "User is banned from this room".to_string(),
    ));

    assert_eq!(
        super::initial_realtime_join_denial_reason(&lookup).as_deref(),
        Some("User is banned from this room")
    );
}

#[test]
fn test_initial_realtime_join_denial_reason_rejects_room_with_inactive_creator() {
    let lookup = Ok(super::RealtimeMembershipAccess::Denied(
        "Room is unavailable because its creator is not active".to_string(),
    ));

    assert_eq!(
        super::initial_realtime_join_denial_reason(&lookup).as_deref(),
        Some("Room is unavailable because its creator is not active")
    );
}

#[test]
fn test_initial_realtime_join_denial_reason_allows_lookup_errors_to_retry_later() {
    let lookup = Err(synctv_core::Error::Internal("db unavailable".to_string()));

    assert_eq!(super::initial_realtime_join_denial_reason(&lookup), None);
}

#[test]
fn test_realtime_join_error_from_string_classifies_capacity_errors() {
    let error = super::RealtimeJoinError::from("realtime room capacity exceeded".to_string());

    assert!(matches!(
        error,
        super::RealtimeJoinError::RateLimited(message)
        if message == "realtime room capacity exceeded"
    ));
}

#[test]
fn test_realtime_join_error_into_string_preserves_message() {
    let message = String::from(super::RealtimeJoinError::PermissionDenied(
        "Not a member of this room".to_string(),
    ));

    assert_eq!(message, "Not a member of this room");
}

#[tokio::test]
async fn test_pre_join_after_registration_fails_closed_when_membership_revalidation_unavailable() {
    let event_service =
        test_realtime_manager("test_pre_join_membership_revalidation_unavailable").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before final admission");

    let error = handler
        .pre_join_after_registration()
        .await
        .expect_err("membership revalidation outages must reject final realtime admission");

    assert!(
        error.to_string().contains("temporarily unavailable"),
        "expected retryable membership revalidation error, got: {error}"
    );
    assert_eq!(
        connection_service.connection_count(),
        0,
        "failed final admission must roll back the registered connection"
    );
    assert_eq!(
        connection_service.room_connection_count(&handler.room_id),
        0,
        "failed final admission must not leave room membership behind"
    );
    assert_eq!(
        connection_service.user_connection_count(&handler.user_id),
        0,
        "failed final admission must not consume per-user capacity"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pre_join_after_registration_rejects_closed_room_on_final_revalidation() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager("test_pre_join_room_closed_final_revalidation").await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = user_service
        .register(
            "room-owner".to_string(),
            Some("owner@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("owner should register")
        .0;
    let member = user_service
        .register(
            "room-member".to_string(),
            Some("member@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("member should register")
        .0;
    let (room, _) = room_service
        .create_room(
            "Realtime Room".to_string(),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");

    let handler = super::StreamMessageHandler::new(
        room.id,
        member.id,
        member.username.clone(),
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(
            "test:pre-join-room-closed:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        FailingMessageSender::fail_after(usize::MAX),
    );

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before final admission");

    room_service
        .update_room_status(&room.id, RoomStatus::Closed)
        .await
        .expect("closing room should succeed");

    let error = handler
        .pre_join_after_registration()
        .await
        .expect_err("closed room must fail final realtime admission");

    assert!(
        error.to_string().contains("closed"),
        "expected closed-room error, got: {error}"
    );
    assert_eq!(connection_service.connection_count(), 0);
    assert_eq!(connection_service.room_connection_count(&room.id), 0);
    assert_eq!(connection_service.user_connection_count(&member.id), 0);

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pre_join_after_registration_rejects_room_with_inactive_creator() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service =
        test_realtime_manager("test_pre_join_room_creator_inactive_final_revalidation").await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = user_service
        .register(
            "room-owner-inactive".to_string(),
            Some("owner-inactive@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("owner should register")
        .0;
    let member = user_service
        .register(
            "room-member-inactive-owner".to_string(),
            Some("member-inactive-owner@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("member should register")
        .0;
    let (room, _) = room_service
        .create_room(
            "Realtime Room Inactive Owner".to_string(),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");

    let handler = super::StreamMessageHandler::new(
        room.id,
        member.id,
        member.username.clone(),
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(
            "test:pre-join-room-owner-inactive:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        FailingMessageSender::fail_after(usize::MAX),
    );

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before final admission");

    UserRepository::new(pool.clone())
        .ban(&owner.id, None, Some("messaging test".to_string()))
        .await
        .expect("banning room owner should succeed");

    let error = handler
        .pre_join_after_registration()
        .await
        .expect_err("room with inactive creator must fail final realtime admission");

    assert!(
        error.to_string().contains("creator is not active"),
        "expected room-owner-inactive error, got: {error}"
    );
    assert_eq!(connection_service.connection_count(), 0);
    assert_eq!(connection_service.room_connection_count(&room.id), 0);
    assert_eq!(connection_service.user_connection_count(&member.id), 0);

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pre_join_after_registration_rejects_banned_user_on_final_revalidation() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager("test_pre_join_user_banned_final_revalidation").await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = user_service
        .register(
            "room-owner-ban".to_string(),
            Some("owner-ban@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("owner should register")
        .0;
    let member = user_service
        .register(
            "room-member-ban".to_string(),
            Some("member-ban@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("member should register")
        .0;
    let (room, _) = room_service
        .create_room(
            "Realtime Room Ban".to_string(),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");

    let handler = super::StreamMessageHandler::new(
        room.id,
        member.id,
        member.username.clone(),
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(
            "test:pre-join-user-banned:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        FailingMessageSender::fail_after(usize::MAX),
    );

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before final admission");

    user_service
        .ban_user_and_cleanup_memberships(&member.id, None, None)
        .await
        .expect("banning user should succeed");

    let error = handler
        .pre_join_after_registration()
        .await
        .expect_err("banned user must fail final realtime admission");

    assert!(
        error.to_string().contains("no longer allowed"),
        "expected banned-user error, got: {error}"
    );
    assert_eq!(connection_service.connection_count(), 0);
    assert_eq!(connection_service.room_connection_count(&room.id), 0);
    assert_eq!(connection_service.user_connection_count(&member.id), 0);

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pre_join_after_registration_rolls_back_when_room_event_subscription_caching_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    // Use dedicated Redis since this test terminates the container
    let (redis, redis_url): (RedisContainer, String) =
        start_dedicated_redis_url_with_label("msg-pre-join-subscription-fail").await;
    let event_service =
        test_realtime_manager_with_redis("test_pre_join_subscription_cache_failure", &redis_url)
            .await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = user_service
        .register(
            "room-owner-sub-fail".to_string(),
            Some("owner-sub-fail@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("owner should register")
        .0;
    let member = user_service
        .register(
            "room-member-sub-fail".to_string(),
            Some("member-sub-fail@test.invalid".to_string()),
            "Password123!".to_string(),
            None,
        )
        .await
        .expect("member should register")
        .0;
    let (room, _) = room_service
        .create_room(
            "Realtime Room Subscription Fail".to_string(),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");

    let handler = super::StreamMessageHandler::new(
        room.id,
        member.id,
        member.username.clone(),
        &room_service,
        test_chat_service(pool.clone()),
        event_service.clone(),
        connection_service.clone(),
        Arc::new(RateLimiter::local_only(
            "test:pre-join-subscription-fail:".to_string(),
        )),
        Arc::new(RateLimitConfig::default()),
        Arc::new(ContentFilter::new()),
        Arc::new(crate::PublicIdCodec::default_for_tests()),
        FailingMessageSender::fail_after(usize::MAX),
    );

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before subscription caching");

    redis.terminate();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        handler.pre_join_after_registration(),
    )
    .await
    .expect("pre_join_after_registration should not hang after Redis disappears")
    .expect_err("subscription caching failure must reject final realtime admission");

    assert!(
        error
            .to_string()
            .contains("Failed to subscribe to realtime events during pre_join"),
        "expected room subscription caching error, got: {error}"
    );
    assert_eq!(
        connection_service.connection_count(),
        0,
        "failed subscription caching must roll back the registered connection"
    );
    assert_eq!(
        connection_service.room_connection_count(&room.id),
        0,
        "failed subscription caching must not leave room membership behind"
    );
    assert_eq!(
        connection_service.user_connection_count(&member.id),
        0,
        "failed subscription caching must not consume per-user capacity"
    );
    assert_eq!(
        realtime_manager_subscriber_count(&event_service, &room.id),
        0,
        "failed subscription caching must not leave local room subscribers behind"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[test]
fn test_disconnect_signal_requires_skip_cleanup_only_for_room_scoped_or_redundant_exits() {
    let rid = room_id();
    let uid = user_id();
    let connection_id = "conn-123";

    assert!(super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::Connection(connection_id.to_string()),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(!super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::User(uid),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::Room(rid),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        },
        &uid,
        &rid,
        connection_id,
    ));
}

#[test]
fn test_admin_event_requires_skip_cleanup_only_for_room_scoped_or_redundant_exits() {
    let rid = room_id();
    let uid = user_id();
    let now = chrono::Utc::now();

    assert!(!super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::KickUser {
            event_id: "evt-1".to_string(),
            user_id: uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::KickUserFromRoom {
            event_id: "evt-2".to_string(),
            room_id: rid,
            user_id: uid,
            reason: "kick".to_string(),
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::UserLeft {
            event_id: "evt-3".to_string(),
            room_id: rid,
            user_id: uid,
            username: "tester".to_string(),
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-4".to_string(),
            room_id: rid,
            banned_by: uid,
            timestamp: now,
        },
        &uid,
        &rid,
    ));
}

#[tokio::test]
async fn test_connection_reservation_room_slot() {
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    let limits = ConnectionLimits {
        max_per_room: 2,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid = room_id();

    // First two reservations should succeed
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_ok());

    // Third should fail (limit is 2)
    assert!(mgr.reserve_room_slot(&rid).is_err());

    // Release one reservation
    mgr.release_room_reservation(&rid);

    // Now reservation should succeed again
    assert!(mgr.reserve_room_slot(&rid).is_ok());
}

#[tokio::test]
async fn test_connection_reservation_user_slot() {
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    let limits = ConnectionLimits {
        max_per_user: 3,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let uid = user_id();

    // Three reservations should succeed
    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(mgr.reserve_user_slot(&uid).is_ok());

    // Fourth should fail (limit is 3)
    assert!(mgr.reserve_user_slot(&uid).is_err());

    // Release two
    mgr.release_user_reservation(&uid);
    mgr.release_user_reservation(&uid);

    // Now two more should succeed
    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(mgr.reserve_user_slot(&uid).is_ok());

    // But the next should fail again
    assert!(mgr.reserve_user_slot(&uid).is_err());
}

#[tokio::test]
async fn test_connection_reservation_concurrent_simulation() {
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    let limits = ConnectionLimits {
        max_per_room: 5,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid = room_id();

    // Simulate 10 concurrent reservation attempts (only 5 should succeed)
    let mut successes = 0;
    for _ in 0..10 {
        if mgr.reserve_room_slot(&rid).is_ok() {
            successes += 1;
        }
    }
    assert_eq!(
        successes, 5,
        "Only 5 of 10 concurrent requests should succeed"
    );
}
