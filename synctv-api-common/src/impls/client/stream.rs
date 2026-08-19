use synctv_core::{
    models::{MediaId, Room, RoomId, UserId},
    service::{PublishKeyOptions, PublishKeyType as CorePublishKeyType},
};

use crate::impls::{ApiError, ClientApiImpl};
use synctv_proto::client::{
    CreateRoomPublishKeyRequest, CreateRoomPublishKeyResponse, GetRoomStreamInfoRequest,
    GetRoomStreamInfoResponse, KickRoomStreamRequest, ListRoomStreamsRequest,
    ListRoomStreamsResponse, PublishKeyType, RoomStreamPublisherInfo, SortDirection, StreamEntry,
};

const LIVESTREAM_UNAVAILABLE_MESSAGE: &str = "Live streaming is not available on this server.";
const PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE: &str =
    "Publish key service is not available on this server.";
const DEFAULT_ROOM_STREAMS_PAGE: i32 = 1;
const DEFAULT_ROOM_STREAMS_PAGE_SIZE: i32 = 50;

fn positive_i32_to_usize(value: i32, field: &'static str) -> Result<usize, ApiError> {
    let value = u32::try_from(value)
        .map_err(|_| ApiError::Internal(format!("{field} must be positive")))?;
    usize::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

pub fn build_room_streams_request(
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

pub fn build_room_streams_response(
    media_ids: Vec<MediaId>,
    req: &ListRoomStreamsRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<ListRoomStreamsResponse, ApiError> {
    let mut media_ids: Vec<String> = media_ids
        .into_iter()
        .map(|media_id| {
            public_id_codec.encode_media_id(media_id).map_err(|error| {
                ApiError::Internal(format!("Failed to encode active stream media id: {error}"))
            })
        })
        .collect::<Result<_, _>>()?;
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

    let page = positive_i32_to_usize(req.page, "page")?;
    let page_size = positive_i32_to_usize(req.page_size, "page_size")?;
    let total = i32::try_from(media_ids.len())
        .map_err(|_| ApiError::Internal("active stream count exceeds i32 range".to_string()))?;
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

    Ok(ListRoomStreamsResponse { streams, total })
}

pub fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
}

fn publish_key_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable(PUBLISH_KEY_SERVICE_UNAVAILABLE_MESSAGE.to_string())
}

pub(crate) fn ensure_room_accepts_live_publish(room: &Room) -> Result<(), ApiError> {
    if room.is_banned {
        return Err(ApiError::Authorization("Room is banned".to_string()));
    }

    if !room.status.is_active() {
        return Err(ApiError::Authorization("Room is not active".to_string()));
    }

    Ok(())
}

fn build_publish_rtmp_url(runtime_settings: &crate::ApiRuntimeSettings, room_id: &str) -> String {
    let rtmp_host = runtime_settings.public_rtmp_host();
    let rtmp_port = runtime_settings.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/{room_id}")
}

async fn filter_usable_stream_media_ids(
    room_service: &synctv_core::service::RoomService,
    room_id: &RoomId,
    media_ids: Vec<MediaId>,
) -> Result<Vec<MediaId>, ApiError> {
    let media = room_service
        .media_service()
        .get_room_media_batch(room_id, &media_ids)
        .await
        .map_err(ApiError::from)?;
    let mut usable = Vec::with_capacity(media.len());
    for media in media {
        match room_service.ensure_client_usable_media(&media).await {
            Ok(()) => usable.push(media.id),
            Err(synctv_core::Error::Authorization(_)) => {}
            Err(error) => return Err(ApiError::from(error)),
        }
    }
    Ok(usable)
}

pub(crate) fn publish_key_options(
    req: &CreateRoomPublishKeyRequest,
) -> Result<Option<PublishKeyOptions>, ApiError> {
    let key_type = match PublishKeyType::try_from(req.r#type)
        .map_err(|_| ApiError::InvalidInput("publish key type is invalid".to_string()))?
    {
        PublishKeyType::SingleUse => CorePublishKeyType::SingleUse,
        PublishKeyType::Expiring => CorePublishKeyType::Expiring,
        PublishKeyType::Permanent => CorePublishKeyType::Permanent,
        PublishKeyType::Unspecified if req.expires_at.is_none() => return Ok(None),
        PublishKeyType::Unspecified => {
            return Err(ApiError::InvalidInput(
                "publish key type is required when expiration is provided".to_string(),
            ));
        }
    };

    Ok(Some(PublishKeyOptions {
        key_type,
        expires_at: req.expires_at,
    }))
}

pub(crate) fn issue_room_publish_key(
    publish_key_service: &dyn synctv_core::service::StreamingPublishKeyService,
    runtime_settings: &crate::ApiRuntimeSettings,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    room_id: RoomId,
    media_id: MediaId,
    actor_user_id: &UserId,
    options: Option<PublishKeyOptions>,
) -> Result<CreateRoomPublishKeyResponse, ApiError> {
    let publish_key = match options {
        Some(options) => publish_key_service.generate_publish_key_with_options(
            &room_id,
            &media_id,
            actor_user_id,
            options,
        ),
        None => publish_key_service.generate_publish_key(&room_id, &media_id, actor_user_id),
    }
    .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let room_id = public_id_codec
        .encode_room_id(room_id)
        .map_err(|error| ApiError::Internal(format!("Failed to encode room id: {error}")))?;
    let media_id = public_id_codec
        .encode_media_id(media_id)
        .map_err(|error| ApiError::Internal(format!("Failed to encode media id: {error}")))?;
    let stream_key = format!("{media_id}?token={}", publish_key.token);

    Ok(CreateRoomPublishKeyResponse {
        publish_key: publish_key.token,
        rtmp_url: build_publish_rtmp_url(runtime_settings, &room_id),
        stream_key,
        expires_at: publish_key.expires_at,
        r#type: match publish_key.key_type {
            CorePublishKeyType::SingleUse => PublishKeyType::SingleUse as i32,
            CorePublishKeyType::Expiring => PublishKeyType::Expiring as i32,
            CorePublishKeyType::Permanent => PublishKeyType::Permanent as i32,
        },
    })
}

pub async fn fetch_stream_info(
    infrastructure: &synctv_livestream::LiveStreamingInfrastructure,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    room_id: &str,
    media_id: &str,
) -> Result<GetRoomStreamInfoResponse, ApiError> {
    match infrastructure.find_publisher(room_id, media_id).await {
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
    pub async fn create_room_publish_key(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: CreateRoomPublishKeyRequest,
    ) -> Result<CreateRoomPublishKeyResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let options = publish_key_options(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let media_id = self
            .public_id_codec
            .decode_media_id(&req.media_id)
            .map_err(ApiError::InvalidInput)?;

        let (media, room) = tokio::join!(
            self.room_service
                .media_service()
                .get_room_media(&rid, &media_id),
            self.room_service.get_room(&rid),
        );
        let media = media
            .map_err(|error| Self::map_media_lookup_error(error, "Media not found"))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(ApiError::from)?;
        let room = room.map_err(ApiError::from)?;
        ensure_room_accepts_live_publish(&room)?;

        self.room_service
            .check_membership_with_room(&room, &uid)
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
            .as_deref()
            .ok_or_else(publish_key_service_unavailable_error)?;
        issue_room_publish_key(
            publish_key_service,
            &self.runtime_settings,
            &self.public_id_codec,
            rid,
            media_id,
            &uid,
            options,
        )
    }

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
            .list_streams_for_room(&rid.to_string())
            .await
            .map_err(|error| Self::map_livestream_backend_error(&*error))?;

        let media_ids = media_ids
            .into_iter()
            .map(|id| id.parse::<MediaId>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApiError::Internal(format!("Invalid stream media id: {error}")))?;
        let media_ids = filter_usable_stream_media_ids(&self.room_service, &rid, media_ids).await?;

        build_room_streams_response(media_ids, &req, &self.public_id_codec)
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
        let media = self
            .room_service
            .media_service()
            .get_room_media(&rid, &media_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(ApiError::from)?;

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
                synctv_core::models::RoomPermission::MANAGE_LIVE_STREAMS,
            )
            .await
            .map_err(Self::map_room_access_error)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;
        if !infrastructure
            .is_stream_active(&rid.to_string(), &media_id.to_string())
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
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
    use super::{
        build_room_streams_request, build_room_streams_response, ensure_room_accepts_live_publish,
        filter_usable_stream_media_ids, publish_key_options,
    };
    use crate::impls::ApiError;

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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn stream_list_filters_media_from_banned_creators() -> TestResult {
        use synctv_core::{
            models::{FromProviderParams, Media, SignupMethod, SourceProvider, User},
            repository::{MediaRepository, UserRepository},
        };
        use synctv_core_testing::{
            create_test_pool, create_test_room_service, direct_url_media_source_config,
        };

        let (_postgres, pool) = create_test_pool().await;
        let user_repository = UserRepository::new(pool.clone());
        let owner = user_repository
            .create(&User::new(
                "stream_filter_owner".to_string(),
                SignupMethod::AdminCreated,
            ))
            .await?;
        let banned_creator = user_repository
            .create(&User::new(
                "stream_filter_banned_creator".to_string(),
                SignupMethod::AdminCreated,
            ))
            .await?;
        let room_service = create_test_room_service(pool.clone());
        let room = room_service
            .create_room(
                "stream lifecycle filter".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await?
            .0;
        room_service
            .join_room(room.id, banned_creator.id, None)
            .await?;
        let media_repository = MediaRepository::new(pool);
        let media = |creator_id, name: &str| {
            Media::from_provider_with_params(FromProviderParams {
                playlist_id: None,
                room_id: room.id,
                creator_id: Some(creator_id),
                name: name.to_string(),
                description: String::new(),
                source_provider: SourceProvider::DirectUrl,
                source_config: direct_url_media_source_config("https://example.com/live.m3u8"),
                provider_instance_name: None,
                position: 0.0,
            })
        };
        let available = media_repository
            .create(&media(owner.id, "available"))
            .await?;
        let unavailable = media_repository
            .create(&media(banned_creator.id, "unavailable"))
            .await?;
        room_service
            .ban_user_and_reset_owned_playback_with_outbox(
                &banned_creator.id,
                None,
                Some("stream filter test".to_string()),
                None,
                &[],
            )
            .await?;

        let filtered = filter_usable_stream_media_ids(
            &room_service,
            &room.id,
            vec![available.id, unavailable.id],
        )
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

        assert_eq!(filtered, vec![available.id]);
        Ok(())
    }

    #[test]
    fn build_room_streams_request_rejects_invalid_proto_request() -> TestResult {
        let error = api_err(build_room_streams_request(
            synctv_proto::client::ListRoomStreamsRequest {
                page: -1,
                page_size: 101,
                search: "a".repeat(101),
                sort_by: 0,
                sort_direction: 0,
            },
        ))?;

        assert!(error.is_invalid_argument(), "{error:?}");
        let message = error.message();
        assert!(message.contains("page"), "{message}");
        assert!(message.contains("page_size"), "{message}");
        assert!(message.contains("search"), "{message}");
        Ok(())
    }

    #[test]
    fn publish_key_options_preserves_legacy_default() -> TestResult {
        let options = api_ok(publish_key_options(
            &synctv_proto::client::CreateRoomPublishKeyRequest {
                media_id: "med_AbC123".to_string(),
                ..Default::default()
            },
        ))?;

        assert!(options.is_none());
        Ok(())
    }

    #[test]
    fn publish_key_options_rejects_ambiguous_legacy_expiration() -> TestResult {
        let error = api_err(publish_key_options(
            &synctv_proto::client::CreateRoomPublishKeyRequest {
                media_id: "med_AbC123".to_string(),
                expires_at: Some(1_800_000_000),
                ..Default::default()
            },
        ))?;

        assert!(error.is_invalid_argument(), "{error:?}");
        Ok(())
    }

    #[test]
    fn build_room_streams_request_normalizes_defaults() -> TestResult {
        let req = api_ok(build_room_streams_request(
            synctv_proto::client::ListRoomStreamsRequest {
                page: 0,
                page_size: 0,
                search: " beta ".to_string(),
                sort_by: 1,
                sort_direction: 2,
            },
        ))?;

        assert_eq!(req.page, 1);
        assert_eq!(req.page_size, 50);
        assert_eq!(req.search, " beta ");
        assert_eq!(req.sort_by, 1);
        assert_eq!(req.sort_direction, 2);
        Ok(())
    }

    #[test]
    fn build_room_streams_response_applies_search_sort_and_pagination() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let media_ids = vec![
            synctv_core::models::MediaId::expect_positive(201),
            synctv_core::models::MediaId::expect_positive(202),
            synctv_core::models::MediaId::expect_positive(203),
        ];
        let mut expected_ids = media_ids
            .iter()
            .map(|media_id| codec_ok(public_id_codec.encode_media_id(*media_id)))
            .collect::<TestResult<Vec<_>>>()?;
        expected_ids.sort_unstable();
        expected_ids.reverse();
        let response = api_ok(build_room_streams_response(
            media_ids,
            &synctv_proto::client::ListRoomStreamsRequest {
                page: 2,
                page_size: 1,
                search: String::new(),
                sort_by: synctv_proto::client::RoomStreamListSortBy::MediaId as i32,
                sort_direction: synctv_proto::client::SortDirection::Desc as i32,
            },
            &public_id_codec,
        ))?;

        assert_eq!(response.total, 3);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, expected_ids[1]);
        assert!(response.streams[0].active);
        Ok(())
    }

    #[test]
    fn live_publish_rejects_banned_room() {
        let mut room = synctv_core::models::Room::new(
            "Banned live room".to_string(),
            synctv_core::models::UserId::expect_positive(1),
        );
        room.ban();

        let error = ensure_room_accepts_live_publish(&room)
            .expect_err("banned rooms must not issue publish keys");
        assert!(
            matches!(&error, ApiError::Authorization(message) if message == "Room is banned"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn live_publish_rejects_closed_room() {
        let room = synctv_core::models::Room::new_with_status(
            "Closed live room".to_string(),
            String::new(),
            synctv_core::models::UserId::expect_positive(1),
            synctv_core::models::RoomStatus::Closed,
        );

        let error = ensure_room_accepts_live_publish(&room)
            .expect_err("closed rooms must not issue publish keys");
        assert!(
            matches!(&error, ApiError::Authorization(message) if message == "Room is not active"),
            "unexpected error: {error:?}"
        );
    }
}
