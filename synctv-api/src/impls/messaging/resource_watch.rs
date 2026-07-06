use std::sync::Arc;

use synctv_core::{
    models::{RoomId, RoomPermission, UserId},
    service::{ChatService, OnlinePresenceService, RoomService},
};
use synctv_realtime::sync::{ConnectionId, RealtimeEvent, SharedRealtimeEvent};

use crate::impls::playback::PlaybackService;
use crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService;
use crate::impls::room_settings_snapshot::RoomSettingsSnapshotService;
use crate::runtime::RealtimeEventService;
use synctv_proto::client::ObserveResource;
use synctv_realtime::sync::ConnectionRuntime;

use super::event_policy::{watch_admin_event_matches, watch_disconnect_signal_matches};
use super::identity::{classify_realtime_join_error_message, RealtimeJoinError, RealtimePrincipal};
use super::membership::{RealtimeMembershipAccess, RealtimeMembershipProbe};
use super::resource_observer::{ResourceObserver, ResourceObserverParams};
use super::MessageSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchResourceKind {
    PlaybackState,
    Playback,
    RoomSettings,
    PlaylistItems,
    RoomMemberEvents,
    ChatEvents,
    ChatPinEvents,
}

impl WatchResourceKind {
    fn observe_id(self) -> &'static str {
        match self {
            Self::PlaybackState => "playback_state",
            Self::Playback => "playback",
            Self::RoomSettings => "room_settings",
            Self::PlaylistItems => "playlist_items",
            Self::RoomMemberEvents => "room_member_events",
            Self::ChatEvents => "chat_events",
            Self::ChatPinEvents => "chat_pin_events",
        }
    }
}

#[derive(Clone)]
pub struct ResourceWatchSessionConfig {
    pub room_id: RoomId,
    pub principal: RealtimePrincipal,
    pub room_service: Arc<RoomService>,
    pub chat_service: Option<Arc<ChatService>>,
    pub clock: Arc<dyn synctv_core::Clock>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub presence_service: Arc<OnlinePresenceService>,
    pub public_id_codec: Arc<crate::public_id::PublicIdCodec>,
    pub sender: Arc<dyn MessageSender>,
    pub playback_service: Arc<dyn PlaybackService>,
    pub playlist_items_snapshot_service: Arc<dyn PlaylistItemsSnapshotService>,
    pub room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
}

pub struct ResourceWatchSession {
    room_id: RoomId,
    principal: RealtimePrincipal,
    user_id: UserId,
    connection_id: ConnectionId,
    room_service: Arc<RoomService>,
    chat_service: Option<Arc<ChatService>>,
    event_service: Arc<dyn RealtimeEventService>,
    connection_service: Arc<dyn ConnectionRuntime>,
    public_id_codec: Arc<crate::public_id::PublicIdCodec>,
    resource_observer: Arc<ResourceObserver>,
}

pub struct PreparedResourceWatchSession {
    session: ResourceWatchSession,
    event_rx: tokio::sync::mpsc::Receiver<SharedRealtimeEvent>,
}

impl ResourceWatchSession {
    pub fn new(config: ResourceWatchSessionConfig) -> Self {
        let ResourceWatchSessionConfig {
            room_id,
            principal,
            room_service,
            chat_service,
            clock,
            event_service,
            connection_service,
            presence_service,
            public_id_codec,
            sender,
            playback_service,
            playlist_items_snapshot_service,
            room_settings_snapshot_service,
        } = config;
        let user_id = principal.connection_user_id();
        let connection_id = generate_resource_watch_connection_id();
        let observer = ResourceObserver::new(ResourceObserverParams {
            room_id,
            user_id,
            actor: principal.room_actor(room_id),
            connection_id: connection_id.as_str().to_string(),
            room_service: Arc::clone(&room_service),
            chat_service: chat_service.clone(),
            clock,
            presence_service: Arc::clone(&presence_service),
            public_id_codec: Arc::clone(&public_id_codec),
            sender,
            playback_service,
            playlist_items_snapshot_service,
            room_settings_snapshot_service,
        });
        let observer = Arc::new(observer);

        Self {
            room_id,
            principal,
            user_id,
            connection_id,
            room_service,
            chat_service,
            event_service,
            connection_service,
            public_id_codec,
            resource_observer: observer,
        }
    }

    pub async fn prepare(
        self,
        observe: &ObserveResource,
    ) -> Result<PreparedResourceWatchSession, RealtimeJoinError> {
        self.connection_service
            .register_actor(
                self.connection_id.clone().into_string(),
                self.user_id,
                self.public_actor_id().map_err(RealtimeJoinError::from)?,
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "Failed to register resource watch connection");
                RealtimeJoinError::from(
                    crate::runtime::RealtimeAdmissionError::from_runtime_message(error),
                )
            })?;

        if let Err(error) = self
            .connection_service
            .join_room(self.connection_id.as_str(), self.room_id)
            .await
        {
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(error),
            ));
        }

        if let Err(error) = self.ensure_realtime_room_access().await {
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        if let Err(error) = self.ensure_observe_resource_allowed(observe).await {
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(classify_realtime_join_error_message(error));
        }

        let event_rx = match self
            .event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map(|(event_rx, _connection_id)| event_rx)
        {
            Ok(event_rx) => event_rx,
            Err(error) => {
                self.connection_service
                    .unregister(self.connection_id.as_str())
                    .await;
                return Err(RealtimeJoinError::Internal(format!(
                    "Failed to subscribe to realtime events: {error}"
                )));
            }
        };

        let chat_replay_service = if matches!(
            observe.resource.as_ref(),
            Some(synctv_proto::client::observe_resource::Resource::ChatEvents(_))
        ) {
            if let Some(chat_service) = self.chat_service.as_ref() {
                Some(Arc::clone(chat_service))
            } else {
                self.event_service.unsubscribe(self.connection_id.as_str());
                self.connection_service
                    .unregister(self.connection_id.as_str())
                    .await;
                return Err(RealtimeJoinError::Internal(
                    "Chat service is not available".to_string(),
                ));
            }
        } else {
            None
        };

        if let Err(error) = self
            .resource_observer
            .handle_observe_resource(observe)
            .await
        {
            self.event_service.unsubscribe(self.connection_id.as_str());
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        if chat_replay_service.is_some() {
            if let Err(error) = self
                .resource_observer
                .replay_chat_events_after(observe)
                .await
            {
                self.event_service.unsubscribe(self.connection_id.as_str());
                self.connection_service
                    .unregister(self.connection_id.as_str())
                    .await;
                return Err(RealtimeJoinError::from(error));
            }
        }
        if let Err(error) = self
            .resource_observer
            .replay_room_resource_events_after(observe)
            .await
        {
            self.event_service.unsubscribe(self.connection_id.as_str());
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        Ok(PreparedResourceWatchSession {
            session: self,
            event_rx,
        })
    }

    fn public_actor_id(&self) -> Result<String, String> {
        self.principal.public_actor_id(&self.public_id_codec)
    }
}

impl PreparedResourceWatchSession {
    pub async fn run(
        self,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let Self {
            session,
            mut event_rx,
        } = self;
        let mut disconnect_rx = session.connection_service.subscribe_disconnect();
        let mut admin_rx = session.event_service.subscribe_admin_events();

        let result = async {
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => break Ok(()),
                    event = event_rx.recv() => {
                        let Some(event) = event else {
                            break Err("Realtime event channel closed".to_string());
                        };
                        if watch_admin_event_matches(&event, &session.user_id, &session.room_id) {
                            tracing::info!(
                                user_id = %session.user_id,
                                room_id = %session.room_id,
                                "Resource watch terminating after room access event"
                            );
                            break Ok(());
                        }
                        if let Err(error) = session
                            .resource_observer
                            .room_hub
                            .refresh_for_room_event(&event, Some(session.connection_id.as_str()))
                            .await
                        {
                            break Err(error);
                        }
                    }
                    signal = disconnect_rx.recv() => {
                        match signal {
                            Ok(signal) => {
                                if watch_disconnect_signal_matches(
                                    &signal,
                                    &session.user_id,
                                    &session.room_id,
                                    session.connection_id.as_str(),
                                ) {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        connection_id = %session.connection_id,
                                        "Resource watch terminating after disconnect signal"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %session.user_id,
                                    room_id = %session.room_id,
                                    "Resource watch disconnect signal channel lagged, re-subscribing and verifying access"
                                );
                                disconnect_rx = session.connection_service.subscribe_disconnect();
                                if let Err(reason) = session.ensure_realtime_room_access().await {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        reason,
                                        "Resource watch access is no longer valid after disconnect signal lag"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break Err("Disconnect signal channel closed".to_string());
                            }
                        }
                    }
                    admin_event = admin_rx.recv() => {
                        match admin_event {
                            Ok(RealtimeEvent::ProviderCredentialChanged { ref event_id, ref user_id, ref provider, ref server_id, .. }) => {
                                session.resource_observer
                                    .handle_provider_credential_changed_admin_event(
                                        event_id,
                                        user_id,
                                        provider,
                                        server_id,
                                    )
                                    .await;
                            }
                            Ok(RealtimeEvent::CacheInvalidate { ref event_id, ref targets, .. }) => {
                                session.resource_observer
                                    .handle_cache_invalidate_admin_event(event_id, targets)
                                    .await;
                            }
                            Ok(event) => {
                                if watch_admin_event_matches(&event, &session.user_id, &session.room_id) {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        "Resource watch terminating after admin event"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %session.user_id,
                                    room_id = %session.room_id,
                                    "Resource watch admin event channel lagged, re-subscribing and verifying access"
                                );
                                admin_rx = session.event_service.subscribe_admin_events();
                                if let Err(reason) = session.ensure_realtime_room_access().await {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        reason,
                                        "Resource watch access is no longer valid after admin event lag"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break Err("Admin event channel closed".to_string());
                            }
                        }
                    }
                    () = async {
                        match session
                            .resource_observer
                            .next_expired_resource_refresh_deadline()
                            .await {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        if let Err(error) = session
                            .resource_observer
                            .refresh_expired_resource_observations()
                            .await
                        {
                            break Err(error);
                        }
                    }
                }
            }
        }
        .await;

        session.resource_observer.clear_observations().await;
        session
            .event_service
            .unsubscribe(session.connection_id.as_str());
        session
            .connection_service
            .unregister(session.connection_id.as_str())
            .await;
        result
    }
}

impl ResourceWatchSession {
    async fn ensure_realtime_room_access(&self) -> Result<(), String> {
        let room = self
            .room_service
            .get_room(&self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        if room.is_banned {
            return Err("This room has been banned".to_string());
        }
        if room.status.is_closed() {
            return Err("This room is closed and not accepting new connections".to_string());
        }
        if self.principal.is_guest() {
            return self.ensure_guest_admission_for_action().await;
        }
        match RealtimeMembershipProbe::new(&self.room_service)
            .probe_realtime_membership_access_with_room(&room, &self.user_id)
            .await
        {
            Ok(RealtimeMembershipAccess::Allowed(_)) => Ok(()),
            Ok(RealtimeMembershipAccess::Denied(reason)) => Err(reason),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn ensure_guest_admission_for_action(&self) -> Result<(), String> {
        match RealtimeMembershipProbe::new(&self.room_service)
            .guest_admission_denial_reason(&self.room_id, &self.user_id, &self.principal)
            .await
        {
            Ok(Some(reason)) => Err(reason),
            Ok(None) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn check_realtime_permission(&self, permission: RoomPermission) -> Result<(), String> {
        if self.principal.is_guest() {
            let permissions = self
                .room_service
                .get_guest_permissions(&self.room_id)
                .await
                .map_err(|error| error.to_string())?;
            if permissions.has(permission) {
                Ok(())
            } else {
                Err("Guests do not have permission to perform this action".to_string())
            }
        } else {
            self.room_service
                .check_permission(&self.room_id, &self.user_id, permission)
                .await
                .map_err(|error| error.to_string())
        }
    }

    async fn ensure_observe_resource_allowed(
        &self,
        observe: &ObserveResource,
    ) -> Result<(), String> {
        let Some(resource) = observe.resource.as_ref() else {
            return Err("observe resource is required".to_string());
        };

        match resource {
            synctv_proto::client::observe_resource::Resource::PlaybackState(_)
            | synctv_proto::client::observe_resource::Resource::RoomSettings(_)
            | synctv_proto::client::observe_resource::Resource::OnlineCount(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                Ok(())
            }
            synctv_proto::client::observe_resource::Resource::PlaylistItems(_) => {
                if self.principal.is_guest() {
                    Err("Guests cannot observe playlist items".to_string())
                } else {
                    Ok(())
                }
            }
            synctv_proto::client::observe_resource::Resource::RoomMemberEvents(_)
            | synctv_proto::client::observe_resource::Resource::OnlineEvent(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_MEMBER_LIST)
                    .await
            }
            synctv_proto::client::observe_resource::Resource::SelfRoomMember(_) => {
                if self.principal.is_guest() {
                    return Err("Guests do not have a room member permission snapshot".to_string());
                }
                Ok(())
            }
            synctv_proto::client::observe_resource::Resource::ChatEvents(_)
            | synctv_proto::client::observe_resource::Resource::ChatPinEvents(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_CHAT_HISTORY)
                    .await
            }
            synctv_proto::client::observe_resource::Resource::Playback(_) => {
                if self.principal.is_guest() {
                    Err(
                        "Guests cannot observe playbacks because playbacks may depend on signed-in provider credentials"
                            .to_string(),
                    )
                } else {
                    Ok(())
                }
            }
        }
    }
}

pub fn watch_playback_state_observe(
    req: synctv_proto::client::WatchPlaybackStateRequest,
) -> Result<ObserveResource, String> {
    let playback_state = req
        .playback_state
        .ok_or_else(|| "playback_state watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::PlaybackState,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::PlaybackState(playback_state),
    )
}

pub fn watch_playback_observe(
    req: synctv_proto::client::WatchPlaybackRequest,
) -> Result<ObserveResource, String> {
    let playback = req
        .playback
        .ok_or_else(|| "playback watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::Playback,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::Playback(playback),
    )
}

pub fn watch_room_settings_observe(
    req: synctv_proto::client::WatchRoomSettingsRequest,
) -> Result<ObserveResource, String> {
    let room_settings = req
        .room_settings
        .ok_or_else(|| "room_settings watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::RoomSettings,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::RoomSettings(room_settings),
    )
}

pub fn watch_playlist_items_observe(
    req: synctv_proto::client::WatchPlaylistItemsRequest,
) -> Result<ObserveResource, String> {
    let playlist_items = req
        .playlist_items
        .ok_or_else(|| "playlist_items watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::PlaylistItems,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::PlaylistItems(playlist_items),
    )
}

pub fn watch_room_member_events_observe(
    req: synctv_proto::client::WatchRoomMemberEventsRequest,
) -> Result<ObserveResource, String> {
    let room_member_events = req.room_member_events.unwrap_or_default();
    build_watch_observe(
        WatchResourceKind::RoomMemberEvents,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::RoomMemberEvents(room_member_events),
    )
}

pub fn watch_chat_events_observe(
    req: synctv_proto::client::WatchChatEventsRequest,
) -> Result<ObserveResource, String> {
    let chat_events = req.chat_events.unwrap_or_default();
    build_watch_observe(
        WatchResourceKind::ChatEvents,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::ChatEvents(chat_events),
    )
}

pub fn watch_chat_pin_events_observe(
    req: synctv_proto::client::WatchChatPinEventsRequest,
) -> Result<ObserveResource, String> {
    let chat_pin_events = req.chat_pin_events.unwrap_or_default();
    build_watch_observe(
        WatchResourceKind::ChatPinEvents,
        req.delivery_mode,
        synctv_proto::client::observe_resource::Resource::ChatPinEvents(chat_pin_events),
    )
}

fn build_watch_observe(
    kind: WatchResourceKind,
    delivery_mode: i32,
    resource: synctv_proto::client::observe_resource::Resource,
) -> Result<ObserveResource, String> {
    Ok(ObserveResource {
        observe_id: kind.observe_id().to_string(),
        delivery_mode,
        resource: Some(resource),
    })
}

fn generate_resource_watch_connection_id() -> ConnectionId {
    ConnectionId::new(format!("conn_c{}", synctv_common::snanoid!(16)))
}
