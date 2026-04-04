//! Live streaming operations: `publish_key`, `validate_live_token`, `stream_info`, live proxy

use std::sync::Arc;
use synctv_core::models::UserId;

use super::ClientApiImpl;
use crate::impls::ApiError;

fn build_publish_rtmp_url(config: &synctv_core::Config, room_id: &str) -> String {
    let rtmp_host = config.public_rtmp_host();
    let rtmp_port = config.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/live/{room_id}")
}

fn paginate_room_stream_ids(
    mut media_ids: Vec<String>,
    page: i32,
    page_size: i32,
) -> crate::proto::client::ListRoomStreamsResponse {
    media_ids.sort_unstable();

    let page = page.max(1) as usize;
    let page_size = if page_size <= 0 {
        50usize
    } else {
        page_size.min(100) as usize
    };
    let total = media_ids.len() as i32;
    let offset = (page - 1) * page_size;
    let streams = media_ids
        .into_iter()
        .skip(offset)
        .take(page_size)
        .map(|media_id| crate::proto::client::StreamEntry {
            media_id,
            active: true,
        })
        .collect();

    crate::proto::client::ListRoomStreamsResponse { streams, total }
}

impl ClientApiImpl {
    pub async fn create_publish_key(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePublishKeyRequest,
    ) -> Result<crate::proto::client::CreatePublishKeyResponse, ApiError> {
        // Validate media ID format
        crate::http::validation::validate_id(&req.id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let media_id = synctv_core::models::MediaId::from_string(req.id.clone());

        // Verify media exists and belongs to this room
        let media = self
            .room_service
            .media_service()
            .get_media(&media_id)
            .await
            .map_err(|e| Self::map_media_lookup_error(e, "Media not found"))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {} not found", req.id)))?;

        if media.room_id.as_str() != room_id {
            return Err(ApiError::InvalidInput(
                "Media does not belong to this room".to_string(),
            ));
        }

        // Check room exists
        let _room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        // Check permission to start live stream
        self.room_service
            .check_permission(&rid, &uid, synctv_core::models::PermissionBits::START_LIVE)
            .await
            .map_err(Self::map_room_access_error)?;

        // Get publish key service
        let publish_key_service = self
            .publish_key_service
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Publish key service not configured".to_string()))?;

        // Generate publish key
        let publish_key = publish_key_service
            .generate_publish_key(rid.clone(), media_id.clone(), uid.clone())
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to generate publish key: {e}")))?;

        // Construct RTMP URL and stream key from server config
        // Use advertise_host for external clients (resolves to POD_IP in K8s, hostname otherwise)
        let rtmp_url = build_publish_rtmp_url(&self.config, rid.as_str());
        let stream_key = format!("{}?key={}", media_id.as_str(), publish_key.token);

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
    ) -> Result<UserId, ApiError> {
        let bearer_token = format!("Bearer {token}");
        let user_id = self
            .jwt_validator
            .validate_http_extract_user_id(&bearer_token)
            .map_err(|e| ApiError::Authentication(format!("Invalid token: {e}")))?;

        // Verify room membership
        let rid = self.parse_room_id(room_id)?;
        let is_member = self
            .room_service
            .member_service()
            .is_member(&rid, &user_id)
            .await
            .map_err(Self::map_membership_probe_error)?;

        if !is_member {
            return Err(ApiError::Authorization(
                "Not a member of this room".to_string(),
            ));
        }

        Ok(user_id)
    }

    /// Get stream info for a specific media in a room.
    pub async fn get_stream_info(
        &self,
        user_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::GetStreamInfoResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership before returning stream info
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Live streaming not configured".to_string()))?;

        match infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
        {
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
            Err(e) => Err(Self::map_livestream_backend_error(&*e)),
        }
    }

    /// List all active streams in a room.
    pub async fn list_room_streams(
        &self,
        user_id: &str,
        room_id: &str,
        page: i32,
        page_size: i32,
    ) -> Result<crate::proto::client::ListRoomStreamsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership before listing streams
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Live streaming not configured".to_string()))?;

        let media_ids = infrastructure
            .registry
            .list_streams_for_room(room_id)
            .await
            .map_err(|e| Self::map_livestream_backend_error(&*e))?;

        Ok(paginate_room_stream_ids(media_ids, page, page_size))
    }

    /// Get a reference to the live streaming infrastructure, if configured.
    #[must_use]
    pub const fn live_infrastructure(
        &self,
    ) -> Option<&Arc<synctv_livestream::api::LiveStreamingInfrastructure>> {
        self.live_streaming_infrastructure.as_ref()
    }

    /// Get the external source URL for a `LiveProxy` media item.
    /// Returns None if the media is not a `live_proxy` type, has no URL,
    /// or does not belong to the specified room.
    pub async fn get_live_proxy_source_url(&self, room_id: &str, media_id: &str) -> Option<String> {
        let mid = synctv_core::models::MediaId::from_string(media_id.to_string());
        let media = self
            .room_service
            .media_service()
            .get_media(&mid)
            .await
            .ok()??;
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
        media
            .source_config
            .get("url")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::paginate_room_stream_ids;

    #[test]
    fn paginate_room_stream_ids_sorts_and_applies_defaults() {
        let response = paginate_room_stream_ids(
            vec![
                "media-c".to_string(),
                "media-a".to_string(),
                "media-b".to_string(),
            ],
            0,
            0,
        );

        assert_eq!(response.total, 3);
        let ids: Vec<_> = response
            .streams
            .into_iter()
            .map(|stream| stream.media_id)
            .collect();
        assert_eq!(ids, vec!["media-a", "media-b", "media-c"]);
    }

    #[test]
    fn paginate_room_stream_ids_respects_page_and_page_size() {
        let response = paginate_room_stream_ids(
            vec![
                "media-a".to_string(),
                "media-b".to_string(),
                "media-c".to_string(),
            ],
            2,
            1,
        );

        assert_eq!(response.total, 3);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, "media-b");
        assert!(response.streams[0].active);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::build_publish_rtmp_url;

    #[test]
    fn build_publish_rtmp_url_prefers_explicit_public_rtmp_host() {
        let mut config = synctv_core::Config::default();
        config.server.advertise_host = "10.0.0.12".to_string();
        config.livestream.public_rtmp_host = "stream.example.com".to_string();
        config.livestream.rtmp_port = 1935;

        let url = build_publish_rtmp_url(&config, "room_123");

        assert_eq!(url, "rtmp://stream.example.com:1935/live/room_123");
    }
}
