use super::{
    build_get_playback_request, sse_event_from_server_message, sse_event_id_from_resource_event,
    watch_after_event_sequence, CancelOnDropStream, ChatAttachmentObjectQuery, GetPlaybackQuery,
    MediaCoverObjectQuery, MediaThumbnailObjectQuery, PlaylistCoverObjectQuery,
    RoomCoverObjectQuery, WatchPlaybackQuery, WatchPlaybackStateQuery, WatchPlaylistItemsQuery,
    WatchQuery,
};
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use synctv_proto::client::{
    AddMediaBatchRequest, CreatePlaylistRequest, DeleteEntriesRequest, DeleteMediaQuery,
    DeletePlaylistQuery, EditMediaRequest, GetChatHistoryRequest, GetHotRoomsRequest,
    JoinRoomRequest, ListPlaylistItemsRequest, ListPlaylistsRequest, MoveMediaRequest,
    MovePlaylistRequest, StartRoomPasswordLoginRequest, UpdatePlaybackStateRequest,
    UpdatePlaylistRequest,
};
use tower::ServiceExt;

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn app_ok<T>(result: Result<T, crate::http::AppError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn app_err<T>(result: Result<T, crate::http::AppError>) -> TestResult<crate::http::AppError> {
    match result {
        Ok(_) => Err(test_error("expected HTTP route error")),
        Err(error) => Ok(error),
    }
}

#[test]
fn test_watch_after_event_sequence_prefers_last_event_id() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("42"));

    let sequence = app_ok(watch_after_event_sequence(&headers, Some(7)))?;

    assert_eq!(sequence, Some(42));
    Ok(())
}

#[test]
fn test_watch_after_event_sequence_rejects_invalid_last_event_id() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("event-42"));

    let error = app_err(watch_after_event_sequence(&headers, Some(7)))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("Last-Event-ID"));
    Ok(())
}

#[test]
fn test_watch_after_event_sequence_rejects_negative_last_event_id() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("-1"));

    let error = app_err(watch_after_event_sequence(&headers, Some(7)))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("event sequence"));
    Ok(())
}

#[test]
fn test_watch_after_event_sequence_rejects_negative_query_sequence() -> TestResult {
    let headers = HeaderMap::new();

    let error = app_err(watch_after_event_sequence(&headers, Some(-1)))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("event sequence"));
    Ok(())
}

#[test]
fn test_watch_after_event_sequence_rejects_non_utf8_last_event_id() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_bytes(&[0xff])?);

    let error = app_err(watch_after_event_sequence(&headers, Some(7)))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("Last-Event-ID"));
    Ok(())
}

#[test]
fn test_build_get_playback_request_parses_generic_profile_query() -> TestResult {
    let request = app_ok(build_get_playback_request(&GetPlaybackQuery {
        stream_preference: Some(synctv_proto::client::PlaybackStreamPreference::Transcode as i32),
        max_streaming_bitrate: Some(8_000_000),
        max_audio_channels: Some(2),
        video_codecs: Some(format!(
            "{},{}",
            synctv_proto::client::PlaybackVideoCodec::H264 as i32,
            synctv_proto::client::PlaybackVideoCodec::Av1 as i32
        )),
        containers: Some(format!(
            "{},{}",
            synctv_proto::client::PlaybackContainer::Mp4 as i32,
            synctv_proto::client::PlaybackContainer::Webm as i32
        )),
        audio_capability: Some(synctv_proto::client::PlaybackAudioCapability::Surround as i32),
        subtitle_preference: Some(
            synctv_proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal as i32,
        ),
    }))?;

    let profile = request
        .playback_client_profile
        .ok_or_else(|| test_error("query should produce playback client profile"))?;
    assert_eq!(
        profile.stream_preference,
        synctv_proto::client::PlaybackStreamPreference::Transcode as i32
    );
    assert_eq!(profile.max_streaming_bitrate, Some(8_000_000));
    assert_eq!(profile.max_audio_channels, Some(2));
    assert_eq!(
        profile.supported_video_codecs,
        vec![
            synctv_proto::client::PlaybackVideoCodec::H264 as i32,
            synctv_proto::client::PlaybackVideoCodec::Av1 as i32,
        ]
    );
    assert_eq!(
        profile.supported_containers,
        vec![
            synctv_proto::client::PlaybackContainer::Mp4 as i32,
            synctv_proto::client::PlaybackContainer::Webm as i32,
        ]
    );
    assert_eq!(
        profile.audio_capability,
        synctv_proto::client::PlaybackAudioCapability::Surround as i32
    );
    assert_eq!(
        profile.subtitle_preference,
        synctv_proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal as i32
    );
    Ok(())
}

#[test]
fn test_build_get_playback_request_omits_profile_when_query_is_empty() -> TestResult {
    let request = app_ok(build_get_playback_request(&GetPlaybackQuery::default()))?;

    assert!(request.playback_client_profile.is_none());
    Ok(())
}

#[test]
fn test_handwritten_room_queries_ignore_unknown_fields() {
    let playback_query =
        serde_urlencoded::from_str::<GetPlaybackQuery>("streamPreference=2&extra=true")
            .expect("playback query should ignore unknown fields");
    assert_eq!(playback_query.stream_preference, Some(2));
    let watch_query =
        serde_urlencoded::from_str::<WatchQuery>("format=json&afterEventSequence=12&extra=true")
            .expect("watch query should ignore unknown fields");
    assert_eq!(watch_query.format.as_deref(), Some("json"));
    assert_eq!(watch_query.after_event_sequence, Some(12));
    let playback_state_watch =
        serde_urlencoded::from_str::<WatchPlaybackStateQuery>("format=json&eventSequence=12")
            .expect("watch playback state should accept known event sequence");
    assert_eq!(playback_state_watch.format.as_deref(), Some("json"));
    assert_eq!(playback_state_watch.event_sequence, Some(12));
    let playlist_items_with_extra = serde_urlencoded::from_str::<WatchPlaylistItemsQuery>(
        "format=json&afterEventSequence=12&page=1&pageSize=25&playlistId=pl_1&extra=true",
    )
    .expect("playlist item watch should ignore unknown fields");
    assert_eq!(
        playlist_items_with_extra.playlist_id.as_deref(),
        Some("pl_1")
    );
    let playback_watch_with_extra =
        serde_urlencoded::from_str::<WatchPlaybackQuery>("format=json&media_id=media_1&extra=true")
            .expect("playback watch query should ignore unknown fields");
    assert_eq!(playback_watch_with_extra.format.as_deref(), Some("json"));
    let playback_watch =
        serde_urlencoded::from_str::<WatchPlaybackQuery>("format=json&afterEventSequence=12")
            .expect("watch playback should ignore replay cursors");
    assert_eq!(playback_watch.format.as_deref(), Some("json"));
    let playlist_items_watch = serde_urlencoded::from_str::<WatchPlaylistItemsQuery>(
        "format=json&afterEventSequence=12&page=1&pageSize=25&playlistId=pl_1",
    )
    .expect("playlist item watch should accept list filters");
    assert_eq!(playlist_items_watch.after_event_sequence, Some(12));
    assert_eq!(playlist_items_watch.page, Some(1));
    assert_eq!(playlist_items_watch.page_size, Some(25));
    assert_eq!(playlist_items_watch.playlist_id.as_deref(), Some("pl_1"));
    let playlist_items_watch = serde_urlencoded::from_str::<WatchPlaylistItemsQuery>(
        "format=json&playlistId=pl_1&target=%7B%22alist%22%3A%7B%22relativePath%22%3A%22season-1%22%7D%7D"
    )
    .expect("unsupported dynamic playlist target is ignored by static playlist watch query");
    assert_eq!(playlist_items_watch.playlist_id.as_deref(), Some("pl_1"));
    let chat_attachment =
        serde_urlencoded::from_str::<ChatAttachmentObjectQuery>("token=token&extra=true")
            .expect("chat attachment query should ignore unknown fields");
    assert_eq!(chat_attachment.token, "token");
    let media_cover = serde_urlencoded::from_str::<MediaCoverObjectQuery>("token=token&extra=true")
        .expect("media cover query should ignore unknown fields");
    assert_eq!(media_cover.token, "token");
    let media_thumbnail =
        serde_urlencoded::from_str::<MediaThumbnailObjectQuery>("token=token&extra=true")
            .expect("media thumbnail query should ignore unknown fields");
    assert_eq!(media_thumbnail.token, "token");
    let room_cover = serde_urlencoded::from_str::<RoomCoverObjectQuery>("token=token&extra=true")
        .expect("room cover query should ignore unknown fields");
    assert_eq!(room_cover.token, "token");
    let playlist_cover =
        serde_urlencoded::from_str::<PlaylistCoverObjectQuery>("token=token&extra=true")
            .expect("playlist cover query should ignore unknown fields");
    assert_eq!(playlist_cover.token, "token");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_video_codec() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        stream_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: Some("1,999".to_string()),
        containers: None,
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown codec must be rejected");

    assert!(error.message().contains("videoCodecs"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_stream_preference() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        stream_preference: Some(999),
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown stream preference must be rejected");

    assert!(error.message().contains("streamPreference"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_container() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        stream_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: Some("1,999".to_string()),
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown container must be rejected");

    assert!(error.message().contains("container"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_audio_capability() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        stream_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: Some(999),
        subtitle_preference: None,
    })
    .expect_err("unknown audio capability must be rejected");

    assert!(error.message().contains("audioCapability"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_subtitle_preference() -> TestResult {
    let error = app_err(build_get_playback_request(&GetPlaybackQuery {
        stream_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: None,
        subtitle_preference: Some(999),
    }))?;

    assert!(error.message().contains("subtitlePreference"), "{error:?}");
    Ok(())
}

#[test]
fn test_scalar_query_parsers_reject_invalid_values() {
    assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=definitely").is_err());
    assert!(serde_urlencoded::from_str::<DeletePlaylistQuery>("force=definitely").is_err());
    assert!(serde_urlencoded::from_str::<WatchQuery>("deliveryMode=notify_only").is_err());
    assert!(
        serde_urlencoded::from_str::<WatchPlaybackQuery>("deliveryMode=push_snapshot").is_err()
    );
}

#[test]
fn test_path_scoped_room_requests_override_path_owned_fields() -> TestResult {
    let mut join_req: JoinRoomRequest =
        serde_json::from_str(r#"{"roomId":"room_body","password":"secret"}"#)?;
    join_req.room_id = "room_1".to_string();
    assert_eq!(join_req.room_id, "room_1");
    assert_eq!(join_req.password, "secret");

    let mut login_req: StartRoomPasswordLoginRequest =
        serde_json::from_str(r#"{"roomId":"room_body","credentialRequest":"AQID"}"#)?;
    login_req.room_id = "room_1".to_string();
    assert_eq!(login_req.room_id, "room_1");
    assert_eq!(login_req.credential_request, vec![1, 2, 3]);

    let mut edit_media_req: EditMediaRequest = serde_json::from_str(
        r#"{"mediaId":"med_body","name":"Episode 1","description":"Updated"}"#,
    )?;
    edit_media_req.media_id = "med_1".to_string();
    assert_eq!(edit_media_req.media_id, "med_1");
    assert_eq!(edit_media_req.name, "Episode 1");
    assert_eq!(edit_media_req.description, "Updated");

    let mut update_playlist_req: UpdatePlaylistRequest = serde_json::from_str(
        r#"{"playlistId":"pl_body","name":"Season 1","description":"Updated"}"#,
    )?;
    update_playlist_req.playlist_id = "pl_1".to_string();
    assert_eq!(update_playlist_req.playlist_id, "pl_1");
    assert_eq!(update_playlist_req.name, "Season 1");
    assert_eq!(update_playlist_req.description, "Updated");

    let mut move_playlist_req: MovePlaylistRequest =
        serde_json::from_str(r#"{"playlistId":"pl_body","beforePlaylistId":"pl_anchor"}"#)?;
    move_playlist_req.playlist_id = "pl_1".to_string();
    assert_eq!(move_playlist_req.playlist_id, "pl_1");
    assert!(matches!(
        move_playlist_req.anchor,
        Some(synctv_proto::client::move_playlist_request::Anchor::BeforePlaylistId(id))
            if id == "pl_anchor"
    ));
    assert!(serde_json::from_str::<MovePlaylistRequest>(
        r#"{"beforePlaylistId":"pl_before","afterPlaylistId":"pl_after"}"#
    )
    .is_err());
    Ok(())
}

#[test]
fn test_kick_room_stream_request_overrides_path_media_id() -> TestResult {
    let mut empty: synctv_proto::client::KickRoomStreamRequest = serde_json::from_str("{}")?;
    empty.media_id = "med_1".to_string();
    assert_eq!(empty.media_id, "med_1");
    assert_eq!(empty.reason, "");

    let mut with_reason: synctv_proto::client::KickRoomStreamRequest =
        serde_json::from_str(r#"{"mediaId":"med_body","reason":"moderation"}"#)?;
    with_reason.media_id = "med_1".to_string();
    assert_eq!(with_reason.media_id, "med_1");
    assert_eq!(with_reason.reason, "moderation");
    Ok(())
}

#[test]
fn test_room_playlist_target_path_request_deserializes_proto_field_names() -> TestResult {
    let req: synctv_proto::client::RoomPlaylistTargetPathRequest =
        serde_json::from_str(r#"{"roomId":"room_1","playlistId":"pl_1"}"#)?;

    assert_eq!(req.room_id, "room_1");
    assert_eq!(req.playlist_id, "pl_1");
    Ok(())
}

#[test]
fn test_list_playlists_query_deserializes_proto_defaults() -> TestResult {
    let query: ListPlaylistsRequest = serde_urlencoded::from_str("")?;

    assert_eq!(query.page, 0);
    assert_eq!(query.page_size, 0);
    assert_eq!(query.sort_by, 0);
    assert_eq!(query.sort_direction, 0);
    assert_eq!(query.availability, 0);
    Ok(())
}

#[test]
fn test_list_playlists_query_deserializes_explicit_values() -> TestResult {
    let query: ListPlaylistsRequest =
        serde_urlencoded::from_str("page=2&pageSize=25&sortBy=4&sortDirection=2&availability=2")?;

    assert_eq!(query.page, 2);
    assert_eq!(query.page_size, 25);
    assert_eq!(
        query.sort_by,
        synctv_proto::client::PlaylistListSortBy::UpdatedAt as i32
    );
    assert_eq!(
        query.sort_direction,
        synctv_proto::client::SortDirection::Desc as i32
    );
    assert_eq!(
        query.availability,
        synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32
    );
    Ok(())
}

#[test]
fn test_chat_history_parser_rejects_invalid_limit() {
    assert!(serde_urlencoded::from_str::<GetChatHistoryRequest>("limit=many").is_err());
}

#[test]
fn test_chat_history_query_preserves_limit_for_shared_validation() -> TestResult {
    let req: GetChatHistoryRequest = serde_urlencoded::from_str("limit=101")?;

    assert_eq!(req.limit, 101);
    assert!(synctv_api_common::impls::validate_proto_request(&req).is_err());
    Ok(())
}

#[test]
fn test_hot_rooms_query_preserves_limit_for_shared_validation() -> TestResult {
    let req: GetHotRoomsRequest = serde_urlencoded::from_str("limit=51")?;

    assert_eq!(req.limit, 51);
    assert!(synctv_api_common::impls::validate_proto_request(&req).is_err());
    Ok(())
}

#[test]
fn test_list_playlist_items_body_deserialize_room_root() -> TestResult {
    let json = r"{}";
    let req: ListPlaylistItemsRequest = serde_json::from_str(json)?;
    assert!(req.playlist_id.is_empty());
    assert!(req.target.is_none());
    assert!(req.pagination.is_none());
    assert_eq!(req.page_size, 0);
    assert_eq!(req.availability, 0);
    Ok(())
}

#[test]
fn test_list_playlist_items_body_deserialize_alist_target() -> TestResult {
    let json = r#"{"playlistId":"pl1","target":{"alist":{"relativePath":"season-1"}},"page":{"page":2},"pageSize":25}"#;
    let req: ListPlaylistItemsRequest = serde_json::from_str(json)?;
    assert_eq!(req.playlist_id, "pl1");
    let Some(synctv_proto::client::ProviderTarget {
        target: Some(synctv_proto::client::provider_target::Target::Alist(target)),
    }) = req.target
    else {
        return Err(test_error("alist target should deserialize"));
    };
    assert_eq!(target.relative_path, "season-1");
    let Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(page)) =
        req.pagination
    else {
        return Err(test_error("page pagination should deserialize"));
    };
    assert_eq!(page.page, 2);
    assert_eq!(req.page_size, 25);
    assert_eq!(req.availability, 0);
    Ok(())
}

#[test]
fn test_update_playback_state_request_deserialize_with_version() -> TestResult {
    let json = r#"{"type": 1, "version": 42}"#;
    let req: UpdatePlaybackStateRequest = serde_json::from_str(json)?;
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Play as i32
    );
    assert_eq!(req.version, Some(42));
    Ok(())
}

#[test]
fn test_add_media_batch_body_deserializes_without_room_id_in_nested_items() -> TestResult {
    let json = r#"{
        "items": [
            {
                "playlistId": "playlist-1",
                "providerInstanceName": "default",
                "sourceConfig": {
                    "directUrl": {
                        "medias": [
                            {
                                "url": "https://example.com/video.mp4"
                            }
                        ]
                    }
                },
                "name": "Example"
            }
        ]
    }"#;
    let body: AddMediaBatchRequest = serde_json::from_str(json)?;
    assert_eq!(body.items.len(), 1);
    Ok(())
}

#[test]
fn test_move_media_request_deserializes_anchor_fields_without_wrapper() -> TestResult {
    let json = r#"{
        "mediaIds": ["media-1"],
        "beforeMediaId": "media-2"
    }"#;
    let req: MoveMediaRequest = serde_json::from_str(json)?;
    assert_eq!(req.media_ids, vec!["media-1".to_string()]);
    assert_eq!(req.before_media_id.as_deref(), Some("media-2"));
    assert!(req.after_media_id.is_none());
    Ok(())
}

#[test]
fn test_parse_chat_history_request_accepts_cursor_only() -> TestResult {
    let req: GetChatHistoryRequest =
        serde_urlencoded::from_str("limit=20&cursor=2026-03-31T12%3A00%3A00%2B00%3A00%7Cmsg_123")?;

    assert_eq!(req.limit, 20);
    assert_eq!(req.cursor, "2026-03-31T12:00:00+00:00|msg_123");
    Ok(())
}

#[test]
fn test_chat_history_query_accepts_json_array_message_types() -> TestResult {
    let query: super::chat::GetChatHistoryQuery =
        serde_urlencoded::from_str("limit=20&includeMessageTypes=%5B1%2C1001%5D")?;
    let req = app_ok(query.into_request())?;

    assert_eq!(req.limit, 20);
    assert_eq!(req.include_message_types, vec![1, 1001]);
    Ok(())
}

#[tokio::test]
async fn test_chat_history_query_rejection_uses_app_error_shape() -> TestResult {
    async fn handler(
        crate::http::validation::ProtoQuery(_query): crate::http::validation::ProtoQuery<
            super::chat::GetChatHistoryQuery,
        >,
    ) -> &'static str {
        "ok"
    }

    let app = Router::new().route("/chat/history", get(handler));
    let request = Request::builder()
        .uri("/chat/history?limit=many")
        .body(Body::empty())?;
    let response = app.oneshot(request).await?.into_response();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["code"], tonic::Code::InvalidArgument as i32);
    assert!(matches!(
        json["message"].as_str(),
        Some(message) if message.contains("limit")
    ));
    Ok(())
}

#[test]
fn test_chat_message_path_injected_queries_ignore_message_id() -> TestResult {
    let _: super::chat::GetChatMessageQuery = serde_urlencoded::from_str("includeDeleted=true")?;
    let _: super::chat::GetChatMessageQuery =
        serde_urlencoded::from_str("messageId=msg_1&includeDeleted=true")?;

    let _: super::chat::GetChatMessageContextQuery =
        serde_urlencoded::from_str("beforeLimit=5&afterLimit=6&includeDeleted=true")?;
    let _: super::chat::GetChatMessageContextQuery =
        serde_urlencoded::from_str("messageId=msg_1&beforeLimit=5")?;
    Ok(())
}

#[test]
fn test_chat_playback_messages_query_accepts_structured_emby_target() -> TestResult {
    let query: super::chat::GetChatPlaybackMessagesQuery = serde_urlencoded::from_str(
        "playbackPlaylistId=pl_45&playbackTarget=%7B%22emby%22%3A%7B%22item%22%3A%7B%22itemId%22%3A%225%22%7D%7D%7D&positionSeconds=12.5",
    )?;
    let query = query.into_request()?;

    assert_eq!(query.playback_playlist_id, "pl_45");
    let Some(synctv_proto::client::ProviderTarget {
        target:
            Some(synctv_proto::client::provider_target::Target::Emby(
                synctv_proto::client::EmbyTarget {
                    target:
                        Some(synctv_proto::client::emby_target::Target::Item(
                            synctv_proto::client::EmbyItemTarget { item_id },
                        )),
                },
            )),
    }) = query.playback_target
    else {
        return Err(test_error("emby playback target should deserialize"));
    };
    assert_eq!(item_id, "5");
    assert!((query.position_seconds - 12.5).abs() < f64::EPSILON);
    Ok(())
}

#[test]
fn test_chat_playback_messages_query_accepts_json_array_message_types() -> TestResult {
    let query: super::chat::GetChatPlaybackMessagesQuery =
        serde_urlencoded::from_str("positionSeconds=12.5&includeMessageTypes=%5B1%2C1001%5D")?;
    let req = app_ok(query.into_request())?;

    assert_eq!(req.include_message_types, vec![1, 1001]);
    Ok(())
}

#[test]
fn test_delete_force_query_deserialization_accepts_bool_only() -> TestResult {
    let query: DeleteMediaQuery = serde_urlencoded::from_str("force=true")?;
    assert!(query.force);

    let query: DeletePlaylistQuery = serde_urlencoded::from_str("force=false")?;
    assert!(!query.force);

    assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=1").is_err());
    Ok(())
}

#[test]
fn test_delete_entries_body_deserializes_force_true() -> TestResult {
    let body: DeleteEntriesRequest = serde_json::from_str(
        r#"{"playlistIds":["playlist-1"],"mediaIds":["media-1"],"force":true}"#,
    )?;

    assert_eq!(body.playlist_ids, vec!["playlist-1"]);
    assert_eq!(body.media_ids, vec!["media-1"]);
    assert!(body.force);
    Ok(())
}

#[test]
fn test_create_playlist_body_deserializes_dynamic_fields() -> TestResult {
    let body: CreatePlaylistRequest = serde_json::from_str(
        r#"{
            "name":"Dynamic Folder",
            "parentId":"playlist-root",
            "sourceProvider":3,
            "sourceConfig":{"alist":{"serverId":"alist-server","path":"/tv"}},
            "providerInstanceName":"alist-main"
        }"#,
    )?;

    assert_eq!(body.name, "Dynamic Folder");
    assert_eq!(body.parent_id, "playlist-root");
    assert_eq!(
        body.source_provider,
        synctv_proto::source_config::SourceProvider::Alist as i32
    );
    let source_config = body
        .source_config
        .and_then(|config| config.provider)
        .ok_or_else(|| test_error("source_config should be present"))?;
    match source_config {
        synctv_proto::source_config::playlist_source_config::Provider::Alist(config) => {
            assert_eq!(config.server_id, "alist-server");
            assert_eq!(config.path, "/tv");
        }
        other => return Err(test_error(format!("unexpected source_config: {other:?}"))),
    }
    assert_eq!(body.provider_instance_name, "alist-main");
    Ok(())
}

#[test]
fn test_move_playlist_request_deserializes_anchor() -> TestResult {
    let body: synctv_proto::client::MovePlaylistRequest =
        serde_json::from_str(r#"{"playlistId":"playlist-1","beforePlaylistId":"playlist-2"}"#)?;

    assert_eq!(
        body.anchor,
        Some(
            synctv_proto::client::move_playlist_request::Anchor::BeforePlaylistId(
                "playlist-2".to_string()
            )
        )
    );
    Ok(())
}

#[test]
fn test_sse_event_id_from_resource_event_uses_event_sequence() {
    let changed = synctv_proto::client::ResourceEvent {
        observe_id: "chat-events".to_string(),
        payload: Some(synctv_proto::client::resource_event::Payload::ChatEvent(
            synctv_proto::client::ChatMessageEvent {
                event_id: " chat-event-3 ".to_string(),
                room_id: "room_test".to_string(),
                kind: synctv_proto::client::ChatMessageEventKind::Created as i32,
                message: None,
                occurred_at: 123,
                sequence: 3,
            },
        )),
        event_cursor: None,
    };

    assert_eq!(
        sse_event_id_from_resource_event(&changed).as_deref(),
        Some("3")
    );
}

#[tokio::test]
async fn test_chat_resource_event_sse_event_includes_event_sequence() -> TestResult {
    use axum::response::IntoResponse;
    use synctv_proto::client::resource_event::Payload;
    use synctv_proto::client::server_message::Message;

    let message = synctv_proto::client::ServerMessage {
        message: Some(Message::ResourceEvent(
            synctv_proto::client::ResourceEvent {
                observe_id: "chat-events".to_string(),
                payload: Some(Payload::ChatEvent(synctv_proto::client::ChatMessageEvent {
                    event_id: "chat-event-3".to_string(),
                    room_id: "room_test".to_string(),
                    kind: synctv_proto::client::ChatMessageEventKind::Created as i32,
                    message: None,
                    occurred_at: 123,
                    sequence: 3,
                })),
                event_cursor: None,
            },
        )),
    };
    let event = sse_event_from_server_message(
        crate::http::websocket::RealtimeTransportFormat::Json,
        message,
    )
    .ok_or_else(|| test_error("resource changed should produce SSE event"))?
    .map_err(|error| test_error(format!("{error:?}")))?;
    let response = axum::response::sse::Sse::new(tokio_stream::iter([Ok::<
        _,
        std::convert::Infallible,
    >(event)]))
    .into_response();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let rendered = std::str::from_utf8(&body)?;

    assert!(rendered.contains("id: 3\n"));
    assert!(rendered.contains("event: changed\n"));
    Ok(())
}

#[test]
fn test_cancel_on_drop_stream_cancels_token() {
    let token = tokio_util::sync::CancellationToken::new();
    let stream = tokio_stream::iter([Ok::<_, std::convert::Infallible>(
        axum::response::sse::Event::default(),
    )]);
    let wrapped = CancelOnDropStream::new(stream, token.clone());

    assert!(!token.is_cancelled());
    drop(wrapped);
    assert!(token.is_cancelled());
}
