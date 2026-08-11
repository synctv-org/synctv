use prost::Message;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::time::Duration;

use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{ChatEventKind, ChatMessageSelection, RealtimeActor, RoomId, UserId},
    service::{
        ChatService, OnlinePresenceService, RoomResourceEventPayload, RoomResourceKind, RoomService,
    },
};
use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

use super::MessageSender;
use crate::impls::client::convert::{
    chat_message_selection_from_proto_values, playback_client_profile_from_proto,
    proto_role_filter_to_room_role, room_settings_to_proto, try_playback_state_to_proto,
    try_room_member_to_proto_with_permissions,
};
use crate::impls::client::RoomActor;
use crate::impls::messaging::{
    chat_message_event_to_proto, chat_pin_event_to_proto, online_event_to_proto,
    room_member_event_to_proto,
};
use crate::impls::playback::PlaybackService;
use crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService;
use crate::impls::room_settings_snapshot::RoomSettingsSnapshotService;
use crate::resource_change::{
    provider_credential_resource_invalidation, resource_invalidations_for_cache_targets,
    resource_invalidations_for_room_event, ResourceInvalidation,
};
use synctv_proto::client::{ResourceDeliveryMode, ServerMessage};

const RESOURCE_EVALUATION_REUSE_WINDOW: Duration = Duration::from_secs(5);
const MEDIA_RESOURCE_REFRESH_DEDUP_WINDOW: Duration = Duration::from_secs(5);
pub(super) const MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION: usize = 64;
const CHAT_EVENT_REPLAY_BATCH_LIMIT: i32 = 500;
const CHAT_EVENT_REPLAY_BATCH_LIMIT_USIZE: usize = 500;
const ROOM_RESOURCE_EVENT_REPLAY_BATCH_LIMIT: i32 = 500;
const ROOM_RESOURCE_EVENT_REPLAY_BATCH_LIMIT_USIZE: usize = 500;
const ONLINE_COUNT_REFRESH_INTERVAL_SECONDS: i64 = 10;

fn event_cursor_for_chat_event(
    event: &synctv_core::models::ChatMessageEvent,
) -> synctv_proto::client::EventCursor {
    synctv_proto::client::EventCursor {
        event_id: Some(event.event_id.clone()),
        sequence: event.sequence,
    }
}

fn event_cursor_for_chat_pin_event(
    event: &synctv_core::models::ChatPinEvent,
) -> synctv_proto::client::EventCursor {
    synctv_proto::client::EventCursor {
        event_id: Some(event.event_id.clone()),
        sequence: event.sequence,
    }
}

fn proto_event_cursor(
    cursor: synctv_core::models::EventCursor,
) -> synctv_proto::client::EventCursor {
    synctv_proto::client::EventCursor {
        event_id: cursor.event_id,
        sequence: cursor.sequence,
    }
}

fn chat_event_visible_to_observation(
    observation: &ResourceObservation,
    event: &synctv_core::models::ChatMessageEvent,
) -> bool {
    match &observation.resource {
        ObservedResource::ChatEvents { selection } => {
            selection.includes(event.message.message.message_type)
        }
        _ => true,
    }
}

pub(super) fn room_member_event_visible_to_observer(
    event: &RealtimeEvent,
    observer_user_id: Option<UserId>,
) -> bool {
    match event {
        RealtimeEvent::UserJoined { user_id, .. }
        | RealtimeEvent::UserLeft { user_id, .. }
        | RealtimeEvent::KickUserFromRoom { user_id, .. } => observer_user_id != Some(*user_id),
        RealtimeEvent::PermissionChanged {
            target_user_id,
            role_changed,
            ..
        } => observer_user_id != Some(*target_user_id) && *role_changed,
        RealtimeEvent::GuestJoined { .. } | RealtimeEvent::GuestLeft { .. } => true,
        _ => false,
    }
}

#[derive(Debug, Clone, Default)]
struct ResourceObserverState {
    observations: HashMap<String, ResourceObservation>,
    pending_observe_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ResourceObservation {
    observe_id: String,
    last_fingerprint: String,
    delivery_mode: ResourceDeliveryMode,
    resource: ObservedResource,
    expires_at: Option<i64>,
    last_sent_event_sequence: i64,
}

#[derive(Debug, Clone)]
struct ObservationUpdate {
    changed: bool,
    changed_message: Option<synctv_proto::client::ResourceEvent>,
}

#[derive(Debug, Clone)]
struct ResourceEvaluation {
    fingerprint: String,
    expires_at: Option<i64>,
    payload: synctv_proto::client::resource_event::Payload,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ObservationEvaluationKey {
    PlaybackState {
        delivery_mode: i32,
    },
    Playback {
        delivery_mode: i32,
        playback_client_profile: Option<String>,
    },
    RoomSettings {
        delivery_mode: i32,
    },
    PlaylistItems {
        delivery_mode: i32,
        request: Vec<u8>,
    },
    PlaybackHistory {
        delivery_mode: i32,
        request: Vec<u8>,
    },
    RoomMemberEvents {
        delivery_mode: i32,
    },
    SelfRoomMember {
        delivery_mode: i32,
    },
    ChatEvents {
        delivery_mode: i32,
        include_message_types: Vec<i32>,
    },
    ChatPinEvents {
        delivery_mode: i32,
    },
    OnlineCount {
        delivery_mode: i32,
        roles: Vec<i32>,
        user_ids: Vec<UserId>,
    },
    OnlineEvent {
        delivery_mode: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ResourceActorScope {
    User(UserId),
    Guest(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SharedObservationEvaluationKey {
    room_id: RoomId,
    actor: Option<ResourceActorScope>,
    service_id: usize,
    evaluation: ObservationEvaluationKey,
}

#[derive(Clone)]
enum SharedResourceServiceWeak {
    RoomService(Weak<RoomService>),
    Playback(Weak<dyn PlaybackService>),
    RoomSettings(Weak<dyn RoomSettingsSnapshotService>),
    PlaylistItems(Weak<dyn PlaylistItemsSnapshotService>),
}

impl SharedResourceServiceWeak {
    fn is_alive(&self) -> bool {
        match self {
            Self::RoomService(service) => service.upgrade().is_some(),
            Self::Playback(service) => service.upgrade().is_some(),
            Self::RoomSettings(service) => service.upgrade().is_some(),
            Self::PlaylistItems(service) => service.upgrade().is_some(),
        }
    }
}

struct SharedResourceServiceIdentity {
    id: usize,
    weak: SharedResourceServiceWeak,
}

static RESOURCE_EVALUATION_SINGLEFLIGHT: LazyLock<
    tokio::sync::Mutex<HashMap<SharedObservationEvaluationKey, Arc<SharedResourceEvaluationEntry>>>,
> = LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

struct SharedResourceEvaluationEntry {
    result: tokio::sync::OnceCell<Result<ResourceEvaluation, String>>,
    completed_at: OnceLock<tokio::time::Instant>,
    resource_generation: u64,
    service: SharedResourceServiceWeak,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResourceSubscriberKey {
    connection_id: String,
    observe_id: String,
}

#[derive(Clone)]
struct ResourceHubSubscription {
    observer: Weak<ResourceObserver>,
    observation: ResourceObservation,
    revision: u64,
}

#[derive(Clone)]
struct ResourceHubRefreshSubscription {
    key: ResourceSubscriberKey,
    observer: Arc<ResourceObserver>,
    observation: ResourceObservation,
    force: bool,
    revision: u64,
}

enum SubscriptionRefreshCommit {
    Committed,
    Stale,
    SendFailed(String),
}

#[derive(Debug, Clone)]
struct ResourceSendFailure {
    connection_id: String,
    error: String,
}

#[derive(Debug, Clone, Default)]
struct ResourceRefreshOutcome {
    send_failures: Vec<ResourceSendFailure>,
}

impl ResourceRefreshOutcome {
    fn record_send_failure(&mut self, connection_id: impl Into<String>, error: impl Into<String>) {
        self.send_failures.push(ResourceSendFailure {
            connection_id: connection_id.into(),
            error: error.into(),
        });
    }

    fn error_for_connection(&self, connection_id: Option<&str>) -> Option<String> {
        let connection_id = connection_id?;
        self.send_failures
            .iter()
            .find(|failure| failure.connection_id == connection_id)
            .map(|failure| failure.error.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MediaResourceHubKey {
    room_id: RoomId,
    service_id: usize,
}

#[derive(Default)]
struct MediaResourceHubState {
    subscriptions: HashMap<ResourceSubscriberKey, ResourceHubSubscription>,
    next_revision: u64,
    subscription_generation: u64,
    resource_generation: u64,
    in_flight_refreshes: HashMap<String, MediaResourceRefreshEntry>,
    completed_refreshes: HashMap<String, CompletedMediaResourceRefresh>,
}

impl MediaResourceHubState {
    fn bump_subscription_generation(&mut self) {
        self.subscription_generation = self.subscription_generation.saturating_add(1);
    }
}

#[derive(Clone)]
struct MediaResourceRefreshEntry {
    result: Arc<tokio::sync::OnceCell<ResourceRefreshOutcome>>,
    subscription_generation: u64,
}

struct CompletedMediaResourceRefresh {
    completed_at: tokio::time::Instant,
    subscription_generation: u64,
}

pub(super) struct MediaResourceHub {
    room_id: RoomId,
    room_service: Arc<RoomService>,
    state: tokio::sync::Mutex<MediaResourceHubState>,
}

static MEDIA_RESOURCE_HUBS: LazyLock<
    parking_lot::Mutex<HashMap<MediaResourceHubKey, Weak<MediaResourceHub>>>,
> = LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

fn media_resource_hub(room_id: RoomId, room_service: &Arc<RoomService>) -> Arc<MediaResourceHub> {
    let key = MediaResourceHubKey {
        room_id,
        service_id: Arc::as_ptr(room_service) as usize,
    };
    let mut hubs = MEDIA_RESOURCE_HUBS.lock();
    if let Some(hub) = hubs.get(&key).and_then(Weak::upgrade) {
        return hub;
    }
    let hub = Arc::new(MediaResourceHub {
        room_id,
        room_service: Arc::clone(room_service),
        state: tokio::sync::Mutex::new(MediaResourceHubState::default()),
    });
    hubs.insert(key, Arc::downgrade(&hub));
    hub
}

impl SharedResourceEvaluationEntry {
    fn new(service: SharedResourceServiceWeak, resource_generation: u64) -> Self {
        Self {
            result: tokio::sync::OnceCell::new(),
            completed_at: OnceLock::new(),
            resource_generation,
            service,
        }
    }

    fn can_reuse_completed(&self, now: tokio::time::Instant) -> bool {
        self.completed_at.get().is_some_and(|completed_at| {
            now.duration_since(*completed_at) <= RESOURCE_EVALUATION_REUSE_WINDOW
        })
    }

    fn mark_completed(&self, completed_at: tokio::time::Instant) {
        if self.completed_at.set(completed_at).is_err() {
            tracing::debug!(
                resource_generation = self.resource_generation,
                "shared resource evaluation completion timestamp was already recorded"
            );
        }
    }
}

fn schedule_resource_evaluation_singleflight_cleanup(
    key: SharedObservationEvaluationKey,
    entry: Arc<SharedResourceEvaluationEntry>,
) {
    spawn_monitored("resource_evaluation_singleflight_cleanup", async move {
        tokio::time::sleep(RESOURCE_EVALUATION_REUSE_WINDOW).await;
        let mut in_flight = RESOURCE_EVALUATION_SINGLEFLIGHT.lock().await;
        if in_flight
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &entry))
        {
            in_flight.remove(&key);
        }
    });
}

#[derive(Debug, Clone)]
enum ObservedResource {
    PlaybackState,
    Playback {
        playback_client_profile: Option<synctv_core::provider::PlaybackClientProfile>,
    },
    RoomSettings,
    PlaylistItems {
        request: synctv_proto::client::ListPlaylistItemsRequest,
    },
    PlaybackHistory {
        request: synctv_proto::client::ListPlaybackHistoryRequest,
    },
    RoomMemberEvents,
    SelfRoomMember,
    ChatEvents {
        selection: ChatMessageSelection,
    },
    ChatPinEvents,
    OnlineCount {
        roles: Vec<i32>,
        user_ids: Vec<UserId>,
    },
    OnlineEvent {
        roles: Vec<i32>,
        kinds: Vec<i32>,
        user_ids: Vec<UserId>,
    },
}

impl ResourceObservation {
    fn accepts_inline_playback_state_event(&self) -> bool {
        matches!(self.resource, ObservedResource::PlaybackState)
    }

    fn room_resource_cursor_types(&self) -> Option<&'static [RoomResourceKind]> {
        match &self.resource {
            ObservedResource::PlaybackState => Some(&[RoomResourceKind::PlaybackState]),
            ObservedResource::Playback { .. } => Some(&[
                RoomResourceKind::PlaybackState,
                RoomResourceKind::Media,
                RoomResourceKind::Playlist,
                RoomResourceKind::PlaylistItems,
            ]),
            ObservedResource::RoomSettings => {
                Some(&[RoomResourceKind::RoomSettings, RoomResourceKind::Room])
            }
            ObservedResource::PlaylistItems { .. } => Some(&[
                RoomResourceKind::PlaylistItems,
                RoomResourceKind::Playlist,
                RoomResourceKind::Media,
            ]),
            ObservedResource::PlaybackHistory { .. } => Some(&[
                RoomResourceKind::PlaybackState,
                RoomResourceKind::Playlist,
                RoomResourceKind::Media,
            ]),
            ObservedResource::RoomMemberEvents => Some(&[RoomResourceKind::RoomMemberEvents]),
            ObservedResource::SelfRoomMember => Some(&[
                RoomResourceKind::RoomMemberEvents,
                RoomResourceKind::RoomSettings,
            ]),
            ObservedResource::ChatEvents { .. } | ObservedResource::OnlineEvent { .. } => None,
            ObservedResource::ChatPinEvents => Some(&[RoomResourceKind::ChatPins]),
            ObservedResource::OnlineCount { .. } => Some(&[RoomResourceKind::OnlineCount]),
        }
    }

    fn exposes_client_event_cursor(&self) -> bool {
        match self.resource {
            ObservedResource::Playback { .. } => false,
            ObservedResource::ChatEvents { .. } | ObservedResource::ChatPinEvents => true,
            _ => self.room_resource_cursor_types().is_some(),
        }
    }

    fn evaluation_key(&self) -> ObservationEvaluationKey {
        let delivery_mode = self.delivery_mode as i32;
        match &self.resource {
            ObservedResource::PlaybackState => {
                ObservationEvaluationKey::PlaybackState { delivery_mode }
            }
            ObservedResource::Playback {
                playback_client_profile,
            } => ObservationEvaluationKey::Playback {
                delivery_mode,
                playback_client_profile: playback_client_profile
                    .as_ref()
                    .map(synctv_core::provider::PlaybackClientProfile::cache_fingerprint),
            },
            ObservedResource::RoomSettings => {
                ObservationEvaluationKey::RoomSettings { delivery_mode }
            }
            ObservedResource::PlaylistItems { request } => {
                ObservationEvaluationKey::PlaylistItems {
                    delivery_mode,
                    request: request.encode_to_vec(),
                }
            }
            ObservedResource::PlaybackHistory { request } => {
                ObservationEvaluationKey::PlaybackHistory {
                    delivery_mode,
                    request: request.encode_to_vec(),
                }
            }
            ObservedResource::RoomMemberEvents => {
                ObservationEvaluationKey::RoomMemberEvents { delivery_mode }
            }
            ObservedResource::SelfRoomMember => {
                ObservationEvaluationKey::SelfRoomMember { delivery_mode }
            }
            ObservedResource::ChatEvents { selection } => ObservationEvaluationKey::ChatEvents {
                delivery_mode,
                include_message_types: selection
                    .message_type_codes()
                    .into_iter()
                    .map(i32::from)
                    .collect(),
            },
            ObservedResource::ChatPinEvents => {
                ObservationEvaluationKey::ChatPinEvents { delivery_mode }
            }
            ObservedResource::OnlineCount { roles, user_ids } => {
                ObservationEvaluationKey::OnlineCount {
                    delivery_mode,
                    roles: roles.clone(),
                    user_ids: user_ids.clone(),
                }
            }
            ObservedResource::OnlineEvent { .. } => {
                ObservationEvaluationKey::OnlineEvent { delivery_mode }
            }
        }
    }

    fn consume_one_shot_options(&mut self) {
        if let ObservedResource::PlaylistItems { request } = &mut self.resource {
            request.refresh = false;
        }
    }
}

pub(super) struct ResourceObserver {
    room_id: RoomId,
    actor: RoomActor,
    connection_id: String,
    room_service: Arc<RoomService>,
    chat_service: Option<Arc<ChatService>>,
    clock: Arc<dyn synctv_core::Clock>,
    presence_service: Arc<OnlinePresenceService>,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    sender: Arc<dyn MessageSender>,
    pub(super) room_hub: Arc<MediaResourceHub>,
    playback_service: Arc<dyn PlaybackService>,
    playlist_items_snapshot_service: Arc<dyn PlaylistItemsSnapshotService>,
    room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
    room_settings_snapshot_service_id: usize,
    state: tokio::sync::Mutex<ResourceObserverState>,
    observation_change: tokio::sync::Notify,
}

pub(super) struct ResourceObserverParams {
    pub(super) room_id: RoomId,
    pub(super) actor: RoomActor,
    pub(super) connection_id: String,
    pub(super) room_service: Arc<RoomService>,
    pub(super) chat_service: Option<Arc<ChatService>>,
    pub(super) clock: Arc<dyn synctv_core::Clock>,
    pub(super) presence_service: Arc<OnlinePresenceService>,
    pub(super) public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pub(super) sender: Arc<dyn MessageSender>,
    pub(super) playback_service: Arc<dyn PlaybackService>,
    pub(super) playlist_items_snapshot_service: Arc<dyn PlaylistItemsSnapshotService>,
    pub(super) room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
}

impl ResourceObserver {
    pub(super) fn new(params: ResourceObserverParams) -> Self {
        let ResourceObserverParams {
            room_id,
            actor,
            connection_id,
            room_service,
            chat_service,
            clock,
            presence_service,
            public_id_codec,
            sender,
            playback_service,
            playlist_items_snapshot_service,
            room_settings_snapshot_service,
        } = params;
        let room_settings_snapshot_service_id =
            Arc::as_ptr(&room_settings_snapshot_service).cast::<()>() as usize;
        let room_hub = media_resource_hub(room_id, &room_service);
        Self {
            room_id,
            actor,
            connection_id,
            room_service,
            chat_service,
            clock,
            presence_service,
            public_id_codec,
            sender,
            room_hub,
            playback_service,
            playlist_items_snapshot_service,
            room_settings_snapshot_service,
            room_settings_snapshot_service_id,
            state: tokio::sync::Mutex::new(ResourceObserverState::default()),
            observation_change: tokio::sync::Notify::new(),
        }
    }

    async fn username_for_chat_message(
        &self,
        message: &synctv_core::models::ChatMessage,
    ) -> Result<Option<String>, String> {
        if message.message_type.is_system() {
            return Ok(None);
        }
        let Some(user_id) = message.user_id else {
            return Ok(None);
        };
        self.room_service
            .user_service()
            .get_username(&user_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn chat_message_event_to_proto(
        &self,
        event: &synctv_core::models::ChatMessageEvent,
    ) -> Result<synctv_proto::client::ChatMessageEvent, String> {
        let username = self
            .username_for_chat_message(&event.message.message)
            .await?;
        let mut proto = chat_message_event_to_proto(event, &self.public_id_codec)?;
        if let Some(message) = proto.message.as_mut() {
            message.username = username;
        }
        Ok(proto)
    }

    async fn chat_pin_event_to_proto(
        &self,
        event: &synctv_core::models::ChatPinEvent,
    ) -> Result<synctv_proto::client::ChatPinEvent, String> {
        let username = self
            .username_for_chat_message(&event.message.message)
            .await?;
        let mut proto = chat_pin_event_to_proto(event, &self.public_id_codec)?;
        if let Some(message) = proto.message.as_mut() {
            message.username = username;
        }
        Ok(proto)
    }

    async fn resource_actor(&self) -> Result<RoomActor, String> {
        match &self.actor {
            RoomActor::User { .. } => Ok(self.actor.clone()),
            RoomActor::Guest(access) => {
                let mut access = access.clone();
                access.permissions = self
                    .room_service
                    .get_guest_permissions(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(RoomActor::Guest(access))
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn has_observation(&self, observe_id: &str) -> bool {
        self.state
            .lock()
            .await
            .observations
            .contains_key(observe_id)
    }

    pub(super) async fn has_chat_events_observation(&self) -> bool {
        self.state
            .lock()
            .await
            .observations
            .values()
            .any(|observation| matches!(observation.resource, ObservedResource::ChatEvents { .. }))
    }

    fn shared_evaluation_key(
        &self,
        observation: &ResourceObservation,
    ) -> SharedObservationEvaluationKey {
        let service_identity = self.resource_evaluation_service_identity(&observation.resource);
        SharedObservationEvaluationKey {
            room_id: self.room_id,
            actor: self.resource_evaluation_actor_scope(&observation.resource),
            service_id: service_identity.id,
            evaluation: observation.evaluation_key(),
        }
    }

    async fn replace_local_observation(&self, observation: ResourceObservation) {
        let mut state = self.state.lock().await;
        if state.observations.contains_key(&observation.observe_id) {
            state
                .observations
                .insert(observation.observe_id.clone(), observation);
            drop(state);
            self.observation_change.notify_waiters();
        }
    }

    async fn local_observation(&self, observe_id: &str) -> Option<ResourceObservation> {
        self.state
            .lock()
            .await
            .observations
            .get(observe_id)
            .cloned()
    }

    async fn remove_local_observation(&self, observe_id: &str) {
        let mut state = self.state.lock().await;
        let removed_observation = state.observations.remove(observe_id).is_some();
        let removed_pending = state.pending_observe_ids.remove(observe_id);
        let changed = removed_observation || removed_pending;
        drop(state);
        if changed {
            self.observation_change.notify_waiters();
        }
    }

    pub(super) async fn clear_observations(&self) {
        let changed = {
            let mut state = self.state.lock().await;
            let changed = !state.observations.is_empty() || !state.pending_observe_ids.is_empty();
            state.observations.clear();
            state.pending_observe_ids.clear();
            changed
        };
        if changed {
            self.observation_change.notify_waiters();
        }
        self.room_hub
            .unregister_connection(&self.connection_id)
            .await;
    }

    pub(super) async fn room_has_playback_observers(room_id: RoomId) -> bool {
        let hubs = {
            let mut registry = MEDIA_RESOURCE_HUBS.lock();
            registry.retain(|_, hub| hub.strong_count() > 0);
            registry
                .iter()
                .filter_map(|(key, hub)| (key.room_id == room_id).then(|| hub.upgrade()).flatten())
                .collect::<Vec<_>>()
        };

        for hub in hubs {
            if hub.has_playback_observations().await {
                return true;
            }
        }
        false
    }

    pub(super) async fn active_playback_rooms() -> Vec<RoomId> {
        let hubs = {
            let mut registry = MEDIA_RESOURCE_HUBS.lock();
            registry.retain(|_, hub| hub.strong_count() > 0);
            registry
                .iter()
                .filter_map(|(key, hub)| hub.upgrade().map(|hub| (key.room_id, hub)))
                .collect::<Vec<_>>()
        };

        let mut rooms = HashSet::new();
        for (room_id, hub) in hubs {
            if hub.has_playback_observations().await {
                rooms.insert(room_id);
            }
        }
        rooms.into_iter().collect()
    }

    async fn try_reserve_observation_slot(&self, observe_id: &str) -> bool {
        let mut state = self.state.lock().await;
        if state.observations.contains_key(observe_id)
            || state.pending_observe_ids.contains(observe_id)
        {
            return true;
        }
        if state.observations.len() + state.pending_observe_ids.len()
            >= MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION
        {
            return false;
        }
        state.pending_observe_ids.insert(observe_id.to_string());
        true
    }

    async fn release_pending_observation_slot(&self, observe_id: &str) {
        self.state
            .lock()
            .await
            .pending_observe_ids
            .remove(observe_id);
    }

    async fn commit_local_observation(
        &self,
        observation: ResourceObservation,
    ) -> Option<ResourceObservation> {
        let observe_id = observation.observe_id.clone();
        let mut state = self.state.lock().await;
        let has_existing_observation = state.observations.contains_key(&observe_id);
        let had_pending_reservation = state.pending_observe_ids.remove(&observe_id);
        if !has_existing_observation
            && !had_pending_reservation
            && state.observations.len() >= MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION
        {
            return None;
        }
        state.observations.insert(observe_id.clone(), observation);
        let observation = state.observations.get(&observe_id).cloned();
        drop(state);
        self.observation_change.notify_waiters();
        observation
    }

    fn observation_start_sequence(
        observation: &ResourceObservation,
        request: &synctv_proto::client::ObserveResource,
    ) -> i64 {
        let requested_sequence = Self::validated_requested_replay_sequence(request).unwrap_or(0);
        if observation.exposes_client_event_cursor() {
            requested_sequence
        } else {
            0
        }
    }

    fn requested_replay_sequence(request: &synctv_proto::client::ObserveResource) -> Option<i64> {
        request
            .resource
            .as_ref()
            .and_then(|resource| match resource {
                synctv_proto::client::observe_resource::Resource::PlaybackState(_)
                | synctv_proto::client::observe_resource::Resource::Playback(_)
                | synctv_proto::client::observe_resource::Resource::OnlineCount(_)
                | synctv_proto::client::observe_resource::Resource::OnlineEvent(_) => None,
                synctv_proto::client::observe_resource::Resource::RoomSettings(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::PlaylistItems(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::RoomMemberEvents(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::SelfRoomMember(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::ChatEvents(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::ChatPinEvents(observe) => {
                    observe.after_event_sequence
                }
                synctv_proto::client::observe_resource::Resource::PlaybackHistory(observe) => {
                    observe.after_event_sequence
                }
            })
    }

    fn requested_playback_state_event_sequence(
        request: &synctv_proto::client::ObserveResource,
    ) -> Option<i64> {
        match request.resource.as_ref()? {
            synctv_proto::client::observe_resource::Resource::PlaybackState(observe) => {
                observe.event_sequence
            }
            _ => None,
        }
    }

    fn validate_requested_replay_sequence(
        request: &synctv_proto::client::ObserveResource,
    ) -> Result<(), String> {
        if Self::requested_replay_sequence(request).is_some_and(|sequence| sequence < 0) {
            return Err("after_event_sequence must be non-negative".to_string());
        }
        if Self::requested_playback_state_event_sequence(request)
            .is_some_and(|sequence| sequence < 0)
        {
            return Err("event_sequence must be non-negative".to_string());
        }
        Ok(())
    }

    fn validated_requested_replay_sequence(
        request: &synctv_proto::client::ObserveResource,
    ) -> Option<i64> {
        Self::requested_replay_sequence(request).inspect(|sequence| {
            debug_assert!(
                *sequence >= 0,
                "requested replay sequence must be validated before use"
            );
        })
    }

    fn validated_requested_playback_state_event_sequence(
        request: &synctv_proto::client::ObserveResource,
    ) -> Option<i64> {
        Self::requested_playback_state_event_sequence(request).inspect(|sequence| {
            debug_assert!(
                *sequence >= 0,
                "requested playback state event sequence must be validated before use"
            );
        })
    }

    fn apply_event_cursor_to_observation(
        observation: &mut ResourceObservation,
        cursor: &synctv_proto::client::EventCursor,
    ) -> bool {
        if cursor.sequence <= observation.last_sent_event_sequence {
            return false;
        }
        observation.last_sent_event_sequence = cursor.sequence;
        true
    }

    async fn initial_event_cursor_for_observation(
        &self,
        observation: &ResourceObservation,
        start_sequence: i64,
        exposes_client_event_cursor: bool,
        has_requested_replay_sequence: bool,
    ) -> Result<Option<synctv_proto::client::EventCursor>, String> {
        if !exposes_client_event_cursor {
            return Ok(None);
        }
        if has_requested_replay_sequence {
            return Ok(Some(synctv_proto::client::EventCursor {
                event_id: None,
                sequence: start_sequence,
            }));
        }
        if let ObservedResource::ChatEvents { selection } = &observation.resource {
            let Some(chat_service) = self.chat_service.as_ref() else {
                return Ok(Some(synctv_proto::client::EventCursor {
                    event_id: None,
                    sequence: start_sequence,
                }));
            };
            return chat_service
                .latest_event_cursor_for_room(&self.room_id, selection)
                .await
                .map(proto_event_cursor)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        let Some(resource_types) = observation.room_resource_cursor_types() else {
            return Ok(None);
        };
        if resource_types.is_empty() {
            return Ok(None);
        }
        let cursor = self
            .room_service
            .latest_room_resource_event_cursor_for_resource_types(&self.room_id, resource_types)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(synctv_proto::client::EventCursor {
            event_id: cursor.event_id,
            sequence: cursor.sequence.max(start_sequence),
        }))
    }
}

impl MediaResourceHub {
    async fn has_playback_observations(&self) -> bool {
        let state = self.state.lock().await;
        state.subscriptions.values().any(|subscription| {
            matches!(
                subscription.observation.resource,
                ObservedResource::Playback { .. }
            )
        })
    }

    async fn register_observation(
        &self,
        observer: &Arc<ResourceObserver>,
        observation: ResourceObservation,
    ) {
        let key = ResourceSubscriberKey {
            connection_id: observer.connection_id.clone(),
            observe_id: observation.observe_id.clone(),
        };
        let mut state = self.state.lock().await;
        state.next_revision = state.next_revision.saturating_add(1);
        let revision = state.next_revision;
        state.bump_subscription_generation();
        state.subscriptions.insert(
            key,
            ResourceHubSubscription {
                observer: Arc::downgrade(observer),
                observation,
                revision,
            },
        );
    }

    async fn unregister_observation(&self, connection_id: &str, observe_id: &str) {
        let mut state = self.state.lock().await;
        if state
            .subscriptions
            .remove(&ResourceSubscriberKey {
                connection_id: connection_id.to_string(),
                observe_id: observe_id.to_string(),
            })
            .is_some()
        {
            state.bump_subscription_generation();
        }
    }

    async fn unregister_connection(&self, connection_id: &str) {
        let mut state = self.state.lock().await;
        let previous_len = state.subscriptions.len();
        state
            .subscriptions
            .retain(|key, _| key.connection_id != connection_id);
        if state.subscriptions.len() != previous_len {
            state.bump_subscription_generation();
        }
    }

    async fn resource_generation(&self) -> u64 {
        self.state.lock().await.resource_generation
    }

    async fn has_subscriptions(&self) -> bool {
        !self.state.lock().await.subscriptions.is_empty()
    }

    async fn bump_resource_generation(&self) {
        let mut state = self.state.lock().await;
        state.resource_generation = state.resource_generation.saturating_add(1);
    }

    pub(super) async fn refresh_for_room_event(
        self: &Arc<Self>,
        event: &RealtimeEvent,
        fatal_connection_id: Option<&str>,
    ) -> Result<(), String> {
        let invalidations = resource_invalidations_for_room_event(event);
        if invalidations.is_empty() {
            return Ok(());
        }
        if !self.has_subscriptions().await {
            return Ok(());
        }
        let event_cursor = match self
            .room_service
            .room_resource_event_cursor_by_event_id(&self.room_id, event.event_id())
            .await
        {
            Ok(cursor) => cursor.map(proto_event_cursor),
            Err(error) => {
                tracing::warn!(
                    room_id = %self.room_id,
                    event_id = %event.event_id(),
                    error = %error,
                    "Failed to load durable room resource event cursor for live refresh"
                );
                None
            }
        };
        if event_cursor.is_none() {
            tracing::warn!(
                room_id = %self.room_id,
                event_id = %event.event_id(),
                event_type = %event.event_type(),
                "Refreshing live room resources without a durable event cursor"
            );
        }
        let outcome = self
            .refresh_for_room_event_invalidations_with_key(
                Some(format!("cluster:{}", event.event_id())),
                invalidations,
                event,
                false,
                event_cursor,
            )
            .await;
        if let Some(error) = outcome.error_for_connection(fatal_connection_id) {
            Err(error)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    async fn refresh_for_invalidations(
        self: &Arc<Self>,
        invalidations: Vec<ResourceInvalidation>,
        force: bool,
    ) -> ResourceRefreshOutcome {
        self.refresh_for_invalidations_with_key(None, invalidations, force, None)
            .await
    }

    #[cfg(test)]
    pub(super) async fn refresh_for_room_event_with_cursor(
        self: &Arc<Self>,
        event: &RealtimeEvent,
        fatal_connection_id: Option<&str>,
        cursor: synctv_proto::client::EventCursor,
    ) -> Result<(), String> {
        let invalidations = resource_invalidations_for_room_event(event);
        if invalidations.is_empty() {
            return Ok(());
        }
        let outcome = self
            .refresh_for_room_event_invalidations_with_key(
                Some(format!("cluster:{}", event.event_id())),
                invalidations,
                event,
                false,
                Some(cursor),
            )
            .await;
        if let Some(error) = outcome.error_for_connection(fatal_connection_id) {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn refresh_for_invalidations_with_key(
        self: &Arc<Self>,
        refresh_key: Option<String>,
        invalidations: Vec<ResourceInvalidation>,
        force: bool,
        event_cursor: Option<synctv_proto::client::EventCursor>,
    ) -> ResourceRefreshOutcome {
        if invalidations.is_empty() {
            return ResourceRefreshOutcome::default();
        }
        self.bump_resource_generation().await;
        if let Some(refresh_key) = refresh_key {
            let Some(entry) = self.start_deduped_refresh(&refresh_key).await else {
                return ResourceRefreshOutcome::default();
            };
            let result = entry
                .result
                .get_or_init(|| async {
                    self.refresh_for_invalidations_uncached(
                        &invalidations,
                        force,
                        event_cursor.clone(),
                    )
                    .await
                })
                .await
                .clone();
            self.finish_deduped_refresh(&refresh_key, &entry).await;
            return result;
        }
        self.refresh_for_invalidations_uncached(&invalidations, force, event_cursor)
            .await
    }

    async fn refresh_for_room_event_invalidations_with_key(
        self: &Arc<Self>,
        refresh_key: Option<String>,
        invalidations: Vec<ResourceInvalidation>,
        event: &RealtimeEvent,
        force: bool,
        event_cursor: Option<synctv_proto::client::EventCursor>,
    ) -> ResourceRefreshOutcome {
        if invalidations.is_empty() {
            return ResourceRefreshOutcome::default();
        }
        self.bump_resource_generation().await;
        if let Some(refresh_key) = refresh_key {
            let Some(entry) = self.start_deduped_refresh(&refresh_key).await else {
                return ResourceRefreshOutcome::default();
            };
            let result = entry
                .result
                .get_or_init(|| async {
                    self.refresh_for_room_event_invalidations_uncached(
                        &invalidations,
                        event,
                        force,
                        event_cursor.clone(),
                    )
                    .await
                })
                .await
                .clone();
            self.finish_deduped_refresh(&refresh_key, &entry).await;
            return result;
        }
        self.refresh_for_room_event_invalidations_uncached(
            &invalidations,
            event,
            force,
            event_cursor,
        )
        .await
    }

    async fn refresh_for_cache_targets(
        self: &Arc<Self>,
        event_id: &str,
        targets: Vec<CacheTarget>,
    ) -> ResourceRefreshOutcome {
        if targets.is_empty() {
            return ResourceRefreshOutcome::default();
        }
        self.bump_resource_generation().await;
        let refresh_key = format!("cache:{event_id}:room:{}", self.room_id);
        let Some(entry) = self.start_deduped_refresh(&refresh_key).await else {
            return ResourceRefreshOutcome::default();
        };
        let result = entry
            .result
            .get_or_init(|| async { self.refresh_for_cache_targets_uncached(&targets).await })
            .await
            .clone();
        self.finish_deduped_refresh(&refresh_key, &entry).await;
        result
    }

    async fn start_deduped_refresh(&self, refresh_key: &str) -> Option<MediaResourceRefreshEntry> {
        let mut state = self.state.lock().await;
        let now = tokio::time::Instant::now();
        state.completed_refreshes.retain(|_, completed| {
            now.duration_since(completed.completed_at) <= MEDIA_RESOURCE_REFRESH_DEDUP_WINDOW
        });
        let subscription_generation = state.subscription_generation;
        if state
            .completed_refreshes
            .get(refresh_key)
            .is_some_and(|completed| {
                completed.subscription_generation == subscription_generation
                    && now.duration_since(completed.completed_at)
                        <= MEDIA_RESOURCE_REFRESH_DEDUP_WINDOW
            })
        {
            return None;
        }
        if let Some(entry) = state.in_flight_refreshes.get(refresh_key) {
            if entry.subscription_generation == subscription_generation {
                return Some(entry.clone());
            }
        }
        let entry = MediaResourceRefreshEntry {
            result: Arc::new(tokio::sync::OnceCell::new()),
            subscription_generation,
        };
        state
            .in_flight_refreshes
            .insert(refresh_key.to_string(), entry.clone());
        Some(entry)
    }

    async fn finish_deduped_refresh(&self, refresh_key: &str, entry: &MediaResourceRefreshEntry) {
        let mut state = self.state.lock().await;
        if state
            .in_flight_refreshes
            .get(refresh_key)
            .is_some_and(|current| {
                current.subscription_generation == entry.subscription_generation
                    && Arc::ptr_eq(&current.result, &entry.result)
            })
        {
            state.in_flight_refreshes.remove(refresh_key);
            state.completed_refreshes.insert(
                refresh_key.to_string(),
                CompletedMediaResourceRefresh {
                    completed_at: tokio::time::Instant::now(),
                    subscription_generation: entry.subscription_generation,
                },
            );
        }
    }

    async fn refresh_for_cache_targets_uncached(
        &self,
        targets: &[CacheTarget],
    ) -> ResourceRefreshOutcome {
        let subscriptions = self.snapshot_subscriptions().await;
        let mut refresh_plan = HashMap::<
            ResourceSubscriberKey,
            (Weak<ResourceObserver>, ResourceObservation, bool, u64),
        >::new();

        for (key, observer, observation, revision) in subscriptions {
            let Some(observer) = observer.upgrade() else {
                self.remove_stale_subscription(&key, revision).await;
                continue;
            };
            let invalidations = resource_invalidations_for_cache_targets(
                targets,
                self.room_id,
                observer.actor.user_id(),
            );
            for invalidation in invalidations {
                if ResourceObserver::observation_invalidated_by_invalidation(
                    &observation,
                    &invalidation,
                ) {
                    refresh_plan
                        .entry(key.clone())
                        .and_modify(|(_, _, refresh_force, _)| *refresh_force = true)
                        .or_insert_with(|| {
                            (
                                Arc::downgrade(&observer),
                                observation.clone(),
                                true,
                                revision,
                            )
                        });
                }
            }
        }

        self.refresh_subscription_batch(
            refresh_plan
                .into_iter()
                .map(|(key, (observer, observation, force, revision))| {
                    (key, observer, observation, force, revision)
                }),
            None,
        )
        .await
    }

    async fn refresh_for_invalidations_uncached(
        &self,
        invalidations: &[ResourceInvalidation],
        force: bool,
        event_cursor: Option<synctv_proto::client::EventCursor>,
    ) -> ResourceRefreshOutcome {
        let subscriptions = self.snapshot_subscriptions().await;
        let mut refresh_plan = HashMap::<
            ResourceSubscriberKey,
            (Weak<ResourceObserver>, ResourceObservation, bool, u64),
        >::new();
        let mut chat_outcome = ResourceRefreshOutcome::default();

        for invalidation in invalidations {
            for (key, observer, observation, revision) in &subscriptions {
                let Some(observer) = observer.upgrade() else {
                    self.remove_stale_subscription(key, *revision).await;
                    continue;
                };

                if let ResourceInvalidation::ChatEvents { event } = invalidation {
                    if !ResourceObserver::observation_invalidated_by_invalidation(
                        observation,
                        invalidation,
                    ) {
                        continue;
                    }
                    if !chat_event_visible_to_observation(observation, event) {
                        continue;
                    }
                    let mut updated_observation = observation.clone();
                    let cursor = event_cursor_for_chat_event(event);
                    if !ResourceObserver::apply_event_cursor_to_observation(
                        &mut updated_observation,
                        &cursor,
                    ) {
                        continue;
                    }
                    let event_payload = match observer.chat_message_event_to_proto(event).await {
                        Ok(event) => event,
                        Err(error) => {
                            tracing::warn!(
                                room_id = %observer.room_id,
                                actor = ?observer.actor,
                                observe_id = %updated_observation.observe_id,
                                error = %error,
                                "Failed to convert chat event for resource observer"
                            );
                            chat_outcome.record_send_failure(key.connection_id.clone(), error);
                            continue;
                        }
                    };
                    let changed = synctv_proto::client::ResourceEvent {
                        observe_id: updated_observation.observe_id.clone(),
                        payload: Some(synctv_proto::client::resource_event::Payload::ChatEvent(
                            event_payload,
                        )),
                        event_cursor: Some(cursor),
                    };
                    match self
                        .send_and_commit_subscription_update(
                            key,
                            *revision,
                            &observer,
                            updated_observation.clone(),
                            Some(changed),
                        )
                        .await
                    {
                        SubscriptionRefreshCommit::Committed => {
                            observer
                                .replace_local_observation(updated_observation)
                                .await;
                        }
                        SubscriptionRefreshCommit::Stale => {}
                        SubscriptionRefreshCommit::SendFailed(error) => {
                            observer.remove_local_observation(&key.observe_id).await;
                            chat_outcome.record_send_failure(key.connection_id.clone(), error);
                        }
                    }
                    continue;
                }

                if let ResourceInvalidation::ChatPinEvents { event } = invalidation {
                    if !ResourceObserver::observation_invalidated_by_invalidation(
                        observation,
                        invalidation,
                    ) {
                        continue;
                    }
                    let mut updated_observation = observation.clone();
                    let cursor = event_cursor_for_chat_pin_event(event);
                    if !ResourceObserver::apply_event_cursor_to_observation(
                        &mut updated_observation,
                        &cursor,
                    ) {
                        continue;
                    }
                    let event_payload = match observer.chat_pin_event_to_proto(event).await {
                        Ok(event) => event,
                        Err(error) => {
                            tracing::warn!(
                                room_id = %observer.room_id,
                                actor = ?observer.actor,
                                observe_id = %updated_observation.observe_id,
                                error = %error,
                                "Failed to convert chat pin event for resource observer"
                            );
                            chat_outcome.record_send_failure(key.connection_id.clone(), error);
                            continue;
                        }
                    };
                    let changed = synctv_proto::client::ResourceEvent {
                        observe_id: updated_observation.observe_id.clone(),
                        payload: Some(synctv_proto::client::resource_event::Payload::ChatPinEvent(
                            event_payload,
                        )),
                        event_cursor: Some(cursor),
                    };
                    match self
                        .send_and_commit_subscription_update(
                            key,
                            *revision,
                            &observer,
                            updated_observation.clone(),
                            Some(changed),
                        )
                        .await
                    {
                        SubscriptionRefreshCommit::Committed => {
                            observer
                                .replace_local_observation(updated_observation)
                                .await;
                        }
                        SubscriptionRefreshCommit::Stale => {}
                        SubscriptionRefreshCommit::SendFailed(error) => {
                            observer.remove_local_observation(&key.observe_id).await;
                            chat_outcome.record_send_failure(key.connection_id.clone(), error);
                        }
                    }
                    continue;
                }

                let should_refresh = if let ResourceInvalidation::ProviderCredential {
                    user_id,
                    provider,
                    server_id,
                } = invalidation
                {
                    match &observation.resource {
                        ObservedResource::Playback { .. } => observer
                            .current_playback_depends_on_provider_credential(
                                user_id, provider, server_id,
                            )
                            .await
                            .unwrap_or_else(|error| {
                                tracing::warn!(
                                    room_id = %self.room_id,
                                    actor = ?observer.actor,
                                    error = %error,
                                    "Failed to resolve playback credential dependencies"
                                );
                                true
                            }),
                        ObservedResource::PlaylistItems { .. } => true,
                        _ => false,
                    }
                } else {
                    ResourceObserver::observation_invalidated_by_invalidation(
                        observation,
                        invalidation,
                    )
                };

                if should_refresh {
                    refresh_plan
                        .entry(key.clone())
                        .and_modify(|(_, _, refresh_force, _)| *refresh_force |= force)
                        .or_insert_with(|| {
                            (
                                Arc::downgrade(&observer),
                                observation.clone(),
                                force,
                                *revision,
                            )
                        });
                }
            }
        }

        let mut outcome = self
            .refresh_subscription_batch(
                refresh_plan
                    .into_iter()
                    .map(|(key, (observer, observation, force, revision))| {
                        (key, observer, observation, force, revision)
                    }),
                event_cursor,
            )
            .await;
        outcome.send_failures.extend(chat_outcome.send_failures);
        outcome
    }

    async fn refresh_for_room_event_invalidations_uncached(
        &self,
        invalidations: &[ResourceInvalidation],
        event: &RealtimeEvent,
        force: bool,
        event_cursor: Option<synctv_proto::client::EventCursor>,
    ) -> ResourceRefreshOutcome {
        let subscriptions = self.snapshot_subscriptions().await;
        let mut refresh_plan = HashMap::<
            ResourceSubscriberKey,
            (Weak<ResourceObserver>, ResourceObservation, bool, u64),
        >::new();
        let mut event_outcome = ResourceRefreshOutcome::default();

        for invalidation in invalidations {
            for (key, observer, observation, revision) in &subscriptions {
                let Some(observer) = observer.upgrade() else {
                    self.remove_stale_subscription(key, *revision).await;
                    continue;
                };

                if !ResourceObserver::observation_invalidated_by_invalidation(
                    observation,
                    invalidation,
                ) {
                    continue;
                }

                match invalidation {
                    ResourceInvalidation::PlaybackState => {
                        let RealtimeEvent::PlaybackStateChanged { state, .. } = event else {
                            refresh_plan.insert(
                                key.clone(),
                                (
                                    Arc::downgrade(&observer),
                                    observation.clone(),
                                    force,
                                    *revision,
                                ),
                            );
                            continue;
                        };
                        if !observation.accepts_inline_playback_state_event() {
                            refresh_plan.insert(
                                key.clone(),
                                (
                                    Arc::downgrade(&observer),
                                    observation.clone(),
                                    force,
                                    *revision,
                                ),
                            );
                            continue;
                        }
                        let cursor = event_cursor.clone().unwrap_or_else(|| {
                            synctv_proto::client::EventCursor {
                                event_id: Some(event.event_id().to_string()),
                                sequence: observation.last_sent_event_sequence.saturating_add(1),
                            }
                        });
                        let mut updated_observation = observation.clone();
                        if !ResourceObserver::apply_event_cursor_to_observation(
                            &mut updated_observation,
                            &cursor,
                        ) {
                            continue;
                        }
                        let mut payload = match try_playback_state_to_proto(
                            state,
                            &observer.public_id_codec,
                        ) {
                            Ok(state) => state,
                            Err(error) => {
                                let error = error.to_string();
                                tracing::warn!(
                                    room_id = %observer.room_id,
                                    actor = ?observer.actor,
                                    observe_id = %updated_observation.observe_id,
                                    error = %error,
                                    "Failed to convert playback state event for resource observer"
                                );
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                                continue;
                            }
                        };
                        if let RealtimeEvent::PlaybackStateChanged {
                            client_operation_id: Some(client_operation_id),
                            ..
                        } = event
                        {
                            payload.client_operation_id.clone_from(client_operation_id);
                        }
                        updated_observation.last_fingerprint = hex::encode(payload.encode_to_vec());
                        let changed = synctv_proto::client::ResourceEvent {
                            observe_id: updated_observation.observe_id.clone(),
                            payload: Some(
                                synctv_proto::client::resource_event::Payload::PlaybackState(
                                    payload,
                                ),
                            ),
                            event_cursor: Some(cursor),
                        };
                        match self
                            .send_and_commit_subscription_update(
                                key,
                                *revision,
                                &observer,
                                updated_observation.clone(),
                                Some(changed),
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed => {
                                observer
                                    .replace_local_observation(updated_observation)
                                    .await;
                            }
                            SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                observer.remove_local_observation(&key.observe_id).await;
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                            }
                        }
                    }
                    ResourceInvalidation::ChatEvents { event: chat_event } => {
                        if !chat_event_visible_to_observation(observation, chat_event) {
                            continue;
                        }
                        if chat_event.kind == ChatEventKind::Deleted
                            && chat_event.message.message.delete_reason.as_deref()
                                == Some("account closure")
                        {
                            if let RealtimeEvent::ChatMessageEvent {
                                event: incoming_event,
                                ..
                            } = event
                            {
                                if let Ok(Some(current)) = observer
                                    .room_service
                                    .get_chat_message_from_primary(
                                        &incoming_event.room_id,
                                        incoming_event.message.message.id,
                                    )
                                    .await
                                {
                                    if current.deleted_at.is_none() {
                                        // A delayed account-delete outbox
                                        // event must not hide a message
                                        // restored since it was queued.
                                        continue;
                                    }
                                }
                            }
                        }
                        let mut updated_observation = observation.clone();
                        let cursor = event_cursor.clone().unwrap_or_else(|| {
                            synctv_proto::client::EventCursor {
                                event_id: Some(chat_event.event_id.clone()),
                                sequence: chat_event
                                    .sequence
                                    .max(observation.last_sent_event_sequence.saturating_add(1)),
                            }
                        });
                        if !ResourceObserver::apply_event_cursor_to_observation(
                            &mut updated_observation,
                            &cursor,
                        ) {
                            continue;
                        }
                        let event_payload =
                            match observer.chat_message_event_to_proto(chat_event).await {
                                Ok(event) => event,
                                Err(error) => {
                                    tracing::warn!(
                                        room_id = %observer.room_id,
                                        actor = ?observer.actor,
                                        observe_id = %updated_observation.observe_id,
                                        error = %error,
                                        "Failed to convert chat event for resource observer"
                                    );
                                    event_outcome
                                        .record_send_failure(key.connection_id.clone(), error);
                                    continue;
                                }
                            };
                        let changed = synctv_proto::client::ResourceEvent {
                            observe_id: updated_observation.observe_id.clone(),
                            payload: Some(
                                synctv_proto::client::resource_event::Payload::ChatEvent(
                                    event_payload,
                                ),
                            ),
                            event_cursor: Some(cursor),
                        };
                        match self
                            .send_and_commit_subscription_update(
                                key,
                                *revision,
                                &observer,
                                updated_observation.clone(),
                                Some(changed),
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed => {
                                observer
                                    .replace_local_observation(updated_observation)
                                    .await;
                            }
                            SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                observer.remove_local_observation(&key.observe_id).await;
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                            }
                        }
                    }
                    ResourceInvalidation::ChatPinEvents { event } => {
                        let mut updated_observation = observation.clone();
                        let cursor = event_cursor_for_chat_pin_event(event);
                        if !ResourceObserver::apply_event_cursor_to_observation(
                            &mut updated_observation,
                            &cursor,
                        ) {
                            continue;
                        }
                        let event_payload = match observer.chat_pin_event_to_proto(event).await {
                            Ok(event) => event,
                            Err(error) => {
                                tracing::warn!(
                                    room_id = %observer.room_id,
                                    actor = ?observer.actor,
                                    observe_id = %updated_observation.observe_id,
                                    error = %error,
                                    "Failed to convert chat pin event for resource observer"
                                );
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                                continue;
                            }
                        };
                        let changed = synctv_proto::client::ResourceEvent {
                            observe_id: updated_observation.observe_id.clone(),
                            payload: Some(
                                synctv_proto::client::resource_event::Payload::ChatPinEvent(
                                    event_payload,
                                ),
                            ),
                            event_cursor: Some(cursor),
                        };
                        match self
                            .send_and_commit_subscription_update(
                                key,
                                *revision,
                                &observer,
                                updated_observation.clone(),
                                Some(changed),
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed => {
                                observer
                                    .replace_local_observation(updated_observation)
                                    .await;
                            }
                            SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                observer.remove_local_observation(&key.observe_id).await;
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                            }
                        }
                    }
                    ResourceInvalidation::RoomMemberEvents => {
                        if !matches!(observation.resource, ObservedResource::RoomMemberEvents) {
                            refresh_plan
                                .entry(key.clone())
                                .and_modify(|(_, _, refresh_force, _)| *refresh_force |= force)
                                .or_insert_with(|| {
                                    (
                                        Arc::downgrade(&observer),
                                        observation.clone(),
                                        force,
                                        *revision,
                                    )
                                });
                            continue;
                        }
                        if !room_member_event_visible_to_observer(event, observer.actor.user_id()) {
                            continue;
                        }
                        let cursor = event_cursor.clone().unwrap_or_else(|| {
                            tracing::warn!(
                                room_id = %observer.room_id,
                                event_id = %event.event_id(),
                                observe_id = %observation.observe_id,
                                "Delivering live room member event without durable cursor"
                            );
                            synctv_proto::client::EventCursor {
                                event_id: Some(event.event_id().to_string()),
                                sequence: observation.last_sent_event_sequence.saturating_add(1),
                            }
                        });
                        let mut updated_observation = observation.clone();
                        if !ResourceObserver::apply_event_cursor_to_observation(
                            &mut updated_observation,
                            &cursor,
                        ) {
                            continue;
                        }
                        let event_payload = match room_member_event_to_proto(
                            event,
                            &observer.public_id_codec,
                            cursor.sequence,
                        ) {
                            Ok(Some(event)) => event,
                            Ok(None) => continue,
                            Err(error) => {
                                tracing::warn!(
                                    room_id = %observer.room_id,
                                    actor = ?observer.actor,
                                    observe_id = %updated_observation.observe_id,
                                    error = %error,
                                    "Failed to convert room member event for resource observer"
                                );
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                                continue;
                            }
                        };
                        let changed = synctv_proto::client::ResourceEvent {
                            observe_id: updated_observation.observe_id.clone(),
                            payload: Some(
                                synctv_proto::client::resource_event::Payload::RoomMemberEvent(
                                    event_payload,
                                ),
                            ),
                            event_cursor: Some(cursor),
                        };
                        match self
                            .send_and_commit_subscription_update(
                                key,
                                *revision,
                                &observer,
                                updated_observation.clone(),
                                Some(changed),
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed => {
                                observer
                                    .replace_local_observation(updated_observation)
                                    .await;
                            }
                            SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                observer.remove_local_observation(&key.observe_id).await;
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                            }
                        }
                    }
                    ResourceInvalidation::OnlineEvent => {
                        if !ResourceObserver::online_event_matches_observation(
                            event,
                            &observation.resource,
                        ) {
                            continue;
                        }
                        let event_payload =
                            match online_event_to_proto(event, &observer.public_id_codec) {
                                Ok(Some(event)) => event,
                                Ok(None) => continue,
                                Err(error) => {
                                    tracing::warn!(
                                        room_id = %observer.room_id,
                                        actor = ?observer.actor,
                                        observe_id = %observation.observe_id,
                                        error = %error,
                                        "Failed to convert online event for resource observer"
                                    );
                                    event_outcome
                                        .record_send_failure(key.connection_id.clone(), error);
                                    continue;
                                }
                            };
                        let changed = synctv_proto::client::ResourceEvent {
                            observe_id: observation.observe_id.clone(),
                            payload: Some(
                                synctv_proto::client::resource_event::Payload::OnlineEvent(
                                    event_payload,
                                ),
                            ),
                            event_cursor: event_cursor.clone(),
                        };
                        match self
                            .send_and_commit_subscription_update(
                                key,
                                *revision,
                                &observer,
                                observation.clone(),
                                Some(changed),
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed
                            | SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                observer.remove_local_observation(&key.observe_id).await;
                                event_outcome.record_send_failure(key.connection_id.clone(), error);
                            }
                        }
                    }
                    ResourceInvalidation::ProviderCredential {
                        user_id,
                        provider,
                        server_id,
                    } => {
                        let should_refresh = match &observation.resource {
                            ObservedResource::Playback { .. } => observer
                                .current_playback_depends_on_provider_credential(
                                    user_id, provider, server_id,
                                )
                                .await
                                .unwrap_or_else(|error| {
                                    tracing::warn!(
                                        room_id = %self.room_id,
                                        actor = ?observer.actor,
                                        error = %error,
                                        "Failed to resolve playback credential dependencies"
                                    );
                                    true
                                }),
                            ObservedResource::PlaylistItems { .. } => true,
                            _ => false,
                        };
                        if should_refresh {
                            refresh_plan
                                .entry(key.clone())
                                .and_modify(|(_, _, refresh_force, _)| *refresh_force |= force)
                                .or_insert_with(|| {
                                    (
                                        Arc::downgrade(&observer),
                                        observation.clone(),
                                        force,
                                        *revision,
                                    )
                                });
                        }
                    }
                    _ => {
                        refresh_plan
                            .entry(key.clone())
                            .and_modify(|(_, _, refresh_force, _)| *refresh_force |= force)
                            .or_insert_with(|| {
                                (
                                    Arc::downgrade(&observer),
                                    observation.clone(),
                                    force,
                                    *revision,
                                )
                            });
                    }
                }
            }
        }

        let mut outcome = self
            .refresh_subscription_batch(
                refresh_plan
                    .into_iter()
                    .map(|(key, (observer, observation, force, revision))| {
                        (key, observer, observation, force, revision)
                    }),
                event_cursor,
            )
            .await;
        outcome.send_failures.extend(event_outcome.send_failures);
        outcome
    }

    async fn snapshot_subscriptions(
        &self,
    ) -> Vec<(
        ResourceSubscriberKey,
        Weak<ResourceObserver>,
        ResourceObservation,
        u64,
    )> {
        let state = self.state.lock().await;
        state
            .subscriptions
            .iter()
            .map(|(key, subscription)| {
                (
                    key.clone(),
                    subscription.observer.clone(),
                    subscription.observation.clone(),
                    subscription.revision,
                )
            })
            .collect()
    }

    async fn refresh_subscription_batch<I>(
        &self,
        subscriptions: I,
        event_cursor: Option<synctv_proto::client::EventCursor>,
    ) -> ResourceRefreshOutcome
    where
        I: IntoIterator<
            Item = (
                ResourceSubscriberKey,
                Weak<ResourceObserver>,
                ResourceObservation,
                bool,
                u64,
            ),
        >,
    {
        let mut groups =
            HashMap::<SharedObservationEvaluationKey, Vec<ResourceHubRefreshSubscription>>::new();
        let mut outcome = ResourceRefreshOutcome::default();

        for (key, observer, observation, force, revision) in subscriptions {
            let Some(observer) = observer.upgrade() else {
                self.remove_stale_subscription(&key, revision).await;
                continue;
            };
            let shared_key = observer.shared_evaluation_key(&observation);
            groups
                .entry(shared_key)
                .or_default()
                .push(ResourceHubRefreshSubscription {
                    key,
                    observer,
                    observation,
                    force,
                    revision,
                });
        }

        for (shared_key, entries) in groups {
            let Some(first) = entries.first() else {
                continue;
            };
            let evaluation = first
                .observer
                .load_resource_evaluation(
                    shared_key.evaluation,
                    &first.observation.resource,
                    first.observation.delivery_mode,
                    false,
                )
                .await;

            match evaluation {
                Ok(evaluation) => {
                    for mut entry in entries {
                        let update = entry.observer.apply_resource_evaluation(
                            &mut entry.observation,
                            entry.force,
                            evaluation.clone(),
                        );
                        if let Some(cursor) = event_cursor.as_ref() {
                            if !ResourceObserver::apply_event_cursor_to_observation(
                                &mut entry.observation,
                                cursor,
                            ) {
                                continue;
                            }
                        }
                        let mut changed_message = update.changed_message;
                        if let (Some(changed), Some(cursor)) =
                            (changed_message.as_mut(), event_cursor.as_ref())
                        {
                            if entry.observation.exposes_client_event_cursor() {
                                changed.event_cursor = Some(cursor.clone());
                            }
                        }
                        match self
                            .send_and_commit_subscription_update(
                                &entry.key,
                                entry.revision,
                                &entry.observer,
                                entry.observation.clone(),
                                changed_message,
                            )
                            .await
                        {
                            SubscriptionRefreshCommit::Committed => {
                                entry
                                    .observer
                                    .replace_local_observation(entry.observation)
                                    .await;
                            }
                            SubscriptionRefreshCommit::Stale => {}
                            SubscriptionRefreshCommit::SendFailed(error) => {
                                entry
                                    .observer
                                    .remove_local_observation(&entry.key.observe_id)
                                    .await;
                                tracing::warn!(
                                    room_id = %self.room_id,
                                    actor = ?entry.observer.actor,
                                    observe_id = %entry.key.observe_id,
                                    error = %error,
                                    "Removed observed resource after ResourceEvent send failure"
                                );
                                outcome.record_send_failure(entry.key.connection_id, error);
                            }
                        }
                    }
                }
                Err(error) => {
                    for entry in entries {
                        if self
                            .remove_stale_subscription(&entry.key, entry.revision)
                            .await
                        {
                            entry
                                .observer
                                .remove_local_observation(&entry.key.observe_id)
                                .await;
                            if let Err(send_error) = entry.observer.send_server_message(
                                ResourceObserver::resource_observe_error_message(
                                    entry.key.observe_id.clone(),
                                    crate::impls::ApiError::Internal(error.clone()),
                                ),
                            ) {
                                tracing::warn!(
                                    room_id = %self.room_id,
                                    actor = ?entry.observer.actor,
                                    observe_id = %entry.key.observe_id,
                                    error = %send_error,
                                    "Failed to send ResourceObserveError after refresh failure"
                                );
                                outcome.record_send_failure(
                                    entry.key.connection_id.clone(),
                                    send_error,
                                );
                            }
                            tracing::warn!(
                                room_id = %self.room_id,
                                actor = ?entry.observer.actor,
                                observe_id = %entry.key.observe_id,
                                error = %error,
                                "Removed observed resource after refresh failure"
                            );
                        }
                    }
                }
            }
        }

        outcome
    }

    async fn send_and_commit_subscription_update(
        &self,
        key: &ResourceSubscriberKey,
        revision: u64,
        observer: &ResourceObserver,
        observation: ResourceObservation,
        changed_message: Option<synctv_proto::client::ResourceEvent>,
    ) -> SubscriptionRefreshCommit {
        let mut state = self.state.lock().await;
        let Some(subscription_revision) = state
            .subscriptions
            .get(key)
            .map(|subscription| subscription.revision)
        else {
            return SubscriptionRefreshCommit::Stale;
        };
        if subscription_revision != revision {
            return SubscriptionRefreshCommit::Stale;
        }
        if let Some(changed) = changed_message {
            if let Err(error) = observer.send_server_message(ServerMessage {
                message: Some(
                    synctv_proto::client::server_message::Message::ResourceEvent(changed),
                ),
            }) {
                state.subscriptions.remove(key);
                state.bump_subscription_generation();
                return SubscriptionRefreshCommit::SendFailed(error);
            }
        }
        if let Some(subscription) = state.subscriptions.get_mut(key) {
            subscription.observation = observation;
        }
        SubscriptionRefreshCommit::Committed
    }

    async fn remove_stale_subscription(&self, key: &ResourceSubscriberKey, revision: u64) -> bool {
        let mut state = self.state.lock().await;
        if state
            .subscriptions
            .get(key)
            .is_some_and(|subscription| subscription.revision == revision)
        {
            state.subscriptions.remove(key);
            state.bump_subscription_generation();
            return true;
        }
        false
    }
}

impl ResourceObserver {
    fn send_server_message(&self, message: ServerMessage) -> Result<(), String> {
        self.sender.send(message)
    }

    fn resource_observe_error_message(
        observe_id: impl Into<String>,
        error: impl Into<crate::impls::ApiError>,
    ) -> ServerMessage {
        let api_error: crate::impls::ApiError = error.into();
        ServerMessage {
            message: Some(
                synctv_proto::client::server_message::Message::ResourceObserveError(
                    synctv_proto::client::ResourceObserveError {
                        observe_id: observe_id.into(),
                        error: Some(api_error.to_proto_error()),
                    },
                ),
            ),
        }
    }

    async fn reject_expired_replay_cursor(
        &self,
        observe_id: &str,
        message: impl Into<String>,
    ) -> Result<(), String> {
        self.remove_local_observation(observe_id).await;
        self.room_hub
            .unregister_observation(&self.connection_id, observe_id)
            .await;
        self.send_server_message(Self::resource_observe_error_message(
            observe_id.to_string(),
            crate::impls::ApiError::InvalidInput(message.into()),
        ))
    }

    fn normalize_delivery_mode(mode: i32) -> Result<ResourceDeliveryMode, String> {
        match ResourceDeliveryMode::try_from(mode)
            .map_err(|_| "Unsupported resource delivery mode".to_string())?
        {
            ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                Ok(ResourceDeliveryMode::PushSnapshot)
            }
            ResourceDeliveryMode::NotifyOnly => Ok(ResourceDeliveryMode::NotifyOnly),
        }
    }

    fn observation_from_request(
        &self,
        request: &synctv_proto::client::ObserveResource,
    ) -> Result<ResourceObservation, String> {
        use synctv_proto::client::observe_resource::Resource;

        let observe_id = request.observe_id.trim();
        if observe_id.is_empty() {
            return Err("observe_id is required".to_string());
        }
        if observe_id.len() > 128 {
            return Err("observe_id is too long".to_string());
        }
        Self::validate_requested_replay_sequence(request)?;

        let resource = match request
            .resource
            .as_ref()
            .ok_or_else(|| "observe resource is required".to_string())?
        {
            Resource::PlaybackState(_) => ObservedResource::PlaybackState,
            Resource::Playback(observe) => {
                let playback_client_profile =
                    playback_client_profile_from_proto(observe.playback_client_profile.as_ref())
                        .map_err(|error| error.to_string())?;
                ObservedResource::Playback {
                    playback_client_profile,
                }
            }
            Resource::RoomSettings(_) => ObservedResource::RoomSettings,
            Resource::PlaylistItems(observe) => ObservedResource::PlaylistItems {
                request: observe
                    .request
                    .clone()
                    .ok_or_else(|| "playlist_items request is required".to_string())?,
            },
            Resource::PlaybackHistory(observe) => ObservedResource::PlaybackHistory {
                request: observe.request.clone().unwrap_or_default(),
            },
            Resource::RoomMemberEvents(_) => ObservedResource::RoomMemberEvents,
            Resource::SelfRoomMember(_) => ObservedResource::SelfRoomMember,
            Resource::ChatEvents(observe) => ObservedResource::ChatEvents {
                selection: chat_message_selection_from_proto_values(
                    &observe.include_message_types,
                )?,
            },
            Resource::ChatPinEvents(_) => ObservedResource::ChatPinEvents,
            Resource::OnlineCount(observe) => ObservedResource::OnlineCount {
                roles: observe.roles.clone(),
                user_ids: observe
                    .user_ids
                    .iter()
                    .map(|user_id| {
                        self.public_id_codec
                            .decode_user_id(user_id)
                            .map_err(|error| {
                                format!("Invalid online_count user_ids entry: {error}")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            },
            Resource::OnlineEvent(observe) => ObservedResource::OnlineEvent {
                roles: observe.roles.clone(),
                kinds: observe.kinds.clone(),
                user_ids: observe
                    .user_ids
                    .iter()
                    .map(|user_id| {
                        self.public_id_codec
                            .decode_user_id(user_id)
                            .map_err(|error| {
                                format!("Invalid online_event user_ids entry: {error}")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            },
        };

        Ok(ResourceObservation {
            observe_id: observe_id.to_string(),
            last_fingerprint: String::new(),
            delivery_mode: Self::normalize_delivery_mode(request.delivery_mode)?,
            resource,
            expires_at: None,
            last_sent_event_sequence: 0,
        })
    }

    pub(super) async fn handle_observe_resource(
        self: &Arc<Self>,
        request: &synctv_proto::client::ObserveResource,
    ) -> Result<(), String> {
        let mut observation = match self.observation_from_request(request) {
            Ok(observation) => observation,
            Err(error) => {
                self.send_server_message(Self::resource_observe_error_message(
                    request.observe_id.clone(),
                    crate::impls::ApiError::InvalidInput(error),
                ))?;
                return Ok(());
            }
        };

        if !self
            .try_reserve_observation_slot(&observation.observe_id)
            .await
        {
            self.send_server_message(Self::resource_observe_error_message(
                observation.observe_id,
                crate::impls::ApiError::RateLimited(format!(
                    "Too many observed resources; maximum per connection is {MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION}"
                )),
            ))?;
            return Ok(());
        }

        let requested_playback_state_sequence =
            Self::validated_requested_playback_state_event_sequence(request);
        let start_sequence = Self::observation_start_sequence(&observation, request);
        let is_event_only_observation = matches!(
            observation.resource,
            ObservedResource::ChatEvents { .. } | ObservedResource::ChatPinEvents
        );
        let exposes_client_event_cursor = observation.exposes_client_event_cursor();
        let has_requested_replay_sequence = Self::requested_replay_sequence(request).is_some();
        let initial_cursor = self
            .initial_event_cursor_for_observation(
                &observation,
                start_sequence,
                exposes_client_event_cursor,
                has_requested_replay_sequence,
            )
            .await?;
        observation.last_sent_event_sequence = initial_cursor
            .as_ref()
            .map_or(start_sequence, |cursor| cursor.sequence);
        let observed_cursor = initial_cursor.clone();
        match self.evaluate_observation(&mut observation).await {
            Ok(mut update) => {
                let has_latest_playback_state_sequence =
                    matches!(observation.resource, ObservedResource::PlaybackState)
                        && requested_playback_state_sequence.is_some_and(|sequence| {
                            observed_cursor
                                .as_ref()
                                .is_some_and(|cursor| sequence == cursor.sequence)
                        });
                if has_latest_playback_state_sequence {
                    update.changed = false;
                    update.changed_message = None;
                }
                if is_event_only_observation {
                    update.changed = false;
                    update.changed_message = None;
                }
                observation.consume_one_shot_options();
                let Some(observation) = self.commit_local_observation(observation).await else {
                    return Ok(());
                };
                let observe_id = observation.observe_id.clone();
                if let Err(error) = self.send_server_message(ServerMessage {
                    message: Some(
                        synctv_proto::client::server_message::Message::ResourceObserved(
                            synctv_proto::client::ResourceObserved {
                                observe_id: observe_id.clone(),
                                changed: update.changed,
                                event_cursor: observed_cursor.clone(),
                            },
                        ),
                    ),
                }) {
                    self.remove_local_observation(&observe_id).await;
                    return Err(error);
                }
                if let Some(mut changed) = update.changed_message {
                    changed.event_cursor = observed_cursor;
                    if let Err(error) = self.send_server_message(ServerMessage {
                        message: Some(
                            synctv_proto::client::server_message::Message::ResourceEvent(changed),
                        ),
                    }) {
                        self.remove_local_observation(&observe_id).await;
                        return Err(error);
                    }
                }

                self.room_hub.register_observation(self, observation).await;
                Ok(())
            }
            Err(error) => {
                let observe_id = observation.observe_id;
                self.release_pending_observation_slot(&observe_id).await;
                self.send_server_message(Self::resource_observe_error_message(
                    observe_id,
                    crate::impls::ApiError::Internal(error),
                ))?;
                Ok(())
            }
        }
    }

    pub(super) async fn replay_chat_events_after(
        self: &Arc<Self>,
        request: &synctv_proto::client::ObserveResource,
    ) -> Result<(), String> {
        let Some(synctv_proto::client::observe_resource::Resource::ChatEvents(observe)) =
            request.resource.as_ref()
        else {
            return Ok(());
        };
        let selection = chat_message_selection_from_proto_values(&observe.include_message_types)?;
        let observe_id = request.observe_id.trim();
        let Some(mut after_event_sequence) = Self::validated_requested_replay_sequence(request)
        else {
            return Ok(());
        };
        let Some(chat_service) = self.chat_service.as_ref() else {
            return Ok(());
        };
        if !observe_id.is_empty() {
            if !chat_service
                .is_event_sequence_retained_for_room(&self.room_id, after_event_sequence)
                .await
                .map_err(|error| error.to_string())?
            {
                self.reject_expired_replay_cursor(
                    observe_id,
                    "Chat event cursor expired; fetch chat history again and restart observe",
                )
                .await?;
                return Ok(());
            }

            loop {
                let events = chat_service
                    .get_events_after_sequence(
                        &self.room_id,
                        after_event_sequence,
                        CHAT_EVENT_REPLAY_BATCH_LIMIT,
                        &selection,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                if events.is_empty() {
                    return Ok(());
                }

                for logged in &events {
                    let event = &logged.event;
                    after_event_sequence = event.sequence;
                    let Some(mut observation) = self.local_observation(observe_id).await else {
                        return Ok(());
                    };
                    let cursor = event_cursor_for_chat_event(event);
                    if !Self::apply_event_cursor_to_observation(&mut observation, &cursor) {
                        continue;
                    }
                    self.send_server_message(ServerMessage {
                        message: Some(
                            synctv_proto::client::server_message::Message::ResourceEvent(
                                synctv_proto::client::ResourceEvent {
                                    observe_id: observe_id.to_string(),
                                    payload: Some(
                                        synctv_proto::client::resource_event::Payload::ChatEvent(
                                            self.chat_message_event_to_proto(event).await?,
                                        ),
                                    ),
                                    event_cursor: Some(cursor),
                                },
                            ),
                        ),
                    })?;
                    self.replace_local_observation(observation.clone()).await;
                    self.room_hub.register_observation(self, observation).await;
                }

                if events.len() < CHAT_EVENT_REPLAY_BATCH_LIMIT_USIZE {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    pub(super) async fn replay_room_resource_events_after(
        self: &Arc<Self>,
        request: &synctv_proto::client::ObserveResource,
    ) -> Result<(), String> {
        if matches!(
            request.resource.as_ref(),
            Some(synctv_proto::client::observe_resource::Resource::ChatEvents(_))
        ) {
            return Ok(());
        }

        let observe_id = request.observe_id.trim();
        let Some(mut after_event_sequence) = Self::validated_requested_replay_sequence(request)
        else {
            return Ok(());
        };
        let Some(observation) = self.local_observation(observe_id).await else {
            return Ok(());
        };
        if !observation.exposes_client_event_cursor() {
            return Ok(());
        }
        let Some(resource_types) = observation.room_resource_cursor_types() else {
            return Ok(());
        };
        if resource_types.is_empty() {
            return Ok(());
        }

        if !self
            .room_service
            .is_room_resource_event_sequence_retained_for_resource_types(
                &self.room_id,
                resource_types,
                after_event_sequence,
            )
            .await
            .map_err(|error| error.to_string())?
        {
            self.reject_expired_replay_cursor(
                observe_id,
                "Room resource event cursor expired; fetch the resource snapshot again and restart observe",
            )
            .await?;
            return Ok(());
        }

        loop {
            let events = self
                .room_service
                .list_room_resource_events_after_sequence_for_resource_types(
                    &self.room_id,
                    resource_types,
                    after_event_sequence,
                    ROOM_RESOURCE_EVENT_REPLAY_BATCH_LIMIT,
                )
                .await
                .map_err(|error| error.to_string())?;
            if events.is_empty() {
                return Ok(());
            }

            for logged in &events {
                after_event_sequence = logged.sequence;
                let cursor = synctv_proto::client::EventCursor {
                    event_id: Some(logged.event_id.clone()),
                    sequence: logged.sequence,
                };
                let Some(mut observation) = self.local_observation(observe_id).await else {
                    return Ok(());
                };
                let Some(payload) = logged.payload.clone() else {
                    if !Self::apply_event_cursor_to_observation(&mut observation, &cursor) {
                        continue;
                    }
                    self.send_server_message(ServerMessage {
                        message: Some(
                            synctv_proto::client::server_message::Message::ResourceEvent(
                                synctv_proto::client::ResourceEvent {
                                    observe_id: observe_id.to_string(),
                                    payload: Some(
                                        synctv_proto::client::resource_event::Payload::ChangedOnly(
                                            synctv_proto::client::ResourceEventOnly {},
                                        ),
                                    ),
                                    event_cursor: Some(cursor),
                                },
                            ),
                        ),
                    })?;
                    self.replace_local_observation(observation.clone()).await;
                    self.room_hub.register_observation(self, observation).await;
                    continue;
                };
                if matches!(observation.resource, ObservedResource::ChatPinEvents) {
                    let RoomResourceEventPayload::ChatPin { mut event } = payload else {
                        return Err(format!(
                            "Room resource event {} at sequence {} is not a chat pin event",
                            logged.event_id, logged.sequence
                        ));
                    };
                    event.sequence = logged.sequence;
                    if !Self::apply_event_cursor_to_observation(&mut observation, &cursor) {
                        continue;
                    }
                    self.send_server_message(ServerMessage {
                        message: Some(
                            synctv_proto::client::server_message::Message::ResourceEvent(
                                synctv_proto::client::ResourceEvent {
                                    observe_id: observe_id.to_string(),
                                    payload: Some(
                                        synctv_proto::client::resource_event::Payload::ChatPinEvent(
                                            self.chat_pin_event_to_proto(&event).await?,
                                        ),
                                    ),
                                    event_cursor: Some(cursor),
                                },
                            ),
                        ),
                    })?;
                    self.replace_local_observation(observation.clone()).await;
                    self.room_hub.register_observation(self, observation).await;
                    continue;
                }
                let RoomResourceEventPayload::Realtime {
                    event: realtime_event,
                } = payload
                else {
                    return Err(format!(
                        "Room resource event {} at sequence {} is not a realtime event",
                        logged.event_id, logged.sequence
                    ));
                };
                let invalidations = resource_invalidations_for_room_event(&realtime_event);
                if !invalidations.iter().any(|invalidation| {
                    Self::observation_invalidated_by_invalidation(&observation, invalidation)
                }) {
                    continue;
                }

                if !Self::apply_event_cursor_to_observation(&mut observation, &cursor) {
                    continue;
                }
                if matches!(observation.resource, ObservedResource::RoomMemberEvents) {
                    if !room_member_event_visible_to_observer(&realtime_event, self.actor.user_id())
                    {
                        self.replace_local_observation(observation.clone()).await;
                        self.room_hub.register_observation(self, observation).await;
                        continue;
                    }
                    if let Some(event) = room_member_event_to_proto(
                        &realtime_event,
                        &self.public_id_codec,
                        logged.sequence,
                    )? {
                        self.send_server_message(ServerMessage {
                            message: Some(
                                synctv_proto::client::server_message::Message::ResourceEvent(
                                    synctv_proto::client::ResourceEvent {
                                        observe_id: observe_id.to_string(),
                                        payload: Some(
                                            synctv_proto::client::resource_event::Payload::RoomMemberEvent(event),
                                        ),
                                        event_cursor: Some(cursor),
                                    },
                                ),
                            ),
                        })?;
                    }
                    self.replace_local_observation(observation.clone()).await;
                    self.room_hub.register_observation(self, observation).await;
                    continue;
                }
                let mut update = self
                    .evaluate_observation_with_force(&mut observation, true)
                    .await?;
                if let Some(changed) = update.changed_message.as_mut() {
                    changed.event_cursor = Some(cursor);
                    self.send_server_message(ServerMessage {
                        message: Some(
                            synctv_proto::client::server_message::Message::ResourceEvent(
                                changed.clone(),
                            ),
                        ),
                    })?;
                }
                self.replace_local_observation(observation.clone()).await;
                self.room_hub.register_observation(self, observation).await;
            }

            if events.len() < ROOM_RESOURCE_EVENT_REPLAY_BATCH_LIMIT_USIZE {
                return Ok(());
            }
        }
    }

    pub(super) async fn handle_unobserve_resource(
        self: &Arc<Self>,
        request: &synctv_proto::client::UnobserveResource,
    ) -> Result<(), String> {
        let observe_id = request.observe_id.trim();
        self.remove_local_observation(observe_id).await;
        self.room_hub
            .unregister_observation(&self.connection_id, observe_id)
            .await;
        Ok(())
    }

    pub(super) async fn next_expired_resource_refresh_deadline(
        &self,
    ) -> Option<tokio::time::Instant> {
        let state = self.state.lock().await;
        let expires_at = state
            .observations
            .values()
            .filter_map(|observation| observation.expires_at)
            .min()?;
        let now_wall = self.clock.now().timestamp();
        let now_instant = tokio::time::Instant::now();
        if expires_at <= now_wall {
            return Some(now_instant);
        }
        let refresh_after = Duration::from_secs((expires_at - now_wall).cast_unsigned());
        Some(
            now_instant
                .checked_add(refresh_after)
                .unwrap_or(now_instant + Duration::from_hours(8760)),
        )
    }

    pub(super) async fn wait_for_expired_resource_refresh_deadline(&self) {
        loop {
            match self.next_expired_resource_refresh_deadline().await {
                Some(deadline) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(deadline) => return,
                        () = self.observation_change.notified() => {}
                    }
                }
                None => {
                    self.observation_change.notified().await;
                }
            }
        }
    }

    pub(super) async fn refresh_expired_resource_observations(&self) -> Result<(), String> {
        let observations = {
            let state = self.state.lock().await;
            let now = self.clock.now().timestamp();
            state
                .observations
                .values()
                .filter(|observation| {
                    observation
                        .expires_at
                        .is_some_and(|expires_at| now >= expires_at)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if observations.is_empty() {
            return Ok(());
        }
        for observation in observations {
            let mut observation = observation;
            let update = self
                .evaluate_observation_with_force(&mut observation, true)
                .await?;
            if let Some(changed) = update.changed_message {
                self.send_server_message(ServerMessage {
                    message: Some(
                        synctv_proto::client::server_message::Message::ResourceEvent(changed),
                    ),
                })?;
            }
            self.replace_local_observation(observation).await;
        }
        Ok(())
    }

    async fn current_playback_depends_on_provider_credential(
        &self,
        changed_user_id: &synctv_core::models::UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<bool, String> {
        let state = self
            .room_service
            .get_playback_state(&self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        let dependencies = self
            .playback_service
            .playback_credential_dependencies(&self.actor, &self.room_id, &state)
            .await
            .map_err(|error| error.to_string())?;

        let changed_user_id_key = changed_user_id.to_string();
        Ok(dependencies
            .iter()
            .any(|dependency| dependency.matches(provider, &changed_user_id_key, server_id)))
    }

    pub(super) async fn handle_provider_credential_changed_admin_event(
        self: &Arc<Self>,
        event_id: &str,
        changed_user_id: &synctv_core::models::UserId,
        provider: &str,
        server_id: &str,
    ) {
        let invalidation =
            provider_credential_resource_invalidation(*changed_user_id, provider, server_id);
        let outcome = self
            .room_hub
            .refresh_for_invalidations_with_key(
                Some(format!(
                    "provider_credential:{event_id}:room:{}",
                    self.room_id
                )),
                vec![invalidation],
                true,
                None,
            )
            .await;
        if let Some(error) = outcome.error_for_connection(Some(&self.connection_id)) {
            tracing::warn!(
                room_id = %self.room_id,
                actor = ?self.actor,
                changed_user_id = %changed_user_id,
                provider,
                server_id,
                error = %error,
                "Failed to refresh observed resources after provider credential change"
            );
        }
    }

    pub(super) async fn handle_cache_invalidate_admin_event(
        self: &Arc<Self>,
        event_id: &str,
        targets: &[CacheTarget],
    ) {
        let outcome = self
            .room_hub
            .refresh_for_cache_targets(event_id, targets.to_vec())
            .await;
        if let Some(error) = outcome.error_for_connection(Some(&self.connection_id)) {
            tracing::warn!(
                room_id = %self.room_id,
                actor = ?self.actor,
                error = %error,
                "Failed to refresh observed resources after cache invalidation"
            );
        }
    }

    #[cfg(test)]
    pub(super) async fn refresh_observations_for_invalidations(
        self: &Arc<Self>,
        invalidations: &[ResourceInvalidation],
    ) -> Result<(), String> {
        self.refresh_observations_for_invalidations_with_force(invalidations, false)
            .await
    }

    #[cfg(test)]
    pub(super) async fn refresh_observations_for_invalidations_with_force(
        self: &Arc<Self>,
        invalidations: &[ResourceInvalidation],
        force: bool,
    ) -> Result<(), String> {
        let outcome = self
            .room_hub
            .refresh_for_invalidations(invalidations.to_vec(), force)
            .await;
        if let Some(error) = outcome.error_for_connection(Some(&self.connection_id)) {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn observation_invalidated_by_invalidation(
        observation: &ResourceObservation,
        invalidation: &ResourceInvalidation,
    ) -> bool {
        match &observation.resource {
            ObservedResource::PlaybackState => {
                matches!(invalidation, ResourceInvalidation::PlaybackState)
            }
            ObservedResource::Playback { .. } => {
                matches!(invalidation, ResourceInvalidation::Playback(_))
            }
            ObservedResource::RoomSettings => {
                matches!(invalidation, ResourceInvalidation::RoomSettings)
            }
            ObservedResource::PlaylistItems { .. } => matches!(
                invalidation,
                ResourceInvalidation::PlaylistItems
                    | ResourceInvalidation::ProviderCredential { .. }
            ),
            ObservedResource::PlaybackHistory { .. } => matches!(
                invalidation,
                ResourceInvalidation::PlaybackState | ResourceInvalidation::PlaylistItems
            ),
            ObservedResource::RoomMemberEvents => {
                matches!(invalidation, ResourceInvalidation::RoomMemberEvents)
            }
            ObservedResource::SelfRoomMember => matches!(
                invalidation,
                ResourceInvalidation::RoomMemberEvents | ResourceInvalidation::RoomSettings
            ),
            ObservedResource::ChatEvents { .. } => {
                matches!(
                    invalidation,
                    ResourceInvalidation::ChatEvents { .. }
                        | ResourceInvalidation::ChatEventsSnapshot
                )
            }
            ObservedResource::ChatPinEvents => {
                matches!(invalidation, ResourceInvalidation::ChatPinEvents { .. })
            }
            ObservedResource::OnlineCount { .. } => {
                matches!(invalidation, ResourceInvalidation::OnlineCount)
            }
            ObservedResource::OnlineEvent { .. } => {
                matches!(invalidation, ResourceInvalidation::OnlineEvent)
            }
        }
    }

    fn online_event_matches_observation(
        event: &RealtimeEvent,
        resource: &ObservedResource,
    ) -> bool {
        let ObservedResource::OnlineEvent {
            roles,
            kinds,
            user_ids,
        } = resource
        else {
            return false;
        };

        let (user_id, role, kind) = match event {
            RealtimeEvent::UserJoined { user_id, role, .. } => (
                *user_id,
                *role,
                synctv_proto::client::OnlineEventKind::Joined as i32,
            ),
            RealtimeEvent::UserLeft { user_id, role, .. } => (
                *user_id,
                *role,
                synctv_proto::client::OnlineEventKind::Left as i32,
            ),
            _ => return false,
        };

        (roles.is_empty() || roles.contains(&role))
            && (kinds.is_empty() || kinds.contains(&kind))
            && (user_ids.is_empty() || user_ids.contains(&user_id))
    }

    async fn evaluate_observation(
        &self,
        observation: &mut ResourceObservation,
    ) -> Result<ObservationUpdate, String> {
        self.evaluate_observation_with_force(observation, false)
            .await
    }

    async fn evaluate_observation_with_force(
        &self,
        observation: &mut ResourceObservation,
        force: bool,
    ) -> Result<ObservationUpdate, String> {
        let key = observation.evaluation_key();
        let evaluation = self
            .load_resource_evaluation(
                key,
                &observation.resource,
                observation.delivery_mode,
                !force,
            )
            .await?;
        Ok(self.apply_resource_evaluation(observation, force, evaluation))
    }

    async fn load_resource_evaluation(
        &self,
        key: ObservationEvaluationKey,
        resource: &ObservedResource,
        delivery_mode: ResourceDeliveryMode,
        allow_completed_reuse: bool,
    ) -> Result<ResourceEvaluation, String> {
        let service_identity = self.resource_evaluation_service_identity(resource);
        let shared_key = SharedObservationEvaluationKey {
            room_id: self.room_id,
            actor: self.resource_evaluation_actor_scope(resource),
            service_id: service_identity.id,
            evaluation: key,
        };
        let resource_generation = self.room_hub.resource_generation().await;
        let entry = {
            let mut in_flight = RESOURCE_EVALUATION_SINGLEFLIGHT.lock().await;
            let now = tokio::time::Instant::now();
            if let Some(entry) = in_flight.get(&shared_key) {
                if entry.service.is_alive()
                    && ((entry.resource_generation == resource_generation
                        && !entry.result.initialized())
                        || (allow_completed_reuse
                            && entry.resource_generation == resource_generation
                            && entry.can_reuse_completed(now)))
                {
                    Arc::clone(entry)
                } else {
                    let entry = Arc::new(SharedResourceEvaluationEntry::new(
                        service_identity.weak.clone(),
                        resource_generation,
                    ));
                    in_flight.insert(shared_key.clone(), Arc::clone(&entry));
                    entry
                }
            } else {
                let entry = Arc::new(SharedResourceEvaluationEntry::new(
                    service_identity.weak.clone(),
                    resource_generation,
                ));
                in_flight.insert(shared_key.clone(), Arc::clone(&entry));
                entry
            }
        };

        let result = entry
            .result
            .get_or_init(|| async {
                self.load_resource_evaluation_uncached(resource, delivery_mode)
                    .await
            })
            .await
            .clone();
        if result.is_ok() {
            entry.mark_completed(tokio::time::Instant::now());
        }

        schedule_resource_evaluation_singleflight_cleanup(shared_key, Arc::clone(&entry));

        result
    }

    fn resource_evaluation_actor_scope(
        &self,
        resource: &ObservedResource,
    ) -> Option<ResourceActorScope> {
        match resource {
            ObservedResource::PlaybackState | ObservedResource::RoomSettings => None,
            ObservedResource::Playback { .. }
            | ObservedResource::PlaylistItems { .. }
            | ObservedResource::PlaybackHistory { .. }
            | ObservedResource::RoomMemberEvents
            | ObservedResource::SelfRoomMember
            | ObservedResource::ChatEvents { .. }
            | ObservedResource::ChatPinEvents
            | ObservedResource::OnlineEvent { .. } => Some(self.actor_scope()),
            ObservedResource::OnlineCount { roles, user_ids } => {
                (!roles.is_empty() || !user_ids.is_empty()).then(|| self.actor_scope())
            }
        }
    }

    fn actor_scope(&self) -> ResourceActorScope {
        match &self.actor {
            RoomActor::User { user_id, .. } => ResourceActorScope::User(*user_id),
            RoomActor::Guest(access) => ResourceActorScope::Guest(access.guest_id.clone()),
        }
    }

    fn resource_evaluation_service_identity(
        &self,
        resource: &ObservedResource,
    ) -> SharedResourceServiceIdentity {
        match resource {
            ObservedResource::PlaybackState
            | ObservedResource::ChatEvents { .. }
            | ObservedResource::ChatPinEvents
            | ObservedResource::RoomMemberEvents
            | ObservedResource::SelfRoomMember
            | ObservedResource::OnlineCount { .. }
            | ObservedResource::OnlineEvent { .. }
            | ObservedResource::PlaybackHistory { .. } => SharedResourceServiceIdentity {
                id: Arc::as_ptr(&self.room_service) as usize,
                weak: SharedResourceServiceWeak::RoomService(Arc::downgrade(&self.room_service)),
            },
            ObservedResource::Playback { .. } => SharedResourceServiceIdentity {
                id: Arc::as_ptr(&self.playback_service).cast::<()>() as usize,
                weak: SharedResourceServiceWeak::Playback(Arc::downgrade(&self.playback_service)),
            },
            ObservedResource::RoomSettings => {
                let id = self.room_settings_snapshot_service_id;
                SharedResourceServiceIdentity {
                    id,
                    weak: SharedResourceServiceWeak::RoomSettings(Arc::downgrade(
                        &self.room_settings_snapshot_service,
                    )),
                }
            }
            ObservedResource::PlaylistItems { .. } => SharedResourceServiceIdentity {
                id: Arc::as_ptr(&self.playlist_items_snapshot_service).cast::<()>() as usize,
                weak: SharedResourceServiceWeak::PlaylistItems(Arc::downgrade(
                    &self.playlist_items_snapshot_service,
                )),
            },
        }
    }

    async fn load_resource_evaluation_uncached(
        &self,
        resource: &ObservedResource,
        delivery_mode: ResourceDeliveryMode,
    ) -> Result<ResourceEvaluation, String> {
        use synctv_proto::client::resource_event::Payload;

        let (fingerprint, expires_at, payload) = match resource {
            ObservedResource::PlaybackState => {
                let state = self
                    .room_service
                    .get_playback_state(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let version = state.version.to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaybackState(
                            try_playback_state_to_proto(&state, &self.public_id_codec)
                                .map_err(|error| error.to_string())?,
                        )
                    }
                };
                (version, None, payload)
            }
            ObservedResource::Playback {
                playback_client_profile,
            } => {
                let service = Arc::clone(&self.playback_service);
                let state = self
                    .room_service
                    .get_playback_state(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let playback = service
                    .get_playback_for_actor(
                        &self.actor,
                        &self.room_id,
                        &state,
                        playback_client_profile.as_ref(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let fingerprint = playback.encode_to_vec();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::Playback(playback.clone())
                    }
                };
                (hex::encode(fingerprint), playback.expires_at, payload)
            }
            ObservedResource::RoomSettings => {
                let service = Arc::clone(&self.room_settings_snapshot_service);
                let snapshot = service
                    .get_room_settings_snapshot(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let version = snapshot.version.to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::RoomSettings(synctv_proto::client::GetRoomSettingsResponse {
                            settings: Some(room_settings_to_proto(&snapshot.settings)),
                            version: snapshot.version,
                        })
                    }
                };
                (version, None, payload)
            }
            ObservedResource::PlaylistItems { request } => {
                let service = Arc::clone(&self.playlist_items_snapshot_service);
                let actor = self.resource_actor().await?;
                let snapshot = service
                    .get_playlist_items_snapshot(&actor, request)
                    .await
                    .map_err(|error| error.to_string())?;
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaylistItems(snapshot.clone())
                    }
                };
                (snapshot.version.clone(), None, payload)
            }
            ObservedResource::PlaybackHistory { request } => {
                let before_entry_id = request
                    .before_entry_id
                    .as_deref()
                    .map(|id| self.public_id_codec.decode_playback_history_entry_id(id))
                    .transpose()
                    .map_err(|_| "Invalid playback history before_entry_id".to_string())?;
                let page = self
                    .room_service
                    .playback_service()
                    .list_playback_history(
                        &self.room_id,
                        before_entry_id,
                        if request.limit == 0 {
                            50
                        } else {
                            request.limit
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = crate::impls::client::convert::playback_history_page_to_proto(
                    page,
                    &self.public_id_codec,
                )
                .map_err(|error| error.to_string())?;
                let fingerprint = hex::encode(snapshot.encode_to_vec());
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaybackHistory(snapshot)
                    }
                };
                (fingerprint, None, payload)
            }
            ObservedResource::RoomMemberEvents | ObservedResource::OnlineEvent { .. } => {
                let version = self.clock.now_millis().to_string();
                (
                    version,
                    None,
                    Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {}),
                )
            }
            ObservedResource::SelfRoomMember => {
                let member = self
                    .self_room_member_snapshot()
                    .await
                    .map_err(|error| error.clone())?;
                let fingerprint = member.encode_to_vec();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::SelfRoomMember(member)
                    }
                };
                (hex::encode(fingerprint), None, payload)
            }
            ObservedResource::ChatEvents { .. } | ObservedResource::ChatPinEvents => {
                let version = self.clock.now_millis().to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly
                    | ResourceDeliveryMode::Unspecified
                    | ResourceDeliveryMode::PushSnapshot => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                };
                (version, None, payload)
            }
            ObservedResource::OnlineCount { roles, user_ids } => {
                let count = self
                    .online_count_for_filters(roles, user_ids)
                    .await
                    .map_err(|error| error.clone())?;
                let expires_at =
                    Some(self.clock.now().timestamp() + ONLINE_COUNT_REFRESH_INTERVAL_SECONDS);
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(synctv_proto::client::ResourceEventOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::OnlineCount(synctv_proto::client::OnlineCount {
                            count: i32::try_from(count).unwrap_or(i32::MAX),
                        })
                    }
                };
                (count.to_string(), expires_at, payload)
            }
        };

        Ok(ResourceEvaluation {
            fingerprint,
            expires_at,
            payload,
        })
    }

    async fn online_count_for_filters(
        &self,
        roles: &[i32],
        user_ids: &[UserId],
    ) -> Result<usize, String> {
        if roles.is_empty() && user_ids.is_empty() {
            return Ok(self
                .presence_service
                .room_stats(self.room_id)
                .await
                .map_err(|error| error.to_string())?
                .online_user_count);
        }

        let mut filtered_user_ids = user_ids.iter().copied().collect::<HashSet<_>>();
        if !roles.is_empty() {
            let mut role_user_ids = HashSet::new();
            for role in roles {
                let role = proto_role_filter_to_room_role(*role)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "online_count role filter is unspecified".to_string())?;
                role_user_ids.extend(self.user_ids_for_room_role(role).await?);
            }

            if filtered_user_ids.is_empty() {
                filtered_user_ids = role_user_ids;
            } else {
                filtered_user_ids.retain(|user_id| role_user_ids.contains(user_id));
            }
        }

        if filtered_user_ids.is_empty() {
            return Ok(0);
        }

        let mut sorted_user_ids = filtered_user_ids.into_iter().collect::<Vec<_>>();
        sorted_user_ids.sort_unstable();
        self.presence_service
            .room_online_user_ids(self.room_id, &sorted_user_ids)
            .await
            .map(|ids| ids.len())
            .map_err(|error| error.to_string())
    }

    async fn user_ids_for_room_role(
        &self,
        role: synctv_core::models::RoomRole,
    ) -> Result<HashSet<UserId>, String> {
        const PAGE_SIZE: u32 = 500;

        let mut page = 1_u32;
        let mut user_ids = HashSet::new();
        loop {
            let query = synctv_core::models::RoomMemberListQuery {
                pagination: synctv_core::models::PageParams {
                    page,
                    page_size: PAGE_SIZE,
                },
                search: None,
                role: Some(role),
                is_online: None,
                sort_by: synctv_core::models::RoomMemberListSortBy::JoinedAt,
                sort_direction: synctv_core::models::SortDirection::Asc,
            };
            let (members, total) = self
                .room_service
                .get_room_members_query(&self.room_id, query)
                .await
                .map_err(|error| error.to_string())?;
            user_ids.extend(members.into_iter().map(|member| member.user_id));

            if user_ids.len() >= usize::try_from(total).unwrap_or(usize::MAX) {
                return Ok(user_ids);
            }
            page = page
                .checked_add(1)
                .ok_or_else(|| "online_count role filter pagination overflow".to_string())?;
        }
    }

    pub(super) async fn self_room_member_snapshot(
        &self,
    ) -> Result<synctv_proto::common::RoomMember, String> {
        const PAGE_SIZE: u32 = 500;
        if let RoomActor::Guest(access) = &self.actor {
            let permissions = self
                .room_service
                .get_guest_permissions(&self.room_id)
                .await
                .map_err(|error| error.to_string())?;
            let stats = self
                .presence_service
                .actor_connection_count_in_room(
                    &RealtimeActor::guest(access.guest_id.clone()),
                    self.room_id,
                )
                .await
                .map_err(|error| error.to_string())?;
            return Ok(synctv_proto::common::RoomMember {
                room_id: self
                    .public_id_codec
                    .encode_room_id(self.room_id)
                    .map_err(|error| format!("Failed to encode room public id: {error}"))?,
                user_id: access.guest_id.clone(),
                username: access.display_name.clone(),
                role: synctv_proto::common::RoomMemberRole::Guest as i32,
                permissions: permissions.bits(),
                joined_at: 0,
                is_online: stats > 0,
                connection_count: i32::try_from(stats)
                    .map_err(|_| "guest connection count exceeds i32 range".to_string())?,
                ..Default::default()
            });
        }
        let user_id = self
            .actor
            .user_id()
            .ok_or_else(|| "Current room actor has no user id".to_string())?;
        let mut page = 1_u32;
        let member = loop {
            let (members, total) = self
                .room_service
                .get_room_members_query(
                    &self.room_id,
                    synctv_core::models::RoomMemberListQuery {
                        pagination: synctv_core::models::PageParams {
                            page,
                            page_size: PAGE_SIZE,
                        },
                        search: None,
                        role: None,
                        is_online: None,
                        sort_by: synctv_core::models::RoomMemberListSortBy::JoinedAt,
                        sort_direction: synctv_core::models::SortDirection::Asc,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            if let Some(member) = members.into_iter().find(|member| member.user_id == user_id) {
                break member;
            }
            let searched = usize::try_from(page)
                .unwrap_or(usize::MAX)
                .saturating_mul(usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX));
            if searched >= usize::try_from(total).unwrap_or(usize::MAX) {
                return Err("Current user is not a room member".to_string());
            }
            page = page
                .checked_add(1)
                .ok_or_else(|| "self room member pagination overflow".to_string())?;
        };
        let room_settings = self
            .room_service
            .get_room_settings(&self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member, &room_settings);
        let mut proto =
            try_room_member_to_proto_with_permissions(&member, permissions, &self.public_id_codec)
                .map_err(|error| error.to_string())?;
        let stats = self
            .presence_service
            .user_room_stats(user_id, self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        proto.is_online = stats.is_online;
        proto.connection_count = i32::try_from(stats.connection_count)
            .map_err(|_| "self room member connection count exceeds i32 range".to_string())?;
        Ok(proto)
    }

    fn apply_resource_evaluation(
        &self,
        observation: &mut ResourceObservation,
        force: bool,
        evaluation: ResourceEvaluation,
    ) -> ObservationUpdate {
        let ResourceEvaluation {
            fingerprint,
            expires_at,
            payload,
        } = evaluation;

        let expired = observation
            .expires_at
            .is_some_and(|expires_at| self.clock.now().timestamp() >= expires_at);
        let changed = force || observation.last_fingerprint != fingerprint || expired;

        observation.last_fingerprint.clone_from(&fingerprint);
        observation.expires_at = expires_at;

        let changed_message = changed.then(|| synctv_proto::client::ResourceEvent {
            observe_id: observation.observe_id.clone(),
            payload: Some(payload),
            event_cursor: None,
        });

        ObservationUpdate {
            changed,
            changed_message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playback_observation() -> ResourceObservation {
        ResourceObservation {
            observe_id: "playback".to_string(),
            last_fingerprint: String::new(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot,
            resource: ObservedResource::Playback {
                playback_client_profile: None,
            },
            expires_at: None,
            last_sent_event_sequence: 0,
        }
    }

    fn playback_state_observation() -> ResourceObservation {
        ResourceObservation {
            observe_id: "playback-state".to_string(),
            last_fingerprint: String::new(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot,
            resource: ObservedResource::PlaybackState,
            expires_at: None,
            last_sent_event_sequence: 0,
        }
    }

    fn playback_history_observation() -> ResourceObservation {
        ResourceObservation {
            observe_id: "playback-history".to_string(),
            last_fingerprint: String::new(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot,
            resource: ObservedResource::PlaybackHistory {
                request: synctv_proto::client::ListPlaybackHistoryRequest::default(),
            },
            expires_at: None,
            last_sent_event_sequence: 0,
        }
    }

    #[test]
    fn playback_tracks_room_resource_dependency_types() {
        let observation = playback_observation();

        assert_eq!(
            observation.room_resource_cursor_types(),
            Some(
                &[
                    RoomResourceKind::PlaybackState,
                    RoomResourceKind::Media,
                    RoomResourceKind::Playlist,
                    RoomResourceKind::PlaylistItems,
                ][..]
            )
        );
    }

    #[test]
    fn playback_state_event_refreshes_playback_history_snapshot() {
        let playback_state = playback_state_observation();
        let playback_history = playback_history_observation();

        assert!(playback_state.accepts_inline_playback_state_event());
        assert!(!playback_history.accepts_inline_playback_state_event());
        assert!(ResourceObserver::observation_invalidated_by_invalidation(
            &playback_history,
            &ResourceInvalidation::PlaybackState,
        ));
    }

    #[test]
    fn playback_hides_client_event_cursor() {
        let observation = playback_observation();

        assert!(!observation.exposes_client_event_cursor());
    }

    #[test]
    fn playback_observation_ignores_requested_event_sequence() {
        let observation = playback_observation();
        let request = synctv_proto::client::ObserveResource {
            observe_id: "playback".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(synctv_proto::client::observe_resource::Resource::Playback(
                synctv_proto::client::ObservePlayback {
                    playback_client_profile: None,
                },
            )),
        };

        assert_eq!(ResourceObserver::requested_replay_sequence(&request), None);
        assert_eq!(
            ResourceObserver::observation_start_sequence(&observation, &request),
            0
        );
    }

    #[test]
    fn online_event_observation_filters_role_kind_and_user_id() {
        let user = UserId::expect_positive(42);
        let other = UserId::expect_positive(43);
        let resource = ObservedResource::OnlineEvent {
            roles: vec![synctv_proto::common::RoomMemberRole::Admin as i32],
            kinds: vec![synctv_proto::client::OnlineEventKind::Joined as i32],
            user_ids: vec![user],
        };
        let matching = RealtimeEvent::UserJoined {
            event_id: "online-match".to_string(),
            room_id: RoomId::expect_positive(7),
            user_id: user,
            username: "admin".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            permissions: synctv_core::models::RoomPermissionSet::default_admin(),
            role: synctv_proto::common::RoomMemberRole::Admin as i32,
            added_permissions: synctv_core::models::RoomPermissionSet(0),
            removed_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
            joined_at: synctv_core::SystemClock.now(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let wrong_user = RealtimeEvent::UserJoined {
            event_id: "online-wrong-user".to_string(),
            room_id: RoomId::expect_positive(7),
            user_id: other,
            username: "admin".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            permissions: synctv_core::models::RoomPermissionSet::default_admin(),
            role: synctv_proto::common::RoomMemberRole::Admin as i32,
            added_permissions: synctv_core::models::RoomPermissionSet(0),
            removed_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
            joined_at: synctv_core::SystemClock.now(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let wrong_kind = RealtimeEvent::UserLeft {
            event_id: "online-left".to_string(),
            room_id: RoomId::expect_positive(7),
            user_id: user,
            username: "admin".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_proto::common::RoomMemberRole::Admin as i32,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert!(ResourceObserver::online_event_matches_observation(
            &matching, &resource
        ));
        assert!(!ResourceObserver::online_event_matches_observation(
            &wrong_user,
            &resource
        ));
        assert!(!ResourceObserver::online_event_matches_observation(
            &wrong_kind,
            &resource
        ));
    }

    #[test]
    fn playback_observation_starts_from_current_snapshot() {
        let observation = playback_observation();
        let request = synctv_proto::client::ObserveResource {
            observe_id: "playback".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(synctv_proto::client::observe_resource::Resource::Playback(
                synctv_proto::client::ObservePlayback {
                    playback_client_profile: None,
                },
            )),
        };

        assert_eq!(
            ResourceObserver::observation_start_sequence(&observation, &request),
            0
        );
        assert_eq!(
            ResourceObserver::requested_replay_sequence(&request),
            None,
            "playback observation has no client event cursor"
        );
    }

    #[test]
    fn playback_state_observation_has_event_cursor_without_replay_sequence() {
        let observation = playback_state_observation();
        let request = synctv_proto::client::ObserveResource {
            observe_id: "playback-state".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::PlaybackState(
                    synctv_proto::client::ObservePlaybackState {
                        event_sequence: None,
                    },
                ),
            ),
        };

        assert_eq!(ResourceObserver::requested_replay_sequence(&request), None);
        assert!(observation.exposes_client_event_cursor());
    }

    #[test]
    fn validate_requested_replay_sequence_rejects_negative_chat_cursor() {
        let request = synctv_proto::client::ObserveResource {
            observe_id: "chat-events".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::ChatEvents(
                    synctv_proto::client::ObserveChatEvents {
                        after_event_sequence: Some(-1),
                        include_message_types: Vec::new(),
                    },
                ),
            ),
        };

        assert!(matches!(
            ResourceObserver::validate_requested_replay_sequence(&request),
            Err(message) if message.contains("after_event_sequence")
        ));
    }

    #[test]
    fn validate_requested_replay_sequence_rejects_negative_playback_state_sequence() {
        let request = synctv_proto::client::ObserveResource {
            observe_id: "playback-state".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::PlaybackState(
                    synctv_proto::client::ObservePlaybackState {
                        event_sequence: Some(-1),
                    },
                ),
            ),
        };

        assert!(matches!(
            ResourceObserver::validate_requested_replay_sequence(&request),
            Err(message) if message.contains("event_sequence")
        ));
    }

    #[test]
    fn playback_state_observation_starts_from_current_snapshot() {
        let observation = playback_state_observation();
        let request = synctv_proto::client::ObserveResource {
            observe_id: "playback-state".to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(
                synctv_proto::client::observe_resource::Resource::PlaybackState(
                    synctv_proto::client::ObservePlaybackState {
                        event_sequence: Some(42),
                    },
                ),
            ),
        };

        assert_eq!(
            ResourceObserver::observation_start_sequence(&observation, &request),
            0
        );
        assert_eq!(
            ResourceObserver::requested_playback_state_event_sequence(&request),
            Some(42)
        );
    }

    #[test]
    fn normalize_delivery_mode_rejects_unknown_values() {
        assert!(matches!(
            ResourceObserver::normalize_delivery_mode(99),
            Err(message) if message.contains("delivery mode")
        ));
    }

    #[test]
    fn normalize_delivery_mode_defaults_unspecified_to_push_snapshot() -> anyhow::Result<()> {
        assert_eq!(
            ResourceObserver::normalize_delivery_mode(ResourceDeliveryMode::Unspecified as i32)
                .map_err(anyhow::Error::msg)?,
            ResourceDeliveryMode::PushSnapshot
        );
        Ok(())
    }
}
