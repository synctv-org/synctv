use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::{
    NewRealtimeOutboxEvent, PermissionChangedOutboxSnapshot,
    RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxUserLeftEventFactory,
    UserLeftOutboxSnapshot,
};
use tokio::sync::{broadcast, mpsc};

use crate::sync::{
    BroadcastResult, ConnectionId, PublishRequest, RealtimeEvent, RealtimeManager,
    SharedRealtimeEvent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeMetrics {
    pub distributed_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeDeliveryRequirement {
    DistributedWhenAvailable,
    DistributedIfAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeDeliveryOutcome {
    local_delivered: bool,
    distributed_delivered: bool,
    distributed_available: bool,
}

impl RealtimeDeliveryOutcome {
    #[must_use]
    pub const fn from_broadcast(result: &BroadcastResult, metrics: RealtimeMetrics) -> Self {
        Self {
            local_delivered: result.local_sent > 0,
            distributed_delivered: result.redis_sent,
            distributed_available: metrics.distributed_enabled,
        }
    }

    #[must_use]
    pub const fn from_publish_only(distributed_delivered: bool, metrics: RealtimeMetrics) -> Self {
        Self {
            local_delivered: false,
            distributed_delivered,
            distributed_available: metrics.distributed_enabled,
        }
    }

    #[must_use]
    pub const fn local_delivered(self) -> bool {
        self.local_delivered
    }

    #[must_use]
    pub const fn distributed_available(self) -> bool {
        self.distributed_available
    }

    #[must_use]
    pub const fn distributed_delivered(self) -> bool {
        self.distributed_delivered
    }

    #[must_use]
    pub const fn delivered_to_any(self) -> bool {
        self.local_delivered || self.distributed_delivered
    }

    #[must_use]
    pub const fn distributed_delivery_missed(self) -> bool {
        self.distributed_available && !self.distributed_delivered
    }

    #[must_use]
    pub const fn satisfies(self, requirement: RealtimeDeliveryRequirement) -> bool {
        match requirement {
            RealtimeDeliveryRequirement::DistributedWhenAvailable => {
                if self.distributed_available {
                    self.distributed_delivered
                } else {
                    self.delivered_to_any()
                }
            }
            RealtimeDeliveryRequirement::DistributedIfAvailable => {
                !self.distributed_available || self.distributed_delivered
            }
        }
    }
}

#[async_trait]
pub trait RealtimeEventService: Send + Sync {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)>;

    fn unsubscribe(&self, connection_id: &str);

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult;

    fn publish_only(&self, event: RealtimeEvent) -> bool;

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize;

    fn broadcast_admin_local(&self, _event: &RealtimeEvent) -> usize {
        0
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent>;

    fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.subscribe_admin_events()
    }

    fn metrics(&self) -> RealtimeMetrics;

    fn broadcast_outcome(&self, event: RealtimeEvent) -> RealtimeDeliveryOutcome {
        let result = self.broadcast(event);
        RealtimeDeliveryOutcome::from_broadcast(&result, self.metrics())
    }

    fn publish_only_outcome(&self, event: RealtimeEvent) -> RealtimeDeliveryOutcome {
        RealtimeDeliveryOutcome::from_publish_only(self.publish_only(event), self.metrics())
    }

    fn node_id(&self) -> &str;

    async fn shutdown(&self);
}

pub struct LocalNoopRealtimeEventService {
    admin_tx: broadcast::Sender<RealtimeEvent>,
}

impl LocalNoopRealtimeEventService {
    #[must_use]
    pub fn new() -> Self {
        let (admin_tx, _) = broadcast::channel(16);
        Self { admin_tx }
    }
}

impl Default for LocalNoopRealtimeEventService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RealtimeEventService for LocalNoopRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)> {
        let (_tx, rx) = mpsc::channel(16);
        Ok((rx, connection_id))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
        BroadcastResult {
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

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.admin_tx.subscribe()
    }

    fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            distributed_enabled: false,
        }
    }

    fn node_id(&self) -> &'static str {
        "local-noop"
    }

    async fn shutdown(&self) {}
}

#[async_trait]
impl RealtimeEventService for RealtimeManager {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)> {
        RealtimeManager::subscribe_with_id(self, room_id, user_id, connection_id).await
    }

    fn unsubscribe(&self, connection_id: &str) {
        RealtimeManager::unsubscribe(self, connection_id);
    }

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
        RealtimeManager::broadcast(self, event)
    }

    fn publish_only(&self, event: RealtimeEvent) -> bool {
        RealtimeManager::publish_only(self, event)
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        RealtimeManager::message_hub(self).broadcast(room_id, event)
    }

    fn broadcast_admin_local(&self, event: &RealtimeEvent) -> usize {
        match RealtimeManager::admin_event_tx(self).send(event.clone()) {
            Ok(subscriber_count) => subscriber_count,
            Err(error) => {
                tracing::warn!(%error, "failed to broadcast admin realtime event");
                0
            }
        }
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        RealtimeManager::subscribe_admin_events(self)
    }

    fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        RealtimeManager::subscribe_lifecycle_events(self)
    }

    fn metrics(&self) -> RealtimeMetrics {
        let metrics = RealtimeManager::metrics(self);
        RealtimeMetrics {
            distributed_enabled: metrics.distributed_enabled,
        }
    }

    fn node_id(&self) -> &str {
        RealtimeManager::node_id(self)
    }

    async fn shutdown(&self) {
        RealtimeManager::shutdown(self).await;
    }
}

#[async_trait]
pub trait RealtimeFanoutService: Send + Sync {
    async fn try_publish(&self, request: PublishRequest) -> bool;

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String>;

    fn publish_after_outbox_commit(&self, event: RealtimeEvent);

    fn is_distributed_enabled(&self) -> bool;

    fn accepts_immediate_publish(&self) -> bool {
        self.is_distributed_enabled()
    }
}

#[async_trait]
pub trait RoomCacheFanoutService: Send + Sync {
    fn publish_invalidation(&self, room_id: &RoomId);

    async fn try_publish_all_invalidation(&self) -> bool;
}

pub trait LocalRealtimeEventPublisher: Send + Sync {
    fn broadcast_room_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize;
}

pub trait MembershipEventFanoutService: Send + Sync {
    fn prepare_permission_changed_outbox_fanout(
        &self,
        target_is_online: bool,
        target_connection_count: usize,
    ) -> PreparedPermissionChangedFanout;

    fn prepare_user_left_outbox_fanout(&self) -> PreparedUserLeftFanout;
}

pub fn publish_best_effort(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    request: PublishRequest,
) {
    if !realtime_fanout.accepts_immediate_publish() {
        return;
    }

    synctv_core::spawn::spawn_monitored("realtime_fanout_best_effort_publish", async move {
        if !realtime_fanout.try_publish(request).await {
            tracing::warn!("Best-effort realtime fanout publish was not accepted");
        }
    });
}

#[derive(Clone)]
pub struct PreparedPermissionChangedFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_publisher: Arc<dyn LocalRealtimeEventPublisher>,
    events: Arc<parking_lot::Mutex<Vec<RealtimeEvent>>>,
    target_is_online: bool,
    target_connection_count: usize,
}

impl PreparedPermissionChangedFanout {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        local_event_publisher: Arc<dyn LocalRealtimeEventPublisher>,
        target_is_online: bool,
        target_connection_count: usize,
    ) -> Self {
        Self {
            realtime_fanout,
            local_event_publisher,
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
            target_is_online,
            target_connection_count,
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxPermissionChangedEventFactory {
        let prepared = self.clone();
        Arc::new(move |snapshot: &PermissionChangedOutboxSnapshot| {
            let event = RealtimeEvent::PermissionChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: snapshot.room_id,
                target_user_id: snapshot.target_user_id,
                target_username: snapshot.target_username.clone(),
                target_remark_name: snapshot.target_remark_name.clone(),
                target_display_tag: snapshot.target_display_tag.clone(),
                changed_by: snapshot.changed_by,
                changed_by_username: snapshot.changed_by_username.clone(),
                role_changed: snapshot.role_changed,
                new_permissions: snapshot.new_permissions,
                role: snapshot.role,
                added_permissions: snapshot.added_permissions,
                removed_permissions: snapshot.removed_permissions,
                admin_added_permissions: snapshot.admin_added_permissions,
                admin_removed_permissions: snapshot.admin_removed_permissions,
                target_is_online: prepared.target_is_online,
                target_connection_count: prepared.target_connection_count,
                timestamp: synctv_core::SystemClock.now(),
            };
            prepared.events.lock().push(event.clone());
            prepared
                .realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(&mut *self.events.lock());
        for event in events {
            if self.realtime_fanout.is_distributed_enabled() {
                self.realtime_fanout.publish_after_outbox_commit(event);
            } else if let Some(room_id) = event.room_id() {
                self.local_event_publisher
                    .broadcast_room_local(room_id, &event);
            }
        }
    }
}

#[derive(Clone)]
pub struct PreparedUserLeftFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event: Arc<parking_lot::Mutex<Option<RealtimeEvent>>>,
}

impl PreparedUserLeftFanout {
    #[doc(hidden)]
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self {
            realtime_fanout,
            event: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxUserLeftEventFactory {
        let prepared = self.clone();
        Arc::new(move |snapshot: &UserLeftOutboxSnapshot| {
            let event = RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: snapshot.room_id,
                user_id: snapshot.user_id,
                username: snapshot.username.clone(),
                remark_name: snapshot.remark_name.clone(),
                display_tag: snapshot.display_tag.clone(),
                role: snapshot.role,
                timestamp: synctv_core::SystemClock.now(),
            };
            *prepared.event.lock() = Some(event.clone());
            prepared
                .realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self.event.lock().take() {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedRealtimeFanoutPlan {
    event: RealtimeEvent,
    outbox_event: NewRealtimeOutboxEvent,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl PreparedRealtimeFanoutPlan {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        event: RealtimeEvent,
    ) -> Result<Self, String> {
        let outbox_event = realtime_fanout.outbox_event(&event)?;
        Ok(Self {
            event,
            outbox_event,
            realtime_fanout,
        })
    }

    #[must_use]
    pub const fn event(&self) -> &RealtimeEvent {
        &self.event
    }

    #[must_use]
    pub fn into_event(self) -> RealtimeEvent {
        self.event
    }

    #[must_use]
    pub const fn outbox_event(&self) -> &NewRealtimeOutboxEvent {
        &self.outbox_event
    }

    #[must_use]
    pub fn cloned_outbox_event(&self) -> NewRealtimeOutboxEvent {
        self.outbox_event.clone()
    }

    pub fn publish_after_outbox_commit(self) {
        self.realtime_fanout.publish_after_outbox_commit(self.event);
    }
}

type RealtimeEventBuilder<T> = Arc<dyn Fn(&T) -> RealtimeEvent + Send + Sync>;
type RealtimeOutboxEventFactory<T> =
    Arc<dyn Fn(&T) -> synctv_core::Result<NewRealtimeOutboxEvent> + Send + Sync>;

#[derive(Clone)]
pub struct PreparedOutboxFanout<T> {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_builder: RealtimeEventBuilder<T>,
    event: Arc<Mutex<Option<RealtimeEvent>>>,
}

impl<T: 'static> PreparedOutboxFanout<T> {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        event_builder: impl Fn(&T) -> RealtimeEvent + Send + Sync + 'static,
    ) -> Self {
        Self {
            realtime_fanout,
            event_builder: Arc::new(event_builder),
            event: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxEventFactory<T> {
        let realtime_fanout = self.realtime_fanout.clone();
        let event_builder = self.event_builder.clone();
        let event_slot = self.event.clone();
        Arc::new(move |value: &T| {
            let event = event_builder(value);
            *event_slot.lock() = Some(event.clone());
            realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self.event.lock().take() {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}
