use prost::Message;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::time::Duration;

use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackState, UserId},
    service::{ChatService, RoomService},
};
use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

use super::MessageSender;
use crate::impls::client::convert::{playback_client_profile_from_proto, playback_state_to_proto};
use crate::impls::client::RoomActor;
use crate::impls::messaging::chat_message_event_to_proto;
use crate::impls::playback_snapshot::PlaybackSnapshotService;
use crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService;
use crate::impls::room_members_snapshot::RoomMembersSnapshotService;
use crate::impls::room_settings_snapshot::RoomSettingsSnapshotService;
use crate::proto::client::{ResourceDeliveryMode, ServerMessage};
use crate::resource_change::{
    provider_credential_resource_invalidation, resource_invalidations_for_cache_targets,
    resource_invalidations_for_room_event, PlaybackSnapshotInvalidation, ResourceInvalidation,
};

const RESOURCE_EVALUATION_REUSE_WINDOW: Duration = Duration::from_millis(25);
const MEDIA_RESOURCE_REFRESH_DEDUP_WINDOW: Duration = Duration::from_secs(5);
pub(super) const MAX_RESOURCE_OBSERVATIONS_PER_CONNECTION: usize = 64;
const CHAT_EVENT_REPLAY_BATCH_LIMIT: i32 = 500;

fn chat_event_version(event: &synctv_core::models::ChatMessageEvent) -> String {
    format!(
        "{}:{}",
        event.occurred_at.timestamp_millis(),
        event.event_id
    )
}

#[derive(Debug, Clone, Default)]
struct ResourceObserverState {
    observations: HashMap<String, ResourceObservation>,
    pending_observe_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ResourceObservation {
    observe_id: String,
    last_version: String,
    delivery_mode: ResourceDeliveryMode,
    resource: ObservedResource,
    last_source: Option<PlaybackSnapshotSource>,
    expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct ObservationUpdate {
    version: String,
    changed: bool,
    changed_message: Option<crate::proto::client::ResourceChanged>,
}

#[derive(Debug, Clone)]
struct ResourceEvaluation {
    version: String,
    source: Option<PlaybackSnapshotSource>,
    expires_at: Option<i64>,
    payload: crate::proto::client::resource_changed::Payload,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ObservationEvaluationKey {
    PlaybackState {
        delivery_mode: i32,
    },
    PlaybackSnapshot {
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
    RoomMembers {
        delivery_mode: i32,
        request: Vec<u8>,
    },
    ChatEvents {
        delivery_mode: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SharedObservationEvaluationKey {
    room_id: RoomId,
    user_id: Option<UserId>,
    service_id: usize,
    evaluation: ObservationEvaluationKey,
}

#[derive(Clone)]
enum SharedResourceServiceWeak {
    AlwaysAlive,
    RoomService(Weak<RoomService>),
    PlaybackSnapshot(Weak<dyn PlaybackSnapshotService>),
    RoomSettings(Weak<dyn RoomSettingsSnapshotService>),
    PlaylistItems(Weak<dyn PlaylistItemsSnapshotService>),
    RoomMembers(Weak<dyn RoomMembersSnapshotService>),
}

impl SharedResourceServiceWeak {
    fn is_alive(&self) -> bool {
        match self {
            Self::AlwaysAlive => true,
            Self::RoomService(service) => service.upgrade().is_some(),
            Self::PlaybackSnapshot(service) => service.upgrade().is_some(),
            Self::RoomSettings(service) => service.upgrade().is_some(),
            Self::PlaylistItems(service) => service.upgrade().is_some(),
            Self::RoomMembers(service) => service.upgrade().is_some(),
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
    PlaybackSnapshot {
        playback_client_profile: Option<synctv_core::provider::PlaybackClientProfile>,
    },
    RoomSettings,
    PlaylistItems {
        request: crate::proto::client::ListPlaylistItemsRequest,
    },
    RoomMembers {
        request: crate::proto::client::GetRoomMembersRequest,
    },
    ChatEvents,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlaybackSnapshotSource {
    media_id: Option<MediaId>,
    playlist_id: Option<PlaylistId>,
    target: Vec<u8>,
    playback_client_profile: Option<synctv_core::provider::PlaybackClientProfile>,
}

impl PlaybackSnapshotSource {
    fn from_state(
        state: &RoomPlaybackState,
        playback_client_profile: Option<synctv_core::provider::PlaybackClientProfile>,
    ) -> Self {
        Self {
            media_id: state.playing_media_id,
            playlist_id: state.playing_playlist_id,
            target: state.target.clone(),
            playback_client_profile,
        }
    }

    fn from_observe(
        observe: &crate::proto::client::ObservePlaybackSnapshot,
        public_id_codec: &crate::PublicIdCodec,
    ) -> Option<Self> {
        let media_id = if observe.media_id.trim().is_empty() {
            None
        } else {
            match public_id_codec.decode_media_id(&observe.media_id) {
                Ok(id) => Some(id),
                Err(_) => return None,
            }
        };
        let playlist_id = if observe.playlist_id.trim().is_empty() {
            None
        } else {
            match public_id_codec.decode_playlist_id(&observe.playlist_id) {
                Ok(id) => Some(id),
                Err(_) => return None,
            }
        };

        Some(Self {
            media_id,
            playlist_id,
            target: observe.target.clone(),
            playback_client_profile: playback_client_profile_from_proto(
                observe.playback_client_profile.as_ref(),
            ),
        })
    }
}

impl ResourceObservation {
    fn evaluation_key(&self) -> ObservationEvaluationKey {
        let delivery_mode = self.delivery_mode as i32;
        match &self.resource {
            ObservedResource::PlaybackState => {
                ObservationEvaluationKey::PlaybackState { delivery_mode }
            }
            ObservedResource::PlaybackSnapshot {
                playback_client_profile,
            } => ObservationEvaluationKey::PlaybackSnapshot {
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
            ObservedResource::RoomMembers { request } => ObservationEvaluationKey::RoomMembers {
                delivery_mode,
                request: request.encode_to_vec(),
            },
            ObservedResource::ChatEvents => ObservationEvaluationKey::ChatEvents { delivery_mode },
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
    user_id: UserId,
    actor: RoomActor,
    connection_id: String,
    room_service: Arc<RoomService>,
    public_id_codec: Arc<crate::PublicIdCodec>,
    sender: Arc<dyn MessageSender>,
    pub(super) room_hub: Arc<MediaResourceHub>,
    playback_snapshot_service: parking_lot::RwLock<Option<Arc<dyn PlaybackSnapshotService>>>,
    playlist_items_snapshot_service:
        parking_lot::RwLock<Option<Arc<dyn PlaylistItemsSnapshotService>>>,
    room_members_snapshot_service: parking_lot::RwLock<Option<Arc<dyn RoomMembersSnapshotService>>>,
    room_settings_snapshot_service: parking_lot::RwLock<Arc<dyn RoomSettingsSnapshotService>>,
    room_settings_snapshot_service_id: parking_lot::RwLock<usize>,
    state: tokio::sync::Mutex<ResourceObserverState>,
}

pub(super) struct ResourceObserverParams {
    pub(super) room_id: RoomId,
    pub(super) user_id: UserId,
    pub(super) actor: RoomActor,
    pub(super) connection_id: String,
    pub(super) room_service: Arc<RoomService>,
    pub(super) public_id_codec: Arc<crate::PublicIdCodec>,
    pub(super) sender: Arc<dyn MessageSender>,
    pub(super) room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
}

impl ResourceObserver {
    pub(super) fn new(params: ResourceObserverParams) -> Self {
        let ResourceObserverParams {
            room_id,
            user_id,
            actor,
            connection_id,
            room_service,
            public_id_codec,
            sender,
            room_settings_snapshot_service,
        } = params;
        let room_settings_snapshot_service_id = Arc::as_ptr(&room_service) as usize;
        let room_hub = media_resource_hub(room_id, &room_service);
        Self {
            room_id,
            user_id,
            actor,
            connection_id,
            room_service,
            public_id_codec,
            sender,
            room_hub,
            playback_snapshot_service: parking_lot::RwLock::new(None),
            playlist_items_snapshot_service: parking_lot::RwLock::new(None),
            room_members_snapshot_service: parking_lot::RwLock::new(None),
            room_settings_snapshot_service: parking_lot::RwLock::new(
                room_settings_snapshot_service,
            ),
            room_settings_snapshot_service_id: parking_lot::RwLock::new(
                room_settings_snapshot_service_id,
            ),
            state: tokio::sync::Mutex::new(ResourceObserverState::default()),
        }
    }

    pub(super) fn set_playback_snapshot_service(
        &self,
        service: Option<Arc<dyn PlaybackSnapshotService>>,
    ) {
        *self.playback_snapshot_service.write() = service;
    }

    pub(super) fn set_connection_id(&mut self, connection_id: String) {
        self.connection_id = connection_id;
    }

    pub(super) fn set_playlist_items_snapshot_service(
        &self,
        service: Option<Arc<dyn PlaylistItemsSnapshotService>>,
    ) {
        *self.playlist_items_snapshot_service.write() = service;
    }

    pub(super) fn set_room_members_snapshot_service(
        &self,
        service: Option<Arc<dyn RoomMembersSnapshotService>>,
    ) {
        *self.room_members_snapshot_service.write() = service;
    }

    pub(super) fn set_room_settings_snapshot_service(
        &self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) {
        *self.room_settings_snapshot_service_id.write() =
            Arc::as_ptr(&service).cast::<()>() as usize;
        *self.room_settings_snapshot_service.write() = service;
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

    #[cfg(test)]
    pub(super) async fn has_chat_events_observation(&self) -> bool {
        self.state
            .lock()
            .await
            .observations
            .values()
            .any(|observation| matches!(observation.resource, ObservedResource::ChatEvents))
    }

    fn public_room_id(&self) -> String {
        self.public_id_codec
            .encode_room_id(self.room_id)
            .expect("positive room ID must encode")
    }

    fn shared_evaluation_key(
        &self,
        observation: &ResourceObservation,
    ) -> SharedObservationEvaluationKey {
        let service_identity = self.resource_evaluation_service_identity(&observation.resource);
        SharedObservationEvaluationKey {
            room_id: self.room_id,
            user_id: self.resource_evaluation_user_scope(&observation.resource),
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
        }
    }

    async fn remove_local_observation(&self, observe_id: &str) {
        let mut state = self.state.lock().await;
        state.observations.remove(observe_id);
        state.pending_observe_ids.remove(observe_id);
    }

    pub(super) async fn clear_observations(&self) {
        {
            let mut state = self.state.lock().await;
            state.observations.clear();
            state.pending_observe_ids.clear();
        }
        self.room_hub
            .unregister_connection(&self.connection_id)
            .await;
    }

    pub(super) async fn room_has_playback_snapshot_observers(room_id: RoomId) -> bool {
        let hubs = {
            let mut registry = MEDIA_RESOURCE_HUBS.lock();
            registry.retain(|_, hub| hub.strong_count() > 0);
            registry
                .iter()
                .filter_map(|(key, hub)| (key.room_id == room_id).then(|| hub.upgrade()).flatten())
                .collect::<Vec<_>>()
        };

        for hub in hubs {
            if hub.has_playback_snapshot_observations().await {
                return true;
            }
        }
        false
    }

    pub(super) async fn active_playback_snapshot_rooms() -> Vec<RoomId> {
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
            if hub.has_playback_snapshot_observations().await {
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
        state.observations.get(&observe_id).cloned()
    }
}

impl MediaResourceHub {
    async fn has_playback_snapshot_observations(&self) -> bool {
        let state = self.state.lock().await;
        state.subscriptions.values().any(|subscription| {
            matches!(
                subscription.observation.resource,
                ObservedResource::PlaybackSnapshot { .. }
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
        let outcome = self
            .refresh_for_invalidations_with_key(
                Some(format!("cluster:{}", event.event_id())),
                invalidations,
                false,
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
        self.refresh_for_invalidations_with_key(None, invalidations, force)
            .await
    }

    async fn refresh_for_invalidations_with_key(
        self: &Arc<Self>,
        refresh_key: Option<String>,
        invalidations: Vec<ResourceInvalidation>,
        force: bool,
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
                    self.refresh_for_invalidations_uncached(&invalidations, force)
                        .await
                })
                .await
                .clone();
            self.finish_deduped_refresh(&refresh_key, &entry).await;
            return result;
        }
        self.refresh_for_invalidations_uncached(&invalidations, force)
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

    async fn refresh_expired_playback_snapshots(
        self: &Arc<Self>,
        fatal_connection_id: Option<&str>,
    ) -> Result<(), String> {
        let refresh_key = format!("playback_snapshot_expiry:room:{}", self.room_id);
        let entry = self.start_in_flight_refresh(&refresh_key).await;
        let result = entry
            .result
            .get_or_init(|| async {
                let now = chrono::Utc::now().timestamp();
                let subscriptions = {
                    let state = self.state.lock().await;
                    state
                        .subscriptions
                        .iter()
                        .filter_map(|(key, subscription)| {
                            let is_expired = matches!(
                                &subscription.observation.resource,
                                ObservedResource::PlaybackSnapshot { .. }
                            ) && subscription
                                .observation
                                .expires_at
                                .is_some_and(|expires_at| now >= expires_at);
                            is_expired.then_some((
                                key.clone(),
                                subscription.observer.clone(),
                                subscription.observation.clone(),
                                true,
                                subscription.revision,
                            ))
                        })
                        .collect::<Vec<_>>()
                };
                if subscriptions.is_empty() {
                    return ResourceRefreshOutcome::default();
                }
                self.bump_resource_generation().await;
                self.refresh_subscription_batch(subscriptions).await
            })
            .await
            .clone();
        self.finish_in_flight_refresh(&refresh_key, &entry).await;
        if let Some(error) = result.error_for_connection(fatal_connection_id) {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn start_in_flight_refresh(&self, refresh_key: &str) -> MediaResourceRefreshEntry {
        let mut state = self.state.lock().await;
        if let Some(entry) = state.in_flight_refreshes.get(refresh_key) {
            return entry.clone();
        }
        let entry = MediaResourceRefreshEntry {
            result: Arc::new(tokio::sync::OnceCell::new()),
            subscription_generation: state.subscription_generation,
        };
        state
            .in_flight_refreshes
            .insert(refresh_key.to_string(), entry.clone());
        entry
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

    async fn finish_in_flight_refresh(&self, refresh_key: &str, entry: &MediaResourceRefreshEntry) {
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
            let invalidations =
                resource_invalidations_for_cache_targets(targets, self.room_id, observer.user_id);
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

        self.refresh_subscription_batch(refresh_plan.into_iter().map(
            |(key, (observer, observation, force, revision))| {
                (key, observer, observation, force, revision)
            },
        ))
        .await
    }

    async fn refresh_for_invalidations_uncached(
        &self,
        invalidations: &[ResourceInvalidation],
        force: bool,
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
                    let mut updated_observation = observation.clone();
                    updated_observation.last_version = chat_event_version(event);
                    let changed = crate::proto::client::ResourceChanged {
                        observe_id: updated_observation.observe_id.clone(),
                        version: updated_observation.last_version.clone(),
                        payload: Some(crate::proto::client::resource_changed::Payload::ChatEvent(
                            chat_message_event_to_proto(
                                event,
                                &observer.public_id_codec,
                                observer.room_id,
                            ),
                        )),
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
                        ObservedResource::PlaybackSnapshot { .. } => observer
                            .current_playback_depends_on_provider_credential(
                                user_id, provider, server_id,
                            )
                            .await
                            .unwrap_or_else(|error| {
                                tracing::warn!(
                                    room_id = %self.room_id,
                                    user_id = %observer.user_id,
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
            .refresh_subscription_batch(refresh_plan.into_iter().map(
                |(key, (observer, observation, force, revision))| {
                    (key, observer, observation, force, revision)
                },
            ))
            .await;
        outcome.send_failures.extend(chat_outcome.send_failures);
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

    async fn refresh_subscription_batch<I>(&self, subscriptions: I) -> ResourceRefreshOutcome
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
                )
                .await;

            match evaluation {
                Ok(evaluation) => {
                    for mut entry in entries {
                        let update = ResourceObserver::apply_resource_evaluation(
                            &mut entry.observation,
                            entry.force,
                            evaluation.clone(),
                        );
                        match self
                            .send_and_commit_subscription_update(
                                &entry.key,
                                entry.revision,
                                &entry.observer,
                                entry.observation.clone(),
                                update.changed_message,
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
                                    user_id = %entry.observer.user_id,
                                    observe_id = %entry.key.observe_id,
                                    error = %error,
                                    "Removed observed resource after ResourceChanged send failure"
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
                            let _ = entry.observer.send_server_message(
                                ResourceObserver::resource_observe_error_message(
                                    entry.key.observe_id.clone(),
                                    crate::impls::ApiError::Internal(error.clone()),
                                ),
                            );
                            tracing::warn!(
                                room_id = %self.room_id,
                                user_id = %entry.observer.user_id,
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
        changed_message: Option<crate::proto::client::ResourceChanged>,
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
                    crate::proto::client::server_message::Message::ResourceChanged(changed),
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
                crate::proto::client::server_message::Message::ResourceObserveError(
                    crate::proto::client::ResourceObserveError {
                        observe_id: observe_id.into(),
                        error: Some(api_error.to_proto_error()),
                    },
                ),
            ),
        }
    }

    fn normalize_delivery_mode(mode: i32) -> ResourceDeliveryMode {
        match ResourceDeliveryMode::try_from(mode).unwrap_or(ResourceDeliveryMode::Unspecified) {
            ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                ResourceDeliveryMode::PushSnapshot
            }
            ResourceDeliveryMode::NotifyOnly => ResourceDeliveryMode::NotifyOnly,
        }
    }

    fn observation_from_request(
        &self,
        request: &crate::proto::client::ObserveResource,
    ) -> Result<ResourceObservation, String> {
        use crate::proto::client::observe_resource::Resource;

        let observe_id = request.observe_id.trim();
        if observe_id.is_empty() {
            return Err("observe_id is required".to_string());
        }
        if observe_id.len() > 128 {
            return Err("observe_id is too long".to_string());
        }

        let (resource, last_source) = match request
            .resource
            .as_ref()
            .ok_or_else(|| "observe resource is required".to_string())?
        {
            Resource::PlaybackState(_) => (ObservedResource::PlaybackState, None),
            Resource::PlaybackSnapshot(observe) => {
                let playback_client_profile =
                    playback_client_profile_from_proto(observe.playback_client_profile.as_ref());
                (
                    ObservedResource::PlaybackSnapshot {
                        playback_client_profile,
                    },
                    PlaybackSnapshotSource::from_observe(observe, &self.public_id_codec),
                )
            }
            Resource::RoomSettings(_) => (ObservedResource::RoomSettings, None),
            Resource::PlaylistItems(observe) => (
                ObservedResource::PlaylistItems {
                    request: observe.request.clone().unwrap_or_default(),
                },
                None,
            ),
            Resource::RoomMembers(observe) => (
                ObservedResource::RoomMembers {
                    request: observe.request.clone().unwrap_or_default(),
                },
                None,
            ),
            Resource::ChatEvents(_) => (ObservedResource::ChatEvents, None),
        };

        Ok(ResourceObservation {
            observe_id: observe_id.to_string(),
            last_version: request.version.clone(),
            delivery_mode: Self::normalize_delivery_mode(request.delivery_mode),
            resource,
            last_source,
            expires_at: None,
        })
    }

    pub(super) async fn handle_observe_resource(
        self: &Arc<Self>,
        request: &crate::proto::client::ObserveResource,
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

        match self.evaluate_observation(&mut observation).await {
            Ok(update) => {
                observation.consume_one_shot_options();
                let Some(observation) = self.commit_local_observation(observation).await else {
                    return Ok(());
                };
                let observe_id = observation.observe_id.clone();
                if let Err(error) = self.send_server_message(ServerMessage {
                    message: Some(
                        crate::proto::client::server_message::Message::ResourceObserved(
                            crate::proto::client::ResourceObserved {
                                observe_id: observe_id.clone(),
                                version: update.version.clone(),
                                changed: update.changed,
                            },
                        ),
                    ),
                }) {
                    self.remove_local_observation(&observe_id).await;
                    return Err(error);
                }
                if let Some(changed) = update.changed_message {
                    if let Err(error) = self.send_server_message(ServerMessage {
                        message: Some(
                            crate::proto::client::server_message::Message::ResourceChanged(changed),
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
        &self,
        chat_service: &ChatService,
        request: &crate::proto::client::ObserveResource,
    ) -> Result<(), String> {
        let Some(crate::proto::client::observe_resource::Resource::ChatEvents(chat_events)) =
            request.resource.as_ref()
        else {
            return Ok(());
        };
        let observe_id = request.observe_id.trim();
        let mut after_event_id = chat_events.after_event_id.trim().to_string();
        if observe_id.is_empty() || after_event_id.is_empty() {
            return Ok(());
        }

        loop {
            let events = chat_service
                .get_events_after(
                    &self.room_id,
                    Some(after_event_id.as_str()),
                    CHAT_EVENT_REPLAY_BATCH_LIMIT,
                )
                .await
                .map_err(|error| error.to_string())?;
            if events.is_empty() {
                break;
            }

            for logged in &events {
                let event = &logged.event;
                let version = chat_event_version(event);
                self.send_server_message(ServerMessage {
                    message: Some(
                        crate::proto::client::server_message::Message::ResourceChanged(
                            crate::proto::client::ResourceChanged {
                                observe_id: observe_id.to_string(),
                                version: version.clone(),
                                payload: Some(
                                    crate::proto::client::resource_changed::Payload::ChatEvent(
                                        chat_message_event_to_proto(
                                            event,
                                            &self.public_id_codec,
                                            self.room_id,
                                        ),
                                    ),
                                ),
                            },
                        ),
                    ),
                })?;
                self.update_local_observation_version(observe_id, version)
                    .await;
                after_event_id = event.event_id.clone();
            }

            if events.len() < usize::try_from(CHAT_EVENT_REPLAY_BATCH_LIMIT).unwrap_or(usize::MAX) {
                break;
            }
        }

        Ok(())
    }

    pub(super) async fn handle_unobserve_resource(
        self: &Arc<Self>,
        request: &crate::proto::client::UnobserveResource,
    ) -> Result<(), String> {
        let observe_id = request.observe_id.trim();
        self.remove_local_observation(observe_id).await;
        self.room_hub
            .unregister_observation(&self.connection_id, observe_id)
            .await;
        Ok(())
    }

    async fn update_local_observation_version(&self, observe_id: &str, version: String) {
        let mut state = self.state.lock().await;
        if let Some(observation) = state.observations.get_mut(observe_id) {
            observation.last_version = version;
        }
    }

    pub(super) async fn next_playback_snapshot_refresh_deadline(
        &self,
    ) -> Option<tokio::time::Instant> {
        let state = self.state.lock().await;
        let expires_at = state
            .observations
            .values()
            .filter_map(|observation| match &observation.resource {
                ObservedResource::PlaybackSnapshot { .. } => observation.expires_at,
                _ => None,
            })
            .min()?;
        let seconds_until_refresh = expires_at.saturating_sub(chrono::Utc::now().timestamp());
        Some(
            tokio::time::Instant::now()
                + Duration::from_secs(u64::try_from(seconds_until_refresh).unwrap_or_default()),
        )
    }

    pub(super) async fn refresh_expired_playback_snapshot_observations(
        &self,
    ) -> Result<(), String> {
        self.room_hub
            .refresh_expired_playback_snapshots(Some(&self.connection_id))
            .await
    }

    async fn current_playback_depends_on_provider_credential(
        &self,
        changed_user_id: &synctv_core::models::UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<bool, String> {
        let service = {
            let service = self.playback_snapshot_service.read();
            service.clone()
        };
        let Some(service) = service else {
            return Ok(false);
        };

        let state = self
            .room_service
            .get_playback_state(&self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        let dependencies = service
            .playback_credential_dependencies(&self.user_id, &self.room_id, &state)
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
            )
            .await;
        if let Some(error) = outcome.error_for_connection(Some(&self.connection_id)) {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
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
                user_id = %self.user_id,
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
            ObservedResource::PlaybackSnapshot { .. } => {
                Self::playback_snapshot_invalidated_by(observation, invalidation)
            }
            ObservedResource::RoomSettings => {
                matches!(invalidation, ResourceInvalidation::RoomSettings)
            }
            ObservedResource::PlaylistItems { .. } => matches!(
                invalidation,
                ResourceInvalidation::PlaylistItems
                    | ResourceInvalidation::ProviderCredential { .. }
            ),
            ObservedResource::RoomMembers { .. } => {
                matches!(invalidation, ResourceInvalidation::RoomMembers)
            }
            ObservedResource::ChatEvents => {
                matches!(invalidation, ResourceInvalidation::ChatEvents { .. })
            }
        }
    }

    fn playback_snapshot_invalidated_by(
        observation: &ResourceObservation,
        invalidation: &ResourceInvalidation,
    ) -> bool {
        match invalidation {
            ResourceInvalidation::PlaybackSnapshot(reason) => match reason {
                PlaybackSnapshotInvalidation::PlaybackStateChanged
                | PlaybackSnapshotInvalidation::Cache => true,
                PlaybackSnapshotInvalidation::MediaUpdated { media_id } => observation
                    .last_source
                    .as_ref()
                    .is_none_or(|source| source.media_id.as_ref() == Some(media_id)),
                PlaybackSnapshotInvalidation::PlaylistUpdated { playlist_id } => observation
                    .last_source
                    .as_ref()
                    .is_none_or(|source| source.playlist_id.as_ref() == Some(playlist_id)),
            },
            _ => false,
        }
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
            .load_resource_evaluation(key, &observation.resource, observation.delivery_mode)
            .await?;
        Ok(Self::apply_resource_evaluation(
            observation,
            force,
            evaluation,
        ))
    }

    async fn load_resource_evaluation(
        &self,
        key: ObservationEvaluationKey,
        resource: &ObservedResource,
        delivery_mode: ResourceDeliveryMode,
    ) -> Result<ResourceEvaluation, String> {
        let service_identity = self.resource_evaluation_service_identity(resource);
        let shared_key = SharedObservationEvaluationKey {
            room_id: self.room_id,
            user_id: self.resource_evaluation_user_scope(resource),
            service_id: service_identity.id,
            evaluation: key,
        };
        let resource_generation = self.room_hub.resource_generation().await;
        let entry = {
            let mut in_flight = RESOURCE_EVALUATION_SINGLEFLIGHT.lock().await;
            let now = tokio::time::Instant::now();
            if let Some(entry) = in_flight.get(&shared_key) {
                if entry.service.is_alive()
                    && entry.resource_generation == resource_generation
                    && (!entry.result.initialized() || entry.can_reuse_completed(now))
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
        let _ = entry.completed_at.set(tokio::time::Instant::now());

        schedule_resource_evaluation_singleflight_cleanup(shared_key, Arc::clone(&entry));

        result
    }

    fn resource_evaluation_user_scope(&self, resource: &ObservedResource) -> Option<UserId> {
        match resource {
            ObservedResource::PlaybackState | ObservedResource::RoomSettings => None,
            ObservedResource::PlaybackSnapshot { .. }
            | ObservedResource::PlaylistItems { .. }
            | ObservedResource::RoomMembers { .. }
            | ObservedResource::ChatEvents => Some(self.user_id),
        }
    }

    fn resource_evaluation_service_identity(
        &self,
        resource: &ObservedResource,
    ) -> SharedResourceServiceIdentity {
        match resource {
            ObservedResource::PlaybackState | ObservedResource::ChatEvents => {
                SharedResourceServiceIdentity {
                    id: Arc::as_ptr(&self.room_service) as usize,
                    weak: SharedResourceServiceWeak::RoomService(Arc::downgrade(
                        &self.room_service,
                    )),
                }
            }
            ObservedResource::PlaybackSnapshot { .. } => {
                self.playback_snapshot_service.read().as_ref().map_or(
                    SharedResourceServiceIdentity {
                        id: 0,
                        weak: SharedResourceServiceWeak::AlwaysAlive,
                    },
                    |service| SharedResourceServiceIdentity {
                        id: Arc::as_ptr(service).cast::<()>() as usize,
                        weak: SharedResourceServiceWeak::PlaybackSnapshot(Arc::downgrade(service)),
                    },
                )
            }
            ObservedResource::RoomSettings => {
                let id = *self.room_settings_snapshot_service_id.read();
                if id == Arc::as_ptr(&self.room_service) as usize {
                    SharedResourceServiceIdentity {
                        id,
                        weak: SharedResourceServiceWeak::RoomService(Arc::downgrade(
                            &self.room_service,
                        )),
                    }
                } else {
                    let service = self.room_settings_snapshot_service.read();
                    SharedResourceServiceIdentity {
                        id,
                        weak: SharedResourceServiceWeak::RoomSettings(Arc::downgrade(&service)),
                    }
                }
            }
            ObservedResource::PlaylistItems { .. } => {
                self.playlist_items_snapshot_service.read().as_ref().map_or(
                    SharedResourceServiceIdentity {
                        id: 0,
                        weak: SharedResourceServiceWeak::AlwaysAlive,
                    },
                    |service| SharedResourceServiceIdentity {
                        id: Arc::as_ptr(service).cast::<()>() as usize,
                        weak: SharedResourceServiceWeak::PlaylistItems(Arc::downgrade(service)),
                    },
                )
            }
            ObservedResource::RoomMembers { .. } => {
                self.room_members_snapshot_service.read().as_ref().map_or(
                    SharedResourceServiceIdentity {
                        id: 0,
                        weak: SharedResourceServiceWeak::AlwaysAlive,
                    },
                    |service| SharedResourceServiceIdentity {
                        id: Arc::as_ptr(service).cast::<()>() as usize,
                        weak: SharedResourceServiceWeak::RoomMembers(Arc::downgrade(service)),
                    },
                )
            }
        }
    }

    async fn load_resource_evaluation_uncached(
        &self,
        resource: &ObservedResource,
        delivery_mode: ResourceDeliveryMode,
    ) -> Result<ResourceEvaluation, String> {
        use crate::proto::client::resource_changed::Payload;

        let (version, source, expires_at, payload) = match resource {
            ObservedResource::PlaybackState => {
                let state = self
                    .room_service
                    .get_playback_state(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let version = state.version.to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaybackState(playback_state_to_proto(
                            &state,
                            &self.public_id_codec,
                        ))
                    }
                };
                (version, None, None, payload)
            }
            ObservedResource::PlaybackSnapshot {
                playback_client_profile,
            } => {
                let service = {
                    let service = self.playback_snapshot_service.read();
                    service.clone()
                }
                .ok_or_else(|| "Playback snapshot service is not available".to_string())?;
                let state = self
                    .room_service
                    .get_playback_state(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let current_source =
                    PlaybackSnapshotSource::from_state(&state, playback_client_profile.clone());
                let snapshot = service
                    .get_playback_snapshot(
                        &self.user_id,
                        &self.room_id,
                        &state,
                        playback_client_profile.as_ref(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaybackSnapshot(snapshot.clone())
                    }
                };
                (
                    snapshot.version.clone(),
                    Some(current_source),
                    snapshot.expires_at,
                    payload,
                )
            }
            ObservedResource::RoomSettings => {
                let service = { Arc::clone(&self.room_settings_snapshot_service.read()) };
                let snapshot = service
                    .get_room_settings_snapshot(&self.room_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let version = snapshot.version.to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::RoomSettings(crate::proto::client::RoomSettingsChanged {
                            room_id: self.public_room_id(),
                            settings: snapshot.settings,
                            version: snapshot.version,
                        })
                    }
                };
                (version, None, None, payload)
            }
            ObservedResource::PlaylistItems { request } => {
                let service = {
                    let service = self.playlist_items_snapshot_service.read();
                    service.clone()
                }
                .ok_or_else(|| "Playlist items snapshot service is not available".to_string())?;
                let actor = self.resource_actor().await?;
                let snapshot = service
                    .get_playlist_items_snapshot(&actor, request)
                    .await
                    .map_err(|error| error.to_string())?;
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::PlaylistItems(snapshot.clone())
                    }
                };
                (snapshot.version.clone(), None, None, payload)
            }
            ObservedResource::RoomMembers { request } => {
                let service = {
                    let service = self.room_members_snapshot_service.read();
                    service.clone()
                }
                .ok_or_else(|| "Room members snapshot service is not available".to_string())?;
                let actor = self.resource_actor().await?;
                let snapshot = service
                    .get_room_members_snapshot(&actor, request)
                    .await
                    .map_err(|error| error.to_string())?;
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                    ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot => {
                        Payload::RoomMembers(snapshot.clone())
                    }
                };
                (snapshot.version.clone(), None, None, payload)
            }
            ObservedResource::ChatEvents => {
                let version = chrono::Utc::now().timestamp_millis().to_string();
                let payload = match delivery_mode {
                    ResourceDeliveryMode::NotifyOnly
                    | ResourceDeliveryMode::Unspecified
                    | ResourceDeliveryMode::PushSnapshot => {
                        Payload::ChangedOnly(crate::proto::client::ResourceChangedOnly {})
                    }
                };
                (version, None, None, payload)
            }
        };

        Ok(ResourceEvaluation {
            version,
            source,
            expires_at,
            payload,
        })
    }

    fn apply_resource_evaluation(
        observation: &mut ResourceObservation,
        force: bool,
        evaluation: ResourceEvaluation,
    ) -> ObservationUpdate {
        let ResourceEvaluation {
            version,
            source,
            expires_at,
            payload,
        } = evaluation;

        let source_changed =
            source.is_some() && observation.last_source.as_ref() != source.as_ref();
        let expired = observation
            .expires_at
            .is_some_and(|expires_at| chrono::Utc::now().timestamp() >= expires_at);
        let changed = force || observation.last_version != version || source_changed || expired;

        observation.last_version.clone_from(&version);
        observation.last_source = source;
        observation.expires_at = expires_at;

        let changed_message = changed.then(|| crate::proto::client::ResourceChanged {
            observe_id: observation.observe_id.clone(),
            version: version.clone(),
            payload: Some(payload),
        });

        ObservationUpdate {
            version,
            changed,
            changed_message,
        }
    }
}
