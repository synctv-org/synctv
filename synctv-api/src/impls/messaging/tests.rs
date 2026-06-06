use super::*;
use crate::proto::client::server_message::Message;
use crate::runtime::RealtimeDeliveryOutcome;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::notification::{Notification, NotificationType};
use synctv_core::models::{
    ChatEventKind, ChatImage, ChatMessage, ChatMessageEvent, ChatMessageStatus, ChatMessageType,
    ChatMessageWithImages, MediaId, Playlist, PlaylistId, RoomAdminPermissionBits, RoomId,
    RoomMemberPermissionBits, RoomPermission, RoomPermissionSet, RoomPlaybackState, RoomRole,
    SendChatMessage, UserId,
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
use synctv_core_testing::{create_test_request_rate_limiter, opaque_register_user};
use synctv_realtime::sync::{
    build_room_message_runtime, ConnectionLimits, ConnectionManager, RealtimeConfig,
    RealtimeManager,
};
use synctv_realtime::sync::{NotificationLevel, RealtimeEvent, RoomMessageHub};
use tokio::sync::{broadcast, mpsc};

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
    crate::PublicIdCodec::plain()
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

struct LocalRuntimeRealtimeEventService {
    admin_tx: tokio::sync::broadcast::Sender<RealtimeEvent>,
}

impl LocalRuntimeRealtimeEventService {
    fn new() -> Self {
        let (admin_tx, _) = tokio::sync::broadcast::channel(16);
        Self { admin_tx }
    }
}

#[async_trait::async_trait]
impl RealtimeEventService for LocalRuntimeRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        connection_id: String,
    ) -> synctv_realtime::Result<(tokio::sync::mpsc::Receiver<RealtimeEvent>, String)> {
        let (_tx, rx) = tokio::sync::mpsc::channel(16);
        Ok((rx, connection_id))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, _event: RealtimeEvent) -> synctv_realtime::sync::BroadcastResult {
        synctv_realtime::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: false,
        }
    }

    fn publish_only(&self, _event: RealtimeEvent) -> bool {
        false
    }

    fn broadcast_local(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
        0
    }

    fn subscribe_admin_events(&self) -> tokio::sync::broadcast::Receiver<RealtimeEvent> {
        self.admin_tx.subscribe()
    }

    fn metrics(&self) -> crate::runtime::RealtimeMetrics {
        crate::runtime::RealtimeMetrics {
            distributed_enabled: false,
        }
    }

    fn node_id(&self) -> &'static str {
        "local-runtime-test"
    }

    async fn shutdown(&self) {}
}

fn event_cursor(sequence: i64) -> crate::proto::client::EventCursor {
    crate::proto::client::EventCursor {
        event_id: Some(format!("event-{sequence}")),
        sequence,
    }
}

fn chat_image() -> ChatImage {
    ChatImage {
        id: "chat-image-1".to_string(),
        room_id: room_id(),
        message_id: 10,
        message_created_at: chrono::Utc::now(),
        storage_backend: "database".to_string(),
        object_key: "chat/images/chat-image-1".to_string(),
        url: Some("https://cdn.example.test/chat-image-1.png".to_string()),
        mime_type: Some("image/png".to_string()),
        size_bytes: Some(1024),
        width: Some(320),
        height: Some(240),
        metadata: serde_json::json!({"sha256": "abc"}),
        created_at: chrono::Utc::now(),
    }
}

fn test_stream_handler_runtime() -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        heartbeat_schedule: Some(HeartbeatSchedule::fixed(
            Duration::from_millis(10),
            Duration::from_mins(1),
        )),
        ..StreamMessageHandlerRuntime::local(Arc::new(LocalRuntimeRealtimeEventService::new()))
    }
}

fn runtime_with_playback_snapshot_service(
    service: Arc<dyn PlaybackSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        playback_snapshot_service: Some(service),
        ..test_stream_handler_runtime()
    }
}

fn runtime_with_playlist_items_snapshot_service(
    service: Arc<dyn PlaylistItemsSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        playlist_items_snapshot_service: Some(service),
        ..test_stream_handler_runtime()
    }
}

fn runtime_with_room_members_snapshot_service(
    service: Arc<dyn RoomMembersSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        room_members_snapshot_service: Some(service),
        ..test_stream_handler_runtime()
    }
}

fn runtime_with_room_settings_snapshot_service(
    service: Arc<dyn RoomSettingsSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        room_settings_snapshot_service: Some(service),
        ..test_stream_handler_runtime()
    }
}

#[test]
fn internal_guest_user_id_is_deterministic_and_reserved() {
    let first = internal_guest_user_id(room_id(), "session-a")
        .expect("guest internal user id should build");
    let same = internal_guest_user_id(room_id(), "session-a")
        .expect("guest internal user id should build");
    let second = internal_guest_user_id(room_id(), "session-b")
        .expect("guest internal user id should build");
    let lower_bound = super::GUEST_INTERNAL_USER_ID_BASE;
    let upper_bound = super::GUEST_INTERNAL_USER_ID_BASE
        + i64::try_from(super::GUEST_INTERNAL_USER_ID_SPAN)
            .expect("guest internal user id span fits in i64");

    assert_eq!(first, same);
    assert_ne!(first, second);
    assert!(first.as_i64() >= lower_bound);
    assert!(first.as_i64() < upper_bound);
    assert!(second.as_i64() >= lower_bound);
    assert!(second.as_i64() < upper_bound);
}

#[test]
fn core_chat_image_to_proto_requires_storage_metadata() {
    let image = chat_image();

    let proto = core_chat_image_to_proto(&image).expect("valid chat image should convert");
    assert_eq!(proto.mime_type, "image/png");
    assert_eq!(proto.size_bytes, 1024);
    assert_eq!(proto.width, 320);
    assert_eq!(proto.height, 240);

    let mut missing_mime_type = image.clone();
    missing_mime_type.mime_type = None;
    assert!(core_chat_image_to_proto(&missing_mime_type)
        .expect_err("missing mime_type should fail")
        .contains("mime_type"));

    let mut missing_size = image.clone();
    missing_size.size_bytes = None;
    assert!(core_chat_image_to_proto(&missing_size)
        .expect_err("missing size_bytes should fail")
        .contains("size_bytes"));

    let mut missing_width = image.clone();
    missing_width.width = None;
    assert!(core_chat_image_to_proto(&missing_width)
        .expect_err("missing width should fail")
        .contains("width"));

    let mut missing_height = image;
    missing_height.height = Some(0);
    assert!(core_chat_image_to_proto(&missing_height)
        .expect_err("invalid height should fail")
        .contains("height"));
}

#[test]
fn chat_display_metadata_reads_valid_presentation() {
    let metadata = serde_json::json!({
        "presentation": {
            "display_position": " top ",
            "display_color": " #ff0000 "
        }
    });

    assert_eq!(
        chat_display_position_from_metadata(&metadata).expect("display position should parse"),
        "top"
    );
    assert_eq!(
        chat_display_color_from_metadata(&metadata).expect("display color should parse"),
        "#ff0000"
    );
}

#[test]
fn chat_display_metadata_rejects_invalid_presentation_fields() {
    let invalid_container = serde_json::json!({
        "presentation": ["top"]
    });
    let invalid_type = serde_json::json!({
        "presentation": {
            "display_position": 7
        }
    });
    let control_character = serde_json::json!({
        "presentation": {
            "display_color": "red\nblue"
        }
    });

    assert!(matches!(
        chat_display_position_from_metadata(&invalid_container),
        Err(message) if message.contains("presentation")
    ));
    assert!(matches!(
        chat_display_position_from_metadata(&invalid_type),
        Err(message) if message.contains("display position")
    ));
    assert!(matches!(
        chat_display_color_from_metadata(&control_character),
        Err(message) if message.contains("display color")
    ));
}

#[test]
fn chat_playback_metadata_encodes_public_ids() {
    let metadata = serde_json::json!({
        "playback": {
            "media_id": media_id().as_i64().to_string(),
            "playlist_id": playlist().id.as_i64().to_string()
        }
    });
    let codec = public_id_codec();

    assert_eq!(
        chat_playback_media_id_from_metadata(&metadata, &codec),
        Ok(public_media_id())
    );
    assert_eq!(
        chat_playback_playlist_id_from_metadata(&metadata, &codec),
        Ok(public_playlist_id())
    );
}

#[test]
fn chat_playback_metadata_without_source_returns_empty_ids() {
    let metadata = serde_json::json!({});
    let codec = public_id_codec();

    assert_eq!(
        chat_playback_media_id_from_metadata(&metadata, &codec),
        Ok(String::new())
    );
    assert_eq!(
        chat_playback_playlist_id_from_metadata(&metadata, &codec),
        Ok(String::new())
    );
}

#[test]
fn chat_playback_metadata_rejects_invalid_source_ids() {
    let codec = public_id_codec();
    let invalid_container = serde_json::json!({
        "playback": "current"
    });
    let invalid_media = serde_json::json!({
        "playback": {
            "media_id": "abc"
        }
    });
    let invalid_playlist = serde_json::json!({
        "playback": {
            "playlist_id": "0"
        }
    });

    assert!(matches!(
        chat_playback_metadata_from_metadata(&invalid_container, &codec),
        Err(message) if message.contains("playback")
    ));
    assert!(matches!(
        chat_playback_media_id_from_metadata(&invalid_media, &codec),
        Err(message) if message.contains("media_id")
    ));
    assert!(matches!(
        chat_playback_playlist_id_from_metadata(&invalid_playlist, &codec),
        Err(message) if message.contains("playlist_id")
    ));
}

#[test]
fn chat_playback_metadata_decodes_target_hex() {
    let metadata = serde_json::json!({
        "playback": {
            "target_hex": "746172676574"
        }
    });

    assert_eq!(
        chat_playback_target_from_metadata(&metadata),
        Ok(b"target".to_vec())
    );
}

#[test]
fn chat_playback_metadata_derives_target_hash_from_target() {
    let metadata = serde_json::json!({
        "playback": {
            "target_hex": "746172676574",
            "target_hash": "stale"
        }
    });
    let playback = chat_playback_metadata_from_metadata(&metadata, &public_id_codec())
        .expect("playback metadata should parse");

    assert_eq!(playback.target, b"target".to_vec());
    assert_eq!(playback.target_hash, chat_playback_target_hash(b"target"));
}

#[test]
fn chat_playback_metadata_rejects_invalid_position_seconds() {
    let metadata = serde_json::json!({
        "playback": {
            "position_seconds": -1.0
        }
    });

    assert!(matches!(
        chat_playback_metadata_from_metadata(&metadata, &public_id_codec()),
        Err(message) if message.contains("position_seconds")
    ));
}

#[test]
fn chat_playback_metadata_rejects_invalid_target_hex() {
    let metadata = serde_json::json!({
        "playback": {
            "target_hex": "not-hex"
        }
    });
    let invalid_type = serde_json::json!({
        "playback": {
            "target_hex": 42
        }
    });

    assert!(matches!(
        chat_playback_target_from_metadata(&metadata),
        Err(message) if message.contains("target_hex")
    ));
    assert!(matches!(
        chat_playback_target_from_metadata(&invalid_type),
        Err(message) if message.contains("target_hex")
    ));
}

#[test]
fn watch_observe_builders_require_resource_bodies() {
    assert!(matches!(
        watch_playback_state_observe(crate::proto::client::WatchPlaybackStateRequest::default()),
        Err(message) if message.contains("playback_state")
    ));
    assert!(matches!(
        watch_playback_snapshot_observe(
            crate::proto::client::WatchPlaybackSnapshotRequest::default()
        ),
        Err(message) if message.contains("playback_snapshot")
    ));
    assert!(matches!(
        watch_room_settings_observe(crate::proto::client::WatchRoomSettingsRequest::default()),
        Err(message) if message.contains("room_settings")
    ));
    assert!(matches!(
        watch_playlist_items_observe(crate::proto::client::WatchPlaylistItemsRequest::default()),
        Err(message) if message.contains("playlist_items")
    ));
    assert!(matches!(
        watch_room_members_observe(crate::proto::client::WatchRoomMembersRequest::default()),
        Err(message) if message.contains("room_members")
    ));
    assert!(matches!(
        watch_chat_events_observe(crate::proto::client::WatchChatEventsRequest::default()),
        Ok(observe) if matches!(
            observe.resource,
            Some(crate::proto::client::observe_resource::Resource::ChatEvents(_))
        )
    ));
}

fn chat_event_with_content(
    room_id: RoomId,
    user_id: UserId,
    event_id: impl Into<String>,
    content: impl Into<String>,
) -> ChatMessageEvent {
    let now = chrono::Utc::now();
    ChatMessageEvent {
        event_id: event_id.into(),
        sequence: 1,
        room_id,
        actor_user_id: user_id,
        kind: ChatEventKind::Created,
        message: ChatMessageWithImages {
            message: ChatMessage {
                id: 1,
                room_id,
                user_id: Some(user_id),
                client_message_id: None,
                content: content.into(),
                message_type: ChatMessageType::Text,
                status: ChatMessageStatus::Active,
                version: 1,
                reply_to_message_id: None,
                reply_to_message_created_at: None,
                metadata: serde_json::Value::Object(Default::default()),
                edited_at: None,
                deleted_at: None,
                deleted_by: None,
                delete_reason: None,
                created_at: now,
            },
            images: Vec::new(),
            reactions: Vec::new(),
        },
        occurred_at: now,
    }
}

fn server_message_contains_chat_event_content(
    message: &crate::proto::client::ServerMessage,
    content: &str,
) -> bool {
    match &message.message {
        Some(Message::ResourceChanged(changed)) => matches!(
            changed.payload.as_ref(),
            Some(crate::proto::client::resource_changed::Payload::ChatEvent(event))
                if event
                    .message
                    .as_ref()
                    .is_some_and(|message| message.content == content)
        ),
        _ => false,
    }
}

fn server_message_is_user_joined_for(
    message: &crate::proto::client::ServerMessage,
    user_id: &str,
) -> bool {
    matches!(
        &message.message,
        Some(Message::UserJoined(joined))
            if joined
                .member
                .as_ref()
                .is_some_and(|member| member.user_id == user_id)
    )
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
        description: String::new(),
        cover_file_reference_id: None,
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
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::PlaybackState(
                    crate::proto::client::ObservePlaybackState {
                        after_event_sequence: None,
                    },
                ),
            ),
        },
    )
}

fn optional_trimmed_string(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn observe_playback_snapshot_message(
    observe_id: &'static str,
    media_id: impl Into<String>,
    playlist_id: impl Into<String>,
    target: Vec<u8>,
    playback_client_profile: Option<crate::proto::client::PlaybackClientProfile>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::PlaybackSnapshot(
                    crate::proto::client::ObservePlaybackSnapshot {
                        media_id: optional_trimmed_string(media_id),
                        playlist_id: optional_trimmed_string(playlist_id),
                        target,
                        playback_client_profile,
                        after_event_sequence: None,
                    },
                ),
            ),
        },
    )
}

fn observe_room_settings_message(
    observe_id: impl Into<String>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::RoomSettings(
                    crate::proto::client::ObserveRoomSettings {
                        after_event_sequence: None,
                    },
                ),
            ),
        },
    )
}

fn observe_playlist_items_message(
    observe_id: &'static str,
    request: crate::proto::client::ListPlaylistItemsRequest,
) -> crate::proto::client::client_message::Message {
    observe_playlist_items_message_with_sequence(observe_id, request, None)
}

fn observe_playlist_items_message_with_sequence(
    observe_id: &'static str,
    request: crate::proto::client::ListPlaylistItemsRequest,
    after_event_sequence: Option<i64>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        observe_playlist_items_resource_with_sequence(observe_id, request, after_event_sequence),
    )
}

fn observe_playlist_items_resource_with_sequence(
    observe_id: &'static str,
    request: crate::proto::client::ListPlaylistItemsRequest,
    after_event_sequence: Option<i64>,
) -> crate::proto::client::ObserveResource {
    crate::proto::client::ObserveResource {
        observe_id: observe_id.to_string(),
        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: Some(
            crate::proto::client::observe_resource::Resource::PlaylistItems(
                crate::proto::client::ObservePlaylistItems {
                    request: Some(request),
                    after_event_sequence,
                },
            ),
        ),
    }
}

fn observe_room_members_message(
    observe_id: &'static str,
    request: crate::proto::client::GetRoomMembersRequest,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::RoomMembers(
                    crate::proto::client::ObserveRoomMembers {
                        request: Some(request),
                        after_event_sequence: None,
                    },
                ),
            ),
        },
    )
}

fn observe_chat_events_message(
    observe_id: impl Into<String>,
) -> crate::proto::client::client_message::Message {
    observe_chat_events_message_with_sequence(observe_id, None)
}

fn observe_chat_events_message_with_sequence(
    observe_id: impl Into<String>,
    after_event_sequence: Option<i64>,
) -> crate::proto::client::client_message::Message {
    crate::proto::client::client_message::Message::ObserveResource(
        crate::proto::client::ObserveResource {
            observe_id: observe_id.into(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::NotifyOnly as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::ChatEvents(
                    crate::proto::client::ObserveChatEvents {
                        after_event_sequence,
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

async fn register_test_user(
    user_service: &synctv_core::service::UserService,
    username: impl Into<String>,
    email: impl Into<String>,
) -> synctv_core::models::User {
    opaque_register_user(
        user_service,
        username.into(),
        Some(email.into()),
        "Password123!",
    )
    .await
    .expect("test user should register")
    .0
}

fn test_chat_service(pool: sqlx::PgPool) -> Arc<ChatService> {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = create_test_request_rate_limiter("test:chat:");
    let content_filter = ContentFilter::new();
    let user_service = Arc::new(synctv_core_testing::create_test_user_service(pool.clone()));
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool);
    let permission_service = PermissionService::new_with_runtime(
        member_repo,
        room_repo,
        synctv_core::service::permission::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::permission::PermissionServiceRuntime::default()
        },
    )
    .expect("permission service should build");

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
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            user_service,
            file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
            audit_service: None,
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

#[allow(dead_code)]
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

struct FailingRoomMessageRuntime;

#[async_trait::async_trait]
impl synctv_realtime::sync::RoomMessageRuntime for FailingRoomMessageRuntime {
    fn subscribe_lifecycle(
        &self,
    ) -> broadcast::Receiver<synctv_realtime::sync::RoomLifecycleEvent> {
        let (_tx, rx) = broadcast::channel(1);
        rx
    }

    async fn subscribe(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        _connection_id: String,
    ) -> synctv_realtime::Result<mpsc::Receiver<RealtimeEvent>> {
        Err(synctv_realtime::Error::Internal(anyhow::anyhow!(
            "injected room subscription failure"
        )))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
        0
    }

    async fn broadcast_reliably(&self, _room_id: &RoomId, _event: RealtimeEvent) -> usize {
        0
    }

    async fn broadcast_to_connection(
        &self,
        _room_id: &RoomId,
        _connection_id: &str,
        _event: RealtimeEvent,
    ) -> usize {
        0
    }

    fn room_count(&self) -> usize {
        0
    }

    fn active_room_ids(&self) -> Vec<RoomId> {
        Vec::new()
    }

    fn connection_count(&self) -> usize {
        0
    }

    fn remove_room(&self, _room_id: &RoomId) {}

    fn get_room_subscribers(&self, _room_id: &RoomId) -> Vec<(UserId, String)> {
        Vec::new()
    }

    async fn get_room_subscribers_replicas_wide(
        &self,
        _room_id: &RoomId,
    ) -> synctv_realtime::Result<Vec<(UserId, String)>> {
        Ok(Vec::new())
    }

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String> {
        Ok(0)
    }

    fn spawn_shared_subscription_cleanup_task(
        &self,
        _cleanup_interval: Duration,
        _cancel_token: tokio_util::sync::CancellationToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        None
    }

    async fn shutdown(&self) {}
}

async fn test_realtime_manager_with_failing_subscription(node_id: &str) -> Arc<RealtimeManager> {
    Arc::new(
        RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(FailingRoomMessageRuntime),
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
        .expect("realtime manager with failing subscription runtime"),
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
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(RoomPlaybackState::new(*room_id))
    }

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
    dependencies: Arc<parking_lot::Mutex<Vec<synctv_core::provider::ProviderCredentialDependency>>>,
    probe: SnapshotCallProbe,
}

impl MutablePlaybackSnapshotService {
    fn new(snapshot: crate::proto::client::PlaybackSnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            dependencies: Arc::new(parking_lot::Mutex::new(Vec::new())),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: crate::proto::client::PlaybackSnapshot) {
        *self.snapshot.lock() = snapshot;
    }

    fn replace_dependencies(
        &self,
        dependencies: Vec<synctv_core::provider::ProviderCredentialDependency>,
    ) {
        *self.dependencies.lock() = dependencies;
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }
}

#[async_trait::async_trait]
impl crate::impls::playback_snapshot::PlaybackSnapshotService for MutablePlaybackSnapshotService {
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(RoomPlaybackState::new(*room_id))
    }

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

    async fn playback_credential_dependencies(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, crate::impls::ApiError>
    {
        Ok(self.dependencies.lock().clone())
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
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(RoomPlaybackState::new(*room_id))
    }

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

    fn call_count(&self) -> usize {
        self.probe.call_count()
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
    test_message_handler_for_user_with_runtime(
        sender,
        event_service,
        connection_service,
        user_id(),
        test_stream_handler_runtime(),
    )
}

fn test_message_handler_for_user(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    user_id: UserId,
) -> StreamMessageHandler {
    test_message_handler_for_user_with_runtime(
        sender,
        event_service,
        connection_service,
        user_id,
        test_stream_handler_runtime(),
    )
}

fn test_message_handler_for_user_with_runtime(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    user_id: UserId,
    runtime: StreamMessageHandlerRuntime,
) -> StreamMessageHandler {
    test_message_handler_for_user_with_runtime_and_concurrency(
        sender,
        event_service,
        connection_service,
        user_id,
        runtime,
        Arc::new(MessageConcurrencyConfig::default()),
    )
}

fn test_message_handler_for_user_with_runtime_and_concurrency(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    user_id: UserId,
    runtime: StreamMessageHandlerRuntime,
    concurrency_config: Arc<MessageConcurrencyConfig>,
) -> StreamMessageHandler {
    let pool = test_pool();
    StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id: room_id(),
            principal: RealtimePrincipal::user(user_id, "tester".to_string()),
            connection_id: None,
            room_service: Arc::clone(&test_room_service(pool.clone())),
            chat_service: test_chat_service(pool),
            event_service,
            connection_service,
            rate_limiter: Arc::new(RateLimiter::local_only("test:handler:".to_string())),
            rate_limit_config: Arc::new(RateLimitConfig::default()),
            content_filter: Arc::new(ContentFilter::new()),
            public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
            sender,
            concurrency_config,
        },
        runtime,
    )
}

fn test_guest_principal_with_permissions(permissions: RoomPermissionSet) -> RealtimePrincipal {
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
    .expect("guest principal should build")
}

fn test_guest_message_handler(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    permissions: RoomPermissionSet,
) -> StreamMessageHandler {
    test_guest_message_handler_with_runtime(
        sender,
        event_service,
        connection_service,
        permissions,
        test_stream_handler_runtime(),
    )
}

fn test_guest_message_handler_with_runtime(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    permissions: RoomPermissionSet,
    runtime: StreamMessageHandlerRuntime,
) -> StreamMessageHandler {
    let pool = test_pool();
    StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id: room_id(),
            principal: test_guest_principal_with_permissions(permissions),
            connection_id: None,
            room_service: Arc::clone(&test_room_service(pool.clone())),
            chat_service: test_chat_service(pool),
            event_service,
            connection_service,
            rate_limiter: Arc::new(RateLimiter::local_only("test:guest-handler:".to_string())),
            rate_limit_config: Arc::new(RateLimitConfig::default()),
            content_filter: Arc::new(ContentFilter::new()),
            public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
            sender,
            concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
        },
        runtime,
    )
}

fn rebuild_test_handler_with_runtime<H>(
    handler: H,
    runtime: StreamMessageHandlerRuntime,
) -> StreamMessageHandler
where
    H: std::borrow::Borrow<StreamMessageHandler>,
{
    let handler = handler.borrow();
    StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id: handler.room_id,
            principal: handler.principal.clone(),
            connection_id: Some(handler.connection_id.clone()),
            room_service: Arc::clone(&handler.room_service),
            chat_service: Arc::clone(&handler.chat_service),
            event_service: Arc::clone(&handler.event_service),
            connection_service: Arc::clone(&handler.connection_service),
            rate_limiter: Arc::clone(&handler.rate_limiter),
            rate_limit_config: Arc::clone(&handler.rate_limit_config),
            content_filter: Arc::clone(&handler.content_filter),
            public_id_codec: Arc::clone(&handler.public_id_codec),
            sender: Arc::clone(&handler.sender),
            concurrency_config: Arc::clone(&handler.concurrency_config),
        },
        runtime,
    )
}

fn test_handler_with_playback_snapshot_service<H>(
    handler: H,
    service: Arc<dyn PlaybackSnapshotService>,
) -> StreamMessageHandler
where
    H: std::borrow::Borrow<StreamMessageHandler>,
{
    rebuild_test_handler_with_runtime(handler, runtime_with_playback_snapshot_service(service))
}

fn test_handler_with_playlist_items_snapshot_service<H>(
    handler: H,
    service: Arc<dyn PlaylistItemsSnapshotService>,
) -> StreamMessageHandler
where
    H: std::borrow::Borrow<StreamMessageHandler>,
{
    rebuild_test_handler_with_runtime(
        handler,
        runtime_with_playlist_items_snapshot_service(service),
    )
}

fn test_handler_with_room_members_snapshot_service<H>(
    handler: H,
    service: Arc<dyn RoomMembersSnapshotService>,
) -> StreamMessageHandler
where
    H: std::borrow::Borrow<StreamMessageHandler>,
{
    rebuild_test_handler_with_runtime(handler, runtime_with_room_members_snapshot_service(service))
}

fn test_handler_with_room_settings_snapshot_service<H>(
    handler: H,
    service: Arc<dyn RoomSettingsSnapshotService>,
) -> StreamMessageHandler
where
    H: std::borrow::Borrow<StreamMessageHandler>,
{
    rebuild_test_handler_with_runtime(
        handler,
        runtime_with_room_settings_snapshot_service(service),
    )
}

trait StreamMessageHandlerTestRuntimeExt {
    fn with_playback_snapshot_service(
        self,
        service: Arc<dyn PlaybackSnapshotService>,
    ) -> StreamMessageHandler;
    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> StreamMessageHandler;
    fn with_room_members_snapshot_service(
        self,
        service: Arc<dyn RoomMembersSnapshotService>,
    ) -> StreamMessageHandler;
    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> StreamMessageHandler;
}

impl StreamMessageHandlerTestRuntimeExt for StreamMessageHandler {
    fn with_playback_snapshot_service(
        self,
        service: Arc<dyn PlaybackSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_playback_snapshot_service(self, service)
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_playlist_items_snapshot_service(self, service)
    }

    fn with_room_members_snapshot_service(
        self,
        service: Arc<dyn RoomMembersSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_room_members_snapshot_service(self, service)
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_room_settings_snapshot_service(self, service)
    }
}

impl StreamMessageHandlerTestRuntimeExt for &StreamMessageHandler {
    fn with_playback_snapshot_service(
        self,
        service: Arc<dyn PlaybackSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_playback_snapshot_service(self, service)
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_playlist_items_snapshot_service(self, service)
    }

    fn with_room_members_snapshot_service(
        self,
        service: Arc<dyn RoomMembersSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_room_members_snapshot_service(self, service)
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> StreamMessageHandler {
        test_handler_with_room_settings_snapshot_service(self, service)
    }
}

/// Creates a StreamMessageHandler backed by a real PostgreSQL database with a
/// registered user, created room, and accepted membership so that
/// `start()` (which calls `pre_join_after_registration`) can pass the
/// admission revalidation checks.
async fn create_start_handler_fixture(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
) -> StartTestFixture {
    create_start_handler_fixture_with_runtime(node_id, sender, test_stream_handler_runtime()).await
}

async fn create_start_handler_fixture_with_runtime(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
    runtime: StreamMessageHandlerRuntime,
) -> StartTestFixture {
    create_start_handler_fixture_with_runtime_builder(node_id, sender, |_, _| runtime).await
}

async fn create_start_handler_fixture_with_runtime_builder<F>(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
    build_runtime: F,
) -> StartTestFixture
where
    F: FnOnce(RoomId, UserId) -> StreamMessageHandlerRuntime,
{
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager(node_id).await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();

    let owner = register_test_user(
        &user_service,
        bounded_fixture_username(&format!("{node_id}_owner")),
        format!("fixture-{node_id}-owner@test.invalid"),
    )
    .await;

    let (room, _) = room_service
        .create_room(
            format!("Fixture Room {node_id}"),
            "test".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("fixture room should be created");

    let user = register_test_user(
        &user_service,
        bounded_fixture_username(&format!("{node_id}_member")),
        format!("fixture-{node_id}-member@test.invalid"),
    )
    .await;
    room_service
        .join_room(room.id, user.id, None)
        .await
        .expect("fixture member should join room");
    let runtime = build_runtime(room.id, user.id);

    let handler = StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id: room.id,
            principal: RealtimePrincipal::user(user.id, user.username.clone()),
            connection_id: None,
            room_service: Arc::clone(&room_service),
            chat_service: test_chat_service(pool.clone()),
            event_service: event_service.clone(),
            connection_service: connection_service.clone(),
            rate_limiter: Arc::new(RateLimiter::local_only(format!("test:fixture:{node_id}:"))),
            rate_limit_config: Arc::new(RateLimitConfig::default()),
            content_filter: Arc::new(ContentFilter::new()),
            public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
            sender,
            concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
        },
        runtime,
    );

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
    let initial_join_state = if handler.principal.is_guest() {
        InitialRealtimeJoinState {
            member: None,
            room_settings: None,
        }
    } else {
        InitialRealtimeJoinState {
            member: Some(RoomMember::new(
                handler.room_id,
                handler.user_id,
                synctv_core::models::RoomRole::Member,
            )),
            room_settings: Some(RoomSettings::default()),
        }
    };
    handler
        .cache_initial_realtime_join_state(initial_join_state)
        .await
        .expect("initial join state should cache before run_after_join");
    handler
        .cache_room_event_subscription()
        .await
        .expect("room subscription should cache before run_after_join");
}

async fn promote_handler_to_room_admin(
    fixture: &StartTestFixture,
) -> synctv_core::models::RoomMember {
    let member_repo = RoomMemberRepository::new(fixture.pool.clone());
    let member = member_repo
        .get(&fixture.handler.room_id, &fixture.handler.user_id)
        .await
        .expect("fixture member should load")
        .expect("fixture member should exist");
    member_repo
        .update_role(
            &fixture.handler.room_id,
            &fixture.handler.user_id,
            RoomRole::Admin,
            member.version,
        )
        .await
        .expect("fixture member should promote to admin")
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
async fn test_start_user_joined_payload_uses_room_permission_overrides() {
    let sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("start_joined_room_permission_overrides", sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let mut settings = handler
        .room_service
        .get_room_settings(&handler.room_id)
        .await
        .expect("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE,
        );
    handler
        .room_service
        .set_room_settings(&handler.room_id, &settings)
        .await
        .expect("room settings should update");

    let (_tx, cancel_token) = handler.start().await.expect("start should return");

    let messages = sender.sent_messages();
    let joined = messages
        .iter()
        .find_map(|message| match &message.message {
            Some(Message::UserJoined(joined)) => Some(joined),
            _ => None,
        })
        .expect("initial UserJoined payload should be sent");
    let member = joined.member.as_ref().expect("joined member should be set");

    assert!(
        RoomPermissionSet::default_member().has(RoomPermission::CREATE_MEDIA_RESOURCE),
        "static member defaults include CREATE_MEDIA_RESOURCE, so the payload must prove it used room overrides"
    );
    assert!(
        !RoomPermissionSet(member.permissions).has(RoomPermission::CREATE_MEDIA_RESOURCE),
        "initial UserJoined payload must apply room-level permission removals"
    );

    cancel_token.cancel();
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
        display_position: None,
        display_color: None,
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
        display_position: None,
        display_color: None,
    });

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_run_after_join_filters_own_join_broadcast() {
    let event_service = test_realtime_manager("test_run_after_join_filters_own_join").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        RecordingMessageSender::new(),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = RecordingStream::new();
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;

    event_service.broadcast(RealtimeEvent::UserJoined {
        event_id: "evt-own-join".to_string(),
        room_id: handler.room_id,
        user_id: handler.user_id,
        username: handler.username.clone(),
        permissions: RoomPermissionSet::default_member(),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet::default(),
        removed_permissions: RoomPermissionSet::default(),
        admin_added_permissions: RoomPermissionSet::default(),
        admin_removed_permissions: RoomPermissionSet::default(),
        joined_at: now(),
        timestamp: now(),
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let public_user_id = handler
        .public_id_codec
        .encode_user_id(handler.user_id)
        .expect("handler user id should encode");
    let own_join_count = stream_state
        .sent_messages()
        .iter()
        .filter(|message| server_message_is_user_joined_for(message, &public_user_id))
        .count();
    assert_eq!(
        own_join_count, 1,
        "the connection should keep the initial UserJoined payload and skip its own room broadcast"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_run_after_join_records_heartbeat_activity() {
    let event_service = test_realtime_manager("test_run_after_join_records_heartbeat").await;
    let connection_service = test_connection_manager();
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let heartbeat = ClientMessage {
        message: Some(crate::proto::client::client_message::Message::Heartbeat(
            crate::proto::client::HeartbeatMessage { timestamp: 42 },
        )),
    };
    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![heartbeat]);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let heartbeat_ack_sent = message_sender
                .sent_messages()
                .iter()
                .any(|msg| matches!(msg.message, Some(Message::HeartbeatAck(_))));
            let message_count = connection_service
                .get_connection(&handler.connection_id)
                .map_or(0, |connection| connection.message_count);
            if heartbeat_ack_sent && message_count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("heartbeat should refresh activity and receive an ack");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_cached_room_subscription_delivers_pre_run_chat_event_after_explicit_observe() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "test_pre_join_caches_room_subscription",
        message_sender.clone(),
    )
    .await;
    let handler = &fixture.handler;
    let event_service = &fixture.event_service;
    let connection_service = &fixture.connection_service;
    prepare_handler_for_run_after_join(handler, connection_service).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_chat_events_message("chat-events")),
        })
        .await
        .expect("chat observe should register");
    assert!(
        handler
            .resource_observer
            .has_chat_events_observation()
            .await
    );
    assert!(message_sender
        .sent_messages()
        .iter()
        .any(|msg| matches!(msg.message, Some(Message::ResourceObserved(_)))));

    event_service.broadcast(RealtimeEvent::ChatMessageEvent {
        event_id: "evt-prejoin-window".to_string(),
        room_id: handler.room_id,
        actor_user_id: UserId::expect_positive(113_001),
        event: chat_event_with_content(
            handler.room_id,
            UserId::expect_positive(113_001),
            "evt-prejoin-window",
            "arrived-before-run-after-join",
        ),
        timestamp: now(),
    });

    let (mut stream, stream_state) = RecordingStream::new();
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|msg| {
                server_message_contains_chat_event_content(msg, "arrived-before-run-after-join")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("chat event should be delivered through chat_events observation");

    let messages = stream_state.sent_messages();
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg.message, Some(Message::UserJoined(_)))),
        "run_after_join should still send the initial UserJoined payload"
    );
    let messages = message_sender.sent_messages();
    assert!(
        messages
            .iter()
            .any(|msg| server_message_contains_chat_event_content(
                msg,
                "arrived-before-run-after-join"
            )),
        "room event broadcast after caching the subscription but before run_after_join must not be lost"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_observe_chat_events_replays_single_event_after_sequence() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager("test_chat_event_replay_after_sequence").await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = register_test_user(
        &user_service,
        "chat-replay-owner",
        "chat-replay-owner@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Chat Replay Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");

    let first = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-1".to_string()),
            content: "first replay".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        })
        .await
        .expect("first message should be stored");
    chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-2".to_string()),
            content: "second replay".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        })
        .await
        .expect("second message should be stored");
    chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-3".to_string()),
            content: "third replay".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        })
        .await
        .expect("third message should be stored");

    let message_sender = RecordingMessageSender::new();
    let handler = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(owner.id, owner.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service,
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only("test:chat-replay:".to_string())),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: message_sender.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_chat_events_message_with_sequence(
                "chat-replay",
                Some(first.sequence),
            )),
        })
        .await
        .expect("chat observe should replay events after sequence");

    let replayed = message_sender
        .sent_messages()
        .iter()
        .filter_map(|message| match resource_changed_payload(message) {
            Some(crate::proto::client::resource_changed::Payload::ChatEvent(event)) => event
                .message
                .as_ref()
                .map(|message| message.content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replayed, vec!["second replay", "third replay"]);

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[tokio::test]
async fn test_observe_chat_events_replays_events_after_sequence() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let event_service = test_realtime_manager("test_chat_event_replay_after_sequence").await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = register_test_user(
        &user_service,
        "chat-replay-seq-owner",
        "chat-replay-seq-owner@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Chat Replay Sequence Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");

    let first = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-seq-1".to_string()),
            content: "first sequence replay".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        })
        .await
        .expect("first message should be stored");
    let second = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-seq-2".to_string()),
            content: "second sequence replay".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        })
        .await
        .expect("second message should be stored");

    let message_sender = RecordingMessageSender::new();
    let handler = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(owner.id, owner.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service,
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:chat-replay-sequence:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: message_sender.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_chat_events_message_with_sequence(
                "chat-replay-sequence",
                Some(first.sequence),
            )),
        })
        .await
        .expect("chat observe should replay events after sequence");

    let replayed = message_sender
        .sent_messages()
        .iter()
        .filter_map(|message| match resource_changed_payload(message) {
            Some(crate::proto::client::resource_changed::Payload::ChatEvent(event)) => event
                .message
                .as_ref()
                .map(|message| (message.content.clone(), event.sequence)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replayed,
        vec![("second sequence replay".to_string(), second.sequence)]
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
}

#[tokio::test]
async fn test_run_after_join_filters_chat_events_until_explicit_observe() {
    let event_service = test_realtime_manager("test_run_after_join_filters_chat_events").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        RecordingMessageSender::new(),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = RecordingStream::new();
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;

    event_service.broadcast(RealtimeEvent::ChatMessageEvent {
        event_id: "evt-filtered-chat".to_string(),
        room_id: handler.room_id,
        actor_user_id: UserId::expect_positive(113_002),
        event: chat_event_with_content(
            handler.room_id,
            UserId::expect_positive(113_002),
            "evt-filtered-chat",
            "filtered-before-observe",
        ),
        timestamp: now(),
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let messages = stream_state.sent_messages();
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg.message, Some(Message::UserJoined(_)))),
        "run_after_join should emit the initial UserJoined payload"
    );
    assert!(
        messages
            .iter()
            .all(|msg| !server_message_contains_chat_event_content(msg, "filtered-before-observe")),
        "chat events must wait for an explicit chat_events observation"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_observe_chat_events_requires_view_chat_history_permission_for_member() {
    let sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_chat_events_permission", sender.clone()).await;
    let mut settings = fixture
        .handler
        .room_service
        .get_room_settings(&fixture.handler.room_id)
        .await
        .expect("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        );
    fixture
        .handler
        .room_service
        .set_room_settings(&fixture.handler.room_id, &settings)
        .await
        .expect("room settings should update");

    let error = fixture
        .handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_chat_events_message("chat-events")),
        })
        .await
        .expect_err("chat events observation should require VIEW_CHAT_HISTORY");

    assert!(
        error.contains("permission") || error.contains("Permission"),
        "permission denial should be surfaced, got: {error}"
    );
    assert!(
        !fixture
            .handler
            .resource_observer
            .has_chat_events_observation()
            .await
    );
    assert!(sender.sent_messages().is_empty());

    fixture.shutdown().await;
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
async fn test_observe_playback_state_without_cursor_sends_current_state_immediately() {
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
        message: Some(observe_playback_state_message("playback-state")),
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
async fn test_observe_playback_snapshot_without_cursor_sends_snapshot_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_playback_snapshot_initial",
        message_sender.clone(),
        |room_id, _| {
            runtime_with_playback_snapshot_service(Arc::new(FakePlaybackSnapshotService {
                snapshot: crate::proto::client::PlaybackSnapshot {
                    media_id: public_media_id(),
                    playlist_id: String::new(),
                    room_id: public_id_codec().encode_room_id(room_id).unwrap(),
                    name: "test media".to_string(),
                    playlist_position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: std::collections::HashMap::new(),
                    version: "snapshot-v1".to_string(),
                    expires_at: Some(12345),
                },
            }))
        },
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
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
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
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_snapshot_with_replay_cursor_returns_event_cursor() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_pb_snapshot_cursor",
        message_sender.clone(),
        |room_id, _| {
            runtime_with_playback_snapshot_service(Arc::new(FakePlaybackSnapshotService {
                snapshot: crate::proto::client::PlaybackSnapshot {
                    media_id: public_media_id(),
                    playlist_id: String::new(),
                    room_id: public_id_codec().encode_room_id(room_id).unwrap(),
                    name: "test media".to_string(),
                    playlist_position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: std::collections::HashMap::new(),
                    version: "snapshot-v1".to_string(),
                    expires_at: Some(12345),
                },
            }))
        },
    )
    .await;
    let handler = &fixture.handler;
    let request = crate::proto::client::ObserveResource {
        observe_id: "playback-snapshot".to_string(),
        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: Some(
            crate::proto::client::observe_resource::Resource::PlaybackSnapshot(
                crate::proto::client::ObservePlaybackSnapshot {
                    media_id: None,
                    playlist_id: None,
                    target: Vec::new(),
                    playback_client_profile: None,
                    after_event_sequence: Some(42),
                },
            ),
        ),
    };

    handler
        .resource_observer
        .handle_observe_resource(&request)
        .await
        .expect("playback snapshot observe should register");

    let messages = message_sender.sent_messages();
    let observed = messages
        .iter()
        .find_map(|message| match &message.message {
            Some(Message::ResourceObserved(observed)) => Some(observed),
            _ => None,
        })
        .expect("observe should send ResourceObserved");
    assert_eq!(
        observed.event_cursor.as_ref().map(|cursor| cursor.sequence),
        Some(42)
    );

    let changed = messages
        .iter()
        .find_map(|message| match &message.message {
            Some(Message::ResourceChanged(changed))
                if changed.observe_id == "playback-snapshot" =>
            {
                Some(changed)
            }
            _ => None,
        })
        .expect("observe should send initial playback snapshot");
    assert_eq!(
        changed.event_cursor.as_ref().map(|cursor| cursor.sequence),
        Some(42)
    );
    assert!(resource_playback_snapshot(&ServerMessage {
        message: Some(Message::ResourceChanged(changed.clone()))
    })
    .is_some_and(|snapshot| snapshot.version == "snapshot-v1"));
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_snapshot_replays_room_resource_events_after_cursor() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_snapshot_replay", message_sender.clone()).await;
    let now = chrono::Utc::now();
    let state = RoomPlaybackState {
        room_id: fixture.handler.room_id,
        playing_media_id: Some(media_id()),
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 12.5,
        speed: 1.0,
        is_playing: true,
        updated_at: now,
        version: 2,
    };
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "playback-snapshot-replay-event".to_string(),
        room_id: fixture.handler.room_id,
        user_id: fixture.handler.user_id,
        username: fixture.handler.username.clone(),
        state,
        timestamp: now,
    };
    let repository = synctv_core::repository::RoomResourceEventRepository::new(
        fixture.handler.room_service.pool().clone(),
    );
    repository
        .insert(&synctv_core::repository::NewRoomResourceEvent {
            event_id: event.event_id().to_string(),
            scope_type: synctv_core::repository::RoomResourceEventScope::Room,
            room_id: Some(fixture.handler.room_id.as_i64()),
            user_id: None,
            aggregate_type: "room_playback_state".to_string(),
            aggregate_id: fixture.handler.room_id.to_string(),
            resource_type: "playback_state".to_string(),
            resource_id: fixture.handler.room_id.to_string(),
            event_type: "playback_state_changed".to_string(),
            event_version: 1,
            aggregate_version: Some(2),
            actor_user_id: Some(fixture.handler.user_id.as_i64()),
            payload: Some(serde_json::to_value(&event).expect("event should serialize")),
            summary: serde_json::json!({
                "position": 12.5,
                "is_playing": true,
            }),
            occurred_at: now,
        })
        .await
        .expect("room resource event should insert");

    let handler = fixture
        .handler
        .clone()
        .with_playback_snapshot_service(Arc::new(FakePlaybackSnapshotService {
            snapshot: crate::proto::client::PlaybackSnapshot {
                media_id: public_media_id(),
                playlist_id: String::new(),
                room_id: public_id_codec()
                    .encode_room_id(fixture.handler.room_id)
                    .unwrap(),
                name: "test media".to_string(),
                playlist_position: 0.0,
                playback_infos: std::collections::HashMap::new(),
                default_mode: String::new(),
                metadata: std::collections::HashMap::new(),
                version: "snapshot-v2".to_string(),
                expires_at: Some(12345),
            },
        }));
    let request = crate::proto::client::ObserveResource {
        observe_id: "playback-snapshot".to_string(),
        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: Some(
            crate::proto::client::observe_resource::Resource::PlaybackSnapshot(
                crate::proto::client::ObservePlaybackSnapshot {
                    media_id: None,
                    playlist_id: None,
                    target: Vec::new(),
                    playback_client_profile: None,
                    after_event_sequence: Some(0),
                },
            ),
        ),
    };

    handler
        .resource_observer
        .handle_observe_resource(&request)
        .await
        .expect("playback snapshot observe should register");
    handler
        .resource_observer
        .replay_room_resource_events_after(&request)
        .await
        .expect("playback snapshot replay should succeed");

    let replayed = message_sender
        .sent_messages()
        .into_iter()
        .filter_map(|message| match message.message {
            Some(Message::ResourceChanged(changed))
                if changed.observe_id == "playback-snapshot" =>
            {
                Some(changed)
            }
            _ => None,
        })
        .find(|changed| {
            changed
                .event_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.sequence == 1)
        })
        .expect("playback snapshot replay should emit the durable room resource cursor");
    assert!(matches!(
        replayed.payload,
        Some(crate::proto::client::resource_changed::Payload::PlaybackSnapshot(_))
    ));

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_replay_room_resource_event_without_payload_advances_cursor() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_resource_replay_no_payload", message_sender.clone())
            .await;
    let repository = synctv_core::repository::RoomResourceEventRepository::new(
        fixture.handler.room_service.pool().clone(),
    );
    let now = chrono::Utc::now();
    repository
        .insert(&synctv_core::repository::NewRoomResourceEvent {
            event_id: "playlist-items-audit-only".to_string(),
            scope_type: synctv_core::repository::RoomResourceEventScope::Room,
            room_id: Some(fixture.handler.room_id.as_i64()),
            user_id: None,
            aggregate_type: "playlist".to_string(),
            aggregate_id: fixture.handler.room_id.to_string(),
            resource_type: "playlist_items".to_string(),
            resource_id: fixture.handler.room_id.to_string(),
            event_type: "playlist_items_changed".to_string(),
            event_version: 1,
            aggregate_version: Some(1),
            actor_user_id: Some(fixture.handler.user_id.as_i64()),
            payload: None,
            summary: serde_json::json!({
                "changed": true,
                "reason": "audit-only test event",
            }),
            occurred_at: now,
        })
        .await
        .expect("audit-only room resource event should insert");

    let handler = fixture
        .handler
        .clone()
        .with_playlist_items_snapshot_service(Arc::new(FakePlaylistItemsSnapshotService {
            snapshot: empty_playlist_items_response("playlist-items-v1"),
        }));
    let request = observe_playlist_items_resource_with_sequence(
        "playlist-items",
        crate::proto::client::ListPlaylistItemsRequest::default(),
        Some(0),
    );

    handler
        .resource_observer
        .handle_observe_resource(&request)
        .await
        .expect("playlist items observe should register");
    handler
        .resource_observer
        .replay_room_resource_events_after(&request)
        .await
        .expect("playlist items replay should succeed");

    let replayed = message_sender
        .sent_messages()
        .into_iter()
        .filter_map(|message| match message.message {
            Some(Message::ResourceChanged(changed)) if changed.observe_id == "playlist-items" => {
                Some(changed)
            }
            _ => None,
        })
        .find(|changed| {
            changed
                .event_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.sequence == 1)
        })
        .expect("audit-only event should produce a cursor-advancing change");
    assert!(matches!(
        replayed.payload,
        Some(crate::proto::client::resource_changed::Payload::ChangedOnly(_))
    ));

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_state_sends_current_snapshot() {
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
        message: Some(observe_playback_state_message("playback-state")),
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
            .any(|message| { resource_playback_state(message).is_some() }),
        "observe should send the current playback state snapshot"
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
async fn test_observe_playback_snapshot_with_matching_source_sends_current_snapshot() {
    let message_sender = RecordingMessageSender::new();
    let snapshot_service_slot = Arc::new(std::sync::Mutex::new(None));
    let snapshot_service_out = Arc::clone(&snapshot_service_slot);
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_pb_snap_same_src",
        message_sender.clone(),
        move |room_id, _| {
            let snapshot_service =
                MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
                    media_id: String::new(),
                    playlist_id: String::new(),
                    room_id: public_id_codec().encode_room_id(room_id).unwrap(),
                    name: "test media".to_string(),
                    playlist_position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: std::collections::HashMap::new(),
                    version: "snapshot-v1".to_string(),
                    expires_at: Some(chrono::Utc::now().timestamp() + 3600),
                });
            *snapshot_service_out
                .lock()
                .expect("snapshot slot should lock") = Some(snapshot_service.clone());
            runtime_with_playback_snapshot_service(snapshot_service)
        },
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;
    let snapshot_service = snapshot_service_slot
        .lock()
        .expect("snapshot slot should lock")
        .clone()
        .expect("snapshot service should be captured");

    prepare_handler_for_run_after_join(handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
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
            .any(|message| resource_playback_snapshot(message)
                .is_some_and(|snapshot| snapshot.version == "snapshot-v1")),
        "observe should send the current playback snapshot: {sent_messages:?}"
    );
    let stream_messages = stream_state.sent_messages();
    assert!(
        stream_messages
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
                    playlist_position: 0.0,
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
            public_id_codec()
                .encode_media_id(MediaId::expect_positive(999))
                .expect("stale media id should encode"),
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
            playlist_position: 0.0,
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
            .any(|message| resource_playback_snapshot(message)
                .is_some_and(|snapshot| snapshot.version == "1")),
        "observe should send the current playback snapshot"
    );

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "test media".to_string(),
        playlist_position: 0.0,
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
            current_progress_id: None,
            position: 12.0,
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
async fn test_provider_credential_change_refreshes_dependent_playback_snapshot() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture("pb_snap_cred", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;
    promote_handler_to_room_admin(&fixture).await;
    let media = synctv_core::repository::MediaRepository::new(fixture.pool.clone())
        .create(&synctv_core::models::Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: handler.room_id,
            creator_id: Some(handler.user_id),
            name: "provider credential dependent media".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({
                "url": "https://example.com/provider-credential-dependent.mp4"
            }),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .expect("media should be created for provider credential observe test");

    handler
        .room_service
        .update_playback(
            handler.room_id,
            handler.user_id,
            |state| {
                state.playing_media_id = Some(media.id);
            },
            RoomPermission::PLAY_CONTROL,
        )
        .await
        .expect("playback state should be set");

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: public_id_codec().encode_media_id(media.id).unwrap(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "credential-backed media".to_string(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "snapshot-v1".to_string(),
            expires_at: Some(4_102_444_800),
        });
    snapshot_service.replace_dependencies(vec![
        synctv_core::provider::ProviderCredentialDependency::new(
            "bilibili",
            handler.user_id.to_string(),
            "bilibili",
        ),
    ]);
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: public_id_codec().encode_media_id(media.id).unwrap(),
        playlist_id: String::new(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "credential-backed media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: std::collections::HashMap::new(),
        version: "snapshot-v2".to_string(),
        expires_at: Some(4_102_444_801),
    });

    event_service.broadcast(RealtimeEvent::ProviderCredentialChanged {
        event_id: "evt-provider-credential-dependent".to_string(),
        user_id: handler.user_id,
        provider: "bilibili".to_string(),
        server_id: "bilibili".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_snapshot(message)
                    .is_some_and(|snapshot| snapshot.version == "snapshot-v2")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dependent provider credential change should refresh playback snapshot");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_provider_credential_change_does_not_refresh_unrelated_playback_snapshot() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture("pb_snap_cred_unrel", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;
    promote_handler_to_room_admin(&fixture).await;
    let media = synctv_core::repository::MediaRepository::new(fixture.pool.clone())
        .create(&synctv_core::models::Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: handler.room_id,
            creator_id: Some(handler.user_id),
            name: "provider credential unrelated media".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({
                "url": "https://example.com/provider-credential-unrelated.mp4"
            }),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .expect("media should be created for provider credential observe test");

    handler
        .room_service
        .update_playback(
            handler.room_id,
            handler.user_id,
            |state| {
                state.playing_media_id = Some(media.id);
            },
            RoomPermission::PLAY_CONTROL,
        )
        .await
        .expect("playback state should be set");

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: public_id_codec().encode_media_id(media.id).unwrap(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "credential-backed media".to_string(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: "snapshot-v1".to_string(),
            expires_at: Some(4_102_444_800),
        });
    snapshot_service.replace_dependencies(vec![
        synctv_core::provider::ProviderCredentialDependency::new(
            "bilibili",
            handler.user_id.to_string(),
            "bilibili",
        ),
    ]);
    let handler = handler
        .clone()
        .with_playback_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_snapshot_message(
            "playback-snapshot",
            String::new(),
            String::new(),
            Vec::new(),
            None,
        )),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });
    snapshot_service.wait_for_calls(1).await;

    event_service.broadcast(RealtimeEvent::ProviderCredentialChanged {
        event_id: "evt-provider-credential-unrelated".to_string(),
        user_id: UserId::expect_positive(handler.user_id.get() + 1),
        provider: "bilibili".to_string(),
        server_id: "bilibili".to_string(),
        timestamp: now(),
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(
        snapshot_service.probe.call_count(),
        1,
        "unrelated credential changes must not reload observed playback snapshots"
    );

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
    promote_handler_to_room_admin(&fixture).await;

    let media = synctv_core::repository::MediaRepository::new(fixture.pool.clone())
        .create(&synctv_core::models::Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: handler.room_id,
            creator_id: Some(handler.user_id),
            name: "observe-playback-media-update".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({
                "url": "https://example.com/observe-playback-media-update.mp4"
            }),
            provider_instance_name: None,
            cover_file_reference_id: None,
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
                state.position = 0.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            RoomPermission::PLAY_CONTROL,
        )
        .await
        .expect("playback should point at created media");

    let snapshot_service =
        MutablePlaybackSnapshotService::new(crate::proto::client::PlaybackSnapshot {
            media_id: public_id_codec().encode_media_id(media.id).unwrap(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: "observe-playback-media-update".to_string(),
            playlist_position: 0.0,
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
            .any(|message| resource_playback_snapshot(message)
                .is_some_and(|snapshot| snapshot.version == media.version.to_string())),
        "observe should send the current playback snapshot"
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
        playlist_position: 0.0,
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
            creator_id: Some(handler.user_id),
            name: "observe-playback-playlist-update".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
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
    playback_state.position = 0.0;
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
            playlist_position: 0.0,
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
            .any(|message| resource_playback_snapshot(message)
                .is_some_and(|snapshot| snapshot.version == playlist.version.to_string())),
        "observe should send the current playback snapshot"
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
                description: None,
            },
        )
        .await
        .expect("editing current playback playlist should succeed");

    snapshot_service.replace(crate::proto::client::PlaybackSnapshot {
        media_id: String::new(),
        playlist_id: public_id_codec().encode_playlist_id(playlist.id).unwrap(),
        room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
        name: "observe-playback-playlist-update-v2".to_string(),
        playlist_position: 0.0,
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
    promote_handler_to_room_admin(&fixture).await;

    let snapshot_service = SequencedPlaybackSnapshotService::new([
        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            name: String::new(),
            playlist_position: 0.0,
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
            playlist_position: 0.0,
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
            .any(|message| resource_playback_snapshot(message)
                .is_some_and(|snapshot| snapshot.version == "1")),
        "observe should send the current playback snapshot"
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
                state.position = 12.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            RoomPermission::PLAY_CONTROL,
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
            playlist_position: 0.0,
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
            current_progress_id: None,
            position: 5.0,
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
            playlist_position: 0.0,
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
            playlist_position: 0.0,
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
async fn test_observe_room_settings_without_cursor_sends_current_settings_immediately() {
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
        message: Some(observe_room_settings_message("room-settings")),
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
async fn test_observe_playlist_items_without_cursor_sends_snapshot_immediately() {
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
                    description: String::new(),
                    metadata: Vec::new(),
                    position: 1.0,
                    added_at: 1,
                    creator_id: handler.user_id.to_string(),
                    provider_instance_name: String::new(),
                    source_config: Vec::new(),
                    availability: crate::proto::client::ResourceAvailability::Available as i32,
                    version: 3,
                    cover: None,
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
    let handler = test_message_handler_for_user_with_runtime(
        message_sender.clone(),
        test_realtime_manager("playlist_items_coalesce").await,
        test_connection_manager(),
        user_id(),
        runtime_with_playlist_items_snapshot_service(snapshot_service.clone()),
    );
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
                request.clone(),
            )),
        })
        .await
        .expect("first observe should register");
    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message("playlist-items-b", request)),
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
    let handler = test_message_handler_for_user_with_runtime(
        sender.clone(),
        event_service,
        connection_service,
        user_id(),
        runtime_with_room_settings_snapshot_service(snapshot_service.clone()),
    );
    let max_observations = super::resource_observer::MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION;

    for index in 0..max_observations {
        let observe_id = format!("room-settings-{index}");
        handler
            .handle_client_message(&ClientMessage {
                message: Some(observe_room_settings_message(&observe_id)),
            })
            .await
            .expect("observe should register while under the per-connection limit");
    }
    let snapshot_calls_before_over_limit = snapshot_service.call_count();

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message("room-settings-over-limit")),
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
async fn test_observe_playlist_items_requires_inner_request() {
    let sender = RecordingMessageSender::new();
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
    let handler = test_message_handler_for_user_with_runtime(
        sender.clone(),
        test_realtime_manager("playlist_items_missing_request").await,
        test_connection_manager(),
        user_id(),
        runtime_with_playlist_items_snapshot_service(snapshot_service.clone()),
    );

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                crate::proto::client::client_message::Message::ObserveResource(
                    crate::proto::client::ObserveResource {
                        observe_id: "playlist-items-missing-request".to_string(),
                        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot
                            as i32,
                        resource: Some(
                            crate::proto::client::observe_resource::Resource::PlaylistItems(
                                crate::proto::client::ObservePlaylistItems {
                                    request: None,
                                    after_event_sequence: None,
                                },
                            ),
                        ),
                    },
                ),
            ),
        })
        .await
        .expect("invalid observe should send ResourceObserveError without closing");

    assert_eq!(snapshot_service.call_count(), 0);
    assert!(sender
        .sent_messages()
        .iter()
        .filter_map(resource_observe_error)
        .any(|error| error
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("playlist_items request"))));
}

#[tokio::test]
async fn test_observe_room_members_requires_inner_request() {
    let sender = RecordingMessageSender::new();
    let snapshot_service =
        MutableRoomMembersSnapshotService::new(crate::proto::client::GetRoomMembersResponse {
            members: Vec::new(),
            total: 0,
            version: "members-v1".to_string(),
        });
    let handler = test_message_handler_for_user_with_runtime(
        sender.clone(),
        test_realtime_manager("room_members_missing_request").await,
        test_connection_manager(),
        user_id(),
        runtime_with_room_members_snapshot_service(snapshot_service.clone()),
    );

    handler
        .resource_observer
        .handle_observe_resource(&crate::proto::client::ObserveResource {
            observe_id: "room-members-missing-request".to_string(),
            delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                crate::proto::client::observe_resource::Resource::RoomMembers(
                    crate::proto::client::ObserveRoomMembers {
                        request: None,
                        after_event_sequence: None,
                    },
                ),
            ),
        })
        .await
        .expect("invalid observe should send ResourceObserveError without closing");

    assert_eq!(snapshot_service.call_count(), 0);
    assert!(sender
        .sent_messages()
        .iter()
        .filter_map(resource_observe_error)
        .any(|error| error
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("room_members request"))));
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
    let handler =
        test_handler_with_playlist_items_snapshot_service(handler, snapshot_service.clone());
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
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
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
    let message_sender = FailingMessageSender::fail_after(2);
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("resource_changed_send_failure").await,
        test_connection_manager(),
    );
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));
    let handler =
        test_handler_with_playlist_items_snapshot_service(handler, snapshot_service.clone());
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
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
        .await
        .expect("observe should register with initial snapshot sent");
    assert_eq!(message_sender.send_calls(), 2);

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
    let failing_sender = FailingMessageSender::fail_after(2);
    let healthy_sender = RecordingMessageSender::new();
    let event_service = test_realtime_manager("other_subscriber_send_failure").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));

    let failing_handler = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "slow-client".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: Arc::clone(&chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:other-send-fail:a:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: failing_sender.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let healthy_handler = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(
            UserId::expect_positive(222),
            "healthy-client".to_string(),
        ),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service,
        event_service,
        connection_service,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:other-send-fail:b:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: healthy_sender.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
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
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-failing",
            request.clone(),
            Some(0),
        ))
        .await
        .expect("failing observer should register before its queue fails");
    healthy_handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-healthy",
            request,
            Some(0),
        ))
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
        .refresh_for_room_event_with_cursor(
            &event,
            Some(healthy_handler.connection_id()),
            event_cursor(101),
        )
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
    let handler =
        test_handler_with_playlist_items_snapshot_service(handler, snapshot_service.clone());
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
            message: Some(observe_playlist_items_message("playlist-items", request)),
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
    let handler =
        test_handler_with_playlist_items_snapshot_service(handler, snapshot_service.clone());
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
            message: Some(observe_playlist_items_message("playlist-items", request)),
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
            request.clone(),
        )),
    };
    let message_b = ClientMessage {
        message: Some(observe_playlist_items_message("playlist-items-b", request)),
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
    assert!(sender_a.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v1")
    }));
    assert!(sender_b.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v1")
    }));
}

#[tokio::test]
async fn test_room_event_refresh_without_durable_cursor_refreshes_best_effort() {
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));
    let sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        sender.clone(),
        test_realtime_manager("room_event_without_cursor").await,
        test_connection_manager(),
    )
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: "missing-durable-cursor".to_string(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: crate::proto::client::MediaListSortBy::Position as i32,
        sort_direction: crate::proto::client::SortDirection::Asc as i32,
        availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
        .await
        .expect("observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    let event = RealtimeEvent::MediaAdded {
        event_id: "room-event-without-durable-cursor".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "tester".to_string(),
        media_id: media_id(),
        media_title: "Updated media".to_string(),
        timestamp: now(),
    };

    handler
        .resource_observer
        .room_hub
        .refresh_for_room_event(&event, Some(handler.connection_id()))
        .await
        .expect("missing durable cursor should refresh without failing the stream");

    assert_eq!(
        snapshot_service.call_count(),
        2,
        "live room resource refresh should still update subscribers without a durable cursor"
    );
    assert!(sender.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));
}

#[tokio::test]
async fn test_media_resource_hub_coalesces_event_refresh_and_fans_out() {
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
    let event_service = test_realtime_manager("media_resource_hub_event_refresh").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-a".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: Arc::clone(&chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc::new(RateLimiter::local_only("test:hub:a:".to_string())),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: sender_a.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let handler_b = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-b".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service,
        event_service,
        connection_service,
        rate_limiter: Arc::new(RateLimiter::local_only("test:hub:b:".to_string())),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: sender_b.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
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
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-a",
            request.clone(),
            Some(0),
        ))
        .await
        .expect("first observe should register");
    handler_b
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-b",
            request,
            Some(0),
        ))
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
            .refresh_for_room_event_with_cursor(
                &event,
                Some(handler_a.connection_id()),
                event_cursor(201),
            ),
        handler_b
            .resource_observer
            .room_hub
            .refresh_for_room_event_with_cursor(
                &event,
                Some(handler_b.connection_id()),
                event_cursor(201),
            )
    );
    refresh_a.expect("first event refresh should succeed");
    refresh_b.expect("deduped event refresh should succeed");
    snapshot_service.wait_for_calls(2).await;

    assert_eq!(
        snapshot_service.call_count(),
        2,
        "room hub should use one initial load and one shared event refresh load"
    );
    assert!(sender_a.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));
    assert!(sender_b.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));
}

#[tokio::test]
async fn test_media_resource_hub_refresh_dedupe_tracks_subscription_generation() {
    let snapshot_service =
        BlockingPlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"), 2);
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("media_resource_hub_generation_dedupe").await;
    let connection_service = test_connection_manager();
    let pool = test_pool();
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-a".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: Arc::clone(&chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:hub:generation:a:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: sender_a.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let handler_b = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-b".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service,
        event_service,
        connection_service,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:hub:generation:b:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: sender_b.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
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
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-a",
            request_a,
            Some(0),
        ))
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
            .refresh_for_room_event_with_cursor(
                &refresh_event_a,
                Some(&connection_id_a),
                event_cursor(301),
            )
            .await
    });
    snapshot_service.wait_for_calls(2).await;

    handler_b
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-b",
            request_b,
            Some(0),
        ))
        .await
        .expect("second observe should register while first refresh is in flight");
    snapshot_service.wait_for_calls(3).await;
    assert!(!sender_b.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    snapshot_service.release();
    refresh_a
        .await
        .expect("first refresh task should join")
        .expect("first refresh should finish");

    handler_b
        .resource_observer
        .room_hub
        .refresh_for_room_event_with_cursor(
            &event,
            Some(handler_b.connection_id()),
            event_cursor(302),
        )
        .await
        .expect("second refresh should not be suppressed by the stale completed refresh");

    assert_eq!(
        snapshot_service.call_count(),
        5,
        "subscriber generation changes should force a new refresh batch for the same event key"
    );
    assert!(sender_b.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));
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
        message: Some(observe_room_settings_message("room-settings-a")),
    };
    let message_b = ClientMessage {
        message: Some(observe_room_settings_message("room-settings-b")),
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
    assert!(sender_a.sent_messages().iter().any(|message| {
        resource_room_settings(message).is_some_and(|settings| settings.version == 7)
    }));
    assert!(sender_b.sent_messages().iter().any(|message| {
        resource_room_settings(message).is_some_and(|settings| settings.version == 7)
    }));
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
    let handler =
        test_handler_with_room_settings_snapshot_service(handler, snapshot_service.clone());

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message("room-settings-a")),
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
            message: Some(observe_room_settings_message("room-settings-b")),
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
async fn test_observe_playlist_items_sends_current_snapshot() {
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
            .any(|message| resource_playlist_items(message)
                .is_some_and(|items| items.version == "items-v1")),
        "observe should send the current playlist items snapshot"
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
            .any(|message| resource_playlist_items(message)
                .is_some_and(|items| items.version == "items-v1")),
        "observe should send the current playlist items snapshot"
    );

    snapshot_service.replace(crate::proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: vec![crate::proto::client::Media {
            id: "media_test_2".to_string(),
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            source_provider: "direct_url".to_string(),
            name: "next media".to_string(),
            description: String::new(),
            metadata: Vec::new(),
            position: 2.0,
            added_at: 2,
            creator_id: handler.user_id.to_string(),
            provider_instance_name: String::new(),
            source_config: Vec::new(),
            availability: crate::proto::client::ResourceAvailability::Available as i32,
            version: 4,
            cover: None,
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
async fn test_observe_room_members_without_cursor_sends_snapshot_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_room_members_initial", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler = test_handler_with_room_members_snapshot_service(
        handler.clone(),
        Arc::new(FakeRoomMembersSnapshotService {
            snapshot: crate::proto::client::GetRoomMembersResponse {
                members: vec![synctv_proto::common::RoomMember {
                    room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
                    user_id: handler.user_id.to_string(),
                    username: handler.username.clone(),
                    role: synctv_proto::common::RoomMemberRole::Creator as i32,
                    permissions: RoomPermissionSet::all().0,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    joined_at: 1,
                    is_online: true,
                }],
                total: 1,
                version: "members-v1".to_string(),
            },
        }),
    );

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_members_message(
            "room-members",
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
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
async fn test_observe_room_members_sends_current_snapshot() {
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
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
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
            .any(|message| resource_room_members(message)
                .is_some_and(|members| members.version == "members-v1")),
        "observe should send the current room members snapshot"
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
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
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
            .any(|message| resource_room_members(message)
                .is_some_and(|members| members.version == "members-v1")),
        "observe should send the current room members snapshot"
    );

    snapshot_service.replace(crate::proto::client::GetRoomMembersResponse {
        members: vec![synctv_proto::common::RoomMember {
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            user_id: "member002abc".to_string(),
            username: "member_two".to_string(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            permissions: RoomAdminPermissionBits::VIEW_MEMBER_LIST,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: 2,
            is_online: false,
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
        new_permissions: RoomPermissionSet(RoomAdminPermissionBits::VIEW_MEMBER_LIST),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
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
            crate::proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
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
            .any(|message| resource_room_members(message)
                .is_some_and(|members| members.version == "members-v1")),
        "observe should send the current room members snapshot"
    );

    snapshot_service.replace(crate::proto::client::GetRoomMembersResponse {
        members: vec![synctv_proto::common::RoomMember {
            room_id: public_id_codec().encode_room_id(handler.room_id).unwrap(),
            user_id: handler.user_id.to_string(),
            username: handler.username.clone(),
            role: synctv_proto::common::RoomMemberRole::Creator as i32,
            permissions: RoomPermissionSet::all().0 | RoomAdminPermissionBits::PLAY_CONTROL,
            added_permissions: RoomAdminPermissionBits::PLAY_CONTROL,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: 1,
            is_online: true,
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
            "admin_added_permissions": RoomAdminPermissionBits::PLAY_CONTROL
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
async fn test_observe_room_settings_sends_current_snapshot() {
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
        message: Some(observe_room_settings_message("room-settings")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_room_settings(message)
                .is_some_and(|settings| settings.version == 7)),
        "observe should send the current room settings snapshot"
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
        message: Some(observe_room_settings_message("room-settings")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recorded_message_count(&stream_state, 1).await;
    snapshot_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_room_settings(message)
                .is_some_and(|settings| settings.version == 7)),
        "observe should send the current room settings snapshot"
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
    let handler = test_message_handler_for_user_with_runtime_and_concurrency(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
        user_id(),
        test_stream_handler_runtime(),
        Arc::new(MessageConcurrencyConfig::new(0)),
    );
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
    let mut runtime = test_stream_handler_runtime();
    runtime.notification_service = Some(Arc::clone(&notification_service));
    let handler = test_message_handler_for_user_with_runtime(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
        user_id(),
        runtime,
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = FailingStream::fail_after(1);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    let subscriber_count = notification_service.publish_realtime_event(NotificationCreatedEvent {
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
    assert_eq!(subscriber_count, 1);

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    notification_pool.close().await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[test]
fn test_ephemeral_chat_event_conversion() {
    let event = RealtimeEvent::ChatMessage {
        event_id: "evt1".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "alice".to_string(),
        message: "hello world".to_string(),
        timestamp: now(),
        display_position: Some("top".to_string()),
        display_color: Some("#ff0000".to_string()),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    let msg = &msgs[0];
    match &msg.message {
        Some(Message::Chat(chat)) => {
            assert_eq!(chat.room_id, "room_test");
            assert_eq!(chat.user_id, public_actor_id());
            assert_eq!(chat.username, "alice");
            assert_eq!(chat.content, "hello world");
            assert_eq!(chat.display_position, "top");
            assert_eq!(chat.display_color, "#ff0000");
        }
        other => panic!("Expected Chat message, got: {other:?}"),
    }
}

#[test]
fn test_ephemeral_chat_event_rejects_invalid_presentation() {
    let event = RealtimeEvent::ChatMessage {
        event_id: "evt1".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "alice".to_string(),
        message: "hello world".to_string(),
        timestamp: now(),
        display_position: Some("top\nbottom".to_string()),
        display_color: Some("#ff0000".to_string()),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("display position")
    ));
}

#[test]
fn test_durable_chat_message_event_conversion() {
    let created_at = now();
    let event = RealtimeEvent::ChatMessageEvent {
        event_id: "chat-event-1".to_string(),
        room_id: room_id(),
        actor_user_id: user_id(),
        event: ChatMessageEvent {
            event_id: "chat-event-1".to_string(),
            sequence: 1,
            room_id: room_id(),
            actor_user_id: user_id(),
            kind: ChatEventKind::Deleted,
            message: ChatMessageWithImages {
                message: ChatMessage {
                    id: 42,
                    room_id: room_id(),
                    user_id: Some(user_id()),
                    client_message_id: Some("client-42".to_string()),
                    content: String::new(),
                    message_type: ChatMessageType::Text,
                    status: ChatMessageStatus::Deleted,
                    version: 2,
                    reply_to_message_id: None,
                    reply_to_message_created_at: None,
                    metadata: serde_json::Value::Object(Default::default()),
                    edited_at: Some(created_at),
                    deleted_at: Some(created_at),
                    deleted_by: Some(user_id()),
                    delete_reason: Some("policy".to_string()),
                    created_at,
                },
                images: Vec::new(),
                reactions: Vec::new(),
            },
            occurred_at: created_at,
        },
        timestamp: created_at,
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert!(
        msgs.is_empty(),
        "durable chat events must be delivered through ResourceChanged(ChatEvent)"
    );
}

#[test]
fn test_playback_state_changed_event_conversion() {
    let state = RoomPlaybackState {
        room_id: room_id(),
        playing_media_id: Some(media_id()),
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 123.456,
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PlaybackState(ps)) => {
            assert_eq!(ps.room_id, "room_test");
            let s = ps.state.as_ref().unwrap();
            assert!(s.position >= 123.456);
            assert!(
                s.position < 124.0,
                "playing state position should be computed from the persisted anchor, got {}",
                s.position
            );
            assert!((s.speed - 1.5).abs() < f64::EPSILON);
            assert!(s.is_playing);
            assert_eq!(s.playing_media_id, public_media_id());
            assert_eq!(s.version, 7);
        }
        other => panic!("Expected PlaybackState, got: {other:?}"),
    }
}

#[test]
fn test_playback_state_changed_event_rejects_invalid_state_numbers() {
    let mut state = RoomPlaybackState {
        room_id: room_id(),
        playing_media_id: Some(media_id()),
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: f64::NAN,
        speed: 1.0,
        is_playing: false,
        updated_at: now(),
        version: 7,
    };
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-invalid-playback".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state: state.clone(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("position")
    ));

    state.position = 10.0;
    state.speed = 0.0;
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-invalid-speed".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state: state.clone(),
        timestamp: now(),
    };
    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("speed")
    ));

    state.speed = 1.0;
    state.version = -1;
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-invalid-version".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state,
        timestamp: now(),
    };
    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("version")
    ));
}

#[test]
fn test_user_joined_event_conversion() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt3".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "carol".to_string(),
        permissions: RoomPermissionSet::default_member(),
        role: 3,
        added_permissions: RoomPermissionSet(RoomAdminPermissionBits::PLAY_CONTROL),
        removed_permissions: RoomPermissionSet(RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE),
        admin_added_permissions: RoomPermissionSet(RoomAdminPermissionBits::KICK_MEMBER),
        admin_removed_permissions: RoomPermissionSet(RoomAdminPermissionBits::KICK_MEMBER),
        joined_at: now(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::UserJoined(uj)) => {
            assert_eq!(uj.room_id, "room_test");
            let member = uj.member.as_ref().unwrap();
            assert_eq!(member.user_id, public_actor_id());
            assert_eq!(member.username, "carol");
            assert_eq!(member.role, 3);
            assert_eq!(
                member.added_permissions,
                RoomAdminPermissionBits::PLAY_CONTROL
            );
            assert_eq!(
                member.removed_permissions,
                RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
            );
            assert_eq!(
                member.admin_added_permissions,
                RoomAdminPermissionBits::KICK_MEMBER
            );
            assert_eq!(
                member.admin_removed_permissions,
                RoomAdminPermissionBits::KICK_MEMBER
            );
            assert!(member.is_online);
        }
        other => panic!("Expected UserJoined, got: {other:?}"),
    }
}

#[test]
fn test_user_joined_event_rejects_unspecified_role() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt-invalid-role".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "carol".to_string(),
        permissions: RoomPermissionSet::default_member(),
        role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        joined_at: now(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("role")
    ));
}

#[test]
fn test_user_joined_event_rejects_invalid_username() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt-invalid-username".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: " ".to_string(),
        permissions: RoomPermissionSet::default_member(),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        joined_at: now(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("username")
    ));
}

#[test]
fn test_permission_changed_event_conversion_preserves_override_bitspace() {
    let event = RealtimeEvent::PermissionChanged {
        event_id: "evt-permission-override-bitspace".to_string(),
        room_id: room_id(),
        target_user_id: user_id(),
        target_username: "carol".to_string(),
        changed_by: user_id(),
        changed_by_username: "owner".to_string(),
        new_permissions: RoomPermissionSet(RoomAdminPermissionBits::USE_WEBRTC),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(RoomMemberPermissionBits::USE_WEBRTC),
        removed_permissions: RoomPermissionSet(RoomMemberPermissionBits::CHAT),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::PermissionChanged(permission)) => {
            assert_eq!(permission.room_id, "room_test");
            assert_eq!(permission.user_id, public_actor_id());
            assert_eq!(
                permission.effective_permissions,
                RoomAdminPermissionBits::USE_WEBRTC
            );
            assert_eq!(
                permission.added_permissions,
                RoomMemberPermissionBits::USE_WEBRTC
            );
            assert_eq!(
                permission.removed_permissions,
                RoomMemberPermissionBits::CHAT
            );
        }
        other => panic!("Expected PermissionChanged, got: {other:?}"),
    }
}

#[test]
fn test_permission_changed_event_rejects_undefined_role() {
    let event = RealtimeEvent::PermissionChanged {
        event_id: "evt-permission-invalid-role".to_string(),
        room_id: room_id(),
        target_user_id: user_id(),
        target_username: "carol".to_string(),
        changed_by: user_id(),
        changed_by_username: "owner".to_string(),
        new_permissions: RoomPermissionSet(RoomAdminPermissionBits::USE_WEBRTC),
        role: 999,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("role")
    ));
}

#[test]
fn test_permission_changed_event_rejects_invalid_updated_by() {
    let event = RealtimeEvent::PermissionChanged {
        event_id: "evt-permission-invalid-updated-by".to_string(),
        room_id: room_id(),
        target_user_id: user_id(),
        target_username: "carol".to_string(),
        changed_by: user_id(),
        changed_by_username: "\n".to_string(),
        new_permissions: RoomPermissionSet(RoomAdminPermissionBits::USE_WEBRTC),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("updated-by username")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_media_added_event_rejects_invalid_title() {
    let event = RealtimeEvent::MediaAdded {
        event_id: "evt-invalid-media-title".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "eve".to_string(),
        media_id: media_id(),
        media_title: " ".to_string(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("media title")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_media_removed_batch_event_rejects_empty_media_ids() {
    let event = RealtimeEvent::MediaRemovedBatch {
        event_id: "evt-empty-media-batch".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "frank".to_string(),
        media_ids: Vec::new(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("media_ids")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_playlist_reordered_event_rejects_empty_media_ids() {
    let event = RealtimeEvent::PlaylistReordered {
        event_id: "evt-empty-playlist-reorder".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "grace".to_string(),
        media_ids: Vec::new(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("media_ids")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_room_settings_changed_event_rejects_invalid_payload() {
    let mut event = RealtimeEvent::RoomSettingsChanged {
        event_id: "evt-invalid-room-settings".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "judy".to_string(),
        settings_json: br"[]".to_vec(),
        version: 12,
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("Room settings JSON")
    ));

    if let RealtimeEvent::RoomSettingsChanged {
        settings_json,
        version,
        ..
    } = &mut event
    {
        *settings_json = br#"{"chat_enabled":false}"#.to_vec();
        *version = -1;
    }
    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("version")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_webrtc_signaling_event_rejects_invalid_route_fields() {
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: "evt-invalid-webrtc-route".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::Offer,
        from: "conn_a".to_string(),
        to: " ".to_string(),
        data: "sdp_data".to_string(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("webrtc to")
    ));
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    assert!(matches!(
        &msgs[0].message,
        Some(Message::WebrtcIceCandidate(_))
    ));
}

#[test]
fn test_webrtc_join_and_leave_event_conversion() {
    let join = RealtimeEvent::WebRTCJoin {
        event_id: "evt-webrtc-join".to_string(),
        room_id: room_id(),
        actor_id: "user_1".to_string(),
        conn_id: "conn_1".to_string(),
        username: "alice".to_string(),
        timestamp: now(),
    };
    let leave = RealtimeEvent::WebRTCLeave {
        event_id: "evt-webrtc-leave".to_string(),
        room_id: room_id(),
        actor_id: "user_1".to_string(),
        conn_id: "conn_1".to_string(),
        timestamp: now(),
    };

    let join_msgs = realtime_event_to_server_messages(&join, "room_test", &public_id_codec())
        .expect("webrtc join should convert");
    let leave_msgs = realtime_event_to_server_messages(&leave, "room_test", &public_id_codec())
        .expect("webrtc leave should convert");

    match &join_msgs[0].message {
        Some(Message::WebrtcJoin(join)) => {
            assert_eq!(join.user_id, "user_1");
            assert_eq!(join.conn_id, "conn_1");
            assert_eq!(join.username, "alice");
        }
        other => panic!("Expected WebrtcJoin, got: {other:?}"),
    }
    match &leave_msgs[0].message {
        Some(Message::WebrtcLeave(leave)) => {
            assert_eq!(leave.user_id, "user_1");
            assert_eq!(leave.conn_id, "conn_1");
        }
        other => panic!("Expected WebrtcLeave, got: {other:?}"),
    }
}

#[test]
fn test_webrtc_join_event_rejects_invalid_connection_id() {
    let event = RealtimeEvent::WebRTCJoin {
        event_id: "evt-invalid-webrtc-join".to_string(),
        room_id: room_id(),
        actor_id: "user_1".to_string(),
        conn_id: "conn\n1".to_string(),
        username: "alice".to_string(),
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("connection id")
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .expect("realtime event should convert");
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
fn test_system_notification_event_rejects_invalid_message() {
    let event = RealtimeEvent::SystemNotification {
        event_id: "evt-invalid-system-notification".to_string(),
        message: " \n ".to_string(),
        level: NotificationLevel::Warning,
        timestamp: now(),
    };

    assert!(matches!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec()),
        Err(message) if message.contains("system notification message")
    ));
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
    assert!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .expect("realtime event should convert")
            .is_empty()
    );

    let event = RealtimeEvent::KickUser {
        event_id: "evt14".to_string(),
        user_id: user_id(),
        reason: "banned".to_string(),
        timestamp: now(),
    };
    assert!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .expect("realtime event should convert")
            .is_empty()
    );

    let event = RealtimeEvent::RoomBanned {
        event_id: "evt15".to_string(),
        room_id: room_id(),
        banned_by: user_id(),
        timestamp: now(),
    };
    assert_eq!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .expect("realtime event should convert")
            .len(),
        1
    );
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
fn test_parse_optional_chat_message_id_accepts_empty_and_numeric_values() {
    assert_eq!(super::parse_optional_chat_message_id("").unwrap(), None);
    assert_eq!(
        super::parse_optional_chat_message_id(" 42 ").unwrap(),
        Some(42)
    );
}

#[test]
fn test_parse_optional_chat_message_id_rejects_invalid_values() {
    let result = super::parse_optional_chat_message_id("message-42");

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Invalid chat message id");
}

#[test]
fn test_membership_cache_stores_and_retrieves() {
    // Verify the membership cache can store and retrieve entries
    let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
        moka::sync::Cache::builder()
            .time_to_live(super::MEMBERSHIP_CACHE_TTL)
            .build();

    let key = ("room1".to_string(), "user1".to_string());
    let membership = super::CachedMembership { is_member: true };

    cache.insert(key.clone(), membership);
    let cached = cache.get(&key);
    assert!(cached.is_some());
    let cached = cached.unwrap();
    assert!(cached.is_member);
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
    let membership = super::CachedMembership { is_member: true };

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
        super::CachedMembership { is_member: true },
    );
    cache.insert(
        key_user2.clone(),
        super::CachedMembership { is_member: true },
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
fn test_cached_membership_from_member_none() {
    // Verify CachedMembership correctly handles non-members
    let cached = super::CachedMembership::from_member(None);
    assert!(!cached.is_member, "Non-member should have is_member=false");
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
        version: 1,
    };

    let cached = super::CachedMembership::from_member(Some(&member));
    assert!(cached.is_member);
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
    assert!(super::should_persist_playback_progress(None, 100.0));
}

#[tokio::test]
async fn test_progress_throttle_small_position_change_suppressed() {
    assert!(
        !super::should_persist_playback_progress(Some((100.0, tokio::time::Instant::now())), 100.5),
        "Small position change with short elapsed time should be suppressed"
    );
}

#[tokio::test]
async fn test_progress_throttle_large_position_change_allowed() {
    assert!(
        super::should_persist_playback_progress(Some((100.0, tokio::time::Instant::now())), 101.5),
        "Large position change should trigger a write"
    );
}

#[tokio::test]
async fn test_progress_throttle_elapsed_time_allows_write() {
    let last_time = tokio::time::Instant::now() - std::time::Duration::from_secs_f64(6.0);
    assert!(
        super::should_persist_playback_progress(Some((100.0, last_time)), 100.1),
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
        RoomPermissionSet::default_guest(),
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

    let reason = super::guest_token_blacklist_denial_reason(
        &handler.room_service,
        &handler.room_id,
        &handler.user_id,
        &identity.token_jti,
    )
    .await
    .expect("blacklist check should succeed");

    assert_eq!(reason.as_deref(), Some("Guest token has been revoked"));

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_guest_chat_is_rejected_even_if_permission_bits_include_chat() {
    let event_service = test_realtime_manager("guest_chat_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::CHAT),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(crate::proto::client::client_message::Message::Chat(
                crate::proto::client::ChatMessageSend {
                    content: "guest message".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    images: Vec::new(),
                    reply_to_message_id: String::new(),
                },
            )),
        })
        .await
        .expect_err("guest chat must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_guest_chat_with_client_id_is_rejected() {
    let event_service = test_realtime_manager("guest_chat_client_id_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::CHAT),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(crate::proto::client::client_message::Message::Chat(
                crate::proto::client::ChatMessageSend {
                    content: "guest chat with client id".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    images: Vec::new(),
                    reply_to_message_id: String::new(),
                },
            )),
        })
        .await
        .expect_err("guest chat must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
async fn test_guest_playlist_observation_is_rejected_even_if_permission_bits_include_view_media_resources(
) {
    let event_service = test_realtime_manager("guest_playlist_observe_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "guest-playlist-items",
                crate::proto::client::ListPlaylistItemsRequest::default(),
            )),
        })
        .await
        .expect_err("guest playlist observation must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot observe playlist items"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
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
fn test_user_left_delivery_uses_local_fallback_when_distributed_check_fails() {
    let plan = super::should_broadcast_user_left(false, Err(()));
    assert_eq!(plan, super::UserLeftDeliveryPlan::LocalAndRedis);
}

#[test]
fn test_user_left_delivery_local_presence_still_wins_when_distributed_check_fails() {
    let plan = super::should_broadcast_user_left(true, Err(()));
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
        RoomPermissionSet::default_guest(),
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
            assert_eq!(
                guest_id,
                handler
                    .public_actor_id()
                    .expect("guest public actor id should encode")
            );
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
        RoomPermissionSet(
            RoomPermissionSet::default_guest().0 | RoomAdminPermissionBits::USE_WEBRTC,
        ),
    );
    let connection_id = handler.connection_id().to_string();

    connection_service
        .register_actor(
            connection_id.clone(),
            handler.user_id,
            handler
                .public_actor_id()
                .expect("guest public actor id should encode"),
        )
        .await
        .expect("register guest connection");
    connection_service
        .join_room(&connection_id, handler.room_id)
        .await
        .expect("join room");
    connection_service.mark_rtc_joined(&handler.room_id, &handler.user_id, &connection_id, true);

    let guest_target = format!(
        "{}:{}",
        handler
            .public_actor_id()
            .expect("guest public actor id should encode"),
        connection_id
    );
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
    let public_id_codec = Arc::new(crate::PublicIdCodec::plain());

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id,
        principal: RealtimePrincipal::user(user_id, "user".to_string()),
        connection_id: None,
        room_service: Arc::clone(&test_room_service(pool.clone())),
        chat_service: test_chat_service(pool),
        event_service,
        connection_service: manager.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:webrtc-recipient-public-user-match:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::clone(&public_id_codec),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });
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

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id,
        principal: RealtimePrincipal::user(user_id, "user".to_string()),
        connection_id: None,
        room_service: Arc::clone(&test_room_service(pool.clone())),
        chat_service: test_chat_service(pool),
        event_service,
        connection_service: manager.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:webrtc-recipient-malformed-reject:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });
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
fn test_realtime_join_error_from_string_classifies_capacity_errors() {
    let error = super::RealtimeJoinError::from("realtime room capacity exceeded".to_string());

    assert!(matches!(
        error,
        super::RealtimeJoinError::RateLimited(message)
        if message == "realtime room capacity exceeded"
    ));
}

#[test]
fn test_realtime_join_error_from_admission_error_classifies_capacity_errors() {
    let error = super::RealtimeJoinError::from(
        crate::runtime::RealtimeAdmissionError::from_runtime_message(
            "Room at capacity (42 connections, max: 40)".to_string(),
        ),
    );

    assert!(matches!(
        error,
        super::RealtimeJoinError::RateLimited(message)
        if message == "Room at capacity (42 connections, max: 40)"
    ));
}

#[test]
fn test_realtime_join_error_from_admission_error_classifies_cluster_degradation() {
    let error = super::RealtimeJoinError::from(
        crate::runtime::RealtimeAdmissionError::from_runtime_message(
            "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                .to_string(),
        ),
    );

    assert!(matches!(
        error,
        super::RealtimeJoinError::ServiceUnavailable(message)
        if message == "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
    ));
}

#[test]
fn test_realtime_join_error_from_string_classifies_invalid_input() {
    let error =
        super::RealtimeJoinError::from("Invalid input: Invalid chat event cursor".to_string());

    assert!(matches!(
        error,
        super::RealtimeJoinError::InvalidInput(message)
        if message == "Invalid input: Invalid chat event cursor"
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
    let owner = register_test_user(&user_service, "room-owner", "owner@test.invalid").await;
    let member = register_test_user(&user_service, "room-member", "member@test.invalid").await;
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

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: test_chat_service(pool.clone()),
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:pre-join-room-closed:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

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
    let owner = register_test_user(
        &user_service,
        "room-owner-inactive",
        "owner-inactive@test.invalid",
    )
    .await;
    let member = register_test_user(
        &user_service,
        "room-member-inactive-owner",
        "member-inactive-owner@test.invalid",
    )
    .await;
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

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: test_chat_service(pool.clone()),
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:pre-join-room-owner-inactive:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

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
    let owner = register_test_user(
        &user_service,
        "room-owner-user-ban",
        "owner-user-ban@test.invalid",
    )
    .await;
    let member = register_test_user(
        &user_service,
        "room-member-user-ban",
        "member-user-ban@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Realtime User Ban".to_string(),
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

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: test_chat_service(pool.clone()),
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:pre-join-user-banned:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

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
    let event_service =
        test_realtime_manager_with_failing_subscription("test_pre_join_subscription_cache_failure")
            .await;
    let connection_service = test_connection_manager();
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let owner = register_test_user(
        &user_service,
        "room-owner-sub-fail",
        "owner-sub-fail@test.invalid",
    )
    .await;
    let member = register_test_user(
        &user_service,
        "room-member-sub-fail",
        "member-sub-fail@test.invalid",
    )
    .await;
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

    let handler = super::StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: test_chat_service(pool.clone()),
        event_service: event_service.clone(),
        connection_service: connection_service.clone(),
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:pre-join-subscription-fail:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(crate::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    connection_service
        .register(handler.connection_id.clone(), handler.user_id)
        .await
        .expect("register should succeed before subscription caching");

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

#[test]
fn test_watch_disconnect_signal_matches_revocation_targets() {
    let rid = room_id();
    let uid = user_id();
    let other_uid = UserId::expect_positive(2);
    let other_rid = RoomId::expect_positive(2);
    let connection_id = "conn_watch";

    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::Connection(connection_id.to_string()),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::User(uid),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::Room(rid),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        },
        &uid,
        &rid,
        connection_id,
    ));
    assert!(!super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::User(other_uid),
        &uid,
        &rid,
        connection_id,
    ));
    assert!(!super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: other_rid,
        },
        &uid,
        &rid,
        connection_id,
    ));
}

#[test]
fn test_watch_admin_event_matches_access_revocation_events() {
    let rid = room_id();
    let uid = user_id();
    let other_uid = UserId::expect_positive(2);
    let other_rid = RoomId::expect_positive(2);
    let now = chrono::Utc::now();

    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::KickUser {
            event_id: "evt-1".to_string(),
            user_id: uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
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
    assert!(super::watch_admin_event_matches(
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
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::RoomDeleted {
            event_id: "evt-4".to_string(),
            room_id: rid,
            deleted_by: uid,
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-5".to_string(),
            room_id: rid,
            banned_by: uid,
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::RoomOwnerInactive {
            event_id: "evt-6".to_string(),
            room_id: rid,
            owner_id: uid,
            triggered_by: uid,
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(!super::watch_admin_event_matches(
        &RealtimeEvent::KickUser {
            event_id: "evt-7".to_string(),
            user_id: other_uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        &uid,
        &rid,
    ));
    assert!(!super::watch_admin_event_matches(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-8".to_string(),
            room_id: other_rid,
            banned_by: uid,
            timestamp: now,
        },
        &uid,
        &rid,
    ));
}

#[tokio::test]
async fn test_resource_watch_prepare_enforces_room_connection_limit_and_releases_on_cancel() {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let event_service = test_realtime_manager("watch_prepare_limit").await;
    let connection_service = Arc::new(ConnectionManager::new(ConnectionLimits {
        max_per_room: 1,
        max_per_user: 10,
        max_total: 10,
        ..ConnectionLimits::default()
    }));
    let public_id_codec = Arc::new(crate::PublicIdCodec::plain());

    let owner = register_test_user(
        &user_service,
        "watch_limit_owner",
        "watch-limit-owner@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Watch Limit Room".to_string(),
            "watch-limit".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    let member = register_test_user(
        &user_service,
        "watch_limit_member",
        "watch-limit-member@test.invalid",
    )
    .await;
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");

    let observe = watch_room_settings_observe(crate::proto::client::WatchRoomSettingsRequest {
        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        room_settings: Some(crate::proto::client::ObserveRoomSettings {
            after_event_sequence: None,
        }),
    })
    .expect("room settings watch request should build");
    let make_session = || {
        ResourceWatchSession::new(ResourceWatchSessionConfig {
            room_id: room.id,
            principal: RealtimePrincipal::user(member.id, member.username.clone()),
            room_service: Arc::clone(&room_service),
            chat_service: None,
            event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
            connection_service: Arc::clone(&connection_service)
                as Arc<dyn RealtimeConnectionService>,
            public_id_codec: Arc::clone(&public_id_codec),
            sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
            playback_snapshot_service: None,
            playlist_items_snapshot_service: None,
            room_members_snapshot_service: None,
            room_settings_snapshot_service: None,
        })
    };

    let prepared = make_session()
        .prepare(&observe)
        .await
        .expect("first watch should prepare");
    assert_eq!(connection_service.room_connection_count(&room.id), 1);

    let Err(second) = make_session().prepare(&observe).await else {
        panic!("second watch should hit per-room capacity");
    };
    assert!(matches!(second, RealtimeJoinError::RateLimited(_)));
    assert_eq!(
        connection_service.room_connection_count(&room.id),
        1,
        "failed prepare must unregister the rejected connection"
    );

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let run_handle = tokio::spawn(prepared.run(cancel_token.clone()));
    cancel_token.cancel();
    run_handle
        .await
        .expect("watch task should join")
        .expect("watch run should stop cleanly");
    assert_eq!(
        connection_service.room_connection_count(&room.id),
        0,
        "watch cancellation must release realtime room capacity"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
    drop(container);
}

#[tokio::test]
async fn test_resource_watch_prepare_rejects_missing_observe_resource_before_subscription() {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let event_service = test_realtime_manager("watch_prepare_missing_resource").await;
    let connection_service = test_connection_manager();
    let public_id_codec = Arc::new(crate::PublicIdCodec::plain());

    let owner = register_test_user(
        &user_service,
        "watch_missing_resource_owner",
        "watch-missing-resource-owner@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Watch Missing Resource Room".to_string(),
            "watch-missing-resource".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");

    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(owner.id, owner.username.clone()),
        room_service: Arc::clone(&room_service),
        chat_service: None,
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        public_id_codec,
        sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
        playback_snapshot_service: None,
        playlist_items_snapshot_service: None,
        room_members_snapshot_service: None,
        room_settings_snapshot_service: None,
    });
    let observe = crate::proto::client::ObserveResource {
        observe_id: "missing-resource".to_string(),
        delivery_mode: crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: None,
    };

    let Err(error) = session.prepare(&observe).await else {
        panic!("missing observe resource should fail watch prepare");
    };
    assert!(
        matches!(error, RealtimeJoinError::InvalidInput(message) if message.contains("resource"))
    );
    assert_eq!(
        connection_service.room_connection_count(&room.id),
        0,
        "failed watch prepare must release room connection capacity"
    );
    assert_eq!(
        realtime_manager_subscriber_count(&event_service, &room.id),
        0,
        "invalid observe should fail before subscribing to realtime events"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
    drop(container);
}

#[tokio::test]
async fn test_resource_watch_chat_events_requires_view_chat_history_permission() {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let event_service = test_realtime_manager("watch_chat_events_permission").await;
    let connection_service = test_connection_manager();
    let public_id_codec = Arc::new(crate::PublicIdCodec::plain());

    let owner = register_test_user(
        &user_service,
        "watch_chat_perm_owner",
        "watch-chat-perm-owner@test.invalid",
    )
    .await;
    let (room, _) = room_service
        .create_room(
            "Watch Chat Permission Room".to_string(),
            "watch-chat-permission".to_string(),
            owner.id,
            None,
            None,
        )
        .await
        .expect("room should be created");
    let member = register_test_user(
        &user_service,
        "watch_chat_perm_member",
        "watch-chat-perm-member@test.invalid",
    )
    .await;
    room_service
        .join_room(room.id, member.id, None)
        .await
        .expect("member should join room");
    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .expect("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        );
    room_service
        .set_room_settings(&room.id, &settings)
        .await
        .expect("room settings should update");

    let observe = watch_chat_events_observe(crate::proto::client::WatchChatEventsRequest {
        delivery_mode: crate::proto::client::ResourceDeliveryMode::NotifyOnly as i32,
        chat_events: Some(crate::proto::client::ObserveChatEvents {
            after_event_sequence: None,
        }),
    })
    .expect("chat events watch request should build");
    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        room_service: Arc::clone(&room_service),
        chat_service: Some(chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn RealtimeConnectionService>,
        public_id_codec: Arc::clone(&public_id_codec),
        sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
        playback_snapshot_service: None,
        playlist_items_snapshot_service: None,
        room_members_snapshot_service: None,
        room_settings_snapshot_service: None,
    });

    let Err(error) = session.prepare(&observe).await else {
        panic!("chat events watch should require VIEW_CHAT_HISTORY");
    };
    assert!(matches!(error, RealtimeJoinError::PermissionDenied(_)));
    assert_eq!(
        connection_service.room_connection_count(&room.id),
        0,
        "failed prepare should unregister the denied watch"
    );

    shutdown_test_runtime_resources(event_service, connection_service).await;
    pool.close().await;
    drop(container);
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
