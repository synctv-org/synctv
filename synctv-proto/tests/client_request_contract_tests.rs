use prost_reflect::Kind;
use synctv_proto::client::{
    AddMediaRequest, ApproveRoomJoinReviewRequest, CheckRoomRequest, ClearPlaylistRequest,
    CreateWebSocketTicketRequest, DeleteEntriesRequest, DeleteMediaRequest,
    DeleteNotificationRequest, DeletePlaylistRequest, EditMediaRequest,
    ExchangeAuthorizationCodeRequest, GetAuthorizationUrlForBindRequest,
    GetAuthorizationUrlRequest, GetChatHistoryRequest, GetNotificationRequest, GetPlaylistRequest,
    GetRoomMembersRequest, GetRoomRequest, ListMyRoomsRequest, ListNotificationsRequest,
    ListPlaylistItemsRequest, ListPlaylistsRequest, ListRoomJoinReviewsRequest,
    ListRoomStreamsRequest, MarkAsReadRequest, MoveMediaRequest, MovePlaylistRequest,
    OAuth2ProviderInstancePathRequest, OAuth2ProviderTypePathRequest, RejectRoomJoinReviewRequest,
    RoomJoinReviewPathRequest, RoomMediaTargetPathRequest, RoomMemberTargetPathRequest,
    RoomPathRequest, RoomPlaylistTargetPathRequest, RoomStreamListSortBy, SortDirection,
    StartPlaybackRequest, TransferRoomOwnershipRequest, UnlinkProviderRequest,
    UpdatePlaybackStateRequest, UpdatePlaylistRequest, UploadUserAvatarObjectRequest,
    WebSocketConnectRequest,
};
use synctv_proto::providers::common::{
    ListProviderBackendsRequest, ProviderInstanceQuery, ProviderProxyPathRequest,
};
use synctv_proto::providers::rtmp::{CreatePublishKeyRequest, GetStreamInfoRequest};

#[test]
fn test_add_media_request_allows_room_root_without_playlist_id() {
    let request = AddMediaRequest {
        playlist_id: None,
        source_provider: "direct_url".to_string(),
        provider_instance_name: String::new(),
        source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
        name: "Example".to_string(),
        description: String::new(),
    };

    assert!(request.playlist_id.is_none());
}

#[test]
fn test_upload_object_content_type_is_optional() {
    let message = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.UploadUserAvatarObjectRequest")
        .expect("UploadUserAvatarObjectRequest descriptor should exist");
    let content_type = message
        .get_field_by_name("content_type")
        .expect("content_type field should exist");

    assert!(content_type.supports_presence());

    let request: UploadUserAvatarObjectRequest = serde_json::from_str(
        r#"{"encoded_object_key":"object","token":"token","data":[112,97,121,108,111,97,100]}"#,
    )
    .expect("missing content_type should deserialize");

    assert_eq!(request.content_type, None);
}

#[test]
fn test_error_message_code_is_application_int32() {
    let error_message = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.ErrorMessage")
        .expect("ErrorMessage descriptor should exist");
    let code = error_message
        .get_field_by_name("code")
        .expect("ErrorMessage.code field should exist");

    assert_eq!(code.kind(), Kind::Int32);

    let message = synctv_proto::client::ErrorMessage {
        message: "internal failure".to_string(),
        code: 9000,
        detail: String::new(),
    };

    assert_eq!(message.code, 9000);
}

#[test]
fn test_add_media_request_requires_non_empty_provider() {
    let request = AddMediaRequest {
        playlist_id: None,
        source_provider: String::new(),
        provider_instance_name: String::new(),
        source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
        name: "Example".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("source_provider"), "{message}");
}

#[test]
fn test_add_media_request_rejects_invalid_playlist_id() {
    let request = AddMediaRequest {
        playlist_id: Some("bad-playlist".to_string()),
        source_provider: "direct_url".to_string(),
        provider_instance_name: String::new(),
        source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
        name: "Example".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_list_playlist_items_request_allows_room_root_with_empty_playlist_id() {
    let request = ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: String::new(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    let json = serde_json::to_value(&request).expect("serialize list request");
    assert_eq!(json["playlist_id"], "");
    assert!(json["target"].is_null());
    let decoded: ListPlaylistItemsRequest =
        serde_json::from_value(json.clone()).expect("deserialize list request");
    assert!(decoded.target.is_empty());
    assert_eq!(
        json["availability"],
        synctv_proto::client::ResourceAvailabilityFilter::All as i32
    );
}

#[test]
fn test_get_room_request_rejects_invalid_room_id() {
    let request = GetRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_room_path_request_rejects_invalid_room_id() {
    let request = RoomPathRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_room_member_target_path_request_rejects_invalid_room_id() {
    let request = RoomMemberTargetPathRequest {
        room_id: "bad-room".to_string(),
        user_id: "usr_1".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_room_member_target_path_request_rejects_invalid_user_id() {
    let request = RoomMemberTargetPathRequest {
        room_id: "room_1".to_string(),
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_room_join_review_path_request_rejects_invalid_request_id() {
    let request = RoomJoinReviewPathRequest {
        room_id: "room_1".to_string(),
        request_id: "bad-request".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("request_id"), "{message}");
}

#[test]
fn test_list_room_join_reviews_request_accepts_default_pagination_and_pending_status() {
    let request = ListRoomJoinReviewsRequest {
        page: 0,
        page_size: 0,
        status: synctv_proto::common::ReviewStatus::Pending as i32,
        user_id: String::new(),
    };

    synctv_proto::validate(&request).expect("default pagination should be valid");
}

#[test]
fn test_list_room_join_reviews_request_rejects_invalid_page_size() {
    let request = ListRoomJoinReviewsRequest {
        page: 1,
        page_size: 101,
        status: synctv_proto::common::ReviewStatus::Pending as i32,
        user_id: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("page_size"), "{message}");
}

#[test]
fn test_approve_room_join_review_request_requires_valid_request_id() {
    let request = ApproveRoomJoinReviewRequest {
        request_id: "bad-request".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("request_id"), "{message}");
}

#[test]
fn test_reject_room_join_review_request_allows_reason_without_target_user_id() {
    let request = RejectRoomJoinReviewRequest {
        request_id: "rev_AbC123xYz890".to_string(),
        reason: "not eligible".to_string(),
    };

    synctv_proto::validate(&request).expect("request-id based rejection should be valid");
}

#[test]
fn test_room_media_target_path_request_rejects_invalid_media_id() {
    let request = RoomMediaTargetPathRequest {
        room_id: "room_1".to_string(),
        media_id: "bad-media".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_room_playlist_target_path_request_rejects_invalid_playlist_id() {
    let request = RoomPlaylistTargetPathRequest {
        room_id: "room_1".to_string(),
        playlist_id: "bad-playlist".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_delete_media_request_serializes_force_flag() {
    let request = DeleteMediaRequest {
        media_id: "media-1".to_string(),
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete media request");
    assert_eq!(json["media_id"], "media-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_media_request_rejects_invalid_media_id() {
    let request = DeleteMediaRequest {
        media_id: "bad-media".to_string(),
        force: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_delete_playlist_request_serializes_force_flag() {
    let request = DeletePlaylistRequest {
        playlist_id: "playlist-1".to_string(),
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete playlist request");
    assert_eq!(json["playlist_id"], "playlist-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_playlist_request_rejects_invalid_playlist_id() {
    let request = DeletePlaylistRequest {
        playlist_id: "bad-playlist".to_string(),
        force: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_get_playlist_request_rejects_invalid_playlist_id() {
    let request = GetPlaylistRequest {
        playlist_id: "bad-playlist".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_delete_entries_request_serializes_force_flag() {
    let request = DeleteEntriesRequest {
        playlist_ids: vec!["playlist-1".to_string()],
        media_ids: vec!["media-1".to_string()],
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete entries request");
    assert_eq!(json["playlist_ids"][0], "playlist-1");
    assert_eq!(json["media_ids"][0], "media-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_entries_request_rejects_invalid_playlist_id() {
    let request = DeleteEntriesRequest {
        playlist_ids: vec!["bad-playlist".to_string()],
        media_ids: vec!["med_1".to_string()],
        force: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_ids"), "{message}");
}

#[test]
fn test_delete_entries_request_rejects_invalid_media_id() {
    let request = DeleteEntriesRequest {
        playlist_ids: vec!["pl_1".to_string()],
        media_ids: vec!["bad-media".to_string()],
        force: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_ids"), "{message}");
}

#[test]
fn test_clear_playlist_request_serializes_playlist_scope() {
    let request = ClearPlaylistRequest {
        playlist_id: "pl_1".to_string(),
    };

    let json = serde_json::to_value(&request).expect("serialize clear playlist request");
    assert_eq!(json["playlist_id"], "pl_1");
    synctv_proto::validate(&request).expect("request should be valid");
}

#[test]
fn test_clear_playlist_request_rejects_invalid_playlist_id() {
    let request = ClearPlaylistRequest {
        playlist_id: "bad-playlist".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_start_playback_request_serializes_dynamic_playlist_target() {
    let request = StartPlaybackRequest {
        media_id: String::new(),
        playlist_id: "playlist-1".to_string(),
        target: br#"{"item_id":"provider-item-1"}"#.to_vec(),
    };

    let json = serde_json::to_value(&request).expect("serialize start playback request");
    assert_eq!(json["media_id"], "");
    assert_eq!(json["playlist_id"], "playlist-1");
    assert_eq!(
        json["target"],
        serde_json::json!({"item_id":"provider-item-1"})
    );
}

#[test]
fn test_create_playlist_request_serializes_dynamic_fields_without_is_folder() {
    let request = synctv_proto::client::CreatePlaylistRequest {
        name: "Dyn".to_string(),
        parent_id: "playlist-root".to_string(),
        source_provider: "alist".to_string(),
        source_config: br#"{"path":"/tv"}"#.to_vec(),
        provider_instance_name: "alist-main".to_string(),
        description: String::new(),
    };

    let json = serde_json::to_value(&request).expect("serialize create playlist request");
    assert!(json.get("is_folder").is_none());
    assert_eq!(json["source_provider"], "alist");
    assert_eq!(json["provider_instance_name"], "alist-main");
    assert_eq!(json["source_config"], serde_json::json!({"path":"/tv"}));
}

#[test]
fn test_provider_instance_query_allows_empty_instance_name() {
    let request = ProviderInstanceQuery {
        instance_name: String::new(),
    };

    synctv_proto::validate(&request).expect("empty instance name should be allowed");
}

#[test]
fn test_provider_instance_query_rejects_invalid_instance_name() {
    let request = ProviderInstanceQuery {
        instance_name: "bad name".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("instance_name"), "{message}");
}

#[test]
fn test_list_provider_backends_request_rejects_invalid_provider_type_format() {
    let request = ListProviderBackendsRequest {
        provider_type: "bad-name".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider_type"), "{message}");
}

#[test]
fn test_provider_proxy_path_request_rejects_invalid_provider_name_format() {
    let request = ProviderProxyPathRequest {
        provider_name: "bad-provider".to_string(),
        sub_path: "v1/media".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider_name"), "{message}");
}

#[test]
fn test_provider_proxy_path_request_rejects_empty_sub_path() {
    let request = ProviderProxyPathRequest {
        provider_name: "direct_url".to_string(),
        sub_path: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("sub_path"), "{message}");
}

#[test]
fn test_get_authorization_url_request_rejects_invalid_provider_instance_name() {
    let request = GetAuthorizationUrlRequest {
        provider: "bad provider".to_string(),
        redirect_url: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider"), "{message}");
}

#[test]
fn test_oauth2_provider_instance_path_request_rejects_invalid_provider_name() {
    let request = OAuth2ProviderInstancePathRequest {
        provider: "bad provider".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider"), "{message}");
}

#[test]
fn test_get_authorization_url_for_bind_request_rejects_dangerous_redirect_url() {
    let request = GetAuthorizationUrlForBindRequest {
        provider: "github".to_string(),
        redirect_url: "javascript:alert(1)".to_string(),
        verification_id: "verification-id".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("redirect_url"), "{message}");
}

#[test]
fn test_get_authorization_url_request_rejects_non_loopback_http_redirect_url() {
    let request = GetAuthorizationUrlRequest {
        provider: "github".to_string(),
        redirect_url: "http://example.com/oauth2/callback".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("redirect_url"), "{message}");
}

#[test]
fn test_exchange_authorization_code_request_rejects_invalid_state() {
    let request = ExchangeAuthorizationCodeRequest {
        provider: "github".to_string(),
        code: "code.with.dots".to_string(),
        state: "short".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("state"), "{message}");
}

#[test]
fn test_unlink_provider_request_rejects_invalid_provider_type() {
    let request = UnlinkProviderRequest {
        provider: "custom".to_string(),
        provider_user_id: String::new(),
        provider_instance_name: String::new(),
        verification_id: "verification-id".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider"), "{message}");
}

#[test]
fn test_unlink_provider_request_requires_instance_for_specific_identity() {
    let request = UnlinkProviderRequest {
        provider: "github".to_string(),
        provider_user_id: "remote-user-1".to_string(),
        provider_instance_name: String::new(),
        verification_id: "verification-id".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(
        message.contains("oauth2.unlink_provider.instance_for_specific_identity"),
        "{message}"
    );
}

#[test]
fn test_oauth2_provider_type_path_request_rejects_invalid_provider_type() {
    let request = OAuth2ProviderTypePathRequest {
        provider: "github-main".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider"), "{message}");
}

#[test]
fn test_list_rooms_request_rejects_too_long_search() {
    let request = synctv_proto::client::ListRoomsRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        sort_by: synctv_proto::client::RoomListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_get_room_members_request_rejects_too_long_search() {
    let request = GetRoomMembersRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        role: None,
        sort_by: synctv_proto::client::RoomMemberListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_list_playlists_request_rejects_too_long_search() {
    let request = ListPlaylistsRequest {
        parent_id: String::new(),
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        dynamic_only: None,
        sort_by: synctv_proto::client::PlaylistListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_create_playlist_request_rejects_invalid_source_provider_format() {
    let request = synctv_proto::client::CreatePlaylistRequest {
        name: "Dyn".to_string(),
        parent_id: String::new(),
        source_provider: "Bad Provider".to_string(),
        source_config: br#"{"path":"/tv"}"#.to_vec(),
        provider_instance_name: "alist-main".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("source_provider"), "{message}");
}

#[test]
fn test_list_playlists_request_rejects_invalid_provider_filters() {
    let request = ListPlaylistsRequest {
        parent_id: String::new(),
        page: 1,
        page_size: 20,
        search: String::new(),
        source_provider: "Bad Provider".to_string(),
        provider_instance_name: "bad name".to_string(),
        dynamic_only: None,
        sort_by: synctv_proto::client::PlaylistListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(
        message.contains("source_provider") || message.contains("provider_instance_name"),
        "{message}"
    );
}

#[test]
fn test_list_playlist_items_request_rejects_invalid_provider_filters() {
    let request = ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 20,
        search: String::new(),
        source_provider: "Bad Provider".to_string(),
        provider_instance_name: "bad name".to_string(),
        sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(
        message.contains("source_provider") || message.contains("provider_instance_name"),
        "{message}"
    );
}

#[test]
fn test_add_media_request_rejects_invalid_provider_identifiers() {
    let request = AddMediaRequest {
        playlist_id: None,
        source_provider: "Bad Provider".to_string(),
        provider_instance_name: "bad name".to_string(),
        source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
        name: "Example".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(
        message.contains("source_provider") || message.contains("provider_instance_name"),
        "{message}"
    );
}

#[test]
fn test_list_playlist_items_request_rejects_too_long_search() {
    let request = ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_list_my_rooms_request_rejects_too_long_search() {
    let request = ListMyRoomsRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        status: synctv_proto::common::RoomStatus::Unspecified as i32,
        is_banned: None,
        relation: synctv_proto::client::MyRoomRelation::Unspecified as i32,
        sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_list_room_streams_request_rejects_too_long_search() {
    let request = ListRoomStreamsRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        sort_by: RoomStreamListSortBy::Unspecified as i32,
        sort_direction: SortDirection::Unspecified as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_list_notifications_request_rejects_too_long_search() {
    let request = ListNotificationsRequest {
        page: 1,
        page_size: 20,
        is_read: None,
        notification_type: None,
        search: "a".repeat(101),
        sort_by: synctv_proto::client::NotificationListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_transfer_room_ownership_request_rejects_invalid_new_owner_user_id() {
    let request = TransferRoomOwnershipRequest {
        new_owner_user_id: "bad-id".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("new_owner_user_id"), "{message}");
}

#[test]
fn test_check_room_request_rejects_invalid_room_id() {
    let request = CheckRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_create_publish_key_request_rejects_invalid_media_id() {
    let request = CreatePublishKeyRequest {
        room_id: "room1234_abx".to_string(),
        media_id: "bad-media".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_get_stream_info_request_rejects_invalid_media_id() {
    let request = GetStreamInfoRequest {
        room_id: "room1234_abx".to_string(),
        media_id: "bad-media".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_get_chat_history_request_rejects_invalid_limit() {
    let request = GetChatHistoryRequest {
        limit: 101,
        cursor: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("limit"), "{message}");
}

#[test]
fn test_edit_media_request_rejects_invalid_media_id() {
    let request = EditMediaRequest {
        media_id: "bad-media".to_string(),
        name: "Episode 1".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_update_playlist_request_rejects_invalid_playlist_id() {
    let request = UpdatePlaylistRequest {
        playlist_id: "bad-playlist".to_string(),
        name: "Folder".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("playlist_id"), "{message}");
}

#[test]
fn test_move_playlist_request_rejects_invalid_anchor_playlist_id() {
    let request = MovePlaylistRequest {
        playlist_id: "pl_1".to_string(),
        anchor: Some(
            synctv_proto::client::move_playlist_request::Anchor::BeforePlaylistId(
                "bad-playlist".to_string(),
            ),
        ),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("before_playlist_id"), "{message}");
}

#[test]
fn test_move_media_request_rejects_invalid_media_id() {
    let request = MoveMediaRequest {
        media_ids: vec!["bad-media".to_string()],
        source_playlist_id: None,
        target_playlist_id: None,
        all_from_scope: false,
        before_media_id: None,
        after_media_id: None,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_ids"), "{message}");
}

#[test]
fn test_move_media_request_rejects_invalid_source_playlist_id() {
    let request = MoveMediaRequest {
        media_ids: Vec::new(),
        source_playlist_id: Some("bad-playlist".to_string()),
        target_playlist_id: None,
        all_from_scope: true,
        before_media_id: None,
        after_media_id: None,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("source_playlist_id"), "{message}");
}

#[test]
fn test_move_media_request_rejects_invalid_target_playlist_id() {
    let request = MoveMediaRequest {
        media_ids: vec!["med_1".to_string()],
        source_playlist_id: None,
        target_playlist_id: Some("bad-playlist".to_string()),
        all_from_scope: false,
        before_media_id: None,
        after_media_id: None,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("target_playlist_id"), "{message}");
}

#[test]
fn test_move_media_request_rejects_invalid_anchor_media_id() {
    let request = MoveMediaRequest {
        media_ids: vec!["med_1".to_string()],
        source_playlist_id: None,
        target_playlist_id: None,
        all_from_scope: false,
        before_media_id: Some("bad-media".to_string()),
        after_media_id: None,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("before_media_id"), "{message}");
}

#[test]
fn test_update_playback_state_rejects_missing_type() {
    let request = UpdatePlaybackStateRequest {
        r#type: synctv_proto::client::PlaybackUpdateType::Unspecified as i32,
        playing: None,
        position: Some(1.0),
        speed: None,
        version: None,
        expected_media_id: Some(String::new()),
        expected_playlist_id: Some(String::new()),
        expected_target_hash: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        ),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(
        message.contains("update_playback_state.type_required"),
        "{message}"
    );
}

#[test]
fn test_update_playback_state_allows_full_seek_state() {
    let request = UpdatePlaybackStateRequest {
        r#type: synctv_proto::client::PlaybackUpdateType::Seek as i32,
        playing: Some(false),
        position: Some(42.5),
        speed: Some(1.25),
        version: Some(7),
        expected_media_id: Some(String::new()),
        expected_playlist_id: Some(String::new()),
        expected_target_hash: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        ),
    };

    synctv_proto::validate(&request).expect("request should be valid");
}

#[test]
fn test_create_websocket_ticket_request_rejects_invalid_room_id() {
    let request = CreateWebSocketTicketRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_websocket_connect_request_rejects_invalid_ticket_format() {
    let request = WebSocketConnectRequest {
        ticket: "bad ticket".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("ticket"), "{message}");
}

#[test]
fn test_websocket_connect_request_allows_empty_ticket_for_header_auth() {
    let request = WebSocketConnectRequest {
        ticket: String::new(),
    };

    synctv_proto::validate(&request).expect("empty ticket should be allowed for header auth");
}

#[test]
fn test_get_notification_request_accepts_numeric_notification_id() {
    let request = GetNotificationRequest {
        notification_id: 42,
    };

    synctv_proto::validate(&request).expect("numeric notification ID should be valid");
}

#[test]
fn test_get_notification_request_rejects_invalid_notification_id() {
    let request = GetNotificationRequest { notification_id: 0 };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("notification_id"), "{message}");
}

#[test]
fn test_mark_as_read_request_accepts_numeric_notification_ids() {
    let request = MarkAsReadRequest {
        notification_ids: vec![42, 43],
    };

    synctv_proto::validate(&request).expect("numeric notification IDs should be valid");
}

#[test]
fn test_mark_as_read_request_rejects_invalid_notification_id() {
    let request = MarkAsReadRequest {
        notification_ids: vec![42, 0],
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("notification_ids"), "{message}");
}

#[test]
fn test_delete_notification_request_accepts_numeric_notification_id() {
    let request = DeleteNotificationRequest {
        notification_id: 42,
    };

    synctv_proto::validate(&request).expect("numeric notification ID should be valid");
}

#[test]
fn test_delete_notification_request_rejects_invalid_notification_id() {
    let request = DeleteNotificationRequest { notification_id: 0 };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("notification_id"), "{message}");
}
