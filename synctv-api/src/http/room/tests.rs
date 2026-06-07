use std::collections::HashMap;

use super::{
    build_get_playback_request, parse_optional_query_bool, parse_optional_query_i32,
    sse_event_from_server_message, sse_event_id_from_resource_changed, watch_after_event_sequence,
    CancelOnDropStream, ChatImageObjectQuery, GetPlaybackQuery, PlaylistCoverObjectQuery,
    RoomCoverObjectQuery, VideoCoverObjectQuery, WatchPlaybackQuery, WatchQuery,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use synctv_proto::client::{
    AddMediaBatchRequest, CreatePlaylistRequest, DeleteEntriesRequest, DeleteMediaQuery,
    DeletePlaylistQuery, GetChatHistoryRequest, GetChatMessageContextRequest,
    GetChatMessageRequest, GetHotRoomsRequest, GetRoomMembersRequest, ListPlaylistItemsRequest,
    ListPlaylistsRequest, ListRoomsRequest, MoveMediaRequest, UpdatePlaybackRequest,
};

#[test]
fn test_update_playback_deserialize_playing_update() {
    let json = r#"{"type":1,"playing":true}"#;
    let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Play as i32
    );
    assert_eq!(req.playing, Some(true));
    assert!(req.position.is_none());
    assert!(req.speed.is_none());
}

#[test]
fn test_update_playback_deserialize_seek_update() {
    let json = r#"{"type":3,"position": 42.5}"#;
    let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Seek as i32
    );
    assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
    assert!(req.speed.is_none());
}

#[test]
fn test_update_playback_deserialize_speed_update() {
    let json = r#"{"type":4,"speed": 2.0}"#;
    let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Speed as i32
    );
    assert!(req.position.is_none());
    assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
}

#[test]
fn test_update_playback_deserialize_full_state() {
    let json = r#"{"type":3,"playing":false,"position":42.5,"speed":1.25,"version":9}"#;
    let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Seek as i32
    );
    assert_eq!(req.playing, Some(false));
    assert_eq!(req.position, Some(42.5));
    assert_eq!(req.speed, Some(1.25));
    assert_eq!(req.version, Some(9));
}

#[test]
fn test_watch_after_event_sequence_prefers_last_event_id() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("42"));

    let sequence =
        watch_after_event_sequence(&headers, Some(7)).expect("valid Last-Event-ID should parse");

    assert_eq!(sequence, Some(42));
}

#[test]
fn test_watch_after_event_sequence_rejects_invalid_last_event_id() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("event-42"));

    let error = watch_after_event_sequence(&headers, Some(7))
        .expect_err("invalid Last-Event-ID should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("Last-Event-ID"));
}

#[test]
fn test_watch_after_event_sequence_rejects_negative_last_event_id() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("-1"));

    let error = watch_after_event_sequence(&headers, Some(7))
        .expect_err("negative Last-Event-ID should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("event sequence"));
}

#[test]
fn test_watch_after_event_sequence_rejects_negative_query_sequence() {
    let headers = HeaderMap::new();

    let error = watch_after_event_sequence(&headers, Some(-1))
        .expect_err("negative query event sequence should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("event sequence"));
}

#[test]
fn test_watch_after_event_sequence_rejects_non_utf8_last_event_id() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "last-event-id",
        HeaderValue::from_bytes(&[0xff]).expect("header bytes should build"),
    );

    let error = watch_after_event_sequence(&headers, Some(7))
        .expect_err("non-UTF-8 Last-Event-ID should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("Last-Event-ID"));
}

#[test]
fn test_build_get_playback_request_parses_generic_profile_query() {
    let request = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: Some("transcode".to_string()),
        max_streaming_bitrate: Some(8_000_000),
        max_audio_channels: Some(2),
        video_codecs: Some("h264,av1".to_string()),
        containers: Some("mp4,webm".to_string()),
        audio_capability: Some("surround".to_string()),
        subtitle_preference: Some("embedded_or_external".to_string()),
    })
    .expect("playback query should parse");

    let profile = request
        .playback_client_profile
        .expect("query should produce playback client profile");
    assert_eq!(
        profile.delivery_preference,
        synctv_proto::client::PlaybackDeliveryPreference::Transcode as i32
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
}

#[test]
fn test_build_get_playback_request_omits_profile_when_query_is_empty() {
    let request = build_get_playback_request(&GetPlaybackQuery::default())
        .expect("empty query should be valid");

    assert!(request.playback_client_profile.is_none());
}

#[test]
fn test_handwritten_room_queries_reject_unknown_fields() {
    assert!(serde_urlencoded::from_str::<GetPlaybackQuery>(
        "delivery_preference=direct&extra=true"
    )
    .is_err());
    assert!(serde_urlencoded::from_str::<WatchQuery>(
        "format=json&after_event_sequence=12&extra=true"
    )
    .is_err());
    assert!(serde_urlencoded::from_str::<WatchPlaybackQuery>(
        "format=json&media_id=media_1&extra=true"
    )
    .is_err());
    assert!(serde_urlencoded::from_str::<WatchPlaybackQuery>(
        "format=json&after_event_sequence=12"
    )
    .is_err());
    assert!(serde_urlencoded::from_str::<ChatImageObjectQuery>("token=token&extra=true").is_err());
    assert!(serde_urlencoded::from_str::<VideoCoverObjectQuery>("token=token&extra=true").is_err());
    assert!(serde_urlencoded::from_str::<RoomCoverObjectQuery>("token=token&extra=true").is_err());
    assert!(
        serde_urlencoded::from_str::<PlaylistCoverObjectQuery>("token=token&extra=true").is_err()
    );
}

#[test]
fn test_build_get_playback_request_rejects_invalid_video_codec() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: Some("h264,divx".to_string()),
        containers: None,
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown codec must be rejected");

    assert!(error.message.contains("video codec"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_delivery_preference() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: Some("download".to_string()),
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown delivery preference must be rejected");

    assert!(error.message.contains("delivery_preference"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_container() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: Some("mp4,avi".to_string()),
        audio_capability: None,
        subtitle_preference: None,
    })
    .expect_err("unknown container must be rejected");

    assert!(error.message.contains("container"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_audio_capability() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: Some("mono".to_string()),
        subtitle_preference: None,
    })
    .expect_err("unknown audio capability must be rejected");

    assert!(error.message.contains("audio_capability"), "{error:?}");
}

#[test]
fn test_build_get_playback_request_rejects_invalid_subtitle_preference() {
    let error = build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: None,
        max_streaming_bitrate: None,
        max_audio_channels: None,
        video_codecs: None,
        containers: None,
        audio_capability: None,
        subtitle_preference: Some("burn_in".to_string()),
    })
    .expect_err("unknown subtitle preference must be rejected");

    assert!(error.message.contains("subtitle_preference"), "{error:?}");
}

#[test]
fn test_members_query_params_deserialize_sorting_and_filters() {
    let json =
        r#"{"page":2,"page_size":25,"search":"alice","role":2,"sort_by":2,"sort_direction":1}"#;
    let query: GetRoomMembersRequest = serde_json::from_str(json).expect("deserialize");
    assert_eq!(query.page, 2);
    assert_eq!(query.page_size, 25);
    assert_eq!(query.search, "alice");
    assert_eq!(
        query.role,
        Some(synctv_proto::common::RoomMemberRole::Admin as i32)
    );
    assert_eq!(
        query.sort_by,
        synctv_proto::client::RoomMemberListSortBy::Username as i32
    );
    assert_eq!(
        query.sort_direction,
        synctv_proto::client::SortDirection::Asc as i32
    );
}

#[test]
fn test_scalar_query_parsers_reject_invalid_values() {
    let mut params = HashMap::new();
    params.insert("page".to_string(), "abc".to_string());
    assert!(parse_optional_query_i32(&params, "page").is_err());

    let mut params = HashMap::new();
    params.insert("dynamic_only".to_string(), "sometimes".to_string());
    assert!(parse_optional_query_bool(&params, "dynamic_only").is_err());

    assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=definitely").is_err());
    assert!(serde_urlencoded::from_str::<DeletePlaylistQuery>("force=definitely").is_err());
}

#[test]
fn test_list_rooms_query_deserializes_proto_defaults() {
    let query: ListRoomsRequest = serde_urlencoded::from_str("").unwrap();

    assert_eq!(query.page, 0);
    assert_eq!(query.page_size, 0);
    assert!(query.search.is_empty());
    assert_eq!(query.sort_by, 0);
    assert_eq!(query.sort_direction, 0);
}

#[test]
fn test_list_rooms_query_deserializes_explicit_values() {
    let query: ListRoomsRequest =
        serde_urlencoded::from_str("page=2&page_size=25&search=room&sort_by=4&sort_direction=1")
            .unwrap();

    assert_eq!(query.page, 2);
    assert_eq!(query.page_size, 25);
    assert_eq!(query.search, "room");
    assert_eq!(
        query.sort_by,
        synctv_proto::client::RoomListSortBy::Name as i32
    );
    assert_eq!(
        query.sort_direction,
        synctv_proto::client::SortDirection::Asc as i32
    );
}

#[test]
fn test_check_room_path_deserializes_proto_field_name() {
    let req: synctv_proto::client::CheckRoomRequest =
        serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

    assert_eq!(req.room_id, "room_1");
}

#[test]
fn test_room_path_request_deserializes_proto_field_name() {
    let req: synctv_proto::client::RoomPathRequest =
        serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

    assert_eq!(req.room_id, "room_1");
}

#[test]
fn test_room_media_target_path_request_deserializes_proto_field_names() {
    let req: synctv_proto::client::RoomMediaTargetPathRequest =
        serde_json::from_str(r#"{"room_id":"room_1","media_id":"med_1"}"#).unwrap();

    assert_eq!(req.room_id, "room_1");
    assert_eq!(req.media_id, "med_1");
}

#[test]
fn test_kick_room_stream_body_does_not_require_path_media_id() {
    let empty: super::KickRoomStreamBody = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.reason, "");

    let with_reason: super::KickRoomStreamBody =
        serde_json::from_str(r#"{"reason":"moderation"}"#).unwrap();
    assert_eq!(with_reason.reason, "moderation");
}

#[test]
fn test_room_playlist_target_path_request_deserializes_proto_field_names() {
    let req: synctv_proto::client::RoomPlaylistTargetPathRequest =
        serde_json::from_str(r#"{"room_id":"room_1","playlist_id":"pl_1"}"#).unwrap();

    assert_eq!(req.room_id, "room_1");
    assert_eq!(req.playlist_id, "pl_1");
}

#[test]
fn test_list_playlists_query_deserializes_proto_defaults() {
    let query: ListPlaylistsRequest = serde_urlencoded::from_str("").unwrap();

    assert_eq!(query.page, 0);
    assert_eq!(query.page_size, 0);
    assert_eq!(query.sort_by, 0);
    assert_eq!(query.sort_direction, 0);
    assert_eq!(query.availability, 0);
}

#[test]
fn test_list_playlists_query_deserializes_explicit_values() {
    let query: ListPlaylistsRequest =
        serde_urlencoded::from_str("page=2&page_size=25&sort_by=4&sort_direction=2&availability=2")
            .unwrap();

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
}

#[test]
fn test_chat_history_parser_rejects_invalid_limit() {
    let mut params = HashMap::new();
    params.insert("limit".to_string(), "many".to_string());
    assert!(serde_urlencoded::from_str::<GetChatHistoryRequest>("limit=many").is_err());
}

#[test]
fn test_chat_history_query_preserves_limit_for_shared_validation() {
    let req: GetChatHistoryRequest = serde_urlencoded::from_str("limit=101").unwrap();

    assert_eq!(req.limit, 101);
    assert!(crate::impls::validate_proto_request(&req).is_err());
}

#[test]
fn test_hot_rooms_query_preserves_limit_for_shared_validation() {
    let req: GetHotRoomsRequest = serde_urlencoded::from_str("limit=51").unwrap();

    assert_eq!(req.limit, 51);
    assert!(crate::impls::validate_proto_request(&req).is_err());
}

#[test]
fn test_list_playlist_items_body_deserialize_room_root() {
    let json = r"{}";
    let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
    assert!(req.playlist_id.is_empty());
    assert!(req.target.is_empty());
    assert_eq!(req.page, 0);
    assert_eq!(req.page_size, 0);
    assert_eq!(req.availability, 0);
}

#[test]
fn test_list_playlist_items_body_deserialize_dynamic_target() {
    let json = r#"{"playlist_id":"pl1","target":{"cursor":"season-1"},"page":2,"page_size":25}"#;
    let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.playlist_id, "pl1");
    let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
    assert_eq!(target, serde_json::json!({"cursor":"season-1"}));
    assert_eq!(req.page, 2);
    assert_eq!(req.page_size, 25);
    assert_eq!(req.availability, 0);
}

#[test]
fn test_update_playback_request_deserialize_with_version() {
    let json = r#"{"type": 1, "version": 42}"#;
    let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.r#type,
        synctv_proto::client::PlaybackUpdateType::Play as i32
    );
    assert_eq!(req.version, Some(42));
}

#[test]
fn test_add_media_batch_body_deserializes_without_room_id_in_nested_items() {
    let json = r#"{
        "items": [
            {
                "playlist_id": "playlist-1",
                "source_provider": "yt-dlp",
                "provider_instance_name": "default",
                "source_config": [1, 2, 3],
                "name": "Example"
            }
        ]
    }"#;
    let body: AddMediaBatchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(body.items.len(), 1);
}

#[test]
fn test_move_media_request_deserializes_anchor_fields_without_wrapper() {
    let json = r#"{
        "media_ids": ["media-1"],
        "before_media_id": "media-2"
    }"#;
    let req: MoveMediaRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.media_ids, vec!["media-1".to_string()]);
    assert_eq!(req.before_media_id.as_deref(), Some("media-2"));
    assert!(req.after_media_id.is_none());
}

#[test]
fn test_parse_chat_history_request_accepts_cursor_only() {
    let req: GetChatHistoryRequest =
        serde_urlencoded::from_str("limit=20&cursor=2026-03-31T12%3A00%3A00%2B00%3A00%7Cmsg_123")
            .expect("deserialize cursor request");

    assert_eq!(req.limit, 20);
    assert_eq!(req.cursor, "2026-03-31T12:00:00+00:00|msg_123");
}

#[test]
fn test_chat_message_path_injected_queries_deserialize_without_message_id() {
    let message: GetChatMessageRequest =
        serde_urlencoded::from_str("include_deleted=true").expect("deserialize chat message query");
    assert!(message.message_id.is_empty());
    assert!(message.include_deleted);

    let context: GetChatMessageContextRequest =
        serde_urlencoded::from_str("before_limit=5&after_limit=6&include_deleted=true")
            .expect("deserialize chat message context query");
    assert!(context.message_id.is_empty());
    assert_eq!(context.before_limit, 5);
    assert_eq!(context.after_limit, 6);
    assert!(context.include_deleted);
}

#[test]
fn test_delete_force_query_deserialization_accepts_bool_only() {
    let query: DeleteMediaQuery = serde_urlencoded::from_str("force=true").unwrap();
    assert!(query.force);

    let query: DeletePlaylistQuery = serde_urlencoded::from_str("force=false").unwrap();
    assert!(!query.force);

    assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=1").is_err());
}

#[test]
fn test_delete_entries_body_deserializes_force_true() {
    let body: DeleteEntriesRequest = serde_json::from_str(
        r#"{"playlist_ids":["playlist-1"],"media_ids":["media-1"],"force":true}"#,
    )
    .unwrap();

    assert_eq!(body.playlist_ids, vec!["playlist-1"]);
    assert_eq!(body.media_ids, vec!["media-1"]);
    assert!(body.force);
}

#[test]
fn test_create_playlist_body_deserializes_dynamic_fields() {
    let body: CreatePlaylistRequest = serde_json::from_str(
        r#"{
            "name":"Dynamic Folder",
            "parent_id":"playlist-root",
            "source_provider":"alist",
            "source_config":{"path":"/tv"},
            "provider_instance_name":"alist-main"
        }"#,
    )
    .unwrap();

    assert_eq!(body.name, "Dynamic Folder");
    assert_eq!(body.parent_id, "playlist-root");
    assert_eq!(body.source_provider, "alist");
    let source_config: serde_json::Value = serde_json::from_slice(&body.source_config).unwrap();
    assert_eq!(source_config, serde_json::json!({"path":"/tv"}));
    assert_eq!(body.provider_instance_name, "alist-main");
}

#[test]
fn test_move_playlist_body_deserializes_without_path_playlist_id() {
    let body: synctv_proto::client::MovePlaylistRequest =
        serde_json::from_str(r#"{"before_playlist_id":"playlist-2"}"#).expect("deserialize");

    assert!(body.playlist_id.is_empty());
    assert_eq!(
        body.anchor,
        Some(
            synctv_proto::client::move_playlist_request::Anchor::BeforePlaylistId(
                "playlist-2".to_string()
            )
        )
    );
}

#[test]
fn test_sse_event_id_from_resource_changed_uses_event_sequence() {
    let changed = synctv_proto::client::ResourceChanged {
        observe_id: "chat-events".to_string(),
        payload: Some(synctv_proto::client::resource_changed::Payload::ChatEvent(
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
        sse_event_id_from_resource_changed(&changed).as_deref(),
        Some("3")
    );
}

#[tokio::test]
async fn test_chat_resource_changed_sse_event_includes_event_sequence() {
    use axum::response::IntoResponse;
    use synctv_proto::client::resource_changed::Payload;
    use synctv_proto::client::server_message::Message;

    let message = synctv_proto::client::ServerMessage {
        message: Some(Message::ResourceChanged(
            synctv_proto::client::ResourceChanged {
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
    .expect("resource changed should produce SSE event")
    .expect("SSE event should serialize");
    let response = axum::response::sse::Sse::new(tokio_stream::iter([Ok::<
        _,
        std::convert::Infallible,
    >(event)]))
    .into_response();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("SSE body should render");
    let rendered = std::str::from_utf8(&body).expect("SSE body should be utf-8");

    assert!(rendered.contains("id: 3\n"));
    assert!(rendered.contains("event: changed\n"));
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
