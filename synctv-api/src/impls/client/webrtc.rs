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
    /// 2. External STUN servers (dynamic setting: `webrtc.external_stun_servers`)
    /// 3. TURN servers (dynamic setting: `webrtc.turn_servers`)
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

        let webrtc_config = &self.config.webrtc;
        let mut servers = Vec::new();

        // 1. Built-in STUN server (only if it started successfully with a valid external address)
        if let Some(ref stun_url) = self.builtin_stun_url {
            servers.push(IceServer {
                urls: vec![stun_url.clone()],
                username: None,
                credential: None,
                expiry_time: 0, // STUN servers don't expire
            });
        }

        // 2. Dynamic external STUN servers
        if let Some(registry) = &self.settings_registry {
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
        }

        // 3. TURN servers
        if !webrtc_config.turn_shared_secret.is_empty()
            && !webrtc_config.turn_server_urls.is_empty()
        {
            // Generate time-limited HMAC-SHA1 credentials (coturn compatible)
            let cred = synctv_core::service::turn_server::generate_turn_credentials(
                &webrtc_config.turn_shared_secret,
                user_id.as_str(),
                webrtc_config.turn_credential_ttl_seconds,
            );

            // Filter out unhealthy TURN servers
            let turn_urls = if let Some(checker) = &self.turn_health_checker {
                checker
                    .filter_healthy_servers(&webrtc_config.turn_server_urls)
                    .await
            } else {
                // No health checker configured - use all servers
                webrtc_config.turn_server_urls.clone()
            };

            // Only add TURN entry if we have healthy servers
            if turn_urls.is_empty() {
                tracing::warn!(
                    "All configured TURN servers are unhealthy - excluding TURN from ICE server list"
                );
            } else {
                servers.push(IceServer {
                    urls: turn_urls,
                    username: Some(cred.username),
                    credential: Some(cred.password),
                    expiry_time: cred.expiry_timestamp as i64,
                });
            }
        } else if let Some(registry) = &self.settings_registry {
            // Fallback: static TURN credentials from dynamic settings
            if let Ok(turn_list) = registry.turn_servers.get() {
                let expiry_time = if webrtc_config.turn_credential_ttl_seconds > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    (now.saturating_add(webrtc_config.turn_credential_ttl_seconds)) as i64
                } else {
                    0
                };

                for ts in &turn_list.0 {
                    // Filter out unhealthy TURN servers
                    let turn_urls = if let Some(checker) = &self.turn_health_checker {
                        checker.filter_healthy_servers(&ts.urls).await
                    } else {
                        // No health checker configured - use all servers
                        ts.urls.clone()
                    };

                    // Only add TURN entry if we have healthy servers
                    if !turn_urls.is_empty() {
                        servers.push(IceServer {
                            urls: turn_urls,
                            username: ts.username.clone(),
                            credential: ts.credential.clone(),
                            expiry_time,
                        });
                    }
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
                assert!(message.contains("not accepting new connections"))
            }
            other => panic!("expected authorization error, got {other:?}"),
        }
    }
}
