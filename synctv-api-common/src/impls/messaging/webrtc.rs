use synctv_core::models::RoomPermission;
use synctv_proto::client::web_rtc_command::Command;
use synctv_realtime::fanout::RealtimeDeliveryRequirement;
use synctv_realtime::sync::{RealtimeEvent, VoiceRtcJoinOutcome, WebRTCSignalKind};

use super::{
    is_private_ice_candidate_ip, StreamMessageHandler, MAX_ICE_CANDIDATE_SIZE, MAX_SDP_SIZE,
};

const MAX_MEDIA_SWARM_ID_BYTES: usize = 128;
const MAX_ACTIVE_MEDIA_SWARMS_PER_CONNECTION: usize = 8;
const VOICE_CHAT_DISABLED_MESSAGE: &str = "Voice chat is disabled for this room";
const P2P_MEDIA_DISABLED_MESSAGE: &str = "P2P media is disabled for this room";

fn normalize_media_swarm_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_MEDIA_SWARM_ID_BYTES || !value.is_ascii() {
        return Err(format!(
            "WebRTC media swarm id must be non-empty ASCII and at most {MAX_MEDIA_SWARM_ID_BYTES} bytes"
        ));
    }
    Ok(value.to_string())
}
impl StreamMessageHandler {
    async fn require_room_capability(
        &self,
        capability: impl FnOnce(&synctv_core::models::RoomSettings) -> bool,
        disabled_message: &'static str,
    ) -> Result<(), String> {
        let settings = self
            .room_service
            .get_room_settings(&self.room_id)
            .await
            .map_err(|error| format!("Room capability state unavailable: {error}"))?;
        if capability(&settings) {
            Ok(())
        } else {
            Err(disabled_message.to_string())
        }
    }

    async fn require_voice_chat_enabled(&self) -> Result<(), String> {
        self.require_room_capability(
            |settings| settings.voice_chat_enabled.0,
            VOICE_CHAT_DISABLED_MESSAGE,
        )
        .await
    }

    async fn require_p2p_media_enabled(&self) -> Result<(), String> {
        self.require_room_capability(
            |settings| settings.p2p_media_enabled.0,
            P2P_MEDIA_DISABLED_MESSAGE,
        )
        .await
    }

    fn max_voice_participants_per_room(&self) -> Result<usize, String> {
        let value = self.runtime_settings_store.as_ref().map_or(
            Ok(synctv_core::service::DEFAULT_MAX_VOICE_PARTICIPANTS_PER_ROOM),
            |store| store.webrtc.max_voice_participants_per_room.get(),
        );
        value
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
            .map_err(|error| format!("Voice chat runtime setting unavailable: {error}"))
    }

    pub async fn handle_webrtc_command(
        &self,
        command: &synctv_proto::client::WebRtcCommand,
    ) -> Result<(), String> {
        let _transition_guard = self.room_capability_transition_lock.lock().await;
        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        match command.command.as_ref() {
            Some(Command::VoiceOffer(offer)) => self.handle_webrtc_voice_offer(offer).await,
            Some(Command::VoiceAnswer(answer)) => self.handle_webrtc_voice_answer(answer).await,
            Some(Command::VoiceIceCandidate(candidate)) => {
                self.handle_webrtc_voice_ice_candidate(candidate).await
            }
            Some(Command::VoiceJoin(join)) => self.handle_webrtc_voice_join(join).await,
            Some(Command::VoiceLeave(_leave)) => self.leave_webrtc_voice_session().await,
            Some(Command::MediaOffer(offer)) => self.handle_webrtc_media_offer(offer).await,
            Some(Command::MediaAnswer(answer)) => self.handle_webrtc_media_answer(answer).await,
            Some(Command::MediaIceCandidate(candidate)) => {
                self.handle_webrtc_media_ice_candidate(candidate).await
            }
            Some(Command::MediaSwarmJoin(join)) => self.handle_media_swarm_join(join).await,
            Some(Command::MediaSwarmLeave(leave)) => self.handle_media_swarm_leave(leave).await,
            None => Err("Empty WebRTC command".to_string()),
        }
    }

    async fn require_rtc_business_permission(
        &self,
        permission: RoomPermission,
        business: &str,
    ) -> Result<(), String> {
        self.check_realtime_permission(permission)
            .await
            .map_err(|e| format!("{business} permission denied: {e}"))
    }

    async fn require_voice_signaling_permission(&self) -> Result<(), String> {
        self.require_rtc_business_permission(RoomPermission::USE_VOICE_CHAT, "Voice chat")
            .await?;
        self.require_voice_chat_enabled().await
    }

    async fn require_media_signaling_permission(&self) -> Result<(), String> {
        self.require_rtc_business_permission(RoomPermission::USE_P2P_MEDIA, "P2P media")
            .await?;
        self.require_p2p_media_enabled().await
    }

    async fn validate_webrtc_recipient(
        &self,
        recipient: &str,
    ) -> Result<Option<(String, String, bool)>, String> {
        let (target_actor_id, target_conn_id) = Self::parse_webrtc_recipient(recipient)?;
        self.connection_service
            .get_connection(self.connection_id.as_str())
            .ok_or_else(|| "Connection not found".to_string())?;
        let Some(target) = self
            .connection_service
            .get_connection_distributed(target_conn_id)
            .await?
        else {
            return Ok(None);
        };
        if target.actor_id != target_actor_id {
            return Err("WebRTC recipient does not match the target connection owner".to_string());
        }
        if target.room_id.as_ref() != Some(&self.room_id) {
            return Err("Target connection is not in this room".to_string());
        }
        Ok(Some((
            target.actor_id,
            target_conn_id.to_string(),
            target.voice_rtc_joined,
        )))
    }

    fn parse_webrtc_recipient(recipient: &str) -> Result<(&str, &str), String> {
        let (target_actor_id, target_conn_id) = recipient.rsplit_once(':').ok_or_else(|| {
            "WebRTC recipient must be formatted as public_actor_id:conn_id".to_string()
        })?;
        if target_actor_id.is_empty() || target_conn_id.is_empty() {
            return Err(
                "WebRTC recipient must be formatted as public_actor_id:conn_id".to_string(),
            );
        }
        Ok((target_actor_id, target_conn_id))
    }

    pub(super) async fn validate_webrtc_voice_recipient(
        &self,
        recipient: &str,
    ) -> Result<bool, String> {
        Self::parse_webrtc_recipient(recipient)?;
        let source = self
            .connection_service
            .get_connection(self.connection_id.as_str())
            .ok_or_else(|| "Connection not found".to_string())?;
        if !source.voice_rtc_joined {
            return Err("Source connection has not joined room voice chat".to_string());
        }
        let Some((_, _, target_voice_joined)) = self.validate_webrtc_recipient(recipient).await?
        else {
            return Ok(false);
        };
        if !target_voice_joined {
            return Err("Target connection has not joined room voice chat".to_string());
        }
        Ok(true)
    }

    pub(super) async fn validate_webrtc_media_recipient(
        &self,
        recipient: &str,
        swarm_id: &str,
    ) -> Result<Option<String>, String> {
        let swarm_id = normalize_media_swarm_id(swarm_id)?;
        Self::parse_webrtc_recipient(recipient)?;
        if !self.active_media_swarms.lock().contains(&swarm_id) {
            return Err("Source connection has not joined this media swarm".to_string());
        }
        let Some((target_actor_id, target_conn_id, _)) =
            self.validate_webrtc_recipient(recipient).await?
        else {
            return Ok(None);
        };
        if !self
            .media_swarm_tracker
            .contains(self.room_id, &target_actor_id, &target_conn_id, &swarm_id)
            .await?
        {
            return Err("Target connection has not joined this media swarm".to_string());
        }
        Ok(Some(swarm_id))
    }

    fn verify_media_swarm_ticket(&self, swarm_id: &str, ticket: &str) -> Result<(), String> {
        self.swarm_signing_key
            .verify_media_swarm_ticket(
                &self.public_room_id()?,
                &self.public_actor_id()?,
                swarm_id,
                ticket,
            )
            .map_err(|error| format!("Invalid media swarm ticket: {error}"))
    }

    pub fn ice_candidate_contains_private_ip(candidate: &str) -> bool {
        candidate
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '[' | ']'))
            .filter_map(|part| part.parse::<std::net::IpAddr>().ok())
            .any(is_private_ice_candidate_ip)
    }

    fn validate_sdp(data: &str, description: &str) -> Result<(), String> {
        if data.len() > MAX_SDP_SIZE {
            return Err(format!(
                "WebRTC SDP {description} too large ({} bytes, max: {MAX_SDP_SIZE} bytes)",
                data.len()
            ));
        }
        Ok(())
    }

    fn validate_ice_candidate(&self, data: &str) -> Result<(), String> {
        if data.len() > MAX_ICE_CANDIDATE_SIZE {
            return Err(format!(
                "WebRTC ICE candidate too large ({} bytes, max: {MAX_ICE_CANDIDATE_SIZE} bytes)",
                data.len()
            ));
        }
        if self.filter_private_ice_candidates && Self::ice_candidate_contains_private_ip(data) {
            return Err("WebRTC ICE candidate contains a private or local address".to_string());
        }
        Ok(())
    }

    fn broadcast_webrtc_signal(
        &self,
        event: RealtimeEvent,
        signal_name: &'static str,
    ) -> Result<(), String> {
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            return Ok(());
        }
        tracing::warn!(
            room_id = %self.room_id,
            signal_name,
            "WebRTC realtime delivery did not satisfy distributed delivery requirements"
        );
        synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
            .with_label_values(&["webrtc_signal_no_redis"])
            .inc();
        Err(format!(
            "WebRTC {signal_name} delivery failed: distributed realtime fan-out unavailable"
        ))
    }

    fn signal_sender(&self) -> Result<String, String> {
        Ok(format!(
            "{}:{}",
            self.public_actor_id()?,
            self.connection_id
        ))
    }

    pub async fn handle_webrtc_voice_offer(
        &self,
        offer: &synctv_proto::client::WebRtcVoiceOfferCommand,
    ) -> Result<(), String> {
        self.require_voice_signaling_permission().await?;
        Self::validate_sdp(&offer.data, "offer")?;
        if !self.validate_webrtc_voice_recipient(&offer.to).await? {
            return Ok(());
        }
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCVoiceSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::Offer,
                from: self.signal_sender()?,
                to: offer.to.clone(),
                data: offer.data.clone(),
                timestamp: synctv_core::SystemClock.now(),
            },
            "voice offer",
        )
    }

    pub async fn handle_webrtc_voice_answer(
        &self,
        answer: &synctv_proto::client::WebRtcVoiceAnswerCommand,
    ) -> Result<(), String> {
        self.require_voice_signaling_permission().await?;
        Self::validate_sdp(&answer.data, "answer")?;
        if !self.validate_webrtc_voice_recipient(&answer.to).await? {
            return Ok(());
        }
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCVoiceSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::Answer,
                from: self.signal_sender()?,
                to: answer.to.clone(),
                data: answer.data.clone(),
                timestamp: synctv_core::SystemClock.now(),
            },
            "voice answer",
        )
    }

    pub async fn handle_webrtc_voice_ice_candidate(
        &self,
        candidate: &synctv_proto::client::WebRtcVoiceIceCandidateCommand,
    ) -> Result<(), String> {
        self.require_voice_signaling_permission().await?;
        self.validate_ice_candidate(&candidate.data)?;
        if !self.validate_webrtc_voice_recipient(&candidate.to).await? {
            return Ok(());
        }
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCVoiceSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::IceCandidate,
                from: self.signal_sender()?,
                to: candidate.to.clone(),
                data: candidate.data.clone(),
                timestamp: synctv_core::SystemClock.now(),
            },
            "voice ICE candidate",
        )
    }

    pub async fn handle_webrtc_media_offer(
        &self,
        offer: &synctv_proto::client::WebRtcMediaOfferCommand,
    ) -> Result<(), String> {
        self.require_media_signaling_permission().await?;
        Self::validate_sdp(&offer.data, "offer")?;
        let Some(swarm_id) = self
            .validate_webrtc_media_recipient(&offer.to, &offer.swarm_id)
            .await?
        else {
            return Ok(());
        };
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCMediaSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::Offer,
                from: self.signal_sender()?,
                to: offer.to.clone(),
                data: offer.data.clone(),
                swarm_id,
                timestamp: synctv_core::SystemClock.now(),
            },
            "media offer",
        )
    }

    pub async fn handle_webrtc_media_answer(
        &self,
        answer: &synctv_proto::client::WebRtcMediaAnswerCommand,
    ) -> Result<(), String> {
        self.require_media_signaling_permission().await?;
        Self::validate_sdp(&answer.data, "answer")?;
        let Some(swarm_id) = self
            .validate_webrtc_media_recipient(&answer.to, &answer.swarm_id)
            .await?
        else {
            return Ok(());
        };
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCMediaSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::Answer,
                from: self.signal_sender()?,
                to: answer.to.clone(),
                data: answer.data.clone(),
                swarm_id,
                timestamp: synctv_core::SystemClock.now(),
            },
            "media answer",
        )
    }

    pub async fn handle_webrtc_media_ice_candidate(
        &self,
        candidate: &synctv_proto::client::WebRtcMediaIceCandidateCommand,
    ) -> Result<(), String> {
        self.require_media_signaling_permission().await?;
        self.validate_ice_candidate(&candidate.data)?;
        let Some(swarm_id) = self
            .validate_webrtc_media_recipient(&candidate.to, &candidate.swarm_id)
            .await?
        else {
            return Ok(());
        };
        self.broadcast_webrtc_signal(
            RealtimeEvent::WebRTCMediaSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                message_type: WebRTCSignalKind::IceCandidate,
                from: self.signal_sender()?,
                to: candidate.to.clone(),
                data: candidate.data.clone(),
                swarm_id,
                timestamp: synctv_core::SystemClock.now(),
            },
            "media ICE candidate",
        )
    }

    pub async fn handle_webrtc_voice_join(
        &self,
        join: &synctv_proto::client::WebRtcVoiceJoinCommand,
    ) -> Result<(), String> {
        if join
            .client_operation_id
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 128)
        {
            return Err("Voice chat client operation id must be 1 to 128 bytes".to_string());
        }
        self.require_rtc_business_permission(RoomPermission::USE_VOICE_CHAT, "Voice chat")
            .await?;
        self.require_voice_chat_enabled().await?;
        let conn_id = self.connection_id.clone();
        let max_participants = self.max_voice_participants_per_room()?;
        match self
            .connection_service
            .try_join_voice_rtc(
                &self.room_id,
                &self.user_id,
                conn_id.as_str(),
                max_participants,
            )
            .await?
        {
            VoiceRtcJoinOutcome::AlreadyJoined => return Ok(()),
            VoiceRtcJoinOutcome::RoomAtCapacity => {
                return Err(format!(
                    "Voice chat room at capacity (maximum {max_participants} participants)"
                ));
            }
            VoiceRtcJoinOutcome::Joined => {
                self.has_voice_rtc_session
                    .store(true, std::sync::atomic::Ordering::Release);
                synctv_core::metrics::application::WEBRTC_PEERS_ACTIVE.inc();
            }
        }

        let event = RealtimeEvent::WebRTCVoicePeerJoined {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            actor_id: self.public_actor_id()?,
            conn_id: conn_id.into_string(),
            username: self.username.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };

        // WebRTC join is semi-critical: log at warn if not propagated to Redis.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "WebRTC join broadcast missed the distributed fan-out path (peer may not be visible cross-replica)"
            );
        }

        Ok(())
    }

    pub async fn handle_media_swarm_join(
        &self,
        join: &synctv_proto::client::WebRtcMediaSwarmJoin,
    ) -> Result<(), String> {
        let swarm_id = self
            .validate_media_swarm_membership(&join.swarm_id, &join.swarm_ticket)
            .await?;
        self.require_p2p_media_enabled().await?;
        {
            let active = self.active_media_swarms.lock();
            if !active.contains(&swarm_id) && active.len() >= MAX_ACTIVE_MEDIA_SWARMS_PER_CONNECTION
            {
                return Err(format!(
                    "A connection may join at most {MAX_ACTIVE_MEDIA_SWARMS_PER_CONNECTION} media swarms"
                ));
            }
        }
        let peers = self
            .media_swarm_tracker
            .announce(
                self.room_id,
                self.public_actor_id()?,
                self.connection_id.as_str().to_string(),
                &swarm_id,
            )
            .await?;
        self.active_media_swarms.lock().insert(swarm_id.clone());
        self.send_media_swarm_peers(&swarm_id, peers)?;
        Ok(())
    }

    pub async fn handle_media_swarm_leave(
        &self,
        leave: &synctv_proto::client::WebRtcMediaSwarmLeave,
    ) -> Result<(), String> {
        let swarm_id = normalize_media_swarm_id(&leave.swarm_id)?;
        self.leave_media_swarm(&swarm_id).await
    }

    async fn validate_media_swarm_membership(
        &self,
        swarm_id: &str,
        ticket: &str,
    ) -> Result<String, String> {
        self.require_rtc_business_permission(RoomPermission::USE_P2P_MEDIA, "P2P media")
            .await?;
        let swarm_id = normalize_media_swarm_id(swarm_id)?;
        self.verify_media_swarm_ticket(&swarm_id, ticket)?;
        Ok(swarm_id)
    }

    fn send_media_swarm_peers(
        &self,
        swarm_id: &str,
        peers: Vec<super::MediaSwarmPeer>,
    ) -> Result<(), String> {
        use synctv_proto::client::{
            resource_event::Payload, server_message::Message, web_rtc_event::Event, ResourceEvent,
            ServerMessage, WebRtcEvent, WebRtcMediaSwarmPeer, WebRtcMediaSwarmPeers,
        };
        self.sender.send(ServerMessage {
            message: Some(Message::ResourceEvent(ResourceEvent {
                observe_id: "webrtc".to_string(),
                payload: Some(Payload::WebrtcEvent(WebRtcEvent {
                    event: Some(Event::MediaSwarmPeers(WebRtcMediaSwarmPeers {
                        swarm_id: swarm_id.to_string(),
                        swarm_ticket: self.swarm_signing_key.sign_media_swarm_ticket(
                            &self.public_room_id()?,
                            &self.public_actor_id()?,
                            swarm_id,
                        ),
                        peers: peers
                            .into_iter()
                            .map(|peer| WebRtcMediaSwarmPeer {
                                user_id: peer.actor_id,
                                conn_id: peer.connection_id,
                            })
                            .collect(),
                    })),
                })),
                event_cursor: None,
            })),
        })
    }

    pub(super) async fn leave_all_media_swarms(&self) {
        let swarm_ids = self.active_media_swarms.lock().clone();
        for swarm_id in swarm_ids {
            if let Err(error) = self.leave_media_swarm(&swarm_id).await {
                tracing::warn!(
                    room_id = %self.room_id,
                    connection_id = %self.connection_id,
                    error = %error,
                    "Failed to remove media swarm membership during connection cleanup"
                );
            }
        }
    }

    async fn leave_media_swarm(&self, swarm_id: &str) -> Result<(), String> {
        let actor_id = self.public_actor_id()?;
        if !self.active_media_swarms.lock().remove(swarm_id) {
            return Ok(());
        }

        let tracker_result = self
            .media_swarm_tracker
            .leave(
                self.room_id,
                &actor_id,
                self.connection_id.as_str(),
                swarm_id,
            )
            .await;
        let outcome = self
            .event_service
            .broadcast_outcome(RealtimeEvent::MediaSwarmPeerLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                actor_id,
                conn_id: self.connection_id.as_str().to_string(),
                swarm_id: swarm_id.to_string(),
                timestamp: synctv_core::SystemClock.now(),
            });
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                connection_id = %self.connection_id,
                swarm_id,
                "Media swarm leave broadcast missed the distributed fan-out path"
            );
        }
        tracker_result
    }

    async fn rtc_access_after_event(&self, event: &RealtimeEvent) -> Option<(bool, bool)> {
        let (permissions, voice_chat_enabled, p2p_media_enabled) = match event {
            RealtimeEvent::PermissionChanged {
                target_user_id,
                new_permissions,
                ..
            } if *target_user_id == self.user_id => (*new_permissions, true, true),
            RealtimeEvent::RoomSettingsChanged { settings, .. } => {
                let permissions = if self.principal.is_guest() {
                    self.room_service.guest_permissions_for_settings(settings)
                } else {
                    match self
                        .room_service
                        .get_member(&self.room_id, &self.user_id)
                        .await
                    {
                        Ok(Some(member)) => self
                            .room_service
                            .permission_service()
                            .effective_member_permissions(&member, settings),
                        Ok(None) => synctv_core::models::RoomPermissionSet::empty(),
                        Err(error) => {
                            tracing::warn!(
                                room_id = %self.room_id,
                                user_id = %self.user_id,
                                error = %error,
                                "Failed to recalculate RTC permissions after room settings changed"
                            );
                            synctv_core::models::RoomPermissionSet::empty()
                        }
                    }
                };
                (
                    permissions,
                    settings.voice_chat_enabled.0,
                    settings.p2p_media_enabled.0,
                )
            }
            _ => return None,
        };
        Some((
            voice_chat_enabled && permissions.has(RoomPermission::USE_VOICE_CHAT),
            p2p_media_enabled && permissions.has(RoomPermission::USE_P2P_MEDIA),
        ))
    }

    pub(super) async fn apply_rtc_access_change(&self, event: &RealtimeEvent) {
        let _transition_guard = self.room_capability_transition_lock.lock().await;
        let voice_chat_active = self.current_connection_voice_joined();
        let p2p_media_active = !self.active_media_swarms.lock().is_empty();
        if !voice_chat_active && !p2p_media_active {
            return;
        }
        let Some((voice_chat_allowed, p2p_media_allowed)) =
            self.rtc_access_after_event(event).await
        else {
            return;
        };
        if voice_chat_active && !voice_chat_allowed {
            if let Err(error) = self.leave_webrtc_voice_session().await {
                tracing::warn!(
                    room_id = %self.room_id,
                    connection_id = %self.connection_id,
                    error = %error,
                    "Failed to leave voice chat after RTC access changed"
                );
            }
        }
        if p2p_media_active && !p2p_media_allowed {
            self.leave_all_media_swarms().await;
        }
    }

    pub async fn leave_webrtc_voice_session(&self) -> Result<(), String> {
        let conn_id = self.connection_id.clone();
        let current = self
            .connection_service
            .get_connection(&conn_id)
            .ok_or_else(|| "Connection not found".to_string())?;
        if !current.voice_rtc_joined {
            return Ok(());
        }
        if !self
            .connection_service
            .leave_voice_rtc(&self.room_id, &self.user_id, conn_id.as_str())
            .await?
        {
            return Ok(());
        }
        self.has_voice_rtc_session
            .store(false, std::sync::atomic::Ordering::Release);
        synctv_core::metrics::application::WEBRTC_PEERS_ACTIVE.dec();

        // Broadcast Leave event to all RTC-joined users in the room
        let event = RealtimeEvent::WebRTCVoicePeerLeft {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            actor_id: self.public_actor_id()?,
            conn_id: conn_id.into_string(),
            timestamp: synctv_core::SystemClock.now(),
        };

        // WebRTC leave is semi-critical: log at warn if distributed fan-out misses.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "WebRTC leave broadcast missed the distributed fan-out path (peer may remain visible cross-replica)"
            );
        }

        Ok(())
    }
}
