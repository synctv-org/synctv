use synctv_core::models::RoomPermission;
use synctv_proto::client::web_rtc_command::Command;
use synctv_realtime::fanout::RealtimeDeliveryRequirement;
use synctv_realtime::sync::{RealtimeEvent, WebRTCSignalKind};

use super::{
    is_private_ice_candidate_ip, should_transition_webrtc_membership, StreamMessageHandler,
    MAX_ICE_CANDIDATE_SIZE, MAX_SDP_SIZE,
};
impl StreamMessageHandler {
    pub async fn handle_webrtc_command(
        &self,
        command: &synctv_proto::client::WebRtcCommand,
    ) -> Result<(), String> {
        self.ensure_webrtc_command_allowed().await?;

        match command.command.as_ref() {
            Some(Command::Offer(offer)) => self.handle_webrtc_offer(offer),
            Some(Command::Answer(answer)) => self.handle_webrtc_answer(answer),
            Some(Command::IceCandidate(candidate)) => self.handle_webrtc_ice_candidate(candidate),
            Some(Command::Join(join)) => self.handle_webrtc_join(join),
            Some(Command::Leave(_leave)) => self.leave_webrtc_session(),
            None => Err("Empty WebRTC command".to_string()),
        }
    }

    async fn ensure_webrtc_command_allowed(&self) -> Result<(), String> {
        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        self.check_realtime_permission(RoomPermission::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        Ok(())
    }

    pub fn validate_webrtc_recipient(&self, recipient: &str) -> Result<(), String> {
        let Some((target_actor_id, target_conn_id)) = recipient.rsplit_once(':') else {
            return Err(
                "WebRTC recipient must be formatted as public_actor_id:conn_id".to_string(),
            );
        };

        let target = self
            .connection_service
            .get_connection(target_conn_id)
            .ok_or_else(|| "Target connection is no longer active".to_string())?;

        if target.actor_id != target_actor_id {
            return Err("WebRTC recipient does not match the target connection owner".to_string());
        }

        let target_room_id = target
            .room_id
            .as_ref()
            .ok_or_else(|| "Target connection is not currently joined to a room".to_string())?;
        if target_room_id != &self.room_id {
            return Err("Target connection is not in this room".to_string());
        }

        if !target.rtc_joined {
            return Err("Target connection has not joined WebRTC".to_string());
        }

        Ok(())
    }

    pub fn ice_candidate_contains_private_ip(candidate: &str) -> bool {
        candidate
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '[' | ']'))
            .filter_map(|part| part.parse::<std::net::IpAddr>().ok())
            .any(is_private_ice_candidate_ip)
    }

    pub fn handle_webrtc_offer(
        &self,
        offer: &synctv_proto::client::WebRtcOffer,
    ) -> Result<(), String> {
        // Validate SDP payload size
        if offer.data.len() > MAX_SDP_SIZE {
            return Err(format!(
                "WebRTC SDP offer too large ({} bytes, max: {MAX_SDP_SIZE} bytes)",
                offer.data.len()
            ));
        }

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&offer.to)?;

        // P2P relay path: forward offer to target peer via cluster
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Offer,
            from: format!("{}:{}", self.public_actor_id()?, conn_id),
            to: offer.to.clone(),
            data: offer.data.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC offer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC offer delivery failed: distributed realtime fan-out unavailable"
                    .to_string(),
            );
        }

        Ok(())
    }

    pub fn handle_webrtc_answer(
        &self,
        answer: &synctv_proto::client::WebRtcAnswer,
    ) -> Result<(), String> {
        // Validate SDP payload size
        if answer.data.len() > MAX_SDP_SIZE {
            return Err(format!(
                "WebRTC SDP answer too large ({} bytes, max: {MAX_SDP_SIZE} bytes)",
                answer.data.len()
            ));
        }

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&answer.to)?;

        // Create event with server-set 'from' field
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Answer,
            from: format!("{}:{}", self.public_actor_id()?, conn_id),
            to: answer.to.clone(),
            data: answer.data.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC answer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC answer delivery failed: distributed realtime fan-out unavailable"
                    .to_string(),
            );
        }

        Ok(())
    }

    pub fn handle_webrtc_ice_candidate(
        &self,
        candidate: &synctv_proto::client::WebRtcIceCandidate,
    ) -> Result<(), String> {
        // Validate ICE candidate payload size
        if candidate.data.len() > MAX_ICE_CANDIDATE_SIZE {
            return Err(format!(
                "WebRTC ICE candidate too large ({} bytes, max: {MAX_ICE_CANDIDATE_SIZE} bytes)",
                candidate.data.len()
            ));
        }
        if self.filter_private_ice_candidates
            && Self::ice_candidate_contains_private_ip(&candidate.data)
        {
            return Err("WebRTC ICE candidate contains a private or local address".to_string());
        }

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&candidate.to)?;

        // P2P relay path: forward ICE candidate to target peer via cluster
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::IceCandidate,
            from: format!("{}:{}", self.public_actor_id()?, conn_id),
            to: candidate.to.clone(),
            data: candidate.data.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };

        // Cross-replica ICE signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "ICE candidate realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC ICE candidate delivery failed: distributed realtime fan-out unavailable"
                    .to_string(),
            );
        }

        Ok(())
    }

    pub fn handle_webrtc_join(
        &self,
        _join: &synctv_proto::client::WebRtcJoin,
    ) -> Result<(), String> {
        let conn_id = self.connection_id.clone();

        let should_join = should_transition_webrtc_membership(
            self.connection_service
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            true,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_join {
            tracing::debug!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC join for already-joined connection"
            );
            return Ok(());
        }

        // Mark this connection as joined WebRTC session
        self.connection_service
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, true);

        // Track WebRTC peer metrics and session state for cleanup()
        // Order matters: increment metric FIRST, then set the flag.
        // This prevents race condition where cleanup() sees the flag but metric
        // hasn't been incremented yet, which would cause undercount on dec().
        synctv_core::metrics::application::WEBRTC_PEERS_ACTIVE.inc();
        self.has_webrtc_session
            .store(true, std::sync::atomic::Ordering::Release);

        // Broadcast Join event to all RTC-joined users in the room
        let event = RealtimeEvent::WebRTCJoin {
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

    pub fn leave_webrtc_session(&self) -> Result<(), String> {
        let conn_id = self.connection_id.clone();

        let should_leave = should_transition_webrtc_membership(
            self.connection_service
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            false,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_leave {
            tracing::debug!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC leave for already-left connection"
            );
            self.has_webrtc_session
                .store(false, std::sync::atomic::Ordering::Release);
            return Ok(());
        }

        // Mark this connection as left WebRTC session
        self.connection_service
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, false);

        // Track WebRTC peer metrics and session state for cleanup()
        // Order matters: clear the flag FIRST, then decrement metric.
        // This prevents race condition where cleanup() might also try to dec()
        // after we've already decremented, which would cause undercount.
        self.has_webrtc_session
            .store(false, std::sync::atomic::Ordering::Release);
        synctv_core::metrics::application::WEBRTC_PEERS_ACTIVE.dec();

        // Broadcast Leave event to all RTC-joined users in the room
        let event = RealtimeEvent::WebRTCLeave {
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
