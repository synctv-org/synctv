use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use dashmap::DashMap;
use subtle::ConstantTimeEq;
use synctv_xiu::{
    rtmp::{
        auth::{AuthCallback, PublishAuthError, RtmpStreamMode},
        session::common::RtmpStreamHandler,
    },
    streamhub::{
        define::{
            NotifyInfo, PubDataType, PublishType, PublisherInfo, StreamHubEvent,
            StreamHubEventSender, SubDataType, SubscribeType, SubscriberInfo,
        },
        errors::StreamHubErrorValue,
        send_event_with_backpressure_timeout_for,
        stream::StreamIdentifier,
        utils::Uuid,
    },
    webrtc::{create_whep_session, create_whip_session, PeerSession, WebRtcConfig, WebRtcError},
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    api::tracker::StreamSubscriberGuard,
    error::StreamError,
    relay::{StreamRegistryTrait, WebRtcSessionKind, WebRtcSessionOwner},
};

const STREAMHUB_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const STREAMHUB_RESULT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct WebRtcAnswer {
    pub session_id: String,
    pub answer_sdp: String,
}

#[derive(Debug, Clone)]
pub struct WebRtcSessionConfig {
    pub enabled: bool,
    pub peer: WebRtcConfig,
    pub max_sessions: usize,
    pub max_session_duration: Duration,
}

impl Default for WebRtcSessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            peer: WebRtcConfig::default(),
            max_sessions: 1_000,
            max_session_duration: Duration::from_hours(24),
        }
    }
}

enum SessionKind {
    Whip {
        generation_id: Uuid,
        room_id: String,
        media_id: String,
        auth_query: String,
    },
    Whep {
        room_id: String,
        media_id: String,
        subscriber: SubscriberInfo,
    },
}

impl SessionKind {
    fn owner(&self, node_id: &str, cluster_address: &str) -> WebRtcSessionOwner {
        let (room_id, media_id, kind) = match self {
            Self::Whip {
                room_id, media_id, ..
            } => (room_id, media_id, WebRtcSessionKind::Whip),
            Self::Whep {
                room_id, media_id, ..
            } => (room_id, media_id, WebRtcSessionKind::Whep),
        };
        WebRtcSessionOwner {
            node_id: node_id.to_string(),
            cluster_address: cluster_address.to_string(),
            room_id: room_id.clone(),
            media_id: media_id.clone(),
            kind,
        }
    }
}

#[derive(Clone)]
struct WebRtcSessionDirectory {
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    cluster_address: String,
}

struct ManagedSession {
    peer: PeerSession,
    kind: SessionKind,
    cleanup_started: AtomicBool,
    stream_guard: parking_lot::Mutex<Option<StreamSubscriberGuard>>,
    _capacity_permit: OwnedSemaphorePermit,
}

impl ManagedSession {
    fn new(
        peer: PeerSession,
        kind: SessionKind,
        stream_guard: Option<StreamSubscriberGuard>,
        capacity_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            peer,
            kind,
            cleanup_started: AtomicBool::new(false),
            stream_guard: parking_lot::Mutex::new(stream_guard),
            _capacity_permit: capacity_permit,
        }
    }
}

pub struct WebRtcSessionManager {
    event_sender: StreamHubEventSender,
    auth: Option<Arc<dyn AuthCallback>>,
    config: WebRtcSessionConfig,
    sessions: Arc<DashMap<Uuid, Arc<ManagedSession>>>,
    capacity: Arc<Semaphore>,
    cancel_token: CancellationToken,
    session_directory: Option<WebRtcSessionDirectory>,
}

impl WebRtcSessionManager {
    #[must_use]
    pub fn new(
        event_sender: StreamHubEventSender,
        auth: Option<Arc<dyn AuthCallback>>,
        config: WebRtcSessionConfig,
    ) -> Self {
        let max_sessions = config.max_sessions;
        Self {
            event_sender,
            auth,
            config,
            sessions: Arc::new(DashMap::new()),
            capacity: Arc::new(Semaphore::new(max_sessions)),
            cancel_token: CancellationToken::new(),
            session_directory: None,
        }
    }

    #[must_use]
    pub(crate) fn with_session_directory(
        mut self,
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        cluster_address: String,
    ) -> Self {
        self.session_directory = Some(WebRtcSessionDirectory {
            registry,
            node_id,
            cluster_address,
        });
        self
    }

    #[must_use]
    pub fn active_session_count(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.config.enabled
    }

    #[must_use]
    pub const fn max_sdp_bytes(&self) -> usize {
        self.config.peer.max_sdp_bytes
    }

    fn map_peer_error(error: &WebRtcError) -> StreamError {
        match error {
            WebRtcError::EmptySdp
            | WebRtcError::SdpTooLarge { .. }
            | WebRtcError::InvalidSdp(_)
            | WebRtcError::IncompatibleWhipVideoCodec
            | WebRtcError::NoCompatibleWhipMedia => StreamError::InvalidInput(error.to_string()),
            WebRtcError::Negotiation(_)
            | WebRtcError::IceGatheringTimeout(_)
            | WebRtcError::MissingLocalDescription => {
                StreamError::ConnectionFailed(error.to_string())
            }
        }
    }

    fn acquire_capacity(&self) -> Result<OwnedSemaphorePermit, StreamError> {
        Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| StreamError::ResourceExhausted("WebRTC session limit reached".to_string()))
    }

    async fn send_event(&self, event: StreamHubEvent) -> Result<(), StreamError> {
        send_event_with_backpressure_timeout_for(&self.event_sender, event, STREAMHUB_EVENT_TIMEOUT)
            .await
            .map_err(|error| StreamError::StreamHubError(error.to_string()))
    }

    async fn publish_stream(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
        remote_addr: &str,
    ) -> Result<
        (
            synctv_xiu::streamhub::define::FrameDataSender,
            synctv_xiu::streamhub::define::PacketDataSender,
        ),
        StreamError,
    > {
        let (result_sender, result_receiver) = oneshot::channel();
        self.send_event(StreamHubEvent::Publish {
            identifier: StreamIdentifier::Rtmp {
                app_name: room_id.to_string(),
                stream_name: media_id.to_string(),
            },
            info: PublisherInfo {
                id: generation_id,
                pub_type: PublishType::WhipPush,
                pub_data_type: PubDataType::Both,
                notify_info: NotifyInfo {
                    request_url: format!("whip://{room_id}/{media_id}"),
                    remote_addr: remote_addr.to_string(),
                },
            },
            result_sender,
            stream_handler: Arc::new(RtmpStreamHandler::new()),
        })
        .await?;
        let (frame_sender, packet_sender, _) =
            tokio::time::timeout(STREAMHUB_RESULT_TIMEOUT, result_receiver)
                .await
                .map_err(|_| StreamError::StreamHubError("WebRTC publish timed out".to_string()))?
                .map_err(|_| {
                    StreamError::StreamHubError("WebRTC publish result channel closed".to_string())
                })?
                .map_err(|error| StreamError::StreamHubError(error.to_string()))?;
        Ok((
            frame_sender.ok_or_else(|| {
                StreamError::StreamHubError("WebRTC publish returned no frame sender".to_string())
            })?,
            packet_sender.ok_or_else(|| {
                StreamError::StreamHubError("WebRTC publish returned no packet sender".to_string())
            })?,
        ))
    }

    async fn unpublish_stream(&self, room_id: &str, media_id: &str, generation_id: Uuid) {
        if let Err(error) = self
            .send_event(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: room_id.to_string(),
                    stream_name: media_id.to_string(),
                },
                generation_id,
            })
            .await
        {
            warn!(room_id, media_id, %generation_id, %error, "failed to unpublish WHIP stream");
        }
    }

    async fn authenticate_publish(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
        token: &str,
    ) -> Result<(String, String, String, RtmpStreamMode), StreamError> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            StreamError::PermissionDenied("WHIP publishing is not configured".to_string())
        })?;
        let auth_query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", token)
            .finish();
        let rewrite = auth
            .on_publish(generation_id, room_id, media_id, Some(&auth_query))
            .await
            .map_err(|error| {
                if let Some(auth_error) = error.downcast_ref::<PublishAuthError>() {
                    StreamError::Authentication(auth_error.to_string())
                } else {
                    StreamError::PermissionDenied(error.to_string())
                }
            })?;
        let (room_id, media_id, media_mode) = rewrite.map_or_else(
            || {
                (
                    room_id.to_string(),
                    media_id.to_string(),
                    RtmpStreamMode::Default,
                )
            },
            |rewrite| (rewrite.app_name, rewrite.stream_name, rewrite.media_mode),
        );
        Ok((room_id, media_id, auth_query, media_mode))
    }

    async fn rollback_publish_auth(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
        auth_query: &str,
    ) {
        if let Some(auth) = &self.auth {
            auth.on_publish_rollback(generation_id, room_id, media_id, Some(auth_query))
                .await;
        }
    }

    async fn enable_publisher_rtp_capability(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
    ) -> Result<(), StreamError> {
        let Some(directory) = &self.session_directory else {
            return Ok(());
        };
        let generation = directory
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|error| StreamError::RegistryError(error.to_string()))?
            .ok_or_else(|| {
                StreamError::InvalidState(
                    "WHIP publisher registration disappeared during authentication".to_string(),
                )
            })?;
        if generation.generation_id != generation_id.to_string() {
            return Err(StreamError::InvalidState(
                "WHIP publisher ownership changed during authentication".to_string(),
            ));
        }
        let updated = directory
            .registry
            .set_generation_supports_rtp(
                room_id,
                media_id,
                &generation.generation_id,
                generation.lease_epoch,
                true,
            )
            .await
            .map_err(|error| StreamError::RegistryError(error.to_string()))?;
        if !updated {
            return Err(StreamError::InvalidState(
                "WHIP publisher ownership changed before RTP capability update".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn publish_whip(
        &self,
        public_room_id: &str,
        public_media_id: &str,
        token: &str,
        offer_sdp: &str,
        remote_addr: &str,
    ) -> Result<WebRtcAnswer, StreamError> {
        if !self.config.enabled {
            return Err(StreamError::InvalidState(
                "WebRTC livestreaming is disabled".to_string(),
            ));
        }
        if self.cancel_token.is_cancelled() {
            return Err(StreamError::InvalidState(
                "WebRTC session manager is shutting down".to_string(),
            ));
        }
        let capacity_permit = self.acquire_capacity()?;
        let generation_id = Uuid::new();
        let (room_id, media_id, auth_query, media_mode) = self
            .authenticate_publish(generation_id, public_room_id, public_media_id, token)
            .await?;
        if let Err(error) = self
            .enable_publisher_rtp_capability(generation_id, &room_id, &media_id)
            .await
        {
            self.rollback_publish_auth(generation_id, &room_id, &media_id, &auth_query)
                .await;
            return Err(error);
        }
        let (frame_sender, packet_sender) = match self
            .publish_stream(generation_id, &room_id, &media_id, remote_addr)
            .await
        {
            Ok(senders) => senders,
            Err(error) => {
                self.rollback_publish_auth(generation_id, &room_id, &media_id, &auth_query)
                    .await;
                return Err(error);
            }
        };
        let peer = match create_whip_session(
            offer_sdp,
            frame_sender,
            packet_sender,
            media_mode,
            &self.config.peer,
        )
        .await
        {
            Ok(peer) => peer,
            Err(error) => {
                self.unpublish_stream(&room_id, &media_id, generation_id)
                    .await;
                self.rollback_publish_auth(generation_id, &room_id, &media_id, &auth_query)
                    .await;
                return Err(Self::map_peer_error(&error));
            }
        };
        let answer_sdp = peer.answer_sdp.clone();
        let session = Arc::new(ManagedSession::new(
            peer,
            SessionKind::Whip {
                generation_id,
                room_id,
                media_id,
                auth_query,
            },
            None,
            capacity_permit,
        ));
        if let Err(error) = self
            .insert_session(generation_id, Arc::clone(&session))
            .await
        {
            Self::cleanup_session(
                &self.event_sender,
                self.auth.as_ref(),
                &session,
                generation_id,
                self.session_directory.as_ref(),
            )
            .await;
            return Err(error);
        }
        Ok(WebRtcAnswer {
            session_id: generation_id.to_string(),
            answer_sdp,
        })
    }

    async fn subscribe_stream(
        &self,
        session_id: Uuid,
        room_id: &str,
        media_id: &str,
        remote_addr: &str,
    ) -> Result<
        (
            synctv_xiu::streamhub::define::PacketDataReceiver,
            SubscriberInfo,
        ),
        StreamError,
    > {
        let subscriber = SubscriberInfo {
            id: session_id,
            sub_type: SubscribeType::WhepPull,
            sub_data_type: SubDataType::Packet,
            notify_info: NotifyInfo {
                request_url: format!("whep://{room_id}/{media_id}"),
                remote_addr: remote_addr.to_string(),
            },
        };
        let (result_sender, result_receiver) = oneshot::channel();
        self.send_event(StreamHubEvent::Subscribe {
            identifier: StreamIdentifier::Rtmp {
                app_name: room_id.to_string(),
                stream_name: media_id.to_string(),
            },
            info: subscriber.clone(),
            result_sender,
        })
        .await?;
        let (receiver, _) = tokio::time::timeout(STREAMHUB_RESULT_TIMEOUT, result_receiver)
            .await
            .map_err(|_| StreamError::StreamHubError("WHEP subscribe timed out".to_string()))?
            .map_err(|_| {
                StreamError::StreamHubError("WHEP subscribe result channel closed".to_string())
            })?
            .map_err(|error| match error.value {
                StreamHubErrorValue::NotCorrectDataSenderType => StreamError::InvalidState(
                    "Active stream does not provide RTP packets for WHEP".to_string(),
                ),
                other => StreamError::StreamHubError(other.to_string()),
            })?;
        let packet_receiver = receiver.packet_receiver.ok_or_else(|| {
            StreamError::InvalidState(
                "Active stream does not provide RTP packets for WHEP".to_string(),
            )
        })?;
        Ok((packet_receiver, subscriber))
    }

    pub async fn play_whep(
        &self,
        room_id: &str,
        media_id: &str,
        offer_sdp: &str,
        remote_addr: &str,
        stream_guard: StreamSubscriberGuard,
    ) -> Result<WebRtcAnswer, StreamError> {
        if !self.config.enabled {
            return Err(StreamError::InvalidState(
                "WebRTC livestreaming is disabled".to_string(),
            ));
        }
        if self.cancel_token.is_cancelled() {
            return Err(StreamError::InvalidState(
                "WebRTC session manager is shutting down".to_string(),
            ));
        }
        let capacity_permit = self.acquire_capacity()?;
        let session_id = Uuid::new();
        let (packet_receiver, subscriber) = self
            .subscribe_stream(session_id, room_id, media_id, remote_addr)
            .await?;
        let peer = match create_whep_session(offer_sdp, packet_receiver, &self.config.peer).await {
            Ok(peer) => peer,
            Err(error) => {
                self.unsubscribe_stream(room_id, media_id, subscriber).await;
                return Err(Self::map_peer_error(&error));
            }
        };
        let answer_sdp = peer.answer_sdp.clone();
        let session = Arc::new(ManagedSession::new(
            peer,
            SessionKind::Whep {
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
                subscriber,
            },
            Some(stream_guard),
            capacity_permit,
        ));
        if let Err(error) = self.insert_session(session_id, Arc::clone(&session)).await {
            Self::cleanup_session(
                &self.event_sender,
                self.auth.as_ref(),
                &session,
                session_id,
                self.session_directory.as_ref(),
            )
            .await;
            return Err(error);
        }
        Ok(WebRtcAnswer {
            session_id: session_id.to_string(),
            answer_sdp,
        })
    }

    async fn unsubscribe_stream(&self, room_id: &str, media_id: &str, subscriber: SubscriberInfo) {
        if let Err(error) = self
            .send_event(StreamHubEvent::UnSubscribe {
                identifier: StreamIdentifier::Rtmp {
                    app_name: room_id.to_string(),
                    stream_name: media_id.to_string(),
                },
                info: subscriber,
            })
            .await
        {
            debug!(room_id, media_id, %error, "failed to unsubscribe WHEP session");
        }
    }

    async fn insert_session(
        &self,
        session_id: Uuid,
        session: Arc<ManagedSession>,
    ) -> Result<(), StreamError> {
        if let Some(directory) = &self.session_directory {
            let owner = session
                .kind
                .owner(&directory.node_id, &directory.cluster_address);
            let registered = directory
                .registry
                .try_register_webrtc_session(
                    &session_id.to_string(),
                    &owner,
                    self.config.max_session_duration,
                )
                .await
                .map_err(|error| StreamError::RegistryError(error.to_string()))?;
            if !registered {
                return Err(StreamError::Internal(
                    "WebRTC session identifier collision".to_string(),
                ));
            }
        }
        if self
            .sessions
            .insert(session_id, Arc::clone(&session))
            .is_some()
        {
            return Err(StreamError::Internal(
                "WebRTC session identifier collision".to_string(),
            ));
        }
        let sessions = Arc::clone(&self.sessions);
        let event_sender = self.event_sender.clone();
        let auth = self.auth.clone();
        let manager_cancel = self.cancel_token.clone();
        let ttl = self.config.max_session_duration;
        let session_directory = self.session_directory.clone();
        tokio::spawn(async move {
            let peer_cancel = session.peer.cancellation_token();
            tokio::select! {
                () = peer_cancel.cancelled() => {}
                () = manager_cancel.cancelled() => {}
                () = tokio::time::sleep(ttl) => {}
            }
            sessions.remove(&session_id);
            Self::cleanup_session(
                &event_sender,
                auth.as_ref(),
                &session,
                session_id,
                session_directory.as_ref(),
            )
            .await;
        });
        Ok(())
    }

    async fn cleanup_session(
        event_sender: &StreamHubEventSender,
        auth: Option<&Arc<dyn AuthCallback>>,
        session: &ManagedSession,
        session_id: Uuid,
        session_directory: Option<&WebRtcSessionDirectory>,
    ) {
        if session.cleanup_started.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = session.peer.close().await {
            debug!(%error, "WebRTC peer close returned an error");
        }
        match &session.kind {
            SessionKind::Whip {
                generation_id,
                room_id,
                media_id,
                auth_query,
            } => {
                let _ = send_event_with_backpressure_timeout_for(
                    event_sender,
                    StreamHubEvent::UnPublish {
                        identifier: StreamIdentifier::Rtmp {
                            app_name: room_id.clone(),
                            stream_name: media_id.clone(),
                        },
                        generation_id: *generation_id,
                    },
                    STREAMHUB_EVENT_TIMEOUT,
                )
                .await;
                if let Some(auth) = auth {
                    auth.on_unpublish(*generation_id, room_id, media_id, Some(auth_query))
                        .await;
                }
            }
            SessionKind::Whep {
                room_id,
                media_id,
                subscriber,
            } => {
                let _ = send_event_with_backpressure_timeout_for(
                    event_sender,
                    StreamHubEvent::UnSubscribe {
                        identifier: StreamIdentifier::Rtmp {
                            app_name: room_id.clone(),
                            stream_name: media_id.clone(),
                        },
                        info: subscriber.clone(),
                    },
                    STREAMHUB_EVENT_TIMEOUT,
                )
                .await;
            }
        }
        session.stream_guard.lock().take();
        if let Some(directory) = session_directory {
            if let Err(error) = directory
                .registry
                .unregister_webrtc_session(&session_id.to_string(), &directory.node_id)
                .await
            {
                warn!(%session_id, %error, "failed to unregister WebRTC session owner");
            }
        }
    }

    fn parse_session_id(session_id: &str) -> Result<Uuid, StreamError> {
        Uuid::parse_str(session_id)
            .map_err(|_| StreamError::InvalidInput("Invalid WebRTC session identifier".to_string()))
    }

    fn session(&self, session_id: Uuid) -> Option<Arc<ManagedSession>> {
        self.sessions
            .get(&session_id)
            .map(|entry| Arc::clone(entry.value()))
    }

    async fn remove_session(
        &self,
        session_id: Uuid,
        expected: &Arc<ManagedSession>,
    ) -> Result<bool, StreamError> {
        let Some((_, session)) = self.sessions.remove(&session_id) else {
            return Ok(false);
        };
        if !Arc::ptr_eq(&session, expected) {
            self.sessions.insert(session_id, session);
            return Ok(false);
        }
        Self::cleanup_session(
            &self.event_sender,
            self.auth.as_ref(),
            &session,
            session_id,
            self.session_directory.as_ref(),
        )
        .await;
        Ok(true)
    }

    pub async fn delete_whip_session(
        &self,
        session_id: &str,
        room_id: &str,
        media_id: &str,
        token: &str,
    ) -> Result<bool, StreamError> {
        let session_id = Self::parse_session_id(session_id)?;
        let Some(session) = self.session(session_id) else {
            return Ok(false);
        };
        let SessionKind::Whip {
            room_id: session_room_id,
            media_id: session_media_id,
            auth_query,
            ..
        } = &session.kind
        else {
            return Ok(false);
        };
        if session_room_id != room_id || session_media_id != media_id {
            return Ok(false);
        }
        let supplied_auth_query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", token)
            .finish();
        if !bool::from(auth_query.as_bytes().ct_eq(supplied_auth_query.as_bytes())) {
            return Err(StreamError::Authentication(
                "WHIP session credentials do not match".to_string(),
            ));
        }
        self.remove_session(session_id, &session).await
    }

    pub async fn delete_whep_session(
        &self,
        session_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<bool, StreamError> {
        let session_id = Self::parse_session_id(session_id)?;
        let Some(session) = self.session(session_id) else {
            return Ok(false);
        };
        let SessionKind::Whep {
            room_id: session_room_id,
            media_id: session_media_id,
            ..
        } = &session.kind
        else {
            return Ok(false);
        };
        if session_room_id != room_id || session_media_id != media_id {
            return Ok(false);
        }
        self.remove_session(session_id, &session).await
    }

    pub async fn shutdown(&self) {
        self.cancel_token.cancel();
        let sessions: Vec<_> = self
            .sessions
            .iter()
            .map(|entry| (*entry.key(), Arc::clone(entry.value())))
            .collect();
        self.sessions.clear();
        for (session_id, session) in sessions {
            Self::cleanup_session(
                &self.event_sender,
                self.auth.as_ref(),
                &session,
                session_id,
                self.session_directory.as_ref(),
            )
            .await;
        }
    }
}

impl Drop for WebRtcSessionManager {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct VideoOnlyAuth;

    struct InvalidPublishKeyAuth;

    #[async_trait::async_trait]
    impl AuthCallback for VideoOnlyAuth {
        async fn on_publish(
            &self,
            _generation_id: Uuid,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<
            Option<synctv_xiu::rtmp::auth::AuthPublishRewrite>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            Ok(Some(synctv_xiu::rtmp::auth::AuthPublishRewrite {
                app_name: "canonical-room".to_string(),
                stream_name: "canonical-media".to_string(),
                media_mode: RtmpStreamMode::VideoOnly,
            }))
        }

        async fn on_play(
            &self,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl AuthCallback for InvalidPublishKeyAuth {
        async fn on_publish(
            &self,
            _generation_id: Uuid,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<
            Option<synctv_xiu::rtmp::auth::AuthPublishRewrite>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            Err(Box::new(PublishAuthError::new("invalid publish key")))
        }

        async fn on_play(
            &self,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    #[test]
    fn capacity_is_bounded() {
        let (event_sender, _) = tokio::sync::mpsc::channel(1);
        let manager = WebRtcSessionManager::new(
            event_sender,
            None,
            WebRtcSessionConfig {
                max_sessions: 1,
                ..WebRtcSessionConfig::default()
            },
        );
        let permit = manager.acquire_capacity().expect("first permit should fit");
        assert!(matches!(
            manager.acquire_capacity(),
            Err(StreamError::ResourceExhausted(_))
        ));
        drop(permit);
        assert!(manager.acquire_capacity().is_ok());
    }

    #[tokio::test]
    async fn publish_auth_preserves_rewrite_media_mode() {
        let (event_sender, _) = tokio::sync::mpsc::channel(1);
        let manager = WebRtcSessionManager::new(
            event_sender,
            Some(Arc::new(VideoOnlyAuth)),
            WebRtcSessionConfig::default(),
        );

        let (room_id, media_id, auth_query, media_mode) = manager
            .authenticate_publish(Uuid::new(), "public-room", "public-media", "secret")
            .await
            .expect("publish authentication should succeed");

        assert_eq!(room_id, "canonical-room");
        assert_eq!(media_id, "canonical-media");
        assert_eq!(auth_query, "token=secret");
        assert_eq!(media_mode, RtmpStreamMode::VideoOnly);
    }

    #[tokio::test]
    async fn publish_key_errors_remain_authentication_failures() {
        let (event_sender, _) = tokio::sync::mpsc::channel(1);
        let manager = WebRtcSessionManager::new(
            event_sender,
            Some(Arc::new(InvalidPublishKeyAuth)),
            WebRtcSessionConfig::default(),
        );

        let error = manager
            .authenticate_publish(Uuid::new(), "public-room", "public-media", "invalid")
            .await
            .expect_err("invalid publish key should fail authentication");
        assert!(matches!(error, StreamError::Authentication(_)));
    }
}
