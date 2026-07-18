use std::sync::Arc;
use synctv_core::models::{MediaId, Room, UserId};

use crate::impls::{AdminApiImpl, ApiError, ClientApiImpl};
use synctv_proto::providers::rtmp::{
    CreatePublishKeyRequest, CreatePublishKeyResponse, GetStreamInfoRequest, GetStreamInfoResponse,
    StreamPublisherInfo,
};

const LIVESTREAM_UNAVAILABLE_MESSAGE: &str = "Live streaming is not available on this server.";
const PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE: &str =
    "Publish key service is not available on this server.";

#[must_use]
pub(crate) fn build_publish_rtmp_url(
    runtime_settings: &crate::ApiRuntimeSettings,
    room_id: &str,
) -> String {
    let rtmp_host = runtime_settings.public_rtmp_host();
    let rtmp_port = runtime_settings.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/{room_id}")
}

pub(crate) fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
}

pub(crate) fn publish_key_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE.to_string())
}

struct RtmpProviderApiCodec<'a> {
    public_id_codec: &'a synctv_adapter::PublicIdCodec,
}

impl<'a> RtmpProviderApiCodec<'a> {
    const fn new(public_id_codec: &'a synctv_adapter::PublicIdCodec) -> Self {
        Self { public_id_codec }
    }

    fn parse_room_id(&self, room_id: &str) -> Result<synctv_core::models::RoomId, ApiError> {
        crate::impls::parse_room_id_param(room_id, "room_id", self.public_id_codec)
    }

    fn build_create_publish_key_request(
        &self,
        req: CreatePublishKeyRequest,
    ) -> Result<(String, MediaId), ApiError> {
        crate::impls::validate_proto_request(&req)?;
        Ok((
            req.room_id,
            crate::impls::proto_validated_media_id(req.media_id, self.public_id_codec)?,
        ))
    }

    fn build_get_stream_info_request(
        &self,
        req: GetStreamInfoRequest,
    ) -> Result<(String, MediaId), ApiError> {
        crate::impls::validate_proto_request(&req)?;
        Ok((
            req.room_id,
            crate::impls::proto_validated_media_id(req.media_id, self.public_id_codec)?,
        ))
    }

    fn encode_public_stream_ids(
        &self,
        room_id: synctv_core::models::RoomId,
        media_id: MediaId,
    ) -> Result<(String, String), ApiError> {
        let room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(|e| ApiError::Internal(format!("Failed to encode room id: {e}")))?;
        let media_id = self
            .public_id_codec
            .encode_media_id(media_id)
            .map_err(|e| ApiError::Internal(format!("Failed to encode media id: {e}")))?;
        Ok((room_id, media_id))
    }

    async fn fetch_stream_info(
        &self,
        infrastructure: &synctv_livestream::LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
    ) -> Result<GetStreamInfoResponse, ApiError> {
        match infrastructure.find_publisher(room_id, media_id).await {
            Ok(Some(pub_info)) => {
                let user_id = self
                    .public_id_codec
                    .encode_user_id(pub_info.user_id.parse::<UserId>().map_err(|error| {
                        ApiError::Internal(format!("Invalid active publisher user id: {error}"))
                    })?)
                    .map_err(ApiError::Internal)?;
                Ok(GetStreamInfoResponse {
                    active: true,
                    publisher: Some(StreamPublisherInfo {
                        user_id,
                        started_at: pub_info.started_at.timestamp(),
                    }),
                })
            }
            Ok(None) => Ok(GetStreamInfoResponse {
                active: false,
                publisher: None,
            }),
            Err(error) => Err(ApiError::Internal(format!(
                "Failed to get stream info: {error}"
            ))),
        }
    }
}

fn ensure_room_accepts_live_publish(room: &Room) -> Result<(), ApiError> {
    if room.is_banned {
        return Err(ApiError::Authorization("Room is banned".to_string()));
    }

    if !room.status.is_active() {
        return Err(ApiError::Authorization("Room is not active".to_string()));
    }

    Ok(())
}

impl ClientApiImpl {
    pub async fn create_publish_key(
        &self,
        user_id: &UserId,
        req: CreatePublishKeyRequest,
    ) -> Result<CreatePublishKeyResponse, ApiError> {
        let codec = RtmpProviderApiCodec::new(&self.public_id_codec);
        let uid = *user_id;
        let (room_id, media_id) = codec.build_create_publish_key_request(req)?;
        let rid = codec.parse_room_id(&room_id)?;

        let media = self
            .room_service
            .media_service()
            .get_room_media(&rid, &media_id)
            .await
            .map_err(|e| Self::map_media_lookup_error(e, "Media not found"))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        ensure_room_accepts_live_publish(&room)?;

        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        if media.creator_id != Some(uid) {
            self.room_service
                .check_permission(
                    &rid,
                    &uid,
                    synctv_core::models::RoomPermission::MANAGE_LIVE_STREAMS,
                )
                .await
                .map_err(Self::map_room_access_error)?;
        }

        let publish_key_service = self
            .publish_key_service
            .as_ref()
            .ok_or_else(publish_key_service_unavailable_error)?;

        let publish_key = publish_key_service
            .generate_publish_key(&rid, &media_id, &uid)
            .map_err(|e| ApiError::Internal(format!("Failed to generate publish key: {e}")))?;

        let (room_id_key, media_id_key) = codec.encode_public_stream_ids(rid, media_id)?;
        let rtmp_url = build_publish_rtmp_url(&self.runtime_settings, &room_id_key);
        let stream_key = format!("{}?token={}", media_id_key, publish_key.token);

        Ok(CreatePublishKeyResponse {
            publish_key: publish_key.token,
            rtmp_url,
            stream_key,
            expires_at: publish_key.expires_at,
        })
    }

    pub async fn validate_live_token(
        &self,
        token: &str,
        room_id: &str,
    ) -> Result<UserId, ApiError> {
        let bearer_token = format!("Bearer {token}");
        let user_id = self
            .jwt_validator
            .validate_authorization_header_extract_user_id(&bearer_token)
            .map_err(|_| {
                ApiError::Authentication(
                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                )
            })?;

        let codec = RtmpProviderApiCodec::new(&self.public_id_codec);
        let rid = codec.parse_room_id(room_id)?;
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

    pub async fn get_stream_info(
        &self,
        user_id: &UserId,
        room_id: &str,
        media_id: &str,
    ) -> Result<GetStreamInfoResponse, ApiError> {
        let codec = RtmpProviderApiCodec::new(&self.public_id_codec);
        let (room_id, media_id) = codec.build_get_stream_info_request(GetStreamInfoRequest {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
        })?;
        let uid = *user_id;
        let rid = codec.parse_room_id(&room_id)?;

        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let room_id_key = rid.to_string();
        let media_id_key = media_id.to_string();
        codec
            .fetch_stream_info(infrastructure, &room_id_key, &media_id_key)
            .await
    }

    #[must_use]
    pub const fn live_infrastructure(
        &self,
    ) -> Option<&Arc<synctv_livestream::LiveStreamingInfrastructure>> {
        self.live_streaming_infrastructure.as_ref()
    }
}

impl AdminApiImpl {
    pub async fn create_rtmp_publish_key(
        &self,
        req: CreatePublishKeyRequest,
        actor_user_id: &UserId,
    ) -> Result<CreatePublishKeyResponse, ApiError> {
        let codec = RtmpProviderApiCodec::new(&self.public_id_codec);
        let (room_id, media_id) = codec.build_create_publish_key_request(req)?;
        let rid = codec.parse_room_id(&room_id)?;

        let _media = self
            .room_service
            .media_service()
            .get_room_media(&rid, &media_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to load media: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        ensure_room_accepts_live_publish(&room)?;

        let publish_key_service = self
            .publish_key_service
            .as_ref()
            .ok_or_else(publish_key_service_unavailable_error)?;

        let publish_key = publish_key_service
            .generate_publish_key(&rid, &media_id, actor_user_id)
            .map_err(|e| ApiError::Internal(format!("Failed to generate publish key: {e}")))?;
        let token = publish_key.token.clone();
        let (room_id_key, media_id_key) = codec.encode_public_stream_ids(rid, media_id)?;
        let stream_key = format!("{media_id_key}?token={token}");

        Ok(CreatePublishKeyResponse {
            publish_key: token,
            rtmp_url: build_publish_rtmp_url(&self.runtime_settings, &room_id_key),
            stream_key,
            expires_at: publish_key.expires_at,
        })
    }

    pub async fn get_rtmp_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<GetStreamInfoResponse, ApiError> {
        let codec = RtmpProviderApiCodec::new(&self.public_id_codec);
        let (room_id, media_id) = codec.build_get_stream_info_request(GetStreamInfoRequest {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
        })?;
        let rid = codec.parse_room_id(&room_id)?;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let room_id_key = rid.to_string();
        let media_id_key = media_id.to_string();
        codec
            .fetch_stream_info(infrastructure, &room_id_key, &media_id_key)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn api_err<T>(result: Result<T, ApiError>) -> TestResult<ApiError> {
        match result {
            Ok(_) => Err(test_error("expected API error result")),
            Err(error) => Ok(error),
        }
    }

    fn codec_ok<T>(result: Result<T, String>) -> TestResult<T> {
        result.map_err(test_error)
    }

    #[test]
    fn build_get_stream_info_request_rejects_invalid_media_id() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let codec = RtmpProviderApiCodec::new(&public_id_codec);
        let err =
            api_err(
                codec.build_get_stream_info_request(GetStreamInfoRequest {
                    room_id: codec_ok(
                        public_id_codec
                            .encode_room_id(synctv_core::models::RoomId::expect_positive(123)),
                    )?,
                    media_id: "bad-media".to_string(),
                }),
            )?;

        assert!(
            err.is_invalid_argument() && err.message().contains("media_id"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn build_get_stream_info_request_accepts_valid_request() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let codec = RtmpProviderApiCodec::new(&public_id_codec);
        let expected_room_id = codec_ok(
            public_id_codec.encode_room_id(synctv_core::models::RoomId::expect_positive(123)),
        )?;
        let expected_media_id = synctv_core::models::MediaId::expect_positive(123);
        let (room_id, media_id) =
            api_ok(codec.build_get_stream_info_request(GetStreamInfoRequest {
                room_id: expected_room_id.clone(),
                media_id: codec_ok(public_id_codec.encode_media_id(expected_media_id))?,
            }))?;

        assert_eq!(room_id, expected_room_id);
        assert_eq!(media_id, expected_media_id);
        Ok(())
    }

    #[test]
    fn ensure_room_accepts_live_publish_rejects_banned_room() {
        let mut room = Room::new("Banned live room".to_string(), UserId::expect_positive(1));
        room.ban();

        let err = ensure_room_accepts_live_publish(&room)
            .expect_err("banned rooms must not issue publish keys");

        assert!(
            matches!(&err, ApiError::Authorization(message) if message == "Room is banned"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn ensure_room_accepts_live_publish_rejects_closed_room() {
        let room = Room::new_with_status(
            "Closed live room".to_string(),
            String::new(),
            UserId::expect_positive(1),
            synctv_core::models::RoomStatus::Closed,
        );

        let err = ensure_room_accepts_live_publish(&room)
            .expect_err("closed rooms must not issue publish keys");

        assert!(
            matches!(&err, ApiError::Authorization(message) if message == "Room is not active"),
            "unexpected error: {err:?}"
        );
    }
}
