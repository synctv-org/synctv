//! WebRTC operations: ICE servers, network quality

use synctv_core::models::{RoomId, UserId};

use crate::impls::ApiError;
use super::ClientApiImpl;
use super::convert::network_stats_to_proto;

impl ClientApiImpl {
    /// Get ICE servers configuration for WebRTC.
    ///
    /// Combines:
    /// 1. Built-in STUN server (from static config)
    /// 2. External STUN servers (dynamic setting: `webrtc.external_stun_servers`)
    /// 3. TURN servers (dynamic setting: `webrtc.turn_servers`)
    pub async fn get_ice_servers(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetIceServersResponse, ApiError> {
        use crate::proto::client::{IceServer, GetIceServersResponse};

        // Check membership
        self.room_service.check_membership(room_id, user_id).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let webrtc_config = &self.config.webrtc;
        let mut servers = Vec::new();

        // 1. Built-in STUN server (static config)
        if webrtc_config.enable_builtin_stun {
            let stun_url = format!(
                "stun:{}:{}",
                self.config.server.host,
                webrtc_config.stun_port
            );
            servers.push(IceServer {
                urls: vec![stun_url],
                username: None,
                credential: None,
                expiry_time: 0, // STUN servers don't expire
            });
        }

        // 2 & 3. Dynamic settings (external STUN + TURN servers)
        if let Some(registry) = &self.settings_registry {
            // External STUN servers
            if let Ok(stun_list) = registry.external_stun_servers.get() {
                for url in &stun_list.0 {
                    servers.push(IceServer {
                        urls: vec![url.clone()],
                        username: None,
                        credential: None,
                        expiry_time: 0, // External STUN servers don't expire
                    });
                }
            }

            // TURN servers
            if let Ok(turn_list) = registry.turn_servers.get() {
                for ts in &turn_list.0 {
                    servers.push(IceServer {
                        urls: ts.urls.clone(),
                        username: ts.username.clone(),
                        credential: ts.credential.clone(),
                        expiry_time: 0, // TODO: compute TTL from TURN credential rotation policy
                    });
                }
            }
        }

        Ok(GetIceServersResponse { servers })
    }

    /// Get network quality stats for peers in a room
    pub async fn get_network_quality(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetNetworkQualityResponse, ApiError> {
        use crate::proto::client::GetNetworkQualityResponse;

        // Check membership
        self.room_service.check_membership(room_id, user_id).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let sfu_manager = if let Some(mgr) = &self.sfu_manager { mgr } else {
            tracing::debug!(
                room_id = %room_id,
                user_id = %user_id,
                "Network quality requested but SFU manager not enabled"
            );
            return Ok(GetNetworkQualityResponse { peers: vec![] });
        };

        let stats = sfu_manager.get_room_network_quality(
            &synctv_sfu::RoomId::from(room_id.as_str()),
        ).map_err(|e| ApiError::Internal(e.to_string()))?;

        let peers = stats
            .into_iter()
            .map(|(peer_id, ns)| network_stats_to_proto(peer_id, ns))
            .collect();

        Ok(GetNetworkQualityResponse { peers })
    }
}
