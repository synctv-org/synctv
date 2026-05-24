use synctv_core::models::{MediaId, UserId};

use crate::impls::{ApiError, ClientApiImpl};
use crate::proto::client::{
    GetRoomStreamInfoRequest, GetRoomStreamInfoResponse, KickRoomStreamRequest,
    ListRoomStreamsRequest, ListRoomStreamsResponse, RoomStreamPublisherInfo, SortDirection,
    StreamEntry,
};

const LIVESTREAM_UNAVAILABLE_MESSAGE: &str = "Live streaming is not available on this server.";
const DEFAULT_ROOM_STREAMS_PAGE: i32 = 1;
const DEFAULT_ROOM_STREAMS_PAGE_SIZE: i32 = 50;

pub(crate) fn build_room_streams_request(
    req: ListRoomStreamsRequest,
) -> Result<ListRoomStreamsRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(ListRoomStreamsRequest {
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

#[must_use]
pub(crate) fn build_room_streams_response(
    media_ids: Vec<MediaId>,
    req: &ListRoomStreamsRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> ListRoomStreamsResponse {
    let mut media_ids: Vec<String> = media_ids
        .into_iter()
        .filter_map(|media_id| match public_id_codec.encode_media_id(media_id) {
            Ok(public_id) => Some(public_id),
            Err(error) => {
                tracing::warn!(media_id = %media_id, error = %error, "Skipping invalid stream media id");
                None
            }
        })
        .collect();
    if let Some(search) = (!req.search.trim().is_empty()).then(|| req.search.to_ascii_lowercase()) {
        media_ids.retain(|media_id| media_id.to_ascii_lowercase().contains(search.as_str()));
    }

    media_ids.sort_unstable();

    if matches!(
        SortDirection::try_from(req.sort_direction),
        Ok(SortDirection::Desc)
    ) {
        media_ids.reverse();
    }

    let page = usize::try_from(req.page.max(1)).unwrap_or(usize::MAX);
    let page_size = usize::try_from(req.page_size.max(1)).unwrap_or(usize::MAX);
    let total = i32::try_from(media_ids.len()).unwrap_or(i32::MAX);
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    let streams = media_ids
        .into_iter()
        .skip(offset)
        .take(page_size)
        .map(|media_id| StreamEntry {
            media_id,
            active: true,
        })
        .collect();

    ListRoomStreamsResponse { streams, total }
}

pub(crate) fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
}

pub(crate) async fn fetch_stream_info(
    infrastructure: &synctv_livestream::api::LiveStreamingInfrastructure,
    public_id_codec: &crate::PublicIdCodec,
    room_id: &str,
    media_id: &str,
) -> Result<GetRoomStreamInfoResponse, ApiError> {
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
            Ok(GetRoomStreamInfoResponse {
                active: true,
                publisher: Some(RoomStreamPublisherInfo {
                    user_id,
                    started_at: pub_info.started_at.timestamp(),
                }),
            })
        }
        Ok(None) => Ok(GetRoomStreamInfoResponse {
            active: false,
            publisher: None,
        }),
        Err(error) => Err(ApiError::Internal(format!(
            "Failed to get stream info: {error}"
        ))),
    }
}

impl ClientApiImpl {
    pub async fn list_room_streams(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: ListRoomStreamsRequest,
    ) -> Result<ListRoomStreamsResponse, ApiError> {
        let req = build_room_streams_request(req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

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
            .list_streams_for_room(&rid.to_string())
            .await
            .map_err(|error| Self::map_livestream_backend_error(&*error))?;

        let media_ids = media_ids
            .into_iter()
            .map(|id| id.parse::<MediaId>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApiError::Internal(format!("Invalid stream media id: {error}")))?;

        Ok(build_room_streams_response(
            media_ids,
            &req,
            &self.public_id_codec,
        ))
    }

    pub async fn get_room_stream_info(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: GetRoomStreamInfoRequest,
    ) -> Result<GetRoomStreamInfoResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let media_id = self
            .public_id_codec
            .decode_media_id(&req.media_id)
            .map_err(ApiError::InvalidInput)?;

        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        fetch_stream_info(
            infrastructure,
            &self.public_id_codec,
            &rid.to_string(),
            &media_id.to_string(),
        )
        .await
    }

    pub async fn kick_room_stream(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: KickRoomStreamRequest,
    ) -> Result<(), ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let media_id = self
            .public_id_codec
            .decode_media_id(&req.media_id)
            .map_err(ApiError::InvalidInput)?;

        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::LIVE_CONTROL,
            )
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;
        if !infrastructure
            .registry()
            .is_stream_active(&rid.to_string(), &media_id.to_string())
            .await
            .map_err(|error| {
                ApiError::Internal(format!("Failed to check active stream: {error}"))
            })?
        {
            return Err(ApiError::NotFound("Active stream not found".to_string()));
        }

        self.realtime_lifecycle
            .kick_stream(&rid, &media_id, &req.reason)
            .await
            .map_err(|error| crate::impls::map_livestream_stream_error(&error))
    }
}
#[cfg(test)]
mod tests {
    use super::{build_room_streams_request, build_room_streams_response};
    use crate::impls::ApiError;

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
    fn build_room_streams_response_applies_search_sort_and_pagination() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let media_ids = vec![
            synctv_core::models::MediaId::expect_positive(201),
            synctv_core::models::MediaId::expect_positive(202),
            synctv_core::models::MediaId::expect_positive(203),
        ];
        let mut expected_ids = media_ids
            .iter()
            .map(|media_id| public_id_codec.encode_media_id(*media_id).unwrap())
            .collect::<Vec<_>>();
        expected_ids.sort_unstable();
        expected_ids.reverse();
        let response = build_room_streams_response(
            media_ids,
            &crate::proto::client::ListRoomStreamsRequest {
                page: 2,
                page_size: 1,
                search: String::new(),
                sort_by: crate::proto::client::RoomStreamListSortBy::MediaId as i32,
                sort_direction: crate::proto::client::SortDirection::Desc as i32,
            },
            &public_id_codec,
        );

        assert_eq!(response.total, 3);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, expected_ids[1]);
        assert!(response.streams[0].active);
    }
}
