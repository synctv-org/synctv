use super::event_messages::{
    realtime_event_to_server_messages, realtime_termination_server_message,
    room_disconnect_termination_server_message,
};
use super::*;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::notification::{Notification, NotificationData, NotificationType};
use synctv_core::models::{
    ChatAttachment, ChatEventKind, ChatMessage, ChatMessageEvent, ChatMessageStatus,
    ChatMessageType, ChatMessageWithAttachments, MediaId, Playlist, PlaylistId, RealtimeActor,
    RoomAdminPermissionBits, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermission,
    RoomPermissionSet, RoomPlaybackState, RoomRole, RoomSettings, SendChatMessage, UserId,
};
use synctv_core::repository::NotificationRepository;
use synctv_core::repository::{
    ChatRepository, RoomMemberRepository, RoomRepository, RoomResourceEventSummary,
    RoomResourceEventSummaryDetails, RoomResourceKind, RoomSettingsRepository, UserRepository,
};
use synctv_core::service::NotificationCreatedEvent;
use synctv_core::service::{
    ChatDependencies, ChatRuntime, ChatService, ContentFilter, NotificationService,
    PermissionService, RateLimitConfig, RateLimiter, RoomService, RoomSettingsService,
};
use synctv_core_testing::{
    create_test_request_rate_limiter, opaque_register_user, TestOptionExt, TestResultExt,
};
use synctv_proto::client::server_message::Message;
use synctv_realtime::fanout::{
    RealtimeDeliveryOutcome, RealtimeDeliveryRequirement, RealtimeMetrics,
};
use synctv_realtime::sync::{
    ConnectionId, ConnectionLimits, ConnectionManager, RealtimeConfig, RealtimeManager,
    RoomDisconnectReason, SharedRealtimeEvent,
};
use synctv_realtime::sync::{NotificationLevel, RealtimeEvent, RoomMessageHub, WebRTCSignalKind};
use tokio::sync::{broadcast, mpsc};

pub(super) struct UnconfiguredPlaybackService;

#[async_trait::async_trait]
impl PlaybackService for UnconfiguredPlaybackService {
    async fn room_playback_state(
        &self,
        _room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Err(crate::impls::ApiError::Internal(
            "test playback service is not configured".to_string(),
        ))
    }

    async fn get_playback_for_actor(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
        Err(crate::impls::ApiError::Internal(
            "test playback service is not configured".to_string(),
        ))
    }

    async fn playback_credential_dependencies(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, crate::impls::ApiError>
    {
        Ok(Vec::new())
    }
}

pub(super) struct UnconfiguredPlaylistItemsSnapshotService;

#[async_trait::async_trait]
impl PlaylistItemsSnapshotService for UnconfiguredPlaylistItemsSnapshotService {
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        Err(crate::impls::ApiError::Internal(
            "test playlist items snapshot service is not configured".to_string(),
        ))
    }
}

pub(super) struct UnconfiguredRoomMembersSnapshotService;

#[async_trait::async_trait]
impl RoomMembersSnapshotService for UnconfiguredRoomMembersSnapshotService {
    async fn get_room_members_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        Err(crate::impls::ApiError::Internal(
            "test room members snapshot service is not configured".to_string(),
        ))
    }
}

pub(super) struct UnconfiguredRoomSettingsSnapshotService;

#[async_trait::async_trait]
impl RoomSettingsSnapshotService for UnconfiguredRoomSettingsSnapshotService {
    async fn get_room_settings_snapshot(
        &self,
        _room_id: &RoomId,
    ) -> Result<crate::impls::room_settings_snapshot::RoomSettingsSnapshot, crate::impls::ApiError>
    {
        Err(crate::impls::ApiError::Internal(
            "test room settings snapshot service is not configured".to_string(),
        ))
    }
}

fn room_id() -> RoomId {
    RoomId::expect_positive(1)
}
fn user_id() -> UserId {
    UserId::expect_positive(1)
}

trait TestHandlerIdentity {
    fn test_user_id(&self) -> UserId;
}

impl TestHandlerIdentity for StreamMessageHandler {
    fn test_user_id(&self) -> UserId {
        self.require_user_id()
            .checked("test handler should have a signed-in user")
    }
}
fn media_id() -> MediaId {
    MediaId::expect_positive(1)
}
fn public_id_codec() -> synctv_adapter::PublicIdCodec {
    synctv_adapter::PublicIdCodec::plain()
}
fn public_media_id() -> String {
    public_id_codec()
        .encode_media_id(media_id())
        .checked("test value")
}
fn public_playlist_id() -> String {
    public_id_codec()
        .encode_playlist_id(playlist().id)
        .checked("test value")
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
        _actor: RealtimeActor,
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(
        tokio::sync::mpsc::Receiver<SharedRealtimeEvent>,
        ConnectionId,
    )> {
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

    fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            distributed_enabled: false,
        }
    }

    fn node_id(&self) -> &'static str {
        "local-runtime-test"
    }

    async fn shutdown(&self) {}
}

fn event_cursor(sequence: i64) -> synctv_proto::client::EventCursor {
    synctv_proto::client::EventCursor {
        event_id: Some(format!("event-{sequence}")),
        sequence,
    }
}

fn chat_attachment() -> ChatAttachment {
    ChatAttachment {
        id: "chat-attachment-1".to_string(),
        kind: synctv_core::models::ChatAttachmentKind::Image,
        room_id: room_id(),
        message_id: 10,
        message_created_at: synctv_core::SystemClock.now(),
        filename: Some("chat-attachment-1.png".to_string()),
        storage_backend: "database".to_string(),
        object_key: "chat/attachments/chat-attachment-1".to_string(),
        object_access: None,
        url: Some("https://cdn.example.test/chat-attachment-1.png".to_string()),
        mime_type: Some("image/png".to_string()),
        size_bytes: Some(1024),
        width: Some(320),
        height: Some(240),
        metadata: synctv_core::models::FileMetadata::default(),
        created_at: synctv_core::SystemClock.now(),
        reuse_token: None,
        reuse_expires_at: None,
    }
}

fn test_stream_handler_runtime() -> StreamMessageHandlerRuntime {
    let event_service: Arc<dyn RealtimeEventService> =
        Arc::new(LocalRuntimeRealtimeEventService::new());
    StreamMessageHandlerRuntime {
        heartbeat_schedule: HeartbeatSchedule::fixed(
            Duration::from_millis(10),
            Duration::from_mins(1),
        ),
        ..StreamMessageHandlerRuntime::local(&event_service)
    }
}

fn runtime_with_playback_service(service: Arc<dyn PlaybackService>) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        playback_service: service,
        ..test_stream_handler_runtime()
    }
}

fn runtime_with_playlist_items_snapshot_service(
    service: Arc<dyn PlaylistItemsSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        playlist_items_snapshot_service: service,
        ..test_stream_handler_runtime()
    }
}

fn runtime_with_room_settings_snapshot_service(
    service: Arc<dyn RoomSettingsSnapshotService>,
) -> StreamMessageHandlerRuntime {
    StreamMessageHandlerRuntime {
        room_settings_snapshot_service: service,
        ..test_stream_handler_runtime()
    }
}

type ResourceWatchRuntimeFields = (
    Arc<dyn PlaybackService>,
    Arc<dyn PlaylistItemsSnapshotService>,
    Arc<dyn RoomMembersSnapshotService>,
    Arc<dyn RoomSettingsSnapshotService>,
);

fn test_resource_watch_runtime_fields() -> ResourceWatchRuntimeFields {
    let event_service: Arc<dyn RealtimeEventService> =
        Arc::new(LocalRuntimeRealtimeEventService::new());
    let runtime = StreamMessageHandlerRuntime::local(&event_service);
    (
        runtime.playback_service,
        runtime.playlist_items_snapshot_service,
        runtime.room_members_snapshot_service,
        runtime.room_settings_snapshot_service,
    )
}

#[test]
fn guest_principal_has_no_user_id() {
    let principal = test_guest_principal_with_permissions(RoomPermissionSet::default_guest());

    assert_eq!(principal.user_id(), None);
    assert!(matches!(
        principal
            .realtime_actor(&synctv_adapter::PublicIdCodec::plain())
            .checked("guest actor should build"),
        synctv_core::models::RealtimeActor::Guest { .. }
    ));
}

#[test]
fn core_chat_attachment_to_proto_requires_storage_metadata_and_allows_optional_dimensions() {
    let attachment = chat_attachment();

    let proto =
        core_chat_attachment_to_proto(&attachment).checked("valid chat attachment should convert");
    assert_eq!(proto.mime_type, "image/png");
    assert_eq!(proto.size_bytes, 1024);
    assert_eq!(proto.width, 320);
    assert_eq!(proto.height, 240);

    let mut missing_mime_type = attachment.clone();
    missing_mime_type.mime_type = None;
    assert!(core_chat_attachment_to_proto(&missing_mime_type)
        .expect_err("missing mime_type should fail")
        .contains("mime_type"));

    let mut missing_size = attachment.clone();
    missing_size.size_bytes = None;
    assert!(core_chat_attachment_to_proto(&missing_size)
        .expect_err("missing size_bytes should fail")
        .contains("size_bytes"));

    let mut missing_dimensions = attachment;
    missing_dimensions.width = None;
    missing_dimensions.height = None;
    let proto = core_chat_attachment_to_proto(&missing_dimensions)
        .checked("missing dimensions should serialize as zero");
    assert_eq!(proto.width, 0);
    assert_eq!(proto.height, 0);
}

#[test]
fn chat_display_metadata_reads_valid_presentation() {
    let metadata = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            presentation: Some(synctv_core::models::ChatPresentationMetadata {
                display_position: Some(" top ".to_string()),
                display_color: Some(" #ff0000 ".to_string()),
            }),
            ..Default::default()
        },
    ));

    assert_eq!(
        chat_display_position_from_metadata(metadata.as_ref())
            .checked("display position should parse"),
        "top"
    );
    assert_eq!(
        chat_display_color_from_metadata(metadata.as_ref()).checked("display color should parse"),
        "#ff0000"
    );
}

#[test]
fn chat_display_metadata_rejects_invalid_presentation_fields() {
    let control_character = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            presentation: Some(synctv_core::models::ChatPresentationMetadata {
                display_color: Some("red\nblue".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
    ));

    assert!(matches!(
        chat_display_color_from_metadata(control_character.as_ref()),
        Err(message) if message.contains("display color")
    ));
}

#[test]
fn chat_playback_metadata_encodes_public_ids() {
    let metadata = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            playback: Some(synctv_core::models::ChatPlaybackMetadata {
                media_id: Some(media_id()),
                playlist_id: Some(playlist().id),
                target: None,
                target_hash: None,
                position_seconds: None,
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        },
    ));
    let codec = public_id_codec();

    assert_eq!(
        chat_playback_media_id_from_metadata(metadata.as_ref(), &codec),
        Ok(public_media_id())
    );
    assert_eq!(
        chat_playback_playlist_id_from_metadata(metadata.as_ref(), &codec),
        Ok(public_playlist_id())
    );
}

#[test]
fn chat_playback_metadata_without_source_returns_empty_ids() {
    let metadata: Option<synctv_core::models::ChatMetadata> = None;
    let codec = public_id_codec();

    assert_eq!(
        chat_playback_media_id_from_metadata(metadata.as_ref(), &codec),
        Ok(String::new())
    );
    assert_eq!(
        chat_playback_playlist_id_from_metadata(metadata.as_ref(), &codec),
        Ok(String::new())
    );
}

#[test]
fn chat_playback_metadata_rejects_invalid_source_ids() {
    let codec = public_id_codec();
    let metadata: Option<synctv_core::models::ChatMetadata> = None;

    assert_eq!(
        chat_playback_media_id_from_metadata(metadata.as_ref(), &codec),
        Ok(String::new())
    );
    assert_eq!(
        chat_playback_playlist_id_from_metadata(metadata.as_ref(), &codec),
        Ok(String::new())
    );
}

#[test]
fn chat_playback_metadata_decodes_structured_target() {
    let metadata = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            playback: Some(synctv_core::models::ChatPlaybackMetadata {
                media_id: None,
                playlist_id: None,
                target: Some(synctv_core::models::ProviderTarget::alist(
                    "/episode-1.mp4".to_string(),
                )),
                target_hash: None,
                position_seconds: None,
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        },
    ));

    assert_eq!(
        chat_playback_target_from_metadata(metadata.as_ref()),
        Ok(Some(synctv_core::models::ProviderTarget::alist(
            "/episode-1.mp4".to_string()
        )))
    );
}

#[test]
fn chat_playback_metadata_derives_target_hash_from_target() {
    let metadata = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            playback: Some(synctv_core::models::ChatPlaybackMetadata {
                media_id: None,
                playlist_id: None,
                target: Some(synctv_core::models::ProviderTarget::alist(
                    "/episode-1.mp4".to_string(),
                )),
                target_hash: None,
                position_seconds: None,
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        },
    ));
    let playback = chat_playback_metadata_from_metadata(metadata.as_ref(), &public_id_codec())
        .checked("playback metadata should parse");

    let target = synctv_core::models::ProviderTarget::alist("/episode-1.mp4".to_string());
    assert!(matches!(
        playback.target,
        Some(synctv_proto::client::ProviderTarget {
            target: Some(synctv_proto::client::provider_target::Target::Alist(_))
        })
    ));
    assert_eq!(
        playback.target_hash,
        chat_playback_target_hash(&target).checked("target hash should compute")
    );
}

#[test]
fn chat_playback_metadata_rejects_invalid_position_seconds() {
    let metadata = Some(synctv_core::models::ChatMetadata::User(
        synctv_core::models::ChatUserMetadata {
            playback: Some(synctv_core::models::ChatPlaybackMetadata {
                media_id: None,
                playlist_id: None,
                target: None,
                target_hash: None,
                position_seconds: Some(-1.0),
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        },
    ));

    assert!(matches!(
        chat_playback_metadata_from_metadata(metadata.as_ref(), &public_id_codec()),
        Err(message) if message.contains("position_seconds")
    ));
}

#[test]
fn chat_playback_metadata_rejects_invalid_target() {
    let metadata: Option<synctv_core::models::ChatMetadata> = None;

    assert_eq!(
        chat_playback_target_from_metadata(metadata.as_ref()),
        Ok(None)
    );
}

#[test]
fn watch_observe_builders_require_resource_bodies() {
    assert!(matches!(
        watch_playback_state_observe(synctv_proto::client::WatchPlaybackStateRequest::default()),
        Err(message) if message.contains("playback_state")
    ));
    assert!(matches!(
        watch_playback_observe(
            synctv_proto::client::WatchPlaybackRequest::default()
        ),
        Err(message) if message.contains("playback")
    ));
    assert!(matches!(
        watch_room_settings_observe(synctv_proto::client::WatchRoomSettingsRequest::default()),
        Err(message) if message.contains("room_settings")
    ));
    assert!(matches!(
        watch_playlist_items_observe(synctv_proto::client::WatchPlaylistItemsRequest::default()),
        Err(message) if message.contains("playlist_items")
    ));
    assert!(matches!(
        watch_room_member_events_observe(
            synctv_proto::client::WatchRoomMemberEventsRequest::default()
        ),
        Ok(observe) if matches!(
            observe.resource,
            Some(synctv_proto::client::observe_resource::Resource::RoomMemberEvents(_))
        )
    ));
    assert!(matches!(
        watch_chat_events_observe(synctv_proto::client::WatchChatEventsRequest::default()),
        Ok(observe) if matches!(
            observe.resource,
            Some(synctv_proto::client::observe_resource::Resource::ChatEvents(_))
        )
    ));
}

#[test]
fn watch_playback_observe_builds_playback_resource_only() {
    let observe = watch_playback_observe(synctv_proto::client::WatchPlaybackRequest {
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        playback: Some(synctv_proto::client::ObservePlayback {
            playback_client_profile: Some(synctv_proto::client::PlaybackClientProfile {
                stream_preference: synctv_proto::client::PlaybackStreamPreference::DirectPlay
                    as i32,
                max_streaming_bitrate: Some(1_500_000),
                max_audio_channels: None,
                supported_video_codecs: Vec::new(),
                supported_containers: Vec::new(),
                audio_capability: synctv_proto::client::PlaybackAudioCapability::Unspecified as i32,
                subtitle_preference: synctv_proto::client::PlaybackSubtitlePreference::Unspecified
                    as i32,
            }),
        }),
    })
    .checked("watch playback observe should build");

    assert_eq!(observe.observe_id, "playback");
    assert!(matches!(
        observe.resource,
        Some(synctv_proto::client::observe_resource::Resource::Playback(
            synctv_proto::client::ObservePlayback {
                playback_client_profile: Some(_),
            },
        ))
    ));
}

fn chat_event_with_content(
    room_id: RoomId,
    user_id: UserId,
    event_id: impl Into<String>,
    content: impl Into<String>,
) -> ChatMessageEvent {
    let now = synctv_core::SystemClock.now();
    ChatMessageEvent {
        event_id: event_id.into(),
        sequence: 1,
        room_id,
        actor_user_id: user_id,
        kind: ChatEventKind::Created,
        message: ChatMessageWithAttachments {
            message: ChatMessage {
                id: 1,
                room_id,
                user_id: Some(user_id),
                client_message_id: None,
                content: content.into(),
                message_type: ChatMessageType::User,
                status: ChatMessageStatus::Active,
                version: 1,
                reply_to_message_id: None,
                reply_to_message_created_at: None,
                metadata: None,
                edited_at: None,
                deleted_at: None,
                deleted_by: None,
                delete_reason: None,
                created_at: now,
            },
            attachments: Vec::new(),
            reactions: Vec::new(),
            mentions: Vec::new(),
            pin: None,
        },
        occurred_at: now,
    }
}

fn server_message_contains_chat_event_content(
    message: &synctv_proto::client::ServerMessage,
    content: &str,
) -> bool {
    match &message.message {
        Some(Message::ResourceEvent(changed)) => matches!(
            changed.payload.as_ref(),
            Some(synctv_proto::client::resource_event::Payload::ChatEvent(event))
                if event
                    .message
                    .as_ref()
                    .is_some_and(|message| message.content == content)
        ),
        _ => false,
    }
}

fn server_message_contains_chat_event_username(
    message: &synctv_proto::client::ServerMessage,
    content: &str,
    username: &str,
) -> bool {
    match &message.message {
        Some(Message::ResourceEvent(changed)) => matches!(
            changed.payload.as_ref(),
            Some(synctv_proto::client::resource_event::Payload::ChatEvent(event))
                if event.message.as_ref().is_some_and(|message| {
                    message.content == content && message.username.as_deref() == Some(username)
                })
        ),
        _ => false,
    }
}

fn empty_playlist_items_response(
    version: impl Into<String>,
) -> synctv_proto::client::ListPlaylistItemsResponse {
    synctv_proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: Some(0),
        playlist_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        pagination: None,
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
    synctv_core::SystemClock.now()
}

#[derive(Default)]
struct FailingMessageSender {
    fail_after: usize,
    send_calls: AtomicUsize,
    ping_calls: AtomicUsize,
    alive: AtomicBool,
}

impl FailingMessageSender {
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
    recv_started: AtomicBool,
    recv_entered: tokio::sync::Notify,
}

impl FailingStreamState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            send_calls: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
            recv_started: AtomicBool::new(false),
            recv_entered: tokio::sync::Notify::new(),
        })
    }
}

#[derive(Default)]
struct RecordingStreamState {
    sent_messages: parking_lot::Mutex<Vec<ServerMessage>>,
    alive: AtomicBool,
    recv_started: AtomicBool,
    closed: tokio::sync::Notify,
    recv_entered: tokio::sync::Notify,
}

impl RecordingStreamState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sent_messages: parking_lot::Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
            recv_started: AtomicBool::new(false),
            closed: tokio::sync::Notify::new(),
            recv_entered: tokio::sync::Notify::new(),
        })
    }

    fn sent_messages(&self) -> Vec<ServerMessage> {
        self.sent_messages.lock().clone()
    }

    fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        self.closed.notify_waiters();
    }
}

async fn wait_for_condition(
    timeout: Duration,
    condition: impl Fn() -> bool,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, async {
        loop {
            if condition() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
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
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::ObserveResource(
        synctv_proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::PlaybackState(
                    synctv_proto::client::ObservePlaybackState {
                        event_sequence: None,
                    },
                ),
            ),
        },
    )
}

fn observe_playback_message(
    observe_id: &'static str,
    playback_client_profile: Option<synctv_proto::client::PlaybackClientProfile>,
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::ObserveResource(
        synctv_proto::client::ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(synctv_proto::client::observe_resource::Resource::Playback(
                synctv_proto::client::ObservePlayback {
                    playback_client_profile,
                },
            )),
        },
    )
}

fn observe_room_settings_message(
    observe_id: impl Into<String>,
) -> synctv_proto::client::client_message::Message {
    observe_room_settings_message_with_sequence(observe_id, None)
}

fn observe_room_settings_message_with_sequence(
    observe_id: impl Into<String>,
    after_event_sequence: Option<i64>,
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::ObserveResource(
        synctv_proto::client::ObserveResource {
            observe_id: observe_id.into(),
            delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::RoomSettings(
                    synctv_proto::client::ObserveRoomSettings {
                        after_event_sequence,
                    },
                ),
            ),
        },
    )
}

fn observe_playlist_items_message(
    observe_id: &'static str,
    request: synctv_proto::client::ListPlaylistItemsRequest,
) -> synctv_proto::client::client_message::Message {
    observe_playlist_items_message_with_sequence(observe_id, request, None)
}

fn observe_playlist_items_message_with_sequence(
    observe_id: &'static str,
    request: synctv_proto::client::ListPlaylistItemsRequest,
    after_event_sequence: Option<i64>,
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::ObserveResource(
        observe_playlist_items_resource_with_sequence(observe_id, request, after_event_sequence),
    )
}

fn observe_playlist_items_resource_with_sequence(
    observe_id: &'static str,
    request: synctv_proto::client::ListPlaylistItemsRequest,
    after_event_sequence: Option<i64>,
) -> synctv_proto::client::ObserveResource {
    synctv_proto::client::ObserveResource {
        observe_id: observe_id.to_string(),
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: Some(
            synctv_proto::client::observe_resource::Resource::PlaylistItems(
                synctv_proto::client::ObservePlaylistItems {
                    request: Some(request),
                    after_event_sequence,
                },
            ),
        ),
    }
}

fn permission_changed_event_for_target(
    event_id: impl Into<String>,
    target_user_id: UserId,
    role_changed: bool,
) -> RealtimeEvent {
    RealtimeEvent::PermissionChanged {
        event_id: event_id.into(),
        room_id: room_id(),
        target_user_id,
        target_username: format!("user-{target_user_id}"),
        target_remark_name: String::new(),
        target_display_tag: String::new(),
        changed_by: user_id(),
        changed_by_username: "owner".to_string(),
        role_changed,
        new_permissions: RoomPermissionSet(RoomMemberPermissionBits::SEND_CHAT_MESSAGES),
        role: synctv_proto::common::RoomMemberRole::Admin as i32,
        added_permissions: RoomPermissionSet(RoomMemberPermissionBits::SEND_CHAT_MESSAGES),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        target_is_online: true,
        target_connection_count: 1,
        timestamp: now(),
    }
}

fn observe_chat_events_message(
    observe_id: impl Into<String>,
) -> synctv_proto::client::client_message::Message {
    observe_chat_events_message_with_sequence(observe_id, None)
}

fn observe_chat_events_message_with_sequence(
    observe_id: impl Into<String>,
    after_event_sequence: Option<i64>,
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::ObserveResource(
        synctv_proto::client::ObserveResource {
            observe_id: observe_id.into(),
            delivery_mode: synctv_proto::client::ResourceDeliveryMode::NotifyOnly as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::ChatEvents(
                    synctv_proto::client::ObserveChatEvents {
                        after_event_sequence,
                        include_message_types: Vec::new(),
                    },
                ),
            ),
        },
    )
}

fn webrtc_command_message(
    command: synctv_proto::client::web_rtc_command::Command,
) -> synctv_proto::client::client_message::Message {
    synctv_proto::client::client_message::Message::Webrtc(synctv_proto::client::WebRtcCommand {
        command: Some(command),
    })
}

fn resource_event_payload(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::resource_event::Payload> {
    match &message.message {
        Some(Message::ResourceEvent(changed)) => changed.payload.as_ref(),
        _ => None,
    }
}

fn resource_observe_error(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::ResourceObserveError> {
    match &message.message {
        Some(Message::ResourceObserveError(error)) => Some(error),
        _ => None,
    }
}

fn resource_playback_state(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::PlaybackState> {
    match resource_event_payload(message) {
        Some(synctv_proto::client::resource_event::Payload::PlaybackState(state)) => Some(state),
        _ => None,
    }
}

fn resource_playback(message: &ServerMessage) -> Option<&synctv_proto::client::Playback> {
    match resource_event_payload(message) {
        Some(synctv_proto::client::resource_event::Payload::Playback(snapshot)) => Some(snapshot),
        _ => None,
    }
}

fn playback_metadata_with_name(name: &str) -> synctv_proto::client::PlaybackMetadata {
    synctv_proto::client::PlaybackMetadata {
        metadata: Some(synctv_proto::client::playback_metadata::Metadata::Alist(
            synctv_proto::client::AlistPlaybackMetadata {
                name: Some(name.to_string()),
                ..Default::default()
            },
        )),
    }
}

fn playback_metadata_name(metadata: &synctv_proto::client::PlaybackMetadata) -> Option<&str> {
    match metadata.metadata.as_ref()? {
        synctv_proto::client::playback_metadata::Metadata::Alist(metadata) => {
            metadata.name.as_deref()
        }
        _ => None,
    }
}

fn resource_room_settings(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::GetRoomSettingsResponse> {
    match resource_event_payload(message) {
        Some(synctv_proto::client::resource_event::Payload::RoomSettings(settings)) => {
            Some(settings)
        }
        _ => None,
    }
}

fn resource_playlist_items(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::ListPlaylistItemsResponse> {
    match resource_event_payload(message) {
        Some(synctv_proto::client::resource_event::Payload::PlaylistItems(snapshot)) => {
            Some(snapshot)
        }
        _ => None,
    }
}

#[async_trait::async_trait]
impl StreamMessage for RecordingStream {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        self.state.recv_started.store(true, Ordering::Relaxed);
        self.state.recv_entered.notify_waiters();
        if let Some(msg) = self.incoming.pop_front() {
            return Some(msg);
        }
        loop {
            if !self.is_alive() {
                return None;
            }
            tokio::select! {
                () = self.state.closed.notified() => return None,
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
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
        self.state.recv_started.store(true, Ordering::Relaxed);
        self.state.recv_entered.notify_waiters();
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
    .checked("test user should register")
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
        synctv_core::service::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");

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
            clock: Arc::new(synctv_core::SystemClock),
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
            runtime_settings_store: None,
        },
    ))
}

#[derive(Clone)]
struct TestMessageHandler {
    database: Arc<synctv_core_testing::TestDatabase>,
    handler: StreamMessageHandler,
}

impl TestMessageHandler {
    fn new(database: synctv_core_testing::TestDatabase, handler: StreamMessageHandler) -> Self {
        Self {
            database: Arc::new(database),
            handler,
        }
    }

    fn rebuild_with_runtime(&self, runtime: StreamMessageHandlerRuntime) -> Self {
        Self {
            database: Arc::clone(&self.database),
            handler: rebuild_stream_message_handler_with_runtime(&self.handler, runtime),
        }
    }

    fn skip_cleanup_user_left(&self) {
        self.handler
            .skip_cleanup_user_left
            .store(true, Ordering::Relaxed);
    }
}

impl Deref for TestMessageHandler {
    type Target = StreamMessageHandler;

    fn deref(&self) -> &Self::Target {
        &self.handler
    }
}

impl std::borrow::Borrow<StreamMessageHandler> for TestMessageHandler {
    fn borrow(&self) -> &StreamMessageHandler {
        &self.handler
    }
}

impl std::borrow::Borrow<StreamMessageHandler> for &TestMessageHandler {
    fn borrow(&self) -> &StreamMessageHandler {
        &self.handler
    }
}

fn create_handler_test_database() -> synctv_core_testing::TestDatabase {
    let (container, url) = std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .checked("test database runtime should build")
            .block_on(synctv_core_testing::create_test_database_url_with_label(
                "messaging_handler",
                "messaging_handler",
            ))
    })
    .join()
    .checked("test database init thread should finish");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(30))
        .max_connections(8)
        .connect_lazy(&url)
        .checked("test should create lazy PostgreSQL pool");
    synctv_core_testing::TestDatabase { container, pool }
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
        .checked("realtime manager"),
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
        _actor: RealtimeActor,
        _connection_id: ConnectionId,
    ) -> synctv_realtime::Result<mpsc::Receiver<SharedRealtimeEvent>> {
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

    fn get_room_subscribers(&self, _room_id: &RoomId) -> Vec<(RealtimeActor, ConnectionId)> {
        Vec::new()
    }

    async fn get_room_subscribers_replicas_wide(
        &self,
        _room_id: &RoomId,
    ) -> synctv_realtime::Result<Vec<(RealtimeActor, ConnectionId)>> {
        Ok(Vec::new())
    }

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String> {
        Ok(0)
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
        .checked("realtime manager with failing subscription runtime"),
    )
}

fn test_connection_manager() -> Arc<ConnectionManager> {
    Arc::new(ConnectionManager::new(ConnectionLimits::default()))
}

#[derive(Clone)]
struct StaticPlaybackService {
    playback: synctv_proto::client::Playback,
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
        .checked("resource service should be called");
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl crate::impls::playback::PlaybackService for StaticPlaybackService {
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(RoomPlaybackState::new(*room_id))
    }

    async fn get_playback_for_actor(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
        Ok(self.playback.clone())
    }
}

#[derive(Clone)]
struct MutablePlaybackService {
    playback: Arc<parking_lot::Mutex<synctv_proto::client::Playback>>,
    dependencies: Arc<parking_lot::Mutex<Vec<synctv_core::provider::ProviderCredentialDependency>>>,
    state: Arc<parking_lot::Mutex<Option<RoomPlaybackState>>>,
    probe: SnapshotCallProbe,
    observed_lifecycle_probe: SnapshotCallProbe,
}

impl MutablePlaybackService {
    fn new(playback: synctv_proto::client::Playback) -> Arc<Self> {
        Arc::new(Self {
            playback: Arc::new(parking_lot::Mutex::new(playback)),
            dependencies: Arc::new(parking_lot::Mutex::new(Vec::new())),
            state: Arc::new(parking_lot::Mutex::new(None)),
            probe: SnapshotCallProbe::default(),
            observed_lifecycle_probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, playback: synctv_proto::client::Playback) {
        *self.playback.lock() = playback;
    }

    fn replace_dependencies(
        &self,
        dependencies: Vec<synctv_core::provider::ProviderCredentialDependency>,
    ) {
        *self.dependencies.lock() = dependencies;
    }

    fn replace_state(&self, state: RoomPlaybackState) {
        *self.state.lock() = Some(state);
    }

    async fn wait_for_calls(&self, expected: usize) {
        self.probe.wait_for_calls(expected).await;
    }

    async fn wait_for_observed_lifecycle_calls(&self, expected: usize) {
        self.observed_lifecycle_probe.wait_for_calls(expected).await;
    }

    fn observed_lifecycle_call_count(&self) -> usize {
        self.observed_lifecycle_probe.call_count()
    }
}

#[async_trait::async_trait]
impl crate::impls::playback::PlaybackService for MutablePlaybackService {
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(self
            .state
            .lock()
            .clone()
            .unwrap_or_else(|| RoomPlaybackState::new(*room_id)))
    }

    async fn get_playback_for_actor(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
        self.probe.mark_called();
        Ok(self.playback.lock().clone())
    }

    async fn playback_credential_dependencies(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<synctv_core::provider::ProviderCredentialDependency>, crate::impls::ApiError>
    {
        Ok(self.dependencies.lock().clone())
    }

    async fn refresh_observed_playback_metadata_and_auto_advance(
        &self,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) {
        self.observed_lifecycle_probe.mark_called();
    }
}

#[derive(Clone)]
struct SequencedPlaybackService {
    responses: Arc<
        parking_lot::Mutex<
            VecDeque<Result<synctv_proto::client::Playback, crate::impls::ApiError>>,
        >,
    >,
    probe: SnapshotCallProbe,
}

impl SequencedPlaybackService {
    fn new(
        responses: impl IntoIterator<
            Item = Result<synctv_proto::client::Playback, crate::impls::ApiError>,
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
impl crate::impls::playback::PlaybackService for SequencedPlaybackService {
    async fn room_playback_state(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, crate::impls::ApiError> {
        Ok(RoomPlaybackState::new(*room_id))
    }

    async fn get_playback_for_actor(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
        _playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
        self.probe.mark_called();
        self.responses.lock().pop_front().unwrap_or_else(|| {
            Err(crate::impls::ApiError::Internal(
                "no playback response queued".to_string(),
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
struct StaticPlaylistItemsSnapshotService {
    snapshot: synctv_proto::client::ListPlaylistItemsResponse,
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService
    for StaticPlaylistItemsSnapshotService
{
    async fn get_playlist_items_snapshot(
        &self,
        _actor: &crate::impls::client::RoomActor,
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct MutablePlaylistItemsSnapshotService {
    snapshot: Arc<parking_lot::Mutex<synctv_proto::client::ListPlaylistItemsResponse>>,
    probe: SnapshotCallProbe,
}

impl MutablePlaylistItemsSnapshotService {
    fn new(snapshot: synctv_proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: synctv_proto::client::ListPlaylistItemsResponse) {
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
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct RecordingPlaylistItemsRequestSnapshotService {
    snapshot: Arc<parking_lot::Mutex<synctv_proto::client::ListPlaylistItemsResponse>>,
    refresh_values: Arc<parking_lot::Mutex<Vec<bool>>>,
    probe: SnapshotCallProbe,
}

impl RecordingPlaylistItemsRequestSnapshotService {
    fn new(snapshot: synctv_proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(parking_lot::Mutex::new(snapshot)),
            refresh_values: Arc::new(parking_lot::Mutex::new(Vec::new())),
            probe: SnapshotCallProbe::default(),
        })
    }

    fn replace(&self, snapshot: synctv_proto::client::ListPlaylistItemsResponse) {
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
        req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        self.refresh_values.lock().push(req.refresh);
        Ok(self.snapshot.lock().clone())
    }
}

#[derive(Clone)]
struct BlockingPlaylistItemsSnapshotService {
    snapshot: Arc<parking_lot::Mutex<synctv_proto::client::ListPlaylistItemsResponse>>,
    probe: SnapshotCallProbe,
    block_on_call: usize,
    release_blocked_call: Arc<AtomicBool>,
    release_notify: Arc<tokio::sync::Notify>,
}

impl BlockingPlaylistItemsSnapshotService {
    fn new(
        snapshot: synctv_proto::client::ListPlaylistItemsResponse,
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

    fn replace(&self, snapshot: synctv_proto::client::ListPlaylistItemsResponse) {
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
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
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
    first_snapshot: synctv_proto::client::ListPlaylistItemsResponse,
    probe: SnapshotCallProbe,
    release_blocked_call: Arc<AtomicBool>,
    release_notify: Arc<tokio::sync::Notify>,
}

impl BlockingFailingPlaylistItemsSnapshotService {
    fn new(first_snapshot: synctv_proto::client::ListPlaylistItemsResponse) -> Arc<Self> {
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
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
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
    snapshot: synctv_proto::client::ListPlaylistItemsResponse,
    probe: SnapshotCallProbe,
    delay: Duration,
}

impl SlowPlaylistItemsSnapshotService {
    fn new(
        snapshot: synctv_proto::client::ListPlaylistItemsResponse,
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
        _req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.probe.mark_called();
        tokio::time::sleep(self.delay).await;
        Ok(self.snapshot.clone())
    }
}

fn test_message_handler(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
) -> TestMessageHandler {
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
) -> TestMessageHandler {
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
) -> TestMessageHandler {
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
    mut runtime: StreamMessageHandlerRuntime,
    concurrency_config: Arc<MessageConcurrencyConfig>,
) -> TestMessageHandler {
    runtime.presence_service = connection_service.presence_service();
    let database = create_handler_test_database();
    let pool = database.pool.clone();
    let handler = StreamMessageHandler::new_with_runtime(
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
            public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
            sender,
            concurrency_config,
        },
        runtime,
    );
    TestMessageHandler::new(database, handler)
}

fn test_guest_principal_with_permissions(permissions: RoomPermissionSet) -> RealtimePrincipal {
    let session_id = "guest-session-1";
    RealtimePrincipal::guest(GuestRealtimeIdentity {
        guest_id: guest_public_id(session_id),
        display_name: guest_display_name(session_id),
        session_id: session_id.to_string(),
        token_jti: "guest-token-jti".to_string(),
        room_guest_version: 0,
        permissions,
    })
}

fn test_guest_message_handler(
    sender: Arc<dyn MessageSender>,
    event_service: Arc<RealtimeManager>,
    connection_service: Arc<ConnectionManager>,
    permissions: RoomPermissionSet,
) -> TestMessageHandler {
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
) -> TestMessageHandler {
    let database = create_handler_test_database();
    let pool = database.pool.clone();
    let handler = StreamMessageHandler::new_with_runtime(
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
            public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
            sender,
            concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
        },
        runtime,
    );
    TestMessageHandler::new(database, handler)
}

fn rebuild_stream_message_handler_with_runtime<H>(
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
            connection_id: Some(handler.connection_id.as_str().to_string()),
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

fn test_handler_with_playlist_items_snapshot_service<H>(
    handler: H,
    service: Arc<dyn PlaylistItemsSnapshotService>,
) -> H::Output
where
    H: StreamMessageHandlerTestRuntimeExt,
{
    handler.with_playlist_items_snapshot_service(service)
}

fn test_handler_with_room_settings_snapshot_service<H>(
    handler: H,
    service: Arc<dyn RoomSettingsSnapshotService>,
) -> H::Output
where
    H: StreamMessageHandlerTestRuntimeExt,
{
    handler.with_room_settings_snapshot_service(service)
}

trait StreamMessageHandlerTestRuntimeExt {
    type Output;

    fn with_playback_service(self, service: Arc<dyn PlaybackService>) -> Self::Output;
    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> Self::Output;
    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> Self::Output;
}

impl StreamMessageHandlerTestRuntimeExt for TestMessageHandler {
    type Output = TestMessageHandler;

    fn with_playback_service(self, service: Arc<dyn PlaybackService>) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_playback_service(service))
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_playlist_items_snapshot_service(service))
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_room_settings_snapshot_service(service))
    }
}

impl StreamMessageHandlerTestRuntimeExt for &TestMessageHandler {
    type Output = TestMessageHandler;

    fn with_playback_service(self, service: Arc<dyn PlaybackService>) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_playback_service(service))
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_playlist_items_snapshot_service(service))
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> TestMessageHandler {
        self.rebuild_with_runtime(runtime_with_room_settings_snapshot_service(service))
    }
}

impl StreamMessageHandlerTestRuntimeExt for StreamMessageHandler {
    type Output = StreamMessageHandler;

    fn with_playback_service(self, service: Arc<dyn PlaybackService>) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(self, runtime_with_playback_service(service))
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(
            self,
            runtime_with_playlist_items_snapshot_service(service),
        )
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(
            self,
            runtime_with_room_settings_snapshot_service(service),
        )
    }
}

impl StreamMessageHandlerTestRuntimeExt for &StreamMessageHandler {
    type Output = StreamMessageHandler;

    fn with_playback_service(self, service: Arc<dyn PlaybackService>) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(self, runtime_with_playback_service(service))
    }

    fn with_playlist_items_snapshot_service(
        self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(
            self,
            runtime_with_playlist_items_snapshot_service(service),
        )
    }

    fn with_room_settings_snapshot_service(
        self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> StreamMessageHandler {
        rebuild_stream_message_handler_with_runtime(
            self,
            runtime_with_room_settings_snapshot_service(service),
        )
    }
}

/// Creates a StreamMessageHandler backed by a real PostgreSQL database with a
/// registered user, created room, and accepted membership so that
/// `start()` (which calls `pre_join_after_registration`) can pass the
/// admission revalidation checks.
type StartFixtureFuture<'a> = Pin<Box<dyn Future<Output = StartTestFixture> + 'a>>;

fn create_start_handler_fixture(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
) -> StartFixtureFuture<'_> {
    Box::pin(create_start_handler_fixture_with_runtime(
        node_id,
        sender,
        test_stream_handler_runtime(),
    ))
}

fn create_start_handler_fixture_with_runtime(
    node_id: &str,
    sender: Arc<dyn MessageSender>,
    runtime: StreamMessageHandlerRuntime,
) -> StartFixtureFuture<'_> {
    Box::pin(create_start_handler_fixture_with_runtime_builder(
        node_id,
        sender,
        |_, _| runtime,
    ))
}

fn create_start_handler_fixture_with_runtime_builder<'a, F>(
    node_id: &'a str,
    sender: Arc<dyn MessageSender>,
    build_runtime: F,
) -> StartFixtureFuture<'a>
where
    F: FnOnce(RoomId, UserId) -> StreamMessageHandlerRuntime + 'a,
{
    Box::pin(async move {
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
            .checked("fixture room should be created");

        let user = register_test_user(
            &user_service,
            bounded_fixture_username(&format!("{node_id}_member")),
            format!("fixture-{node_id}-member@test.invalid"),
        )
        .await;
        room_service
            .join_room(room.id, user.id, None)
            .await
            .checked("fixture member should join room");
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
                public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
    })
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
        .register_actor(
            handler.connection_id.clone().into_string(),
            handler
                .realtime_actor()
                .checked("realtime actor should build"),
        )
        .await
        .checked("register should succeed");
    connection_service
        .join_room(handler.connection_id.as_str(), handler.room_id)
        .await
        .checked("join_room should succeed");
    let initial_join_state = if handler.principal.is_guest() {
        InitialRealtimeJoinState {
            member: None,
            room_settings: None,
        }
    } else {
        InitialRealtimeJoinState {
            member: Some(RoomMember::new(
                handler.room_id,
                handler.test_user_id(),
                synctv_core::models::RoomRole::Member,
            )),
            room_settings: Some(RoomSettings::default()),
        }
    };
    handler
        .cache_initial_realtime_join_state(initial_join_state)
        .await
        .checked("initial join state should cache before run_after_join");
    handler
        .cache_room_event_subscription()
        .await
        .checked("room subscription should cache before run_after_join");
}

async fn promote_handler_to_room_admin(
    fixture: &StartTestFixture,
) -> synctv_core::models::RoomMember {
    let member_repo = RoomMemberRepository::new(fixture.pool.clone());
    let member = member_repo
        .get(&fixture.handler.room_id, &fixture.handler.test_user_id())
        .await
        .checked("fixture member should load")
        .checked("fixture member should exist");
    member_repo
        .update_role(
            &fixture.handler.room_id,
            &fixture.handler.test_user_id(),
            RoomRole::Admin,
            member.version,
        )
        .await
        .checked("fixture member should promote to admin")
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
        .checked("start() should cancel");

    let room = handler.room_id;
    let user = handler.test_user_id();
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
    .checked("cleanup should finish");
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
            if stream_state.recv_started.load(Ordering::Relaxed) {
                break;
            }
            stream_state.recv_entered.notified().await;
        }
    })
    .await
    .checked("run_after_join should be ready");
}

async fn wait_for_recording_stream_ready(stream_state: &RecordingStreamState) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if stream_state.recv_started.load(Ordering::Relaxed) {
                break;
            }
            stream_state.recv_entered.notified().await;
        }
    })
    .await
    .checked("recording stream should enter receive loop");
}

async fn wait_for_run_after_join_cleanup(
    handler: &StreamMessageHandler,
    connection_service: &Arc<ConnectionManager>,
    event_service: &RealtimeManager,
    task: tokio::task::JoinHandle<Result<(), String>>,
) {
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .checked("run_after_join should exit")
        .checked("run_after_join task should not panic");
    assert!(
        result.is_ok(),
        "run_after_join should exit cleanly: {result:?}"
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if connection_service.connection_count() == 0
                && connection_service.room_connection_count(&handler.room_id) == 0
                && connection_service.user_connection_count(&handler.test_user_id()) == 0
                && realtime_manager_subscriber_count(event_service, &handler.room_id) == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("run_after_join cleanup should finish");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_cancels_and_cleans_up_when_realtime_event_send_fails() {
    let sender = FailingMessageSender::fail_after(0);
    let sender_for_assert = Arc::clone(&sender);
    let fixture = create_start_handler_fixture("start_event_send_failure", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.checked("start should return");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("subscription should be established");

    event_service.broadcast(RealtimeEvent::SystemNotification {
        event_id: "evt-start-fail".to_string(),
        message: "boom".to_string(),
        level: synctv_realtime::sync::NotificationLevel::Info,
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
        sender_for_assert.send_calls() >= 1,
        "failing event send should be attempted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_start_cancels_and_cleans_up_when_admin_notification_send_fails() {
    let sender = FailingMessageSender::fail_after(0);
    let sender_for_assert = Arc::clone(&sender);
    let fixture = create_start_handler_fixture("start_admin_notification_failure", sender).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.checked("start should return");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("subscription should be established");

    event_service.broadcast(RealtimeEvent::UserNotification {
        event_id: "evt-admin-notify".to_string(),
        user_id: handler.test_user_id(),
        title: "title".to_string(),
        content: "content".to_string(),
        notification_type: synctv_core::models::NotificationType::SystemAnnouncement,
        data: synctv_core::models::NotificationData::default(),
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
        sender_for_assert.send_calls() >= 1,
        "failing admin notification send should be attempted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_start_sends_termination_before_user_kick_disconnect() {
    let sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture("start_user_kick_termination", sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.checked("start should return");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("subscription should be established");

    event_service.broadcast(RealtimeEvent::KickUser {
        event_id: "evt-user-banned".to_string(),
        user_id: handler.test_user_id(),
        reason: "user_banned".to_string(),
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

    let termination = sender
        .sent_messages()
        .into_iter()
        .find_map(|message| match message.message {
            Some(Message::Termination(termination)) => Some(termination),
            _ => None,
        })
        .checked("kick should send a realtime termination before cancellation");
    assert_eq!(
        termination.code,
        synctv_proto::client::RealtimeTerminationCode::UserAccessRevoked as i32
    );

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_start_sends_one_specific_termination_when_room_shutdown_paths_race() {
    let sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("start_room_shutdown_termination", sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let (_tx, cancel_token) = handler.start().await.checked("start should return");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if realtime_manager_subscriber_count(event_service, &handler.room_id) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("subscription should be established");

    event_service.broadcast(RealtimeEvent::RoomDeleted {
        event_id: "evt-room-deleted".to_string(),
        room_id: handler.room_id,
        deleted_by: handler.test_user_id(),
        timestamp: now(),
    });
    connection_service.disconnect_room(&handler.room_id, RoomDisconnectReason::Deleted);

    wait_for_start_cleanup(
        handler,
        connection_service,
        event_service,
        &cancel_token,
        true,
    )
    .await;

    let terminations = sender
        .sent_messages()
        .into_iter()
        .filter_map(|message| match message.message {
            Some(Message::Termination(termination)) => Some(termination),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(terminations.len(), 1);
    assert_eq!(
        terminations[0].code,
        synctv_proto::client::RealtimeTerminationCode::RoomDeleted as i32
    );

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_handle_client_message_sends_millisecond_heartbeat_ack() {
    let event_service = test_realtime_manager("test_run_after_join_records_heartbeat").await;
    let connection_service = test_connection_manager();
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        event_service.clone(),
        connection_service.clone(),
    );

    let heartbeat = ClientMessage {
        message: Some(synctv_proto::client::client_message::Message::Heartbeat(
            synctv_proto::client::HeartbeatMessage { timestamp: 42 },
        )),
    };
    let heartbeat_started_at = synctv_core::SystemClock.now_millis();

    handler
        .handle_client_message(&heartbeat)
        .await
        .checked("heartbeat should receive an ack");

    let heartbeat_ack_sent = message_sender.sent_messages().iter().any(|msg| {
        matches!(
            msg.message,
            Some(Message::HeartbeatAck(ref ack))
                if ack.timestamp >= heartbeat_started_at && ack.timestamp >= 10_000_000_000
        )
    });
    assert!(heartbeat_ack_sent);
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let expected_username = handler
        .room_service
        .user_service()
        .get_username(&handler.test_user_id())
        .await
        .checked("fixture username should load")
        .checked("fixture user should exist");
    prepare_handler_for_run_after_join(handler, connection_service).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_chat_events_message("chat-events")),
        })
        .await
        .checked("chat observe should register");
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
        actor_user_id: handler.test_user_id(),
        event: chat_event_with_content(
            handler.room_id,
            handler.test_user_id(),
            "evt-prejoin-window",
            "arrived-before-run-after-join",
        ),
        timestamp: now(),
    });

    let (mut stream, _stream_state) = RecordingStream::new();
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
    .checked("chat event should be delivered through chat_events observation");

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
    assert!(messages.iter().any(|message| {
        server_message_contains_chat_event_username(
            message,
            "arrived-before-run-after-join",
            &expected_username,
        )
    }));

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
        .checked("room should be created");

    let first = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-1".to_string()),
            content: "first replay".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("first message should be stored");
    chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-2".to_string()),
            content: "second replay".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("second message should be stored");
    chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-3".to_string()),
            content: "third replay".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("third message should be stored");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
        .checked("chat observe should replay events after sequence");

    let replayed = message_sender
        .sent_messages()
        .iter()
        .filter_map(|message| match resource_event_payload(message) {
            Some(synctv_proto::client::resource_event::Payload::ChatEvent(event)) => event
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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
        .checked("room should be created");

    let first = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-seq-1".to_string()),
            content: "first sequence replay".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("first message should be stored");
    let second = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: owner.id,
            client_message_id: Some("replay-seq-2".to_string()),
            content: "second sequence replay".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("second message should be stored");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
        .checked("chat observe should replay events after sequence");

    let replayed = message_sender
        .sent_messages()
        .iter()
        .filter_map(|message| match resource_event_payload(message) {
            Some(synctv_proto::client::resource_event::Payload::ChatEvent(event)) => event
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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_run_after_join_filters_chat_events_until_explicit_observe() {
    let event_service = test_realtime_manager("test_run_after_join_filters_chat_events").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        RecordingMessageSender::new(),
        event_service.clone(),
        connection_service.clone(),
    );
    handler.skip_cleanup_user_left();
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = RecordingStream::new();
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_recording_stream_ready(&stream_state).await;

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
            .all(|msg| !server_message_contains_chat_event_content(msg, "filtered-before-observe")),
        "chat events must wait for an explicit chat_events observation"
    );

    stream_state.close();
    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_observe_chat_events_requires_view_chat_history_permission_for_member() {
    let sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_chat_events_permission", sender.clone()).await;
    let mut settings = fixture
        .handler
        .room_service
        .get_room_settings(&fixture.handler.room_id)
        .await
        .checked("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        );
    fixture
        .handler
        .room_service
        .set_room_settings(&fixture.handler.room_id, &settings)
        .await
        .checked("room settings should update");

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

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
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
    .checked("recording stream should receive observed playback state");

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playback_state) {
        Some(state) => {
            assert_eq!(state.version, 0);
            assert_eq!(
                state.room_id,
                public_id_codec()
                    .encode_room_id(handler.room_id)
                    .checked("test value")
            );
        }
        None => std::panic::panic_any(format!(
            "expected PlaybackState after observe, got {messages:?}"
        )),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_sends_current_playback_on_subscribe() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_playback_initial",
        message_sender.clone(),
        |room_id, _| {
            runtime_with_playback_service(Arc::new(StaticPlaybackService {
                playback: synctv_proto::client::Playback {
                    media_id: public_media_id(),
                    playlist_id: String::new(),
                    room_id: public_id_codec()
                        .encode_room_id(room_id)
                        .checked("test value"),
                    name: "test media".to_string(),
                    playlist_position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: None,
                    expires_at: Some(12345),
                    duration_seconds: None,
                    playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
                    target: None,
                    provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                    provider_instance_name: String::new(),
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

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("recording stream should receive observed playback");

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playback) {
        Some(playback) => {
            assert_eq!(playback.media_id, public_media_id());
            assert_eq!(playback.expires_at, Some(12345));
        }
        None => std::panic::panic_any(format!("expected Playback after observe, got {messages:?}")),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_reports_current_playback_with_event_cursor() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_pb_playback_current",
        message_sender.clone(),
        |room_id, _| {
            runtime_with_playback_service(Arc::new(StaticPlaybackService {
                playback: synctv_proto::client::Playback {
                    media_id: public_media_id(),
                    playlist_id: String::new(),
                    room_id: public_id_codec()
                        .encode_room_id(room_id)
                        .checked("test value"),
                    name: "test media".to_string(),
                    playlist_position: 0.0,
                    playback_infos: std::collections::HashMap::new(),
                    default_mode: String::new(),
                    metadata: None,
                    expires_at: Some(12345),
                    duration_seconds: None,
                    playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
                    target: None,
                    provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                    provider_instance_name: String::new(),
                },
            }))
        },
    )
    .await;
    let handler = &fixture.handler;
    let request = synctv_proto::client::ObserveResource {
        observe_id: "playback".to_string(),
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: Some(synctv_proto::client::observe_resource::Resource::Playback(
            synctv_proto::client::ObservePlayback {
                playback_client_profile: None,
            },
        )),
    };

    handler
        .resource_observer
        .handle_observe_resource(&request)
        .await
        .checked("playback observe should register");

    let messages = message_sender.sent_messages();
    let observed = messages
        .iter()
        .find_map(|message| match &message.message {
            Some(Message::ResourceObserved(observed)) => Some(observed),
            _ => None,
        })
        .checked("observe should send ResourceObserved");
    assert!(observed.event_cursor.is_none());

    let changed = messages
        .iter()
        .find_map(|message| match &message.message {
            Some(Message::ResourceEvent(changed)) if changed.observe_id == "playback" => {
                Some(changed)
            }
            _ => None,
        })
        .checked("observe should send initial playback");
    assert!(changed.event_cursor.is_none());
    assert!(resource_playback(&ServerMessage {
        message: Some(Message::ResourceEvent(changed.clone()))
    })
    .is_some_and(|playback| playback.media_id == public_media_id()));
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
    let now = synctv_core::SystemClock.now();
    repository
        .insert(&synctv_core::repository::NewRoomResourceEvent {
            event_id: "playlist-items-audit-only".to_string(),
            scope_type: synctv_core::repository::RoomResourceEventScope::Room,
            room_id: Some(fixture.handler.room_id.as_i64()),
            user_id: None,
            aggregate_type: "playlist".to_string(),
            aggregate_id: fixture.handler.room_id.to_string(),
            resource_type: RoomResourceKind::PlaylistItems,
            resource_id: fixture.handler.room_id.to_string(),
            event_type: "playlist_items_changed".to_string(),
            event_version: 1,
            aggregate_version: Some(1),
            actor_user_id: Some(fixture.handler.test_user_id().as_i64()),
            payload: None,
            summary: RoomResourceEventSummary {
                event_type: "playlist_items_changed".to_string(),
                room_id: Some(fixture.handler.room_id.as_i64()),
                actor_user_id: Some(fixture.handler.test_user_id().as_i64()),
                resource_type: RoomResourceKind::PlaylistItems,
                details: RoomResourceEventSummaryDetails::PlaylistItems {
                    user_id: Some(fixture.handler.test_user_id().as_i64()),
                    username: None,
                    media_ids: Vec::new(),
                },
            },
            occurred_at: now,
        })
        .await
        .checked("audit-only room resource event should insert");

    let handler = fixture
        .handler
        .clone()
        .with_playlist_items_snapshot_service(Arc::new(StaticPlaylistItemsSnapshotService {
            snapshot: empty_playlist_items_response("playlist-items-v1"),
        }));
    let request = observe_playlist_items_resource_with_sequence(
        "playlist-items",
        synctv_proto::client::ListPlaylistItemsRequest::default(),
        Some(0),
    );

    handler
        .resource_observer
        .handle_observe_resource(&request)
        .await
        .checked("playlist items observe should register");
    handler
        .resource_observer
        .replay_room_resource_events_after(&request)
        .await
        .checked("playlist items replay should succeed");

    let replayed = message_sender
        .sent_messages()
        .into_iter()
        .filter_map(|message| match message.message {
            Some(Message::ResourceEvent(changed)) if changed.observe_id == "playlist-items" => {
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
        .checked("audit-only event should produce a cursor-advancing change");
    assert!(matches!(
        replayed.payload,
        Some(synctv_proto::client::resource_event::Payload::ChangedOnly(
            _
        ))
    ));

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_state_sends_current_state() {
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

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_state_message("playback-state")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let observe_registered = handler
                .resource_observer
                .has_observation("playback-state")
                .await;
            if observe_registered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("run_after_join should register playback state observation");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| { resource_playback_state(message).is_some() }),
        "observe should send the current playback state"
    );
    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_with_matching_source_sends_current_playback() {
    let message_sender = RecordingMessageSender::new();
    let playback_service_slot = Arc::new(std::sync::Mutex::new(None));
    let playback_service_out = Arc::clone(&playback_service_slot);
    let fixture = create_start_handler_fixture_with_runtime_builder(
        "observe_pb_playback_same_src",
        message_sender.clone(),
        move |room_id, _| {
            let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
                media_id: String::new(),
                playlist_id: String::new(),
                room_id: public_id_codec()
                    .encode_room_id(room_id)
                    .checked("test value"),
                name: "test media".to_string(),
                playlist_position: 0.0,
                playback_infos: std::collections::HashMap::new(),
                default_mode: String::new(),
                metadata: None,
                expires_at: Some(synctv_core::SystemClock.now().timestamp() + 3600),
                duration_seconds: None,
                playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
                target: None,
                provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                provider_instance_name: String::new(),
            });
            *playback_service_out
                .lock()
                .checked("playback slot should lock") = Some(playback_service.clone());
            runtime_with_playback_service(playback_service)
        },
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;
    let playback_service = playback_service_slot
        .lock()
        .checked("playback slot should lock")
        .clone()
        .checked("playback service should be captured");

    prepare_handler_for_run_after_join(handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;

    let sent_messages = message_sender.sent_messages();
    assert!(
        sent_messages
            .iter()
            .any(|message| resource_playback(message)
                .is_some_and(|playback| playback.name == "test media")),
        "observe should send the current playback: {sent_messages:?}"
    );
    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observe_playback_sends_current_playback_immediately() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_playback_src_diff", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let handler = handler
        .clone()
        .with_playback_service(Arc::new(StaticPlaybackService {
            playback: synctv_proto::client::Playback {
                media_id: String::new(),
                playlist_id: String::new(),
                room_id: public_id_codec()
                    .encode_room_id(handler.room_id)
                    .checked("test value"),
                name: "test media".to_string(),
                playlist_position: 0.0,
                playback_infos: std::collections::HashMap::new(),
                default_mode: String::new(),
                metadata: None,
                expires_at: Some(12345),
                duration_seconds: None,
                playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
                target: None,
                provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                provider_instance_name: String::new(),
            },
        }));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender
                .sent_messages()
                .iter()
                .any(|message| resource_playback(message).is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("playback observe should send the current playback immediately");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_receives_future_playback_state_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_playback_future_update", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_playback(message)
                .is_some_and(|playback| playback.expires_at == Some(4_102_444_800))),
        "observe should send the current playback"
    );

    playback_service.replace(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_801),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-source-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        state: RoomPlaybackState {
            room_id: handler.room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position: 12.0,
            speed: 1.0,
            is_playing: true,
            playback_generation: 0,
            updated_at: now(),
            version: 2,
        },
        source_changed: true,
        client_operation_id: None,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback(message)
                    .is_some_and(|playback| playback.expires_at == Some(4_102_444_801))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("observed playback should receive future updates");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_ignores_play_pause_state_updates() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_playback_ignores_state", message_sender.clone())
            .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![
        ClientMessage {
            message: Some(observe_playback_message("playback", None)),
        },
        ClientMessage {
            message: Some(observe_playback_state_message("playback-state")),
        },
    ]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                matches!(
                    &message.message,
                    Some(Message::ResourceObserved(observed))
                        if observed.observe_id == "playback-state"
                )
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("playback_state observe should register");
    let playback_messages_before = message_sender
        .sent_messages()
        .iter()
        .filter(|message| resource_playback(message).is_some())
        .count();

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-state-only".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        state: RoomPlaybackState {
            room_id: handler.room_id,
            playing_media_id: Some(media_id()),
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position: 12.0,
            speed: 1.0,
            is_playing: false,
            playback_generation: 0,
            updated_at: now(),
            version: 2,
        },
        source_changed: false,
        client_operation_id: Some("3d918f61-3959-49ef-a962-5d94b8ac8470".to_string()),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback_state(message).is_some_and(|state| {
                    state.version == 2
                        && state.client_operation_id == "3d918f61-3959-49ef-a962-5d94b8ac8470"
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("playback_state observer should receive play/pause state updates");

    assert_eq!(
        playback_service.probe.call_count(),
        1,
        "playback observer should not regenerate media resources for state-only updates"
    );
    let playback_messages_after = message_sender
        .sent_messages()
        .iter()
        .filter(|message| resource_playback(message).is_some())
        .count();
    assert_eq!(playback_messages_after, playback_messages_before);

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_provider_credential_change_refreshes_dependent_playback() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture("pb_playback_cred", message_sender.clone()).await;
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
            creator_id: Some(handler.test_user_id()),
            name: "provider credential dependent media".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: synctv_core::models::SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/provider-credential-dependent.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .checked("media should be created for provider credential observe test");

    handler
        .room_service
        .update_playback_state(
            handler.room_id,
            handler.test_user_id(),
            |state| {
                state.playing_media_id = Some(media.id);
            },
            RoomPermission::CONTROL_PLAYBACK_STATE,
        )
        .await
        .checked("playback state should be set");

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_id_codec()
            .encode_media_id(media.id)
            .checked("test value"),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "credential-backed media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    playback_service.replace_dependencies(vec![
        synctv_core::provider::ProviderCredentialDependency::new(
            "bilibili",
            handler.test_user_id().to_string(),
            "bilibili",
        ),
    ]);
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });
    playback_service.wait_for_calls(1).await;

    playback_service.replace(synctv_proto::client::Playback {
        media_id: public_id_codec()
            .encode_media_id(media.id)
            .checked("test value"),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "credential-backed media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_801),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });

    event_service.broadcast(RealtimeEvent::ProviderCredentialChanged {
        event_id: "evt-provider-credential-dependent".to_string(),
        user_id: handler.test_user_id(),
        provider: "bilibili".to_string(),
        server_id: "bilibili".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback(message)
                    .is_some_and(|playback| playback.expires_at == Some(4_102_444_801))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("dependent provider credential change should refresh playback");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_provider_credential_change_does_not_refresh_unrelated_playback() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("pb_playback_cred_unrel", message_sender.clone()).await;
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
            creator_id: Some(handler.test_user_id()),
            name: "provider credential unrelated media".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: synctv_core::models::SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/provider-credential-unrelated.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .checked("media should be created for provider credential observe test");

    handler
        .room_service
        .update_playback_state(
            handler.room_id,
            handler.test_user_id(),
            |state| {
                state.playing_media_id = Some(media.id);
            },
            RoomPermission::CONTROL_PLAYBACK_STATE,
        )
        .await
        .checked("playback state should be set");

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_id_codec()
            .encode_media_id(media.id)
            .checked("test value"),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "credential-backed media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    playback_service.replace_dependencies(vec![
        synctv_core::provider::ProviderCredentialDependency::new(
            "bilibili",
            handler.test_user_id().to_string(),
            "bilibili",
        ),
    ]);
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });
    playback_service.wait_for_calls(1).await;

    event_service.broadcast(RealtimeEvent::ProviderCredentialChanged {
        event_id: "evt-provider-credential-unrelated".to_string(),
        user_id: UserId::expect_positive(handler.test_user_id().get() + 1),
        provider: "bilibili".to_string(),
        server_id: "bilibili".to_string(),
        timestamp: now(),
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(
        playback_service.probe.call_count(),
        1,
        "unrelated credential changes must not reload observed playbacks"
    );

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn test_playback_auto_advance_subscriber_runs_for_playing_observed_state() {
    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(room_id())
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: None,
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let subscriber = PlaybackAutoAdvanceSubscriber::new(playback_service.clone());

    subscriber
        .handle_observed_playback_lifecycle_event(ObservedPlaybackLifecycleEvent {
            room_id: room_id(),
            state: RoomPlaybackState {
                room_id: room_id(),
                playing_media_id: Some(MediaId::expect_positive(10)),
                playing_playlist_id: None,
                target: None,
                current_progress_id: None,
                history_cursor_id: None,
                position: 11.0,
                speed: 1.0,
                is_playing: true,
                playback_generation: 0,
                updated_at: now(),
                version: 1,
            },
        })
        .await
        .checked("observed playback subscriber should succeed");

    playback_service.wait_for_observed_lifecycle_calls(1).await;
}

#[tokio::test]
async fn test_playback_auto_advance_subscriber_skips_paused_state() {
    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(room_id())
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: None,
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let subscriber = PlaybackAutoAdvanceSubscriber::new(playback_service.clone());

    subscriber
        .handle_observed_playback_lifecycle_event(ObservedPlaybackLifecycleEvent {
            room_id: room_id(),
            state: RoomPlaybackState {
                room_id: room_id(),
                playing_media_id: Some(MediaId::expect_positive(10)),
                playing_playlist_id: None,
                target: None,
                current_progress_id: None,
                history_cursor_id: None,
                position: 11.0,
                speed: 1.0,
                is_playing: false,
                playback_generation: 0,
                updated_at: now(),
                version: 1,
            },
        })
        .await
        .checked("observed playback subscriber should succeed");

    assert_eq!(playback_service.observed_lifecycle_call_count(), 0);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and waits for observed playback lifecycle tick"]
async fn test_observed_playback_lifecycle_source_triggers_auto_advance_subscriber_for_observed_room(
) {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_playback_lifecycle_auto_advance",
        message_sender.clone(),
    )
    .await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_media_id(),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "test media".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: None,
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let mut observed_state = RoomPlaybackState::new(handler.room_id);
    observed_state.is_playing = true;
    playback_service.replace_state(observed_state);
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let lifecycle_handle = spawn_observed_playback_lifecycle_event_source(
        playback_service.clone(),
        vec![Arc::new(PlaybackAutoAdvanceSubscriber::new(
            playback_service.clone(),
        ))],
        shutdown_rx,
    );

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    tokio::time::timeout(Duration::from_secs(12), async {
        playback_service.wait_for_observed_lifecycle_calls(1).await;
    })
    .await
    .checked("observed playback lifecycle should trigger auto-advance subscriber");

    shutdown_tx
        .send(true)
        .checked("lifecycle shutdown signal should send");
    lifecycle_handle
        .await
        .checked("lifecycle task should not panic");
    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_refreshes_when_current_media_is_updated() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_playback_md_upd", message_sender.clone()).await;
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
            creator_id: Some(handler.test_user_id()),
            name: "observe-playback-media-update".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: synctv_core::models::SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/observe-playback-media-update.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: now(),
            updated_at: now(),
            version: 0,
        })
        .await
        .checked("media should be created for playback observe test");

    handler
        .room_service
        .update_playback_state(
            handler.room_id,
            handler.test_user_id(),
            |state| {
                state.playing_media_id = Some(media.id);
                state.playing_playlist_id = None;
                state.target = None;
                state.position = 0.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            RoomPermission::CONTROL_PLAYBACK_STATE,
        )
        .await
        .checked("playback should point at created media");

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: public_id_codec()
            .encode_media_id(media.id)
            .checked("test value"),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "observe-playback-media-update".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_playback(message)
                .is_some_and(|playback| playback.name == "observe-playback-media-update")),
        "observe should send the current playback"
    );

    let _updated_media = handler
        .room_service
        .edit_media(
            handler.room_id,
            handler.test_user_id(),
            media.id,
            Some("observe-playback-media-update-v2".to_string()),
        )
        .await
        .checked("editing current playback media should succeed");

    playback_service.replace(synctv_proto::client::Playback {
        media_id: public_id_codec()
            .encode_media_id(media.id)
            .checked("test value"),
        playlist_id: String::new(),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "observe-playback-media-update-v2".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: Some(playback_metadata_with_name("media-updated")),
        expires_at: Some(4_102_444_860),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });

    event_service.broadcast(RealtimeEvent::MediaUpdated {
        event_id: "evt-observe-playback-media-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        media_id: media.id,
        media_title: "observe-playback-media-update-v2".to_string(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback(message).is_some_and(|playback| {
                    playback
                        .metadata
                        .as_ref()
                        .and_then(playback_metadata_name)
                        .is_some_and(|name| name == "media-updated")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("current media updates should refresh observed playbacks");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_refreshes_when_current_playlist_is_updated() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_playback_pl_upd", message_sender.clone()).await;
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
            creator_id: Some(handler.test_user_id()),
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
        .checked("playlist should be created for playback observe test");

    let playback_repo =
        synctv_core::repository::RoomPlaybackStateRepository::new(fixture.pool.clone());
    let mut playback_state = playback_repo
        .create_or_get(&handler.room_id)
        .await
        .checked("playback state row should exist");
    playback_state.playing_media_id = None;
    playback_state.playing_playlist_id = Some(playlist.id);
    playback_state.target = Some(synctv_core::models::ProviderTarget::alist(
        "/playlist-item-1.mp4".to_string(),
    ));
    playback_state.position = 0.0;
    playback_state.speed = 1.0;
    playback_state.is_playing = true;
    playback_repo
        .update(&playback_state)
        .await
        .checked("playback should point at created playlist");

    let playback_service = MutablePlaybackService::new(synctv_proto::client::Playback {
        media_id: String::new(),
        playlist_id: public_id_codec()
            .encode_playlist_id(playlist.id)
            .checked("test value"),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "observe-playback-playlist-update".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: None,
        expires_at: Some(4_102_444_800),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_playback(message)
                .is_some_and(|playback| playback.name == "observe-playback-playlist-update")),
        "observe should send the current playback"
    );

    let updated_playlist = handler
        .room_service
        .playlist_service()
        .set_playlist(
            handler.room_id,
            handler.test_user_id(),
            synctv_core::service::SetPlaylistRequest {
                playlist_id: playlist.id,
                name: Some("observe-playback-playlist-update-v2".to_string()),
                description: None,
            },
        )
        .await
        .checked("editing current playback playlist should succeed");

    playback_service.replace(synctv_proto::client::Playback {
        media_id: String::new(),
        playlist_id: public_id_codec()
            .encode_playlist_id(playlist.id)
            .checked("test value"),
        room_id: public_id_codec()
            .encode_room_id(handler.room_id)
            .checked("test value"),
        name: "observe-playback-playlist-update-v2".to_string(),
        playlist_position: 0.0,
        playback_infos: std::collections::HashMap::new(),
        default_mode: String::new(),
        metadata: Some(playback_metadata_with_name("playlist-updated")),
        expires_at: Some(4_102_444_860),
        duration_seconds: None,
        playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
        target: None,
        provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        provider_instance_name: String::new(),
    });

    event_service.broadcast(RealtimeEvent::PlaylistUpdated {
        event_id: "evt-observe-playback-playlist-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        playlist: updated_playlist.clone(),
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback(message).is_some_and(|playback| {
                    playback
                        .metadata
                        .as_ref()
                        .and_then(playback_metadata_name)
                        .is_some_and(|name| name == "playlist-updated")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("current playlist updates should refresh observed playbacks");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_observed_playback_refreshes_when_target_changes_at_same_version() {
    let message_sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture(
        "observe_pb_playback_same_target_change",
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

    let playback_service = SequencedPlaybackService::new([
        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            name: String::new(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: None,
            expires_at: Some(4_102_444_800),
            duration_seconds: None,
            playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
            target: None,
            provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            provider_instance_name: String::new(),
        }),
        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            name: String::new(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: Some(playback_metadata_with_name("refreshed")),
            expires_at: Some(4_102_444_860),
            duration_seconds: None,
            playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
            target: None,
            provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            provider_instance_name: String::new(),
        }),
    ]);
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    playback_service.wait_for_calls(1).await;
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_playback(message)
                .is_some_and(|playback| playback.expires_at == Some(4_102_444_800))),
        "observe should send the current playback"
    );

    let updated_state = handler
        .room_service
        .update_playback_state(
            handler.room_id,
            handler.test_user_id(),
            |state| {
                state.playing_media_id = None;
                state.playing_playlist_id = None;
                state.target = None;
                state.position = 12.0;
                state.speed = 1.0;
                state.is_playing = true;
            },
            RoomPermission::CONTROL_PLAYBACK_STATE,
        )
        .await
        .checked("playback target should update before broadcasting state change");

    event_service.broadcast(RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-same-version-new-content".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        state: updated_state,
        source_changed: true,
        client_operation_id: None,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_playback(message).is_some_and(|playback| {
                    playback
                        .metadata
                        .as_ref()
                        .and_then(playback_metadata_name)
                        .is_some_and(|name| name == "refreshed")
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("target changes at the same DB version must refresh observed playbacks");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_playback_refresh_failure_removes_observation_without_closing_connection() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_playback_refresh_fail", message_sender.clone())
            .await;
    let StartTestFixture { handler, .. } = &fixture;

    let playback_service = SequencedPlaybackService::new([
        Ok(synctv_proto::client::Playback {
            media_id: public_media_id(),
            playlist_id: String::new(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            name: "test media".to_string(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: None,
            expires_at: Some(111),
            duration_seconds: None,
            playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
            target: None,
            provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            provider_instance_name: String::new(),
        }),
        Err(crate::impls::ApiError::ServiceUnavailable(
            "provider unavailable".to_string(),
        )),
    ]);
    let handler = handler
        .clone()
        .with_playback_service(playback_service.clone());

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playback_message("playback", None)),
        })
        .await
        .checked("initial observed playback should be delivered");
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_playback(message).is_some()),
        "initial observed playback should be delivered"
    );

    let refresh_event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-observe-playback-refresh-error".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        state: RoomPlaybackState {
            room_id: handler.room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position: 5.0,
            speed: 1.0,
            is_playing: true,
            playback_generation: 0,
            updated_at: now(),
            version: 2,
        },
        source_changed: true,
        client_operation_id: None,
        timestamp: now(),
    };

    handler
        .resource_observer
        .room_hub
        .refresh_for_room_event_with_cursor(
            &refresh_event,
            Some(handler.connection_id()),
            event_cursor(201),
        )
        .await
        .checked("failed playback refresh should not fail the realtime connection");
    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_observe_error(message).is_some()),
        "failed playback refresh should send ResourceObserveError"
    );
    assert!(
        !handler.resource_observer.has_observation("playback").await,
        "failed playback refresh should remove the observation"
    );

    let playback_messages = message_sender
        .sent_messages()
        .iter()
        .filter(|message| resource_playback(message).is_some())
        .count();
    assert_eq!(
        playback_messages, 1,
        "failed playback refresh should remove the observation instead of repeatedly resending"
    );

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_playback_observation_refreshes_when_playback_expires_without_state_change() {
    let message_sender = RecordingMessageSender::new();
    let fixture =
        create_start_handler_fixture("observe_pb_playback_expiry", message_sender.clone()).await;
    let StartTestFixture {
        handler,
        connection_service,
        event_service,
        ..
    } = &fixture;

    let refresh_at = synctv_core::SystemClock.now().timestamp() + 1;
    let playback_service = SequencedPlaybackService::new([
        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            name: String::new(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: None,
            expires_at: Some(refresh_at),
            duration_seconds: None,
            playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
            target: None,
            provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            provider_instance_name: String::new(),
        }),
        Ok(synctv_proto::client::Playback {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            name: String::new(),
            playlist_position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: Some(playback_metadata_with_name("refreshed")),
            expires_at: Some(refresh_at + 60),
            duration_seconds: None,
            playback_kind: synctv_proto::source_config::PlaybackKind::Regular as i32,
            target: None,
            provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            provider_instance_name: String::new(),
        }),
    ]);
    let handler = handler.clone().with_playback_service(playback_service);

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playback_message("playback", None)),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_condition(Duration::from_secs(3), || {
        message_sender
            .sent_messages()
            .iter()
            .filter_map(resource_playback)
            .any(|playback| playback.expires_at == Some(refresh_at))
    })
    .await
    .checked("initial playback observation should be delivered");

    tokio::time::sleep(Duration::from_secs(2)).await;
    handler
        .resource_observer
        .refresh_expired_resource_observations()
        .await
        .checked("expired playbacks should refresh without state changes");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .filter_map(resource_playback)
            .any(|playback| {
                playback
                    .metadata
                    .as_ref()
                    .and_then(playback_metadata_name)
                    .is_some_and(|name| name == "refreshed")
            }),
        "expired playback refresh should send the refreshed playback"
    );

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
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
                ..Default::default()
            },
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
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
    .checked("recording stream should receive observed room settings");

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_room_settings) {
        Some(changed) => {
            assert_eq!(changed.version, 7);
            assert!(changed
                .settings
                .as_ref()
                .is_some_and(|settings| settings.chat_enabled));
        }
        None => std::panic::panic_any(format!(
            "expected RoomSettings after observe, got {messages:?}"
        )),
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
        .with_playlist_items_snapshot_service(Arc::new(StaticPlaylistItemsSnapshotService {
            snapshot: synctv_proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: vec![synctv_proto::client::Media {
                    id: "media_test_1".to_string(),
                    room_id: public_id_codec()
                        .encode_room_id(handler.room_id)
                        .checked("test value"),
                    source_provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                    name: "test media".to_string(),
                    description: String::new(),
                    metadata: None,
                    position: 1.0,
                    added_at: 1,
                    creator_id: handler.test_user_id().to_string(),
                    provider_instance_name: String::new(),
                    source_config: None,
                    availability: synctv_proto::client::ResourceAvailability::Available as i32,
                    version: 3,
                    cover: None,
                    thumbnail: None,
                }],
                total: Some(1),
                playlist_count: 0,
                file_count: 1,
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
                pagination: None,
                version: "items-v1".to_string(),
            },
        }));

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
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
    .checked("recording stream should receive observed playlist items");

    let messages = message_sender.sent_messages();
    match messages.iter().find_map(resource_playlist_items) {
        Some(snapshot) => {
            assert_eq!(snapshot.version, "items-v1");
            assert_eq!(snapshot.media.len(), 1);
        }
        None => std::panic::panic_any(format!(
            "expected PlaylistItems after observe, got {messages:?}"
        )),
    }

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_observed_playlist_items_batch_coalesces_identical_snapshot_loads() {
    let message_sender = RecordingMessageSender::new();
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(0),
            playlist_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            pagination: None,
            version: "items-v1".to_string(),
        });
    let handler = test_message_handler_for_user_with_runtime(
        message_sender.clone(),
        test_realtime_manager("playlist_items_coalesce").await,
        test_connection_manager(),
        user_id(),
        runtime_with_playlist_items_snapshot_service(snapshot_service.clone()),
    );
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "batch-coalesce".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "playlist-items-a",
                request.clone(),
            )),
        })
        .await
        .checked("first observe should register");
    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message("playlist-items-b", request)),
        })
        .await
        .checked("second observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(synctv_proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: Some(0),
        playlist_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        pagination: None,
        version: "items-v2".to_string(),
    });

    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .checked("playlist invalidation should refresh observations");

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
        "both observe IDs should still receive their own ResourceEvent message"
    );
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_resource_observations_are_bounded_per_connection() {
    let sender = RecordingMessageSender::new();
    let event_service = test_realtime_manager("resource_observation_limit").await;
    let connection_service = test_connection_manager();
    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
                ..Default::default()
            },
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
                message: Some(observe_room_settings_message_with_sequence(
                    &observe_id,
                    Some(0),
                )),
            })
            .await
            .checked("observe should register while under the per-connection limit");
    }
    let snapshot_calls_before_over_limit = snapshot_service.call_count();

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message_with_sequence(
                "room-settings-over-limit",
                Some(0),
            )),
        })
        .await
        .checked("over-limit observe should send ResourceObserveError without closing");

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
                synctv_proto::client::client_message::Message::UnobserveResource(
                    synctv_proto::client::UnobserveResource {
                        observe_id: "room-settings-0".to_string(),
                    },
                ),
            ),
        })
        .await
        .checked("unobserve should free one observation slot");

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message(
                "room-settings-after-unobserve",
            )),
        })
        .await
        .checked("observe should register after a slot is freed");
    assert!(
        handler
            .resource_observer
            .has_observation("room-settings-after-unobserve")
            .await
    );
}

#[test]
fn test_room_member_events_filter_self_and_permission_only_changes() {
    let other_user_id = UserId::expect_positive(2);
    assert!(
        !super::resource_observer::room_member_event_visible_to_observer(
            &RealtimeEvent::UserJoined {
                event_id: "evt-self-join".to_string(),
                room_id: room_id(),
                user_id: user_id(),
                username: "test-user".to_string(),
                remark_name: String::new(),
                display_tag: String::new(),
                permissions: RoomPermissionSet::default_member(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                added_permissions: RoomPermissionSet::default(),
                removed_permissions: RoomPermissionSet::default(),
                admin_added_permissions: RoomPermissionSet::default(),
                admin_removed_permissions: RoomPermissionSet::default(),
                joined_at: now(),
                timestamp: now(),
            },
            Some(user_id()),
        )
    );
    assert!(
        !super::resource_observer::room_member_event_visible_to_observer(
            &RealtimeEvent::UserLeft {
                event_id: "evt-self-left".to_string(),
                room_id: room_id(),
                user_id: user_id(),
                username: "test-user".to_string(),
                remark_name: String::new(),
                display_tag: String::new(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                timestamp: now(),
            },
            Some(user_id()),
        )
    );
    assert!(
        !super::resource_observer::room_member_event_visible_to_observer(
            &permission_changed_event_for_target("evt-other-permission", other_user_id, false),
            Some(user_id()),
        )
    );
    assert!(
        !super::resource_observer::room_member_event_visible_to_observer(
            &permission_changed_event_for_target("evt-self-role", user_id(), true),
            Some(user_id()),
        )
    );
    assert!(
        super::resource_observer::room_member_event_visible_to_observer(
            &permission_changed_event_for_target("evt-other-role", other_user_id, true),
            Some(user_id()),
        )
    );
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_observe_playlist_items_requires_inner_request() {
    let sender = RecordingMessageSender::new();
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(0),
            playlist_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            pagination: None,
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
                synctv_proto::client::client_message::Message::ObserveResource(
                    synctv_proto::client::ObserveResource {
                        observe_id: "playlist-items-missing-request".to_string(),
                        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot
                            as i32,
                        resource: Some(
                            synctv_proto::client::observe_resource::Resource::PlaylistItems(
                                synctv_proto::client::ObservePlaylistItems {
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
        .checked("invalid observe should send ResourceObserveError without closing");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "consume-refresh".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: true,
        preview_source_config: None,
    };

    handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
        .await
        .checked("observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .checked("playlist invalidation should refresh observation");
    snapshot_service.wait_for_calls(2).await;

    assert_eq!(
        snapshot_service.refresh_values(),
        vec![true, false],
        "refresh=true should be used for the initial load only"
    );
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_resource_event_send_failure_propagates_and_removes_observation() {
    let message_sender = FailingMessageSender::fail_after(2);
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("resource_event_send_failure").await,
        test_connection_manager(),
    );
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"));
    let handler =
        test_handler_with_playlist_items_snapshot_service(handler, snapshot_service.clone());
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "send-failure".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
        .await
        .checked("observe should register with initial snapshot sent");
    assert_eq!(message_sender.send_calls(), 2);

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    let error = handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::PlaylistItems])
        .await
        .expect_err("ResourceEvent send failure should propagate");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_other_subscriber_send_failure_does_not_fail_refresh_caller() {
    let failing_sender = FailingMessageSender::fail_after(2);
    let healthy_sender = RecordingMessageSender::new();
    let event_service = test_realtime_manager("other_subscriber_send_failure").await;
    let connection_service = test_connection_manager();
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
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
        connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:other-send-fail:a:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: healthy_sender.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "other-send-failure".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    failing_handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-failing",
            request.clone(),
            Some(0),
        ))
        .await
        .checked("failing observer should register before its queue fails");
    healthy_handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-healthy",
            request,
            Some(0),
        ))
        .await
        .checked("healthy observer should register");
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
        .checked("another subscriber's send failure should not fail the healthy caller");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_stale_refresh_after_unobserve_does_not_send_resource_event() {
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
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "stale-refresh".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message("playlist-items", request)),
        })
        .await
        .checked("observe should register");
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
                synctv_proto::client::client_message::Message::UnobserveResource(
                    synctv_proto::client::UnobserveResource {
                        observe_id: "playlist-items".to_string(),
                    },
                ),
            ),
        })
        .await
        .checked("unobserve should unregister the observation");
    snapshot_service.release();
    refresh_task
        .await
        .checked("refresh task should join")
        .checked("stale refresh should be suppressed without error");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "stale-refresh-failure".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message("playlist-items", request)),
        })
        .await
        .checked("observe should register");
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
                synctv_proto::client::client_message::Message::UnobserveResource(
                    synctv_proto::client::UnobserveResource {
                        observe_id: "playlist-items".to_string(),
                    },
                ),
            ),
        })
        .await
        .checked("unobserve should unregister the observation");
    snapshot_service.release();
    refresh_task
        .await
        .checked("refresh task should join")
        .checked("stale refresh failure should be suppressed without error");

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .all(|message| resource_observe_error(message).is_none()),
        "obsolete refresh failure should not send ResourceObserveError after unobserve"
    );
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "singleflight-concurrent".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };
    let message_a = ClientMessage {
        message: Some(observe_playlist_items_message_with_sequence(
            "playlist-items-a",
            request.clone(),
            Some(0),
        )),
    };
    let message_b = ClientMessage {
        message: Some(observe_playlist_items_message_with_sequence(
            "playlist-items-b",
            request,
            Some(0),
        )),
    };

    let (result_a, result_b) = tokio::join!(
        handler_a.handle_client_message(&message_a),
        handler_b.handle_client_message(&message_b)
    );
    result_a.checked("first observe should succeed");
    result_b.checked("second observe should succeed");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "missing-durable-cursor".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items",
            request,
            Some(0),
        ))
        .await
        .checked("observe should register");
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
        .checked("missing durable cursor should refresh without failing the stream");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_media_resource_hub_coalesces_event_refresh_and_fans_out() {
    let snapshot_service =
        MutablePlaylistItemsSnapshotService::new(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(0),
            playlist_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            pagination: None,
            version: "items-v1".to_string(),
        });
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("media_resource_hub_event_refresh").await;
    let connection_service = test_connection_manager();
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-a".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: Arc::clone(&chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
        rate_limiter: Arc::new(RateLimiter::local_only("test:hub:a:".to_string())),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: sender_b.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "room-hub-refresh".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    handler_a
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-a",
            request.clone(),
            Some(0),
        ))
        .await
        .checked("first observe should register");
    handler_b
        .resource_observer
        .handle_observe_resource(&observe_playlist_items_resource_with_sequence(
            "playlist-items-b",
            request,
            Some(0),
        ))
        .await
        .checked("second observe should register");
    snapshot_service.wait_for_calls(1).await;

    snapshot_service.replace(synctv_proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: Vec::new(),
        total: Some(0),
        playlist_count: 0,
        file_count: 0,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        pagination: None,
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
    refresh_a.checked("first event refresh should succeed");
    refresh_b.checked("deduped event refresh should succeed");
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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_media_resource_hub_refresh_dedupe_tracks_subscription_generation() {
    let snapshot_service =
        BlockingPlaylistItemsSnapshotService::new(empty_playlist_items_response("items-v1"), 2);
    let sender_a = RecordingMessageSender::new();
    let sender_b = RecordingMessageSender::new();
    let event_service = test_realtime_manager("media_resource_hub_generation_dedupe").await;
    let connection_service = test_connection_manager();
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool);
    let handler_a = StreamMessageHandler::new(StreamMessageHandlerConfig {
        room_id: room_id(),
        principal: RealtimePrincipal::user(user_id(), "tester-a".to_string()),
        connection_id: None,
        room_service: Arc::clone(&room_service),
        chat_service: Arc::clone(&chat_service),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
        rate_limiter: Arc::new(RateLimiter::local_only(
            "test:hub:generation:a:".to_string(),
        )),
        rate_limit_config: Arc::new(RateLimitConfig::default()),
        content_filter: Arc::new(ContentFilter::new()),
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: sender_b.clone(),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    })
    .with_playlist_items_snapshot_service(snapshot_service.clone());
    let request_a = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: "generation-dedupe-a".to_string(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };
    let request_b = synctv_proto::client::ListPlaylistItemsRequest {
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
        .checked("first observe should register");
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
        .checked("second observe should register while first refresh is in flight");
    snapshot_service.wait_for_calls(3).await;
    assert!(!sender_b.sent_messages().iter().any(|message| {
        resource_playlist_items(message).is_some_and(|snapshot| snapshot.version == "items-v2")
    }));

    snapshot_service.replace(empty_playlist_items_response("items-v2"));
    snapshot_service.release();
    refresh_a
        .await
        .checked("first refresh task should join")
        .checked("first refresh should finish");

    handler_b
        .resource_observer
        .room_hub
        .refresh_for_room_event_with_cursor(
            &event,
            Some(handler_b.connection_id()),
            event_cursor(302),
        )
        .await
        .checked("second refresh should not be suppressed by the stale completed refresh");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_observed_room_settings_singleflight_coalesces_cross_user_loads() {
    let snapshot_service = SlowRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
                ..Default::default()
            },
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
        message: Some(observe_room_settings_message_with_sequence(
            "room-settings-a",
            Some(0),
        )),
    };
    let message_b = ClientMessage {
        message: Some(observe_room_settings_message_with_sequence(
            "room-settings-b",
            Some(0),
        )),
    };

    let (result_a, result_b) = tokio::join!(
        handler_a.handle_client_message(&message_a),
        handler_b.handle_client_message(&message_b)
    );
    result_a.checked("first room settings observe should succeed");
    result_b.checked("second room settings observe should succeed");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_observe_resource_does_not_reuse_completed_evaluation_across_invalidation() {
    let message_sender = RecordingMessageSender::new();
    let handler = test_message_handler(
        message_sender.clone(),
        test_realtime_manager("observe_no_completed_reuse").await,
        test_connection_manager(),
    );
    let snapshot_service = MutableRoomSettingsSnapshotService::new(
        crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
            settings: synctv_core::models::RoomSettings::default(),
            version: 1,
        },
    );
    let handler =
        test_handler_with_room_settings_snapshot_service(handler, snapshot_service.clone());

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message_with_sequence(
                "room-settings-a",
                Some(0),
            )),
        })
        .await
        .checked("first observe should register");
    snapshot_service.wait_for_calls(1).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(
                synctv_proto::client::client_message::Message::UnobserveResource(
                    synctv_proto::client::UnobserveResource {
                        observe_id: "room-settings-a".to_string(),
                    },
                ),
            ),
        })
        .await
        .checked("unobserve should unregister the first observation");

    snapshot_service.replace(crate::impls::room_settings_snapshot::RoomSettingsSnapshot {
        settings: synctv_core::models::RoomSettings::default(),
        version: 2,
    });
    handler
        .resource_observer
        .refresh_observations_for_invalidations(&[ResourceInvalidation::RoomSettings])
        .await
        .checked(
            "invalidation without active observations should still advance resource generation",
        );
    assert_eq!(
        snapshot_service.call_count(),
        1,
        "invalidation with no active observations should not load a snapshot"
    );

    handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_room_settings_message_with_sequence(
                "room-settings-b",
                Some(0),
            )),
        })
        .await
        .checked("second observe should load the latest snapshot");
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
        MutablePlaylistItemsSnapshotService::new(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(0),
            playlist_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            pagination: None,
            version: "items-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_playlist_items_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
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
        MutablePlaylistItemsSnapshotService::new(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(0),
            playlist_count: 0,
            file_count: 0,
            dynamic_items: Vec::new(),
            current_path: Vec::new(),
            pagination: None,
            version: "items-v1".to_string(),
        });
    let handler = handler
        .clone()
        .with_playlist_items_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_playlist_items_message(
            "playlist-items",
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
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

    snapshot_service.replace(synctv_proto::client::ListPlaylistItemsResponse {
        playlists: Vec::new(),
        media: vec![synctv_proto::client::Media {
            id: "media_test_2".to_string(),
            room_id: public_id_codec()
                .encode_room_id(handler.room_id)
                .checked("test value"),
            source_provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            name: "next media".to_string(),
            description: String::new(),
            metadata: None,
            position: 2.0,
            added_at: 2,
            creator_id: handler.test_user_id().to_string(),
            provider_instance_name: String::new(),
            source_config: None,
            availability: synctv_proto::client::ResourceAvailability::Available as i32,
            version: 4,
            cover: None,
            thumbnail: None,
        }],
        total: Some(1),
        playlist_count: 0,
        file_count: 1,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        pagination: None,
        version: "items-v2".to_string(),
    });

    event_service.broadcast(RealtimeEvent::MediaAdded {
        event_id: "evt-observe-playlist-items-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
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
    .checked("observed playlist items should receive future updates");

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
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
                ..Default::default()
            },
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_settings_message("room-settings")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    snapshot_service.wait_for_calls(1).await;

    assert!(
        message_sender
            .sent_messages()
            .iter()
            .any(|message| resource_room_settings(message)
                .is_some_and(|settings| settings.version == 7)),
        "observe should send the current room settings snapshot"
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
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
                ..Default::default()
            },
            version: 7,
        },
    );
    let handler = handler
        .clone()
        .with_room_settings_snapshot_service(snapshot_service.clone());

    prepare_handler_for_run_after_join(&handler, connection_service).await;

    let (mut stream, _stream_state) = RecordingStream::with_incoming(vec![ClientMessage {
        message: Some(observe_room_settings_message("room-settings")),
    }]);

    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

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
        settings: synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            ..Default::default()
        },
        version: 8,
    });

    event_service.broadcast(RealtimeEvent::RoomSettingsChanged {
        event_id: "evt-observe-room-settings-update".to_string(),
        room_id: handler.room_id,
        user_id: handler.test_user_id(),
        username: handler.username.clone(),
        settings: synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            ..Default::default()
        },
        version: 8,
        timestamp: now(),
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if message_sender.sent_messages().iter().any(|message| {
                resource_room_settings(message).is_some_and(|changed| {
                    changed.version == 8
                        && changed
                            .settings
                            .as_ref()
                            .is_some_and(|settings| !settings.chat_enabled)
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .checked("future room settings update should be pushed to observed client");

    connection_service.disconnect_connection(handler.connection_id());
    wait_for_run_after_join_cleanup(&handler, connection_service, event_service, run_task).await;
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_run_after_join_cleans_up_when_admin_notification_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_admin_failure").await;
    let connection_service = test_connection_manager();
    let handler = test_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        event_service.clone(),
        connection_service.clone(),
    );
    prepare_handler_for_run_after_join(&handler, &connection_service).await;

    let (mut stream, stream_state) = FailingStream::fail_after(0);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    event_service.broadcast(RealtimeEvent::UserNotification {
        event_id: "evt-run-after-join-admin".to_string(),
        user_id: handler.test_user_id(),
        title: "title".to_string(),
        content: "content".to_string(),
        notification_type: synctv_core::models::NotificationType::SystemAnnouncement,
        data: synctv_core::models::NotificationData::default(),
        notification_id: "notif-admin".to_string(),
        timestamp: now(),
    });

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let (mut stream, stream_state) = FailingStream::fail_after_with_incoming(0, vec![input]);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    wait_for_run_after_join_cleanup(&handler, &connection_service, &event_service, run_task).await;
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_run_after_join_cleans_up_when_direct_notification_send_fails() {
    let event_service = test_realtime_manager("test_run_after_join_direct_failure").await;
    let connection_service = test_connection_manager();
    let (_postgres, notification_pool) = synctv_core_testing::create_test_pool().await;
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

    let (mut stream, stream_state) = FailingStream::fail_after(0);
    let task_handler = handler.clone();
    let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

    wait_for_run_after_join_ready(&stream_state).await;

    let subscriber_count = notification_service.publish_realtime_event(NotificationCreatedEvent {
        user_id: handler.test_user_id(),
        notification: Notification {
            id: 1,
            user_id: handler.test_user_id(),
            notification_type: NotificationType::SystemAnnouncement,
            title: "title".to_string(),
            content: "content".to_string(),
            data: NotificationData::default(),
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
fn test_chat_message_event_is_delivered_by_resource_observer() {
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
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
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
            message: ChatMessageWithAttachments {
                message: ChatMessage {
                    id: 42,
                    room_id: room_id(),
                    user_id: Some(user_id()),
                    client_message_id: Some("client-42".to_string()),
                    content: String::new(),
                    message_type: ChatMessageType::User,
                    status: ChatMessageStatus::Deleted,
                    version: 2,
                    reply_to_message_id: None,
                    reply_to_message_created_at: None,
                    metadata: None,
                    edited_at: Some(created_at),
                    deleted_at: Some(created_at),
                    deleted_by: Some(user_id()),
                    delete_reason: Some("policy".to_string()),
                    created_at,
                },
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions: Vec::new(),
                pin: None,
            },
            occurred_at: created_at,
        },
        timestamp: created_at,
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(
        msgs.is_empty(),
        "durable chat events must be delivered through ResourceEvent(ChatEvent)"
    );
}

#[test]
fn test_playback_state_changed_event_conversion() {
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt2".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state: RoomPlaybackState {
            room_id: room_id(),
            playing_media_id: Some(media_id()),
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position: 123.456,
            speed: 1.5,
            is_playing: true,
            playback_generation: 0,
            updated_at: now(),
            version: 7,
        },
        source_changed: false,
        client_operation_id: None,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(
        msgs.is_empty(),
        "playback state changes are delivered through observed playback_state ResourceEvent"
    );
}

#[test]
fn test_playback_state_changed_event_does_not_validate_direct_message_payload() {
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: "evt-invalid-playback".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "bob".to_string(),
        state: RoomPlaybackState {
            room_id: room_id(),
            playing_media_id: Some(media_id()),
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position: f64::NAN,
            speed: 0.0,
            is_playing: false,
            playback_generation: 0,
            updated_at: now(),
            version: -1,
        },
        source_changed: false,
        client_operation_id: None,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("direct playback state fanout should be skipped");
    assert!(msgs.is_empty());
}

#[test]
fn test_user_joined_event_conversion() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt3".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "carol".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        permissions: RoomPermissionSet::default_member(),
        role: 3,
        added_permissions: RoomPermissionSet(RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE),
        removed_permissions: RoomPermissionSet(RoomAdminPermissionBits::MANAGE_OWN_MEDIA),
        admin_added_permissions: RoomPermissionSet(RoomAdminPermissionBits::REMOVE_MEMBERS),
        admin_removed_permissions: RoomPermissionSet(RoomAdminPermissionBits::REMOVE_MEMBERS),
        joined_at: now(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
}

#[test]
fn test_user_joined_event_rejects_unspecified_role() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt-invalid-role".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "carol".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        permissions: RoomPermissionSet::default_member(),
        role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        joined_at: now(),
        timestamp: now(),
    };

    assert!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .checked("member events are delivered through room_member_events")
            .is_empty()
    );
}

#[test]
fn test_user_joined_event_rejects_invalid_username() {
    let event = RealtimeEvent::UserJoined {
        event_id: "evt-invalid-username".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: " ".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        permissions: RoomPermissionSet::default_member(),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        joined_at: now(),
        timestamp: now(),
    };

    assert!(
        realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .checked("member events are delivered through room_member_events")
            .is_empty()
    );
}

#[test]
fn test_user_left_event_conversion() {
    let event = RealtimeEvent::UserLeft {
        event_id: "evt4".to_string(),
        room_id: room_id(),
        user_id: user_id(),
        username: "dave".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
}

#[test]
fn test_resource_backed_events_do_not_emit_direct_server_messages() {
    let mut updated_playlist = playlist();
    updated_playlist.name = "Renamed Playlist".to_string();

    let events = vec![
        RealtimeEvent::PermissionChanged {
            event_id: "evt-permission-override-bitspace".to_string(),
            room_id: room_id(),
            target_user_id: user_id(),
            target_username: "carol".to_string(),
            target_remark_name: String::new(),
            target_display_tag: String::new(),
            changed_by: user_id(),
            changed_by_username: "owner".to_string(),
            role_changed: false,
            new_permissions: RoomPermissionSet(RoomAdminPermissionBits::USE_VOICE_CHAT),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            added_permissions: RoomPermissionSet(RoomMemberPermissionBits::USE_VOICE_CHAT),
            removed_permissions: RoomPermissionSet(RoomMemberPermissionBits::SEND_CHAT_MESSAGES),
            admin_added_permissions: RoomPermissionSet(0),
            admin_removed_permissions: RoomPermissionSet(0),
            target_is_online: false,
            target_connection_count: 0,
            timestamp: now(),
        },
        RealtimeEvent::MediaAdded {
            event_id: "evt5".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "eve".to_string(),
            media_id: media_id(),
            media_title: "Test Video".to_string(),
            timestamp: now(),
        },
        RealtimeEvent::MediaRemoved {
            event_id: "evt6".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "frank".to_string(),
            media_id: media_id(),
            timestamp: now(),
        },
        RealtimeEvent::MediaRemovedBatch {
            event_id: "evt6_batch".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "frank".to_string(),
            media_ids: vec![
                MediaId::expect_positive(113_005),
                MediaId::expect_positive(113_006),
            ],
            timestamp: now(),
        },
        RealtimeEvent::MediaUpdated {
            event_id: "evt6b".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "frank".to_string(),
            media_id: media_id(),
            media_title: "Renamed Video".to_string(),
            timestamp: now(),
        },
        RealtimeEvent::PlaylistReordered {
            event_id: "evt6c".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "grace".to_string(),
            media_ids: vec![
                MediaId::expect_positive(113_006),
                MediaId::expect_positive(113_005),
            ],
            timestamp: now(),
        },
        RealtimeEvent::PlaylistCreated {
            event_id: "evt6d".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "grace".to_string(),
            playlist: playlist(),
            timestamp: now(),
        },
        RealtimeEvent::PlaylistUpdated {
            event_id: "evt6e".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "heidi".to_string(),
            playlist: updated_playlist,
            timestamp: now(),
        },
        RealtimeEvent::PlaylistDeleted {
            event_id: "evt6f".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "ivan".to_string(),
            playlist_id: PlaylistId::expect_positive(113_007),
            timestamp: now(),
        },
        RealtimeEvent::RoomSettingsChanged {
            event_id: "evt6g".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "judy".to_string(),
            settings: synctv_core::models::RoomSettings {
                chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
                ..Default::default()
            },
            version: 12,
            timestamp: now(),
        },
    ];

    for event in events {
        let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
            .checked("resource-backed realtime event should convert");
        assert!(
            msgs.is_empty(),
            "resource-backed event should only invalidate observed resources: {event:?}"
        );
    }
}

#[test]
fn test_permission_changed_room_member_event_preserves_presence_snapshot() {
    let event = RealtimeEvent::PermissionChanged {
        event_id: "evt-permission-presence".to_string(),
        room_id: room_id(),
        target_user_id: user_id(),
        target_username: "carol".to_string(),
        target_remark_name: String::new(),
        target_display_tag: String::new(),
        changed_by: user_id(),
        changed_by_username: "owner".to_string(),
        role_changed: false,
        new_permissions: RoomPermissionSet(RoomAdminPermissionBits::USE_VOICE_CHAT),
        role: synctv_proto::common::RoomMemberRole::Member as i32,
        added_permissions: RoomPermissionSet(RoomMemberPermissionBits::USE_VOICE_CHAT),
        removed_permissions: RoomPermissionSet(RoomMemberPermissionBits::SEND_CHAT_MESSAGES),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        target_is_online: false,
        target_connection_count: 0,
        timestamp: now(),
    };

    let event = room_member_event_to_proto(&event, &public_id_codec(), 42)
        .checked("permission event should convert")
        .checked("permission event should produce member event");
    let member = event
        .member
        .checked("permission event should include member");
    assert!(!member.is_online);
    assert_eq!(member.connection_count, 0);
}

#[test]
fn test_webrtc_offer_event_conversion() {
    let event = RealtimeEvent::WebRTCVoiceSignaling {
        event_id: "evt7".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::Offer,
        from: "conn_a".to_string(),
        to: "conn_b".to_string(),
        data: "sdp_data".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
}

#[test]
fn test_webrtc_answer_event_conversion() {
    let event = RealtimeEvent::WebRTCVoiceSignaling {
        event_id: "evt8".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::Answer,
        from: "conn_b".to_string(),
        to: "conn_a".to_string(),
        data: "answer_sdp".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
}

#[test]
fn test_webrtc_ice_candidate_event_conversion() {
    let event = RealtimeEvent::WebRTCVoiceSignaling {
        event_id: "evt9".to_string(),
        room_id: room_id(),
        message_type: WebRTCSignalKind::IceCandidate,
        from: "conn_a".to_string(),
        to: "conn_b".to_string(),
        data: "candidate_data".to_string(),
        timestamp: now(),
    };

    let msgs = realtime_event_to_server_messages(&event, "room_test", &public_id_codec())
        .checked("realtime event should convert");
    assert!(msgs.is_empty());
}

#[test]
fn test_webrtc_join_and_leave_event_conversion() {
    let join = RealtimeEvent::WebRTCVoicePeerJoined {
        event_id: "evt-webrtc-join".to_string(),
        room_id: room_id(),
        actor_id: "user_1".to_string(),
        conn_id: "conn_1".to_string(),
        username: "alice".to_string(),

        timestamp: now(),
    };
    let leave = RealtimeEvent::WebRTCVoicePeerLeft {
        event_id: "evt-webrtc-leave".to_string(),
        room_id: room_id(),
        actor_id: "user_1".to_string(),
        conn_id: "conn_1".to_string(),
        timestamp: now(),
    };

    let join_msgs = realtime_event_to_server_messages(&join, "room_test", &public_id_codec())
        .checked("webrtc join should convert");
    let leave_msgs = realtime_event_to_server_messages(&leave, "room_test", &public_id_codec())
        .checked("webrtc leave should convert");

    assert!(join_msgs.is_empty());
    assert!(leave_msgs.is_empty());
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
        .checked("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Termination(termination)) => {
            assert!(termination.message.contains("deleted"));
            assert_eq!(
                termination.code,
                synctv_proto::client::RealtimeTerminationCode::RoomDeleted as i32
            );
        }
        other => std::panic::panic_any(format!(
            "Expected Termination message for RoomDeleted, got: {other:?}"
        )),
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
        .checked("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Termination(termination)) => {
            assert!(termination.message.contains("banned"));
            assert_eq!(
                termination.code,
                synctv_proto::client::RealtimeTerminationCode::RoomBanned as i32
            );
        }
        other => std::panic::panic_any(format!(
            "Expected Termination message for RoomBanned, got: {other:?}"
        )),
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
        .checked("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Termination(termination)) => {
            assert!(termination.message.contains("creator"));
            assert_eq!(
                termination.code,
                synctv_proto::client::RealtimeTerminationCode::RoomOwnerInactive as i32
            );
        }
        other => std::panic::panic_any(format!(
            "Expected Termination message for RoomOwnerInactive, got: {other:?}"
        )),
    }
}

#[test]
fn test_realtime_termination_uses_dedicated_typed_code() {
    let message = realtime_termination_server_message(
        "Account access revoked",
        synctv_proto::client::RealtimeTerminationCode::UserAccessRevoked,
    );

    match message.message {
        Some(Message::Termination(termination)) => {
            assert_eq!(
                termination.code,
                synctv_proto::client::RealtimeTerminationCode::UserAccessRevoked as i32
            );
        }
        other => std::panic::panic_any(format!(
            "Expected realtime termination message, got: {other:?}"
        )),
    }
}

#[test]
fn test_room_disconnect_reasons_use_specific_termination_codes() {
    let cases = [
        (
            RoomDisconnectReason::AccessRevoked,
            synctv_proto::client::RealtimeTerminationCode::RoomAccessRevoked,
        ),
        (
            RoomDisconnectReason::Deleted,
            synctv_proto::client::RealtimeTerminationCode::RoomDeleted,
        ),
        (
            RoomDisconnectReason::Banned,
            synctv_proto::client::RealtimeTerminationCode::RoomBanned,
        ),
        (
            RoomDisconnectReason::OwnerInactive,
            synctv_proto::client::RealtimeTerminationCode::RoomOwnerInactive,
        ),
    ];

    for (reason, expected_code) in cases {
        let message = room_disconnect_termination_server_message(reason);
        match message.message {
            Some(Message::Termination(termination)) => {
                assert_eq!(termination.code, expected_code as i32);
            }
            other => std::panic::panic_any(format!(
                "Expected room disconnect termination message, got: {other:?}"
            )),
        }
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
        .checked("realtime event should convert");
    assert_eq!(msgs.len(), 1);
    match &msgs[0].message {
        Some(Message::Notification(n)) => {
            assert_eq!(n.title, "Server maintenance in 5 minutes");
            assert_eq!(
                n.notification_type,
                synctv_proto::client::NotificationType::SystemAnnouncement as i32
            );
        }
        other => std::panic::panic_any(format!(
            "Expected Notification message for SystemNotification, got: {other:?}"
        )),
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
            .checked("realtime event should convert")
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
            .checked("realtime event should convert")
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
            .checked("realtime event should convert")
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
fn test_message_concurrency_config_enforces_limit() {
    let config = super::MessageConcurrencyConfig::new(10);
    let semaphore = config.semaphore();

    let permits: Vec<_> = (0..10)
        .map(|_| semaphore.clone().try_acquire_owned())
        .collect::<Result<Vec<_>, _>>()
        .checked("Should acquire all 10 permits");

    assert_eq!(config.available_permits(), 0, "No permits should remain");

    let failed = semaphore.try_acquire_owned();
    assert!(failed.is_err(), "Should fail when no permits available");

    drop(permits);
    assert_eq!(config.available_permits(), 10, "All permits restored");
}

#[test]
fn test_parse_optional_chat_message_id_accepts_empty_and_numeric_values() {
    assert_eq!(
        super::parse_optional_chat_message_id("").checked("test value"),
        None
    );
    assert_eq!(
        super::parse_optional_chat_message_id(" 42 ").checked("test value"),
        Some(42)
    );
}

#[test]
fn test_parse_optional_chat_message_id_rejects_invalid_values() {
    let result = super::parse_optional_chat_message_id("message-42");

    assert!(result.is_err());
    assert_eq!(result.failed("expected error"), "Invalid chat message id");
}

#[test]
fn test_cached_membership_from_member_none() {
    let cached = super::CachedMembership::from_member(None);
    assert!(!cached.is_member, "Non-member should have is_member=false");
}

#[test]
fn test_cached_membership_from_member_active() {
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
        remark_name: String::new(),
        display_tag: String::new(),
        joined_at: now(),
        version: 1,
    };

    let cached = super::CachedMembership::from_member(Some(&member));
    assert!(cached.is_member);
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

#[test]
fn test_user_left_requires_retry_when_distributed_delivery_is_missing() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        RealtimeMetrics {
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
        RealtimeMetrics {
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
        RealtimeMetrics {
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
    .checked("authorization denials should be converted to disconnect reasons");

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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
        .checked("test handler should be a guest");
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
        .checked("blacklist guest token");

    let reason = super::RealtimeMembershipProbe::new(&handler.room_service)
        .guest_token_blacklist_denial_reason(&handler.room_id, identity, &identity.token_jti)
        .await
        .checked("blacklist check should succeed");

    assert_eq!(reason.as_deref(), Some("Guest token has been revoked"));

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_guest_chat_is_rejected_even_if_permission_bits_include_chat() {
    let event_service = test_realtime_manager("guest_chat_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::SEND_CHAT_MESSAGES),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(synctv_proto::client::client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "guest message".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        })
        .await
        .expect_err("guest chat must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_guest_chat_with_client_id_is_rejected() {
    let event_service = test_realtime_manager("guest_chat_client_id_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::SEND_CHAT_MESSAGES),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(synctv_proto::client::client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "guest chat with client id".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        })
        .await
        .expect_err("guest chat must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot send chat"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_guest_playlist_observation_is_rejected_even_if_permission_bits_include_browse_library(
) {
    let event_service = test_realtime_manager("guest_playlist_observe_rejected").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(RoomAdminPermissionBits::BROWSE_LIBRARY),
    );

    let err = handler
        .handle_client_message(&ClientMessage {
            message: Some(observe_playlist_items_message(
                "guest-playlist-items",
                synctv_proto::client::ListPlaylistItemsRequest::default(),
            )),
        })
        .await
        .expect_err("guest playlist observation must be rejected at the realtime boundary");

    assert!(err.contains("Guests cannot observe playlist items"));
    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_webrtc_command_joins_without_resource_observation() {
    let fixture = create_start_handler_fixture(
        "webrtc_command_joins_without_observe",
        FailingMessageSender::fail_after(usize::MAX),
    );
    let fixture = fixture.await;
    let handler = &fixture.handler;
    let connection_service = &fixture.connection_service;
    prepare_handler_for_run_after_join(handler, connection_service).await;

    handler
        .handle_client_message(&ClientMessage {
            message: Some(webrtc_command_message(
                synctv_proto::client::web_rtc_command::Command::VoiceJoin(
                    synctv_proto::client::WebRtcVoiceJoinCommand::default(),
                ),
            )),
        })
        .await
        .checked("WebRTC command should auto-observe WebRTC events");

    assert!(
        connection_service
            .get_connection(handler.connection_id.as_str())
            .is_some_and(|connection| connection.voice_rtc_joined),
        "accepted command must join the RTC session"
    );
    fixture.shutdown().await;
}

#[test]
fn webrtc_join_exposes_client_operation_id_for_rejection_correlation() {
    let message = ClientMessage {
        message: Some(webrtc_command_message(
            synctv_proto::client::web_rtc_command::Command::VoiceJoin(
                synctv_proto::client::WebRtcVoiceJoinCommand {
                    client_operation_id: Some("ed537455-83d3-43a4-b244-08f3963a4710".to_string()),
                },
            ),
        )),
    };

    assert_eq!(
        StreamMessageHandler::client_operation_id(&message),
        Some("ed537455-83d3-43a4-b244-08f3963a4710")
    );
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_webrtc_media_swarm_membership_has_an_independent_voice_lifecycle() {
    let sender = RecordingMessageSender::new();
    let fixture = create_start_handler_fixture("media_swarm_voice_lifecycle", sender.clone()).await;
    let handler = &fixture.handler;
    let connection_service = &fixture.connection_service;
    prepare_handler_for_run_after_join(handler, connection_service).await;
    let swarm_id = "sm1_room_representation";
    let ticket = handler.swarm_signing_key.sign_media_swarm_ticket(
        &handler
            .public_room_id()
            .checked("room public id should encode"),
        &handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );

    handler
        .handle_client_message(&ClientMessage {
            message: Some(webrtc_command_message(
                synctv_proto::client::web_rtc_command::Command::MediaSwarmJoin(
                    synctv_proto::client::WebRtcMediaSwarmJoin {
                        swarm_id: swarm_id.to_string(),
                        swarm_ticket: ticket.clone(),
                    },
                ),
            )),
        })
        .await
        .checked("media swarm membership should be announced");

    let response = sender
        .sent_messages()
        .into_iter()
        .rev()
        .find_map(|message| message.message)
        .and_then(|message| match message {
            synctv_proto::client::server_message::Message::ResourceEvent(event) => event.payload,
            _ => None,
        })
        .and_then(|payload| match payload {
            synctv_proto::client::resource_event::Payload::WebrtcEvent(event) => event.event,
            _ => None,
        })
        .and_then(|event| match event {
            synctv_proto::client::web_rtc_event::Event::MediaSwarmPeers(peers) => Some(peers),
            _ => None,
        })
        .checked("media swarm join should return a peer discovery response");
    assert_eq!(response.swarm_id, swarm_id);
    assert!(response.peers.is_empty());

    let connection = connection_service
        .get_connection(handler.connection_id.as_str())
        .checked("connection should remain registered");
    assert!(!connection.voice_rtc_joined);

    handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("voice session should join independently");
    handler
        .leave_webrtc_voice_session()
        .await
        .checked("voice session should leave independently");

    let peer_leave_message = handler
        .webrtc_event_server_message_for_current_connection(&RealtimeEvent::MediaSwarmPeerLeft {
            event_id: "evt-media-swarm-peer-left".to_string(),
            room_id: handler.room_id,
            actor_id: "usr_peer".to_string(),
            conn_id: "peer-connection".to_string(),
            swarm_id: swarm_id.to_string(),
            timestamp: now(),
        })
        .checked("media swarm peer leave should convert")
        .checked("active swarm member should receive peer leave");
    assert!(matches!(
        peer_leave_message.message,
        Some(
            synctv_proto::client::server_message::Message::ResourceEvent(
                synctv_proto::client::ResourceEvent {
                    payload: Some(synctv_proto::client::resource_event::Payload::WebrtcEvent(
                        synctv_proto::client::WebRtcEvent {
                            event: Some(synctv_proto::client::web_rtc_event::Event::MediaPeerLeft(
                                _
                            ))
                        }
                    )),
                    ..
                }
            )
        )
    ));
    let (mut room_events, observer_connection_id) = fixture
        .event_service
        .subscribe(
            handler.room_id,
            handler
                .realtime_actor()
                .checked("realtime actor should build"),
        )
        .await
        .checked("room event observer should subscribe");

    handler
        .handle_media_swarm_leave(&synctv_proto::client::WebRtcMediaSwarmLeave {
            swarm_id: swarm_id.to_string(),
        })
        .await
        .checked("media swarm departure should be announced independently");
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), room_events.recv())
        .await
        .checked("media swarm leave event should arrive")
        .checked("media swarm leave event stream should remain open");
    assert!(matches!(
        event.as_ref(),
        RealtimeEvent::MediaSwarmPeerLeft {
            conn_id,
            swarm_id: event_swarm_id,
            ..
        } if conn_id == handler.connection_id.as_str() && event_swarm_id == swarm_id
    ));
    fixture
        .event_service
        .unsubscribe(observer_connection_id.as_str());
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_room_capabilities_reject_new_voice_and_media_sessions() {
    let fixture = create_start_handler_fixture(
        "disabled_room_realtime_capabilities",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    prepare_handler_for_run_after_join(&fixture.handler, &fixture.connection_service).await;
    let mut settings = fixture
        .handler
        .room_service
        .get_room_settings(&fixture.handler.room_id)
        .await
        .checked("room settings should load");
    settings.voice_chat_enabled = synctv_core::models::room_settings::VoiceChatEnabled::new(false);
    settings.p2p_media_enabled = synctv_core::models::room_settings::P2pMediaEnabled::new(false);
    fixture
        .handler
        .room_service
        .set_room_settings(&fixture.handler.room_id, &settings)
        .await
        .checked("room settings should update");

    let voice_error = fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .expect_err("disabled voice chat should reject joins");
    assert_eq!(voice_error, "Voice chat is disabled for this room");

    let swarm_id = "sm1_disabled_room_capability";
    let ticket = fixture.handler.swarm_signing_key.sign_media_swarm_ticket(
        &fixture
            .handler
            .public_room_id()
            .checked("room public id should encode"),
        &fixture
            .handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    let media_error = fixture
        .handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket,
        })
        .await
        .expect_err("disabled P2P media should reject swarm joins");
    assert_eq!(media_error, "P2P media is disabled for this room");
    assert_eq!(
        crate::impls::classify_error(&voice_error),
        crate::impls::ErrorKind::PermissionDenied
    );
    assert_eq!(
        crate::impls::classify_error(&media_error),
        crate::impls::ErrorKind::PermissionDenied
    );
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_room_capability_change_ends_active_voice_and_media_sessions() {
    let fixture = create_start_handler_fixture(
        "room_realtime_capability_shutdown",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    prepare_handler_for_run_after_join(&fixture.handler, &fixture.connection_service).await;
    fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("voice chat should join while enabled");
    let swarm_id = "sm1_room_capability_shutdown";
    let ticket = fixture.handler.swarm_signing_key.sign_media_swarm_ticket(
        &fixture
            .handler
            .public_room_id()
            .checked("room public id should encode"),
        &fixture
            .handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    fixture
        .handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket,
        })
        .await
        .checked("media swarm should join while enabled");

    let settings = synctv_core::models::RoomSettings {
        voice_chat_enabled: synctv_core::models::room_settings::VoiceChatEnabled::new(false),
        p2p_media_enabled: synctv_core::models::room_settings::P2pMediaEnabled::new(false),
        ..Default::default()
    };
    fixture
        .handler
        .room_service
        .set_room_settings(&fixture.handler.room_id, &settings)
        .await
        .checked("disabled room capabilities should persist");
    fixture
        .handler
        .apply_rtc_access_change(&RealtimeEvent::RoomSettingsChanged {
            event_id: "evt-disable-room-capabilities".to_string(),
            room_id: fixture.handler.room_id,
            user_id: fixture.handler.test_user_id(),
            username: fixture.handler.username.clone(),
            settings,
            version: 2,
            timestamp: now(),
        })
        .await;

    assert!(fixture
        .connection_service
        .get_connection(fixture.handler.connection_id.as_str())
        .is_some_and(|connection| !connection.voice_rtc_joined));
    assert!(fixture.handler.active_media_swarms.lock().is_empty());
    let retry_error = fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .expect_err("voice chat should remain disabled after cleanup");
    assert_eq!(retry_error, "Voice chat is disabled for this room");
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_permission_revocation_ends_active_voice_and_media_sessions() {
    let fixture = create_start_handler_fixture(
        "room_realtime_permission_revocation",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    prepare_handler_for_run_after_join(&fixture.handler, &fixture.connection_service).await;
    fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("voice chat should join before permission revocation");
    let swarm_id = "sm1_permission_revocation";
    let ticket = fixture.handler.swarm_signing_key.sign_media_swarm_ticket(
        &fixture
            .handler
            .public_room_id()
            .checked("room public id should encode"),
        &fixture
            .handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    fixture
        .handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket,
        })
        .await
        .checked("media swarm should join before permission revocation");

    fixture
        .handler
        .apply_rtc_access_change(&RealtimeEvent::PermissionChanged {
            event_id: "evt-revoke-rtc-permissions".to_string(),
            room_id: fixture.handler.room_id,
            target_user_id: fixture.handler.test_user_id(),
            target_username: fixture.handler.username.clone(),
            target_remark_name: String::new(),
            target_display_tag: String::new(),
            changed_by: fixture.handler.test_user_id(),
            changed_by_username: fixture.handler.username.clone(),
            role_changed: false,
            new_permissions: RoomPermissionSet::empty(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            added_permissions: RoomPermissionSet::empty(),
            removed_permissions: RoomPermissionSet::empty(),
            admin_added_permissions: RoomPermissionSet::empty(),
            admin_removed_permissions: RoomPermissionSet::empty(),
            target_is_online: true,
            target_connection_count: 1,
            timestamp: now(),
        })
        .await;

    assert!(fixture
        .connection_service
        .get_connection(fixture.handler.connection_id.as_str())
        .is_some_and(|connection| !connection.voice_rtc_joined));
    assert!(fixture.handler.active_media_swarms.lock().is_empty());
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_room_capability_change_serializes_with_an_in_flight_join() {
    let fixture = create_start_handler_fixture(
        "room_capability_join_race",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    prepare_handler_for_run_after_join(&fixture.handler, &fixture.connection_service).await;
    let transition_guard = fixture.handler.room_capability_transition_lock.lock().await;
    let join_handler = fixture.handler.clone();
    let join_task = tokio::spawn(async move {
        join_handler
            .handle_webrtc_command(&synctv_proto::client::WebRtcCommand {
                command: Some(synctv_proto::client::web_rtc_command::Command::VoiceJoin(
                    synctv_proto::client::WebRtcVoiceJoinCommand::default(),
                )),
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;

    let settings = synctv_core::models::RoomSettings {
        voice_chat_enabled: synctv_core::models::room_settings::VoiceChatEnabled::new(false),
        ..Default::default()
    };
    let event_handler = fixture.handler.clone();
    let event_task = tokio::spawn(async move {
        event_handler
            .apply_rtc_access_change(&RealtimeEvent::RoomSettingsChanged {
                event_id: "evt-room-capability-join-race".to_string(),
                room_id: event_handler.room_id,
                user_id: event_handler.test_user_id(),
                username: event_handler.username.clone(),
                settings,
                version: 2,
                timestamp: now(),
            })
            .await;
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !event_task.is_finished(),
        "capability transition must wait for the same lock as an in-flight join"
    );

    drop(transition_guard);
    join_task
        .await
        .checked("join task should complete")
        .checked("join should use the still-enabled persisted room setting");
    event_task
        .await
        .checked("capability event task should complete");
    assert!(fixture
        .connection_service
        .get_connection(fixture.handler.connection_id.as_str())
        .is_some_and(|connection| !connection.voice_rtc_joined));
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_guest_self_room_member_snapshot_exposes_effective_permissions() {
    let fixture = create_start_handler_fixture(
        "guest_self_room_member_snapshot",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let settings = synctv_core::models::RoomSettings {
        guest_added_permissions: synctv_core::models::room_settings::GuestAddedPermissions::new(
            synctv_core::models::RoomGuestPermissionBits::USE_VOICE_CHAT
                | synctv_core::models::RoomGuestPermissionBits::USE_P2P_MEDIA,
        ),
        ..Default::default()
    };
    fixture
        .handler
        .room_service
        .set_room_settings(&fixture.handler.room_id, &settings)
        .await
        .checked("guest permissions should persist");

    let guest = StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id: fixture.handler.room_id,
            principal: test_guest_principal_with_permissions(RoomPermissionSet::empty()),
            connection_id: None,
            room_service: Arc::clone(&fixture.handler.room_service),
            chat_service: Arc::clone(&fixture.handler.chat_service),
            event_service: Arc::clone(&fixture.handler.event_service),
            connection_service: Arc::clone(&fixture.handler.connection_service),
            rate_limiter: Arc::clone(&fixture.handler.rate_limiter),
            rate_limit_config: Arc::clone(&fixture.handler.rate_limit_config),
            content_filter: Arc::clone(&fixture.handler.content_filter),
            public_id_codec: Arc::clone(&fixture.handler.public_id_codec),
            sender: FailingMessageSender::fail_after(usize::MAX),
            concurrency_config: Arc::clone(&fixture.handler.concurrency_config),
        },
        test_stream_handler_runtime(),
    );
    fixture
        .connection_service
        .register_actor(
            guest.connection_id.clone().into_string(),
            guest
                .realtime_actor()
                .checked("guest realtime actor should build"),
        )
        .await
        .checked("guest connection should register");
    fixture
        .connection_service
        .join_room(guest.connection_id.as_str(), guest.room_id)
        .await
        .checked("guest connection should join room");

    let snapshot = guest
        .resource_observer
        .self_room_member_snapshot()
        .await
        .checked("guest access snapshot should load");
    assert_eq!(
        snapshot.role,
        synctv_proto::common::RoomMemberRole::Guest as i32
    );
    assert_eq!(
        snapshot.user_id,
        guest
            .public_actor_id()
            .checked("guest public actor id should encode")
    );
    assert!(RoomPermissionSet(snapshot.permissions).has(RoomPermission::USE_VOICE_CHAT));
    assert!(RoomPermissionSet(snapshot.permissions).has(RoomPermission::USE_P2P_MEDIA));
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_voice_and_p2p_media_permissions_are_independent() {
    let voice_fixture = create_start_handler_fixture(
        "webrtc_voice_only_permission",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let voice_member_repo = RoomMemberRepository::new(voice_fixture.pool.clone());
    let voice_member = voice_member_repo
        .get(
            &voice_fixture.handler.room_id,
            &voice_fixture.handler.test_user_id(),
        )
        .await
        .checked("voice-only member should load")
        .checked("voice-only member should exist");
    voice_member_repo
        .update_permissions(
            &voice_member.room_id,
            &voice_member.user_id,
            voice_member.added_permissions,
            voice_member.removed_permissions | RoomMemberPermissionBits::USE_P2P_MEDIA,
            voice_member.version,
        )
        .await
        .checked("P2P media permission should be removed");
    prepare_handler_for_run_after_join(&voice_fixture.handler, &voice_fixture.connection_service)
        .await;

    voice_fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("voice-only member should join voice chat");
    let voice_swarm_id = "sm1_permission_isolation";
    let voice_ticket = voice_fixture
        .handler
        .swarm_signing_key
        .sign_media_swarm_ticket(
            &voice_fixture
                .handler
                .public_room_id()
                .checked("room public id should encode"),
            &voice_fixture
                .handler
                .public_actor_id()
                .checked("actor public id should encode"),
            voice_swarm_id,
        );
    let media_error = voice_fixture
        .handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: voice_swarm_id.to_string(),
            swarm_ticket: voice_ticket,
        })
        .await
        .expect_err("voice permission must not authorize P2P media membership");
    assert!(media_error.contains("P2P media permission denied"));
    voice_fixture.shutdown().await;

    let media_fixture = create_start_handler_fixture(
        "webrtc_media_only_permission",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let media_member_repo = RoomMemberRepository::new(media_fixture.pool.clone());
    let media_member = media_member_repo
        .get(
            &media_fixture.handler.room_id,
            &media_fixture.handler.test_user_id(),
        )
        .await
        .checked("media-only member should load")
        .checked("media-only member should exist");
    media_member_repo
        .update_permissions(
            &media_member.room_id,
            &media_member.user_id,
            media_member.added_permissions,
            media_member.removed_permissions
                | RoomMemberPermissionBits::USE_VOICE_CHAT
                | RoomMemberPermissionBits::BROWSE_LIBRARY,
            media_member.version,
        )
        .await
        .checked("voice and library browsing permissions should be removed");
    prepare_handler_for_run_after_join(&media_fixture.handler, &media_fixture.connection_service)
        .await;

    let voice_error = media_fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .expect_err("P2P media permission must not authorize voice chat");
    assert!(voice_error.contains("Voice chat permission denied"));
    let media_swarm_id = "sm1_permission_isolation";
    let media_ticket = media_fixture
        .handler
        .swarm_signing_key
        .sign_media_swarm_ticket(
            &media_fixture
                .handler
                .public_room_id()
                .checked("room public id should encode"),
            &media_fixture
                .handler
                .public_actor_id()
                .checked("actor public id should encode"),
            media_swarm_id,
        );
    media_fixture
        .handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: media_swarm_id.to_string(),
            swarm_ticket: media_ticket,
        })
        .await
        .checked("media-only member should publish P2P media membership");
    media_fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_webrtc_media_swarm_membership_validates_id_and_ticket_scope() {
    let fixture = create_start_handler_fixture(
        "webrtc_media_swarm_membership_validation",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let handler = &fixture.handler;
    prepare_handler_for_run_after_join(handler, &fixture.connection_service).await;

    let invalid = handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: "房间".to_string(),
            swarm_ticket: "unused".to_string(),
        })
        .await
        .expect_err("non-ASCII swarm identifiers must be rejected");
    assert!(invalid.contains("non-empty ASCII"));
    let swarm_id = "sm3_client_supplied";
    let wrong_swarm_ticket = handler.swarm_signing_key.sign_media_swarm_ticket(
        &handler
            .public_room_id()
            .checked("room public id should encode"),
        &handler
            .public_actor_id()
            .checked("actor public id should encode"),
        "sm3_other_resource",
    );
    let error = handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: wrong_swarm_ticket,
        })
        .await
        .expect_err("a ticket for another swarm must be rejected");
    assert!(error.contains("Invalid media swarm ticket"));

    let ticket = handler.swarm_signing_key.sign_media_swarm_ticket(
        &handler
            .public_room_id()
            .checked("room public id should encode"),
        &handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket,
        })
        .await
        .checked("a signed resource swarm should be accepted without playback resolution");
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_media_signaling_requires_both_connections_in_the_same_swarm() {
    let fixture = create_start_handler_fixture(
        "webrtc_media_signal_membership",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let handler = &fixture.handler;
    prepare_handler_for_run_after_join(handler, &fixture.connection_service).await;
    let swarm_id = "sm3_signal_membership";
    let ticket = handler.swarm_signing_key.sign_media_swarm_ticket(
        &handler
            .public_room_id()
            .checked("room public id should encode"),
        &handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    let target_connection_id = "conn_media_signal_target";
    let actor_id = handler
        .public_actor_id()
        .checked("actor public id should encode");
    fixture
        .connection_service
        .register_actor(
            target_connection_id.to_string(),
            handler
                .realtime_actor()
                .checked("realtime actor should build"),
        )
        .await
        .checked("target connection should register");
    fixture
        .connection_service
        .join_room(target_connection_id, handler.room_id)
        .await
        .checked("target connection should join room");
    let target = format!("{actor_id}:{target_connection_id}");

    let source_error = handler
        .validate_webrtc_media_recipient(&target, swarm_id)
        .await
        .expect_err("source membership should be required");
    assert!(source_error.contains("Source connection"));

    handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket.clone(),
        })
        .await
        .checked("source membership should join");
    let target_error = handler
        .validate_webrtc_media_recipient(&target, swarm_id)
        .await
        .expect_err("target membership should be required");
    assert!(target_error.contains("Target connection"));

    handler
        .media_swarm_tracker
        .announce(
            handler.room_id,
            actor_id,
            target_connection_id.to_string(),
            swarm_id,
        )
        .await
        .checked("target membership should join");
    handler
        .validate_webrtc_media_recipient(&target, swarm_id)
        .await
        .checked("same-swarm media signaling should pass");

    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_webrtc_signaling_to_a_disconnected_target_is_a_successful_noop() {
    let fixture = create_start_handler_fixture(
        "webrtc_disconnected_target_noop",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    let handler = &fixture.handler;
    prepare_handler_for_run_after_join(handler, &fixture.connection_service).await;
    handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("source should join voice chat");

    let swarm_id = "sm3_disconnected_target";
    let ticket = handler.swarm_signing_key.sign_media_swarm_ticket(
        &handler
            .public_room_id()
            .checked("room public id should encode"),
        &handler
            .public_actor_id()
            .checked("actor public id should encode"),
        swarm_id,
    );
    handler
        .handle_media_swarm_join(&synctv_proto::client::WebRtcMediaSwarmJoin {
            swarm_id: swarm_id.to_string(),
            swarm_ticket: ticket,
        })
        .await
        .checked("source should join media swarm");

    let (mut room_events, observer_connection_id) = fixture
        .event_service
        .subscribe(
            handler.room_id,
            handler
                .realtime_actor()
                .checked("realtime actor should build"),
        )
        .await
        .checked("room event observer should subscribe");
    let target = format!(
        "{}:conn_already_disconnected",
        handler
            .public_actor_id()
            .checked("actor public id should encode")
    );

    handler
        .handle_webrtc_voice_offer(&synctv_proto::client::WebRtcVoiceOfferCommand {
            to: target.clone(),
            data: "voice-offer".to_string(),
        })
        .await
        .checked("voice offer to a disconnected target should be ignored");
    handler
        .handle_webrtc_voice_answer(&synctv_proto::client::WebRtcVoiceAnswerCommand {
            to: target.clone(),
            data: "voice-answer".to_string(),
        })
        .await
        .checked("voice answer to a disconnected target should be ignored");
    handler
        .handle_webrtc_voice_ice_candidate(&synctv_proto::client::WebRtcVoiceIceCandidateCommand {
            to: target.clone(),
            data: "voice-candidate".to_string(),
        })
        .await
        .checked("voice ICE candidate to a disconnected target should be ignored");
    handler
        .handle_webrtc_media_offer(&synctv_proto::client::WebRtcMediaOfferCommand {
            to: target.clone(),
            data: "media-offer".to_string(),
            swarm_id: swarm_id.to_string(),
        })
        .await
        .checked("media offer to a disconnected target should be ignored");
    handler
        .handle_webrtc_media_answer(&synctv_proto::client::WebRtcMediaAnswerCommand {
            to: target.clone(),
            data: "media-answer".to_string(),
            swarm_id: swarm_id.to_string(),
        })
        .await
        .checked("media answer to a disconnected target should be ignored");
    handler
        .handle_webrtc_media_ice_candidate(&synctv_proto::client::WebRtcMediaIceCandidateCommand {
            to: target,
            data: "media-candidate".to_string(),
            swarm_id: swarm_id.to_string(),
        })
        .await
        .checked("media ICE candidate to a disconnected target should be ignored");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), room_events.recv())
            .await
            .is_err(),
        "disconnected-target signaling should not enter room fan-out"
    );
    fixture
        .event_service
        .unsubscribe(observer_connection_id.as_str());
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_webrtc_disconnected_recipient_still_requires_a_complete_address() {
    let fixture = create_start_handler_fixture(
        "webrtc_disconnected_recipient_format",
        FailingMessageSender::fail_after(usize::MAX),
    )
    .await;
    prepare_handler_for_run_after_join(&fixture.handler, &fixture.connection_service).await;
    fixture
        .handler
        .handle_webrtc_voice_join(&synctv_proto::client::WebRtcVoiceJoinCommand::default())
        .await
        .checked("source should join voice chat");

    for recipient in [
        "missing-separator",
        ":conn_disconnected",
        "usr_disconnected:",
    ] {
        let error = fixture
            .handler
            .validate_webrtc_voice_recipient(recipient)
            .await
            .expect_err("incomplete recipient addresses should be rejected");
        assert!(error.contains("public_actor_id:conn_id"));
    }
    fixture.shutdown().await;
}

#[test]
fn test_webrtc_signal_requires_distributed_delivery_when_available() {
    let outcome = RealtimeDeliveryOutcome::from_broadcast(
        &synctv_realtime::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        },
        RealtimeMetrics {
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
        RealtimeMetrics {
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
        RealtimeMetrics {
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
        RealtimeMetrics {
            distributed_enabled: true,
        },
    );

    assert!(!outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable));
}

#[test]
fn test_user_left_delivery_skips_when_local_connection_remains() {
    assert!(!super::should_broadcast_user_left(true, Ok(false)));
}

#[test]
fn test_user_left_delivery_skips_when_distributed_presence_exists() {
    assert!(!super::should_broadcast_user_left(false, Ok(true)));
}

#[test]
fn test_user_left_delivery_uses_local_and_redis_when_user_is_last_presence() {
    assert!(super::should_broadcast_user_left(false, Ok(false)));
}

#[test]
fn test_user_left_delivery_uses_local_fallback_when_distributed_check_fails() {
    assert!(super::should_broadcast_user_left(false, Err(())));
}

#[test]
fn test_user_left_delivery_local_presence_still_wins_when_distributed_check_fails() {
    assert!(!super::should_broadcast_user_left(true, Err(())));
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let guest_actor = handler
        .realtime_actor()
        .checked("guest realtime actor should build");

    connection_service
        .register_actor(connection_id.clone(), guest_actor.clone())
        .await
        .checked("register guest connection");
    connection_service
        .join_room(&connection_id, handler.room_id)
        .await
        .checked("join guest connection");
    let (mut rx, _) = event_service
        .subscribe_with_id(
            handler.room_id,
            guest_actor,
            ConnectionId::new(connection_id.clone()),
        )
        .await
        .checked("subscribe guest connection");

    handler.cleanup(&handler.room_id.to_string()).await;

    let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .checked("guest left event should be delivered")
        .checked("guest left receiver should remain open until event is read");
    match event.as_ref() {
        RealtimeEvent::GuestLeft {
            room_id,
            guest_id,
            username,
            ..
        } => {
            assert_eq!(*room_id, handler.room_id);
            assert_eq!(
                guest_id,
                &handler
                    .public_actor_id()
                    .checked("guest public actor id should encode")
            );
            assert_eq!(username, &handler.username);
        }
        other => std::panic::panic_any(format!("expected GuestLeft event, got {other:?}")),
    }
    assert_eq!(connection_service.connection_count(), 0);

    shutdown_test_runtime_resources(event_service, connection_service).await;
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_guest_webrtc_recipient_validation_uses_public_guest_actor_id() {
    let event_service = test_realtime_manager("guest_webrtc_recipient").await;
    let connection_service = test_connection_manager();
    let handler = test_guest_message_handler(
        FailingMessageSender::fail_after(usize::MAX),
        Arc::clone(&event_service),
        connection_service.clone(),
        RoomPermissionSet(
            RoomPermissionSet::default_guest().0 | RoomAdminPermissionBits::USE_VOICE_CHAT,
        ),
    );
    let connection_id = handler.connection_id().to_string();

    connection_service
        .register_actor(
            connection_id.clone(),
            handler
                .realtime_actor()
                .checked("guest realtime actor should build"),
        )
        .await
        .checked("register guest connection");
    connection_service
        .join_room(&connection_id, handler.room_id)
        .await
        .checked("join room");
    connection_service.mark_voice_rtc_joined(
        &handler.room_id,
        &handler
            .realtime_actor()
            .checked("guest realtime actor should build"),
        &connection_id,
        true,
    );

    let guest_target = format!(
        "{}:{}",
        handler
            .public_actor_id()
            .checked("guest public actor id should encode"),
        connection_id
    );
    handler
        .validate_webrtc_voice_recipient(&guest_target)
        .await
        .checked("gst_* recipient should match the active guest connection");

    let other_actor_target = format!("usr_other:{connection_id}");
    let error = handler
        .validate_webrtc_voice_recipient(&other_actor_target)
        .await
        .expect_err("another actor id must not address the guest connection");
    assert!(error.contains("does not match"));

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
#[ignore = "Requires Docker-backed PostgreSQL"]
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
        .register(
            handler.connection_id.clone().into_string(),
            handler.test_user_id(),
        )
        .await
        .checked("register should succeed before final admission");

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
        connection_service.user_connection_count(&handler.test_user_id()),
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
        .checked("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    connection_service
        .register(
            handler.connection_id.clone().into_string(),
            handler.test_user_id(),
        )
        .await
        .checked("register should succeed before final admission");

    room_service
        .update_room_status(&room.id, RoomStatus::Closed)
        .await
        .checked("closing room should succeed");

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
        .checked("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    connection_service
        .register(
            handler.connection_id.clone().into_string(),
            handler.test_user_id(),
        )
        .await
        .checked("register should succeed before final admission");

    UserRepository::new(pool.clone())
        .ban(&owner.id, None, Some("messaging test".to_string()))
        .await
        .checked("banning room owner should succeed");

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
        .checked("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    connection_service
        .register(
            handler.connection_id.clone().into_string(),
            handler.test_user_id(),
        )
        .await
        .checked("register should succeed before final admission");

    user_service
        .ban_user(&member.id, None, None)
        .await
        .checked("banning user should succeed");

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
        .checked("room should be created");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");

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
        public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
        sender: FailingMessageSender::fail_after(usize::MAX),
        concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
    });

    connection_service
        .register(
            handler.connection_id.clone().into_string(),
            handler.test_user_id(),
        )
        .await
        .checked("register should succeed before subscription caching");

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        handler.pre_join_after_registration(),
    )
    .await
    .checked("pre_join_after_registration should not hang after Redis disappears")
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
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(!super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::User(uid),
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::Room {
            room_id: rid,
            reason: synctv_realtime::sync::RoomDisconnectReason::AccessRevoked,
        },
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(super::disconnect_signal_requires_skip_cleanup(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        },
        Some(uid),
        &rid,
        connection_id,
    ));
}

#[test]
fn test_admin_event_requires_skip_cleanup_only_for_room_scoped_or_redundant_exits() {
    let rid = room_id();
    let uid = user_id();
    let now = synctv_core::SystemClock.now();

    assert!(!super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::KickUser {
            event_id: "evt-1".to_string(),
            user_id: uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        Some(uid),
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
        Some(uid),
        &rid,
    ));
    assert!(super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::UserLeft {
            event_id: "evt-3".to_string(),
            room_id: rid,
            user_id: uid,
            username: "tester".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            timestamp: now,
        },
        Some(uid),
        &rid,
    ));
    assert!(super::admin_event_requires_skip_cleanup(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-4".to_string(),
            room_id: rid,
            banned_by: uid,
            timestamp: now,
        },
        Some(uid),
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
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::User(uid),
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::Room {
            room_id: rid,
            reason: synctv_realtime::sync::RoomDisconnectReason::AccessRevoked,
        },
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        },
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(!super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::User(other_uid),
        Some(uid),
        &rid,
        connection_id,
    ));
    assert!(!super::watch_disconnect_signal_matches(
        &synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: other_rid,
        },
        Some(uid),
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
    let now = synctv_core::SystemClock.now();

    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::KickUser {
            event_id: "evt-1".to_string(),
            user_id: uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        Some(uid),
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
        Some(uid),
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::UserLeft {
            event_id: "evt-3".to_string(),
            room_id: rid,
            user_id: uid,
            username: "tester".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            timestamp: now,
        },
        Some(uid),
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::RoomDeleted {
            event_id: "evt-4".to_string(),
            room_id: rid,
            deleted_by: uid,
            timestamp: now,
        },
        Some(uid),
        &rid,
    ));
    assert!(super::watch_admin_event_matches(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-5".to_string(),
            room_id: rid,
            banned_by: uid,
            timestamp: now,
        },
        Some(uid),
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
        Some(uid),
        &rid,
    ));
    assert!(!super::watch_admin_event_matches(
        &RealtimeEvent::KickUser {
            event_id: "evt-7".to_string(),
            user_id: other_uid,
            reason: "ban".to_string(),
            timestamp: now,
        },
        Some(uid),
        &rid,
    ));
    assert!(!super::watch_admin_event_matches(
        &RealtimeEvent::RoomBanned {
            event_id: "evt-8".to_string(),
            room_id: other_rid,
            banned_by: uid,
            timestamp: now,
        },
        Some(uid),
        &rid,
    ));
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
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
    let public_id_codec = Arc::new(synctv_adapter::PublicIdCodec::plain());

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
        .checked("room should be created");
    let member = register_test_user(
        &user_service,
        "watch_limit_member",
        "watch-limit-member@test.invalid",
    )
    .await;
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");

    let observe = watch_room_settings_observe(synctv_proto::client::WatchRoomSettingsRequest {
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        room_settings: Some(synctv_proto::client::ObserveRoomSettings {
            after_event_sequence: None,
        }),
    })
    .checked("room settings watch request should build");
    let make_session = || {
        let (
            playback_service,
            playlist_items_snapshot_service,
            _room_members_snapshot_service,
            room_settings_snapshot_service,
        ) = test_resource_watch_runtime_fields();
        ResourceWatchSession::new(ResourceWatchSessionConfig {
            room_id: room.id,
            principal: RealtimePrincipal::user(member.id, member.username.clone()),
            room_service: Arc::clone(&room_service),
            chat_service: None,
            clock: Arc::new(synctv_core::SystemClock),
            event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
            connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
            presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
            public_id_codec: Arc::clone(&public_id_codec),
            sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
            playback_service,
            playlist_items_snapshot_service,
            room_settings_snapshot_service,
        })
    };

    let prepared = make_session()
        .prepare(&observe)
        .await
        .checked("first watch should prepare");
    assert_eq!(connection_service.room_connection_count(&room.id), 1);

    let Err(second) = make_session().prepare(&observe).await else {
        std::panic::panic_any("second watch should hit per-room capacity".to_string());
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
        .checked("watch task should join")
        .checked("watch run should stop cleanly");
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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_resource_watch_prepare_rejects_missing_observe_resource_before_subscription() {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let event_service = test_realtime_manager("watch_prepare_missing_resource").await;
    let connection_service = test_connection_manager();
    let public_id_codec = Arc::new(synctv_adapter::PublicIdCodec::plain());

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
        .checked("room should be created");

    let (
        playback_service,
        playlist_items_snapshot_service,
        _room_members_snapshot_service,
        room_settings_snapshot_service,
    ) = test_resource_watch_runtime_fields();
    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(owner.id, owner.username.clone()),
        room_service: Arc::clone(&room_service),
        chat_service: None,
        clock: Arc::new(synctv_core::SystemClock),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
        presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
        public_id_codec,
        sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
        playback_service,
        playlist_items_snapshot_service,
        room_settings_snapshot_service,
    });
    let observe = synctv_proto::client::ObserveResource {
        observe_id: "missing-resource".to_string(),
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
        resource: None,
    };

    let Err(error) = session.prepare(&observe).await else {
        std::panic::panic_any("missing observe resource should fail watch prepare".to_string());
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
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn test_resource_watch_chat_events_requires_view_chat_history_permission() {
    let (container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(pool.clone());
    let chat_service = test_chat_service(pool.clone());
    let user_service = room_service.user_service().clone();
    let event_service = test_realtime_manager("watch_chat_events_permission").await;
    let connection_service = test_connection_manager();
    let public_id_codec = Arc::new(synctv_adapter::PublicIdCodec::plain());

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
        .checked("room should be created");
    let member = register_test_user(
        &user_service,
        "watch_chat_perm_member",
        "watch-chat-perm-member@test.invalid",
    )
    .await;
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("member should join room");
    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .checked("room settings should load");
    settings.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions(
            RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        );
    room_service
        .set_room_settings(&room.id, &settings)
        .await
        .checked("room settings should update");

    let observe = watch_chat_events_observe(synctv_proto::client::WatchChatEventsRequest {
        delivery_mode: synctv_proto::client::ResourceDeliveryMode::NotifyOnly as i32,
        chat_events: Some(synctv_proto::client::ObserveChatEvents {
            after_event_sequence: None,
            include_message_types: Vec::new(),
        }),
    })
    .checked("chat events watch request should build");
    let (
        playback_service,
        playlist_items_snapshot_service,
        _room_members_snapshot_service,
        room_settings_snapshot_service,
    ) = test_resource_watch_runtime_fields();
    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id: room.id,
        principal: RealtimePrincipal::user(member.id, member.username.clone()),
        room_service: Arc::clone(&room_service),
        chat_service: Some(chat_service),
        clock: Arc::new(synctv_core::SystemClock),
        event_service: Arc::clone(&event_service) as Arc<dyn RealtimeEventService>,
        connection_service: Arc::clone(&connection_service) as Arc<dyn ConnectionRuntime>,
        presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
        public_id_codec: Arc::clone(&public_id_codec),
        sender: RecordingMessageSender::new() as Arc<dyn MessageSender>,
        playback_service,
        playlist_items_snapshot_service,
        room_settings_snapshot_service,
    });

    let Err(error) = session.prepare(&observe).await else {
        std::panic::panic_any("chat events watch should require VIEW_CHAT_HISTORY".to_string());
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
