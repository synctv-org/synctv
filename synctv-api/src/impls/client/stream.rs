//! Live streaming operations: publish_key, validate_live_token, stream_info, live proxy

use std::sync::Arc;
use synctv_core::models::{RoomId, UserId};

use super::ClientApiImpl;

impl ClientApiImpl {
    pub async fn create_publish_key(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePublishKeyRequest,
    ) -> Result<crate::proto::client::CreatePublishKeyResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Validate media ID
        if req.id.is_empty() {
            return Err("Media ID is required".to_string());
        }
        let media_id = synctv_core::models::MediaId::from_string(req.id.clone());

        // Check room exists
        let _room = self.room_service.get_room(&rid).await
            .map_err(|e| match e {
                synctv_core::Error::NotFound(msg) => format!("Room not found: {msg}"),
                _ => format!("Failed to get room: {e}"),
            })?;

        // Check permission to start live stream
        self.room_service
            .check_permission(&rid, &uid, synctv_core::models::PermissionBits::START_LIVE)
            .await
            .map_err(|e| format!("Permission denied: {e}"))?;

        // Get publish key service
        let publish_key_service = self.publish_key_service.as_ref()
            .ok_or_else(|| "Publish key service not configured".to_string())?;

        // Generate publish key
        let publish_key = publish_key_service
            .generate_publish_key(rid.clone(), media_id.clone(), uid.clone())
            .await
            .map_err(|e| format!("Failed to generate publish key: {e}"))?;

        // Construct RTMP URL and stream key from server config
        let rtmp_host = &self.config.server.host;
        let rtmp_port = self.config.livestream.rtmp_port;
        // If bound to 0.0.0.0, use "localhost" as a safe default hint for clients
        let display_host = if rtmp_host == "0.0.0.0" { "localhost" } else { rtmp_host.as_str() };
        let rtmp_url = format!("rtmp://{display_host}:{rtmp_port}/live/{}", rid.as_str());
        let stream_key = publish_key.token.clone();

        tracing::info!(
            room_id = %rid.as_str(),
            media_id = %media_id.as_str(),
            user_id = %uid.as_str(),
            expires_at = publish_key.expires_at,
            "Generated publish key for live streaming"
        );

        Ok(crate::proto::client::CreatePublishKeyResponse {
            publish_key: publish_key.token,
            rtmp_url,
            stream_key,
            expires_at: publish_key.expires_at,
        })
    }

    /// Validate a live streaming token and verify room membership.
    /// Returns the authenticated `UserId` on success.
    pub async fn validate_live_token(
        &self,
        token: &str,
        room_id: &str,
    ) -> Result<UserId, String> {
        let validator =
            synctv_core::service::auth::JwtValidator::new(Arc::new(self.jwt_service.clone()));
        let bearer_token = format!("Bearer {token}");
        let user_id = validator
            .validate_http_extract_user_id(&bearer_token)
            .map_err(|e| format!("Invalid token: {e}"))?;

        // Verify room membership
        let rid = RoomId::from_string(room_id.to_string());
        let is_member = self
            .room_service
            .member_service()
            .is_member(&rid, &user_id)
            .await
            .map_err(|e| format!("Failed to check membership: {e}"))?;

        if !is_member {
            return Err("Not a member of this room".to_string());
        }

        Ok(user_id)
    }

    /// Get stream info for a specific media in a room.
    pub async fn get_stream_info(
        &self,
        user_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::GetStreamInfoResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning stream info
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let infrastructure = self.live_streaming_infrastructure.as_ref()
            .ok_or_else(|| "Live streaming not configured".to_string())?;

        match infrastructure.registry.get_publisher(room_id, media_id).await {
            Ok(Some(pub_info)) => Ok(crate::proto::client::GetStreamInfoResponse {
                active: true,
                publisher: Some(crate::proto::client::StreamPublisherInfo {
                    user_id: pub_info.user_id,
                    started_at: pub_info.started_at.timestamp(),
                }),
            }),
            Ok(None) => Ok(crate::proto::client::GetStreamInfoResponse {
                active: false,
                publisher: None,
            }),
            Err(e) => {
                tracing::error!("Failed to query stream info: {e}");
                Err("Failed to query stream info".to_string())
            }
        }
    }

    /// List all active streams in a room.
    pub async fn list_room_streams(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::ListRoomStreamsResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before listing streams
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let infrastructure = self.live_streaming_infrastructure.as_ref()
            .ok_or_else(|| "Live streaming not configured".to_string())?;

        let all_streams = infrastructure
            .registry
            .list_active_streams()
            .await
            .map_err(|e| format!("Failed to list streams: {e}"))?;

        let streams = all_streams
            .into_iter()
            .filter(|(rid, _)| rid == room_id)
            .map(|(_, media_id)| crate::proto::client::StreamEntry {
                media_id,
                active: true,
            })
            .collect();

        Ok(crate::proto::client::ListRoomStreamsResponse { streams })
    }

    /// Get a reference to the live streaming infrastructure, if configured.
    #[must_use]
    pub const fn live_infrastructure(&self) -> Option<&Arc<synctv_livestream::api::LiveStreamingInfrastructure>> {
        self.live_streaming_infrastructure.as_ref()
    }

    /// Get the external source URL for a `LiveProxy` media item.
    /// Returns None if the media is not a `live_proxy` type, has no URL,
    /// or does not belong to the specified room.
    pub async fn get_live_proxy_source_url(&self, room_id: &str, media_id: &str) -> Option<String> {
        let mid = synctv_core::models::MediaId::from_string(media_id.to_string());
        let media = self.room_service.media_service()
            .get_media(&mid).await.ok()??;
        // Verify media belongs to the requested room
        if media.room_id.as_str() != room_id {
            tracing::warn!(
                media_id = %media_id,
                expected_room = %room_id,
                actual_room = %media.room_id.as_str(),
                "Media does not belong to requested room"
            );
            return None;
        }
        if media.source_provider != "live_proxy" {
            return None;
        }
        media.source_config.get("url")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}
