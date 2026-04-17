//! WebRTC operations: ICE servers, network quality

use synctv_core::models::{PermissionBits, RoomId, UserId};

use super::ClientApiImpl;
use crate::impls::ApiError;

fn validate_realtime_room_access_for_webrtc(
    room: &synctv_core::models::Room,
) -> Result<(), ApiError> {
    if room.is_banned {
        return Err(ApiError::Authorization(
            "Forbidden: This room is banned".to_string(),
        ));
    }

    if room.status.is_closed() {
        return Err(ApiError::Authorization(
            "Forbidden: This room is closed and not accepting new connections".to_string(),
        ));
    }

    Ok(())
}

impl ClientApiImpl {
    /// Get ICE servers configuration for WebRTC.
    ///
    /// Combines:
    /// 1. Built-in STUN server (from static config)
    /// 2. External ICE servers (dynamic setting: `webrtc.external_ice_servers`)
    pub async fn get_ice_servers(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetIceServersResponse, ApiError> {
        use crate::proto::client::{GetIceServersResponse, IceServer};

        // Check membership
        self.room_service
            .check_membership(room_id, user_id)
            .await
            .map_err(Self::map_room_access_error)?;
        self.room_service
            .check_permission(room_id, user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(Self::map_room_access_error)?;
        let room = self
            .room_service
            .get_room(room_id)
            .await
            .map_err(ApiError::from)?;
        validate_realtime_room_access_for_webrtc(&room)?;

        let mut servers = Vec::new();

        // 1. Built-in STUN server (only if it started successfully with a valid external address)
        if let Some(ref stun_url) = self.builtin_stun_url {
            servers.push(IceServer {
                urls: vec![stun_url.clone()],
                username: None,
                credential: None,
            });
        }

        // 2. Dynamic external ICE servers
        if let Some(registry) = &self.settings_registry {
            if let Ok(ice_servers) = registry.external_ice_servers.get() {
                for server in &ice_servers.0 {
                    servers.push(IceServer {
                        urls: server.urls.clone(),
                        username: server.username.clone(),
                        credential: server.credential.clone(),
                    });
                }
            }
        }

        Ok(GetIceServersResponse { servers })
    }

    /// Get network quality stats for peers in a room
    ///
    /// Note: With SFU removed, this always returns an empty list.
    /// Network quality is now handled purely peer-to-peer without centralized tracking.
    pub async fn get_network_quality(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetNetworkQualityResponse, ApiError> {
        use crate::proto::client::GetNetworkQualityResponse;

        // Check membership
        self.room_service
            .check_membership(room_id, user_id)
            .await
            .map_err(Self::map_room_access_error)?;

        // SFU removed: network quality is now handled peer-to-peer
        tracing::debug!(
            room_id = %room_id,
            user_id = %user_id,
            "Network quality requested but SFU is no longer supported"
        );

        Ok(GetNetworkQualityResponse { peers: vec![] })
    }
}

#[cfg(test)]
mod tests {
    use super::validate_realtime_room_access_for_webrtc;
    use crate::impls::ApiError;
    use synctv_core::models::{Room, RoomStatus, UserId};

    #[test]
    fn validate_realtime_room_access_for_webrtc_rejects_banned_room() {
        let mut room = Room::new("test-room".to_string(), UserId::new());
        room.ban();

        let err = validate_realtime_room_access_for_webrtc(&room)
            .expect_err("banned room must reject webrtc bootstrap");
        match err {
            ApiError::Authorization(message) => assert!(message.contains("banned")),
            other => panic!("expected authorization error, got {other:?}"),
        }
    }

    #[test]
    fn validate_realtime_room_access_for_webrtc_rejects_closed_room() {
        let mut room = Room::new("test-room".to_string(), UserId::new());
        room.status = RoomStatus::Closed;

        let err = validate_realtime_room_access_for_webrtc(&room)
            .expect_err("closed room must reject webrtc bootstrap");
        match err {
            ApiError::Authorization(message) => {
                assert!(message.contains("not accepting new connections"));
            }
            other => panic!("expected authorization error, got {other:?}"),
        }
    }
}
