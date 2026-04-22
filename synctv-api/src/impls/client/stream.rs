//! Live streaming operations: `publish_key`, `validate_live_token`, `stream_info`, live proxy

use std::sync::Arc;
use synctv_core::models::{MediaId, UserId};

use super::ClientApiImpl;
use crate::impls::ApiError;

const LIVESTREAM_UNAVAILABLE_MESSAGE: &str = "Live streaming is not available on this server.";
const PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE: &str =
    "Publish key service is not available on this server.";
const DEFAULT_ROOM_STREAMS_PAGE: i32 = 1;
const DEFAULT_ROOM_STREAMS_PAGE_SIZE: i32 = 50;

fn build_publish_rtmp_url(config: &synctv_core::Config, room_id: &str) -> String {
    let rtmp_host = config.public_rtmp_host();
    let rtmp_port = config.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/{room_id}")
}

pub(crate) fn build_room_streams_request(
    req: crate::proto::client::ListRoomStreamsRequest,
) -> Result<crate::proto::client::ListRoomStreamsRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(crate::proto::client::ListRoomStreamsRequest {
        page: if req.page > 0 {
            req.page
        } else {
            DEFAULT_ROOM_STREAMS_PAGE
        },
        page_size: if req.page_size > 0 {
            req.page_size
        } else {
            DEFAULT_ROOM_STREAMS_PAGE_SIZE
        },
        search: req.search,
        sort_by: req.sort_by,
        sort_direction: req.sort_direction,
    })
}

fn paginate_room_stream_ids(
    media_ids: Vec<String>,
    req: &crate::proto::client::ListRoomStreamsRequest,
) -> crate::proto::client::ListRoomStreamsResponse {
    crate::impls::build_room_streams_response(media_ids, req)
}

fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
}

fn publish_key_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE.to_string())
}

fn build_create_publish_key_request(
    req: crate::proto::client::CreatePublishKeyRequest,
) -> Result<MediaId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    Ok(crate::impls::proto_validated_media_id(req.id))
}

impl ClientApiImpl {
    pub async fn create_publish_key(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePublishKeyRequest,
    ) -> Result<crate::proto::client::CreatePublishKeyResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let media_id = build_create_publish_key_request(req.clone())?;

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
            .ok_or_else(publish_key_service_unavailable_error)?;

        // Generate publish key
        let publish_key = publish_key_service
            .generate_publish_key(&rid, &media_id, &uid)
            .map_err(|e| ApiError::Internal(format!("Failed to generate publish key: {e}")))?;

        // Construct RTMP URL and stream key from server config
        // Use advertise_host for external clients (resolves to POD_IP in K8s, hostname otherwise)
        let rtmp_url = build_publish_rtmp_url(&self.config, rid.as_str());
        let stream_key = format!("{}?token={}", media_id.as_str(), publish_key.token);

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
            .map_err(|_| {
                ApiError::Authentication(
                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                )
            })?;

        // Verify room membership
        let rid = Self::parse_room_id(room_id)?;
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
        let rid = Self::parse_room_id(room_id)?;

        // Check membership before returning stream info
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

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
        req: crate::proto::client::ListRoomStreamsRequest,
    ) -> Result<crate::proto::client::ListRoomStreamsResponse, ApiError> {
        let req = build_room_streams_request(req)?;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Check membership before listing streams
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let media_ids = infrastructure
            .registry
            .list_streams_for_room(room_id)
            .await
            .map_err(|e| Self::map_livestream_backend_error(&*e))?;

        Ok(paginate_room_stream_ids(media_ids, &req))
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
    use super::{
        build_create_publish_key_request, build_room_streams_request,
        live_streaming_unavailable_error, paginate_room_stream_ids,
        publish_key_service_unavailable_error,
    };
    use crate::impls::{ApiError, ErrorKind};

    #[test]
    fn paginate_room_stream_ids_sorts_and_applies_defaults() {
        let response = paginate_room_stream_ids(
            vec![
                "media-c".to_string(),
                "media-a".to_string(),
                "media-b".to_string(),
            ],
            &crate::proto::client::ListRoomStreamsRequest {
                page: 1,
                page_size: 50,
                search: String::new(),
                sort_by: 0,
                sort_direction: 0,
            },
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
            &crate::proto::client::ListRoomStreamsRequest {
                page: 2,
                page_size: 1,
                search: String::new(),
                sort_by: 0,
                sort_direction: 0,
            },
        );

        assert_eq!(response.total, 3);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, "media-b");
        assert!(response.streams[0].active);
    }

    #[test]
    fn build_room_streams_request_rejects_invalid_proto_request() {
        let error = build_room_streams_request(crate::proto::client::ListRoomStreamsRequest {
            page: -1,
            page_size: 101,
            search: "a".repeat(101),
            sort_by: 0,
            sort_direction: 0,
        })
        .expect_err("invalid proto request must be rejected");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("page"), "{message}");
                assert!(message.contains("page_size"), "{message}");
                assert!(message.contains("search"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_room_streams_request_normalizes_defaults() {
        let req = build_room_streams_request(crate::proto::client::ListRoomStreamsRequest {
            page: 0,
            page_size: 0,
            search: " beta ".to_string(),
            sort_by: 1,
            sort_direction: 2,
        })
        .expect("defaultable request must be accepted");

        assert_eq!(req.page, 1);
        assert_eq!(req.page_size, 50);
        assert_eq!(req.search, " beta ");
        assert_eq!(req.sort_by, 1);
        assert_eq!(req.sort_direction, 2);
    }

    #[test]
    fn paginate_room_stream_ids_applies_search_and_desc_sort() {
        let response = paginate_room_stream_ids(
            vec![
                "beta-02".to_string(),
                "alpha-01".to_string(),
                "beta-01".to_string(),
            ],
            &crate::proto::client::ListRoomStreamsRequest {
                page: 1,
                page_size: 10,
                search: "beta".to_string(),
                sort_by: 1,
                sort_direction: 2,
            },
        );

        let ids: Vec<_> = response
            .streams
            .into_iter()
            .map(|stream| stream.media_id)
            .collect();
        assert_eq!(response.total, 2);
        assert_eq!(ids, vec!["beta-02", "beta-01"]);
    }

    #[test]
    fn live_streaming_unavailable_error_is_service_unavailable() {
        let err = live_streaming_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Live streaming is not available on this server."
        );
    }

    #[test]
    fn publish_key_service_unavailable_error_is_service_unavailable() {
        let err = publish_key_service_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Publish key service is not available on this server."
        );
    }

    #[test]
    fn build_create_publish_key_request_rejects_invalid_media_id() {
        let error =
            build_create_publish_key_request(crate::proto::client::CreatePublishKeyRequest {
                id: "bad-media".to_string(),
            })
            .expect_err("invalid proto request must be rejected");

        assert!(matches!(error, ApiError::InvalidInput(_)));
    }

    #[test]
    fn build_create_publish_key_request_parses_proto_validated_media_id() {
        let media_id =
            build_create_publish_key_request(crate::proto::client::CreatePublishKeyRequest {
                id: "AbC123xYz890".to_string(),
            })
            .expect("valid proto media id");

        assert_eq!(media_id.as_str(), "AbC123xYz890");
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

        assert_eq!(url, "rtmp://stream.example.com:1935/room_123");
    }

    #[test]
    fn build_publish_rtmp_url_falls_back_to_local_bind_host_without_explicit_public_host() {
        let mut config = synctv_core::Config::default();
        config.server.host = "0.0.0.0".to_string();
        config.server.advertise_host.clear();
        config.livestream.public_rtmp_host.clear();
        config.livestream.rtmp_port = 1935;

        let url = build_publish_rtmp_url(&config, "room_123");

        assert_eq!(url, "rtmp://127.0.0.1:1935/room_123");
    }
}
