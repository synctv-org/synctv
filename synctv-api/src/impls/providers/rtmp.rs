use std::sync::Arc;
use synctv_core::models::{MediaId, Room, UserId};

use crate::impls::{AdminApiImpl, ApiError, ClientApiImpl};
use crate::proto::providers::rtmp::{
    CreatePublishKeyRequest, CreatePublishKeyResponse, GetStreamInfoRequest, GetStreamInfoResponse,
    StreamPublisherInfo,
};

const LIVESTREAM_UNAVAILABLE_MESSAGE: &str = "Live streaming is not available on this server.";
const PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE: &str =
    "Publish key service is not available on this server.";

fn parse_room_id(
    room_id: &str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::parse_room_id_param(room_id, "room_id", public_id_codec)
}

#[must_use]
pub fn build_publish_rtmp_url(config: &synctv_core::Config, room_id: &str) -> String {
    let rtmp_host = config.public_rtmp_host();
    let rtmp_port = config.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/{room_id}")
}

pub fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
}

pub fn publish_key_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE.to_string())
}

fn build_create_publish_key_request(
    req: CreatePublishKeyRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<(String, MediaId), ApiError> {
    crate::impls::validate_proto_request(&req)?;
    Ok((
        req.room_id,
        crate::impls::proto_validated_media_id(req.media_id, public_id_codec)?,
    ))
}

fn build_get_stream_info_request(
    req: GetStreamInfoRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<(String, MediaId), ApiError> {
    crate::impls::validate_proto_request(&req)?;
    Ok((
        req.room_id,
        crate::impls::proto_validated_media_id(req.media_id, public_id_codec)?,
    ))
}

fn encode_public_stream_ids(
    public_id_codec: &crate::PublicIdCodec,
    room_id: synctv_core::models::RoomId,
    media_id: MediaId,
) -> Result<(String, String), ApiError> {
    let room_id = public_id_codec
        .encode_room_id(room_id)
        .map_err(|e| ApiError::Internal(format!("Failed to encode room id: {e}")))?;
    let media_id = public_id_codec
        .encode_media_id(media_id)
        .map_err(|e| ApiError::Internal(format!("Failed to encode media id: {e}")))?;
    Ok((room_id, media_id))
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

pub async fn fetch_stream_info(
    infrastructure: &synctv_livestream::api::LiveStreamingInfrastructure,
    public_id_codec: &crate::PublicIdCodec,
    room_id: &str,
    media_id: &str,
) -> Result<GetStreamInfoResponse, ApiError> {
    match infrastructure
        .registry()
        .get_publisher(room_id, media_id)
        .await
    {
        Ok(Some(pub_info)) => {
            let user_id = public_id_codec
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

impl ClientApiImpl {
    pub async fn create_publish_key(
        &self,
        user_id: &UserId,
        req: CreatePublishKeyRequest,
    ) -> Result<CreatePublishKeyResponse, ApiError> {
        let uid = *user_id;
        let (room_id, media_id) = build_create_publish_key_request(req, &self.public_id_codec)?;
        let rid = parse_room_id(&room_id, &self.public_id_codec)?;

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
                    synctv_core::models::RoomPermission::LIVE_CONTROL,
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

        let (room_id_key, media_id_key) =
            encode_public_stream_ids(&self.public_id_codec, rid, media_id)?;
        let rtmp_url = build_publish_rtmp_url(&self.config, &room_id_key);
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
            .validate_http_extract_user_id(&bearer_token)
            .map_err(|_| {
                ApiError::Authentication(
                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                )
            })?;

        let rid = parse_room_id(room_id, &self.public_id_codec)?;
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
        let (room_id, media_id) = build_get_stream_info_request(
            GetStreamInfoRequest {
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
            },
            &self.public_id_codec,
        )?;
        let uid = *user_id;
        let rid = parse_room_id(&room_id, &self.public_id_codec)?;

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
        fetch_stream_info(
            infrastructure,
            &self.public_id_codec,
            &room_id_key,
            &media_id_key,
        )
        .await
    }

    #[must_use]
    pub const fn live_infrastructure(
        &self,
    ) -> Option<&Arc<synctv_livestream::api::LiveStreamingInfrastructure>> {
        self.live_streaming_infrastructure.as_ref()
    }
}

impl AdminApiImpl {
    pub async fn create_rtmp_publish_key(
        &self,
        req: CreatePublishKeyRequest,
        actor_user_id: &UserId,
    ) -> Result<CreatePublishKeyResponse, ApiError> {
        let (room_id, media_id) = build_create_publish_key_request(req, &self.public_id_codec)?;
        let rid = parse_room_id(&room_id, &self.public_id_codec)?;

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
        let (room_id_key, media_id_key) =
            encode_public_stream_ids(&self.public_id_codec, rid, media_id)?;
        let stream_key = format!("{media_id_key}?token={token}");

        Ok(CreatePublishKeyResponse {
            publish_key: token,
            rtmp_url: build_publish_rtmp_url(&self.config, &room_id_key),
            stream_key,
            expires_at: publish_key.expires_at,
        })
    }

    pub async fn get_rtmp_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<GetStreamInfoResponse, ApiError> {
        let (room_id, media_id) = build_get_stream_info_request(
            GetStreamInfoRequest {
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
            },
            &self.public_id_codec,
        )?;
        let rid = parse_room_id(&room_id, &self.public_id_codec)?;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let room_id_key = rid.to_string();
        let media_id_key = media_id.to_string();
        fetch_stream_info(
            infrastructure,
            &self.public_id_codec,
            &room_id_key,
            &media_id_key,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_get_stream_info_request_rejects_invalid_media_id() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let err = build_get_stream_info_request(
            GetStreamInfoRequest {
                room_id: public_id_codec
                    .encode_room_id(synctv_core::models::RoomId::expect_positive(123))
                    .unwrap(),
                media_id: "bad-media".to_string(),
            },
            &public_id_codec,
        )
        .expect_err("get stream info should enforce the RTMP provider proto contract");

        assert!(
            matches!(&err, ApiError::InvalidInput(message) if message.contains("media_id")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn build_get_stream_info_request_accepts_valid_request() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let expected_room_id = public_id_codec
            .encode_room_id(synctv_core::models::RoomId::expect_positive(123))
            .unwrap();
        let expected_media_id = synctv_core::models::MediaId::expect_positive(123);
        let (room_id, media_id) = build_get_stream_info_request(
            GetStreamInfoRequest {
                room_id: expected_room_id.clone(),
                media_id: public_id_codec.encode_media_id(expected_media_id).unwrap(),
            },
            &public_id_codec,
        )
        .expect("valid stream info request should be accepted");

        assert_eq!(room_id, expected_room_id);
        assert_eq!(media_id, expected_media_id);
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
