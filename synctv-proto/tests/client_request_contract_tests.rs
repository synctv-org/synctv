use prost_reflect::Kind;
use synctv_proto::client::{
    AddMediaRequest, ApproveRoomJoinReviewRequest, ClearPlaylistRequest, CreateRoomRequest,
    CreateWebSocketTicketRequest, DeleteEntriesRequest, DeleteMediaRequest,
    DeleteNotificationRequest, DeletePlaylistRequest, ExchangeAuthorizationCodeRequest,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlRequest, GetChatHistoryRequest,
    GetNotificationRequest, GetRoomRequest, GetServerTimeRequest, ListPlaylistItemsRequest,
    ListPlaylistsRequest, ListRoomJoinReviewsRequest, ListRoomLabelsRequest, MarkAsReadRequest,
    MoveMediaRequest, MovePlaylistRequest, OAuth2ProviderInstancePathRequest,
    OAuth2ProviderTypePathRequest, PasskeyAuthenticatorAssertionResponse,
    RejectRoomJoinReviewRequest, RoomJoinReviewPathRequest, RoomMemberTargetPathRequest,
    RoomPlaylistTargetPathRequest, RoomSettingsPatch, SortDirection, StartOpaqueLoginRequest,
    StartPlaybackRequest, TransferRoomOwnershipRequest, UnlinkProviderRequest,
    UpdatePlaybackStateRequest, UpdateRoomSettingsRequest, UploadUserAvatarObjectRequest,
    WebSocketConnectRequest,
};
use synctv_proto::providers::common::{ListProviderBackendsRequest, ProviderInstanceQuery};
use synctv_proto::source_config::SourceProvider;

fn emby_target(item_id: &str) -> Option<synctv_proto::client::ProviderTarget> {
    Some(synctv_proto::client::ProviderTarget {
        target: Some(synctv_proto::client::provider_target::Target::Emby(
            synctv_proto::client::EmbyTarget {
                target: Some(synctv_proto::client::emby_target::Target::Item(
                    synctv_proto::client::EmbyItemTarget {
                        item_id: item_id.to_string(),
                    },
                )),
            },
        )),
    })
}

fn direct_url_media_source_config(
    url: &str,
) -> Option<synctv_proto::source_config::MediaSourceConfig> {
    Some(synctv_proto::source_config::MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::DirectUrl(
                synctv_proto::source_config::DirectUrlMediaSourceConfig {
                    medias: vec![synctv_proto::source_config::DirectUrlMediaResourceConfig {
                        name: String::new(),
                        url: url.to_string(),
                        headers: Default::default(),
                        format: String::new(),
                        expires_at: None,
                    }],
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                    playback_kind: None,
                    duration_seconds: None,
                    prefer_proxy: None,
                    proxy_only: None,
                },
            ),
        ),
    })
}

fn alist_playlist_source_config(
    path: &str,
) -> Option<synctv_proto::source_config::PlaylistSourceConfig> {
    Some(synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(
            synctv_proto::source_config::playlist_source_config::Provider::Alist(
                synctv_proto::source_config::AlistPlaylistSourceConfig {
                    server_id: "alist-main".to_string(),
                    path: path.to_string(),
                    password: None,
                },
            ),
        ),
    })
}

#[test]
fn playback_resources_share_the_explicit_p2p_delivery_contract() {
    let pool = &synctv_proto::DESCRIPTOR_POOL;
    let delivery = pool
        .get_message_by_name("synctv.client.P2pResourceDelivery")
        .expect("P2pResourceDelivery descriptor should exist");

    for message_name in ["PlaybackMedia", "PlaybackSubtitle", "PlaybackDanmaku"] {
        let message = pool
            .get_message_by_name(&format!("synctv.client.{message_name}"))
            .expect("playback resource descriptor should exist");
        let field = message
            .get_field_by_name("p2p_delivery")
            .expect("playback resource should declare p2p_delivery");
        let Kind::Message(field_message) = field.kind() else {
            panic!("p2p_delivery should be a message field");
        };
        assert_eq!(field_message.full_name(), delivery.full_name());
    }
}

#[test]
fn test_add_media_request_allows_room_root_without_playlist_id() {
    let request = AddMediaRequest {
        playlist_id: None,
        provider_instance_name: String::new(),
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
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
        r#"{"encodedObjectKey":"object","token":"token","data":"cGF5bG9hZA=="}"#,
    )
    .expect("missing content_type should deserialize");

    assert_eq!(request.content_type, None);
}

#[test]
fn two_factor_changes_use_the_verified_security_request() {
    let preference_update = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.UpdateUserPreferencesRequest")
        .expect("UpdateUserPreferencesRequest descriptor should exist");
    assert!(
        preference_update
            .get_field_by_name("two_factor_enabled")
            .is_none(),
        "ordinary preference updates cannot change two-factor authentication"
    );

    let security_update = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.SetTwoFactorEnabledRequest")
        .expect("SetTwoFactorEnabledRequest descriptor should exist");
    assert!(security_update.get_field_by_name("enabled").is_some());
    assert!(security_update
        .get_field_by_name("verification_id")
        .is_some());
}

#[test]
fn test_protojson_deserialization_uses_lower_camel_case_fields() {
    let request: StartOpaqueLoginRequest =
        serde_json::from_str(r#"{"loginSessionId":"login-session","credentialRequest":"AQID"}"#)
            .expect("lowerCamelCase ProtoJSON request should deserialize");

    assert_eq!(request.login_session_id, "login-session");
    assert_eq!(request.credential_request.as_ref(), b"\x01\x02\x03");

    let error = serde_json::from_str::<StartOpaqueLoginRequest>(
        r#"{"loginSessionId":"login-session","credential_request":"AQID"}"#,
    )
    .expect_err("snake_case field names should be rejected");
    assert!(error.to_string().contains("credential_request"));
}

#[test]
fn test_protojson_custom_json_name_rejects_proto_field_name() {
    let response: PasskeyAuthenticatorAssertionResponse = serde_json::from_str(
        r#"{"authenticatorData":"AQID","clientDataJSON":"BAUG","signature":"BwgJ"}"#,
    )
    .expect("custom json_name field should deserialize");

    assert_eq!(response.authenticator_data.as_ref(), b"\x01\x02\x03");
    assert_eq!(response.client_data_json.as_ref(), b"\x04\x05\x06");
    assert_eq!(response.signature.as_ref(), b"\x07\x08\x09");

    let error = serde_json::from_str::<PasskeyAuthenticatorAssertionResponse>(
        r#"{"authenticatorData":"AQID","client_data_json":"BAUG","signature":"BwgJ"}"#,
    )
    .expect_err("proto snake_case field name should be rejected");
    let message = error.to_string();
    assert!(message.contains("client_data_json"), "{message}");
    assert!(
        !message.contains("expected one of: [\"authenticator_data\""),
        "{message}"
    );
    assert!(!message.contains("\"client_data_json\""), "{message}");
}

#[test]
fn test_room_settings_patch_uses_lower_camel_case_fields() {
    let patch: UpdateRoomSettingsRequest =
        serde_json::from_str(
            r#"{"settings":{"chatEnabled":false,"allowGuestJoin":true,"voiceChatEnabled":false,"p2pMediaEnabled":false},"updateMask":"chatEnabled,allowGuestJoin,voiceChatEnabled,p2pMediaEnabled"}"#,
        )
            .expect("lowerCamelCase room settings patch should deserialize");

    let settings = patch.settings.expect("settings");
    assert_eq!(settings.chat_enabled, Some(false));
    assert_eq!(settings.allow_guest_join, Some(true));
    assert_eq!(settings.voice_chat_enabled, Some(false));
    assert_eq!(settings.p2p_media_enabled, Some(false));

    let error = serde_json::from_str::<UpdateRoomSettingsRequest>(
        r#"{"settings":{"chat_enabled":false,"allow_guest_join":true},"updateMask":"chatEnabled,allowGuestJoin"}"#,
    )
    .expect_err("snake_case room settings patch fields should be rejected");
    assert!(error.to_string().contains("chat_enabled"));
}

#[test]
fn test_get_server_time_request_uses_lower_camel_case_query_field() {
    let request: GetServerTimeRequest =
        serde_urlencoded::from_str("clientSentAtNanos=1700000000123456789")
            .expect("lowerCamelCase server time query should deserialize");

    assert_eq!(request.client_sent_at_nanos, 1_700_000_000_123_456_789);
}

#[test]
fn test_room_settings_patch_rejects_duplicate_canonical_field() {
    let error =
        serde_json::from_str::<RoomSettingsPatch>(r#"{"chatEnabled":true,"chatEnabled":false}"#)
            .expect_err("duplicate room settings field should be rejected");

    assert!(error.to_string().contains("duplicate field"));
}

#[test]
fn test_update_room_settings_rejects_duplicate_canonical_field() {
    let error = serde_json::from_str::<UpdateRoomSettingsRequest>(
        r#"{"settings":{"chatEnabled":true},"settings":{"chatEnabled":false},"updateMask":"chatEnabled"}"#,
    )
    .expect_err("duplicate room settings field should be rejected");

    assert!(error.to_string().contains("duplicate field"));
}

#[test]
fn test_protojson_deserialization_uses_integer_enums() {
    let request: ListPlaylistsRequest = serde_json::from_str(
        r#"{"page":1,"pageSize":20,"sourceProvider":3,"sortBy":1,"sortDirection":1}"#,
    )
    .expect("integer enum values should deserialize");

    assert_eq!(request.source_provider, SourceProvider::Alist as i32);
    assert_eq!(request.sort_direction, SortDirection::Asc as i32);

    let error = serde_json::from_str::<ListPlaylistsRequest>(
        r#"{"page":1,"pageSize":20,"sourceProvider":"SOURCE_PROVIDER_ALIST"}"#,
    )
    .expect_err("enum string values should be rejected");
    assert!(error.is_data());
}

#[test]
fn test_protojson_query_deserialization_uses_lower_camel_case_and_integer_enums() {
    let request: ListPlaylistsRequest =
        serde_urlencoded::from_str("page=1&pageSize=20&sourceProvider=3&sortBy=1&sortDirection=1")
            .expect("lowerCamelCase integer enum query should deserialize");

    assert_eq!(request.page_size, 20);
    assert_eq!(request.source_provider, SourceProvider::Alist as i32);

    let field_error =
        serde_urlencoded::from_str::<ListPlaylistsRequest>("page=1&page_size=20&sourceProvider=3")
            .expect_err("snake_case query fields should be rejected");
    assert!(field_error.to_string().contains("page_size"));

    let enum_error = serde_urlencoded::from_str::<ListPlaylistsRequest>(
        "page=1&pageSize=20&sourceProvider=SOURCE_PROVIDER_ALIST",
    )
    .expect_err("enum string query values should be rejected");
    assert!(!enum_error.to_string().is_empty());
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
        client_operation_id: String::new(),
    };

    assert_eq!(message.code, 9000);
}

#[test]
fn test_add_media_request_requires_source_config() {
    let request = AddMediaRequest {
        playlist_id: None,
        provider_instance_name: String::new(),
        source_config: None,
        name: "Example".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("source_config"), "{message}");
}

#[test]
fn test_add_media_request_rejects_invalid_playlist_id() {
    let request = AddMediaRequest {
        playlist_id: Some("bad-playlist".to_string()),
        provider_instance_name: String::new(),
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
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
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 50,
        search: String::new(),
        source_provider: SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
    };

    let json = serde_json::to_value(&request).expect("serialize list request");
    assert!(json.get("playlistId").is_none());
    assert!(json["target"].is_null());
    let decoded: ListPlaylistItemsRequest =
        serde_json::from_value(json.clone()).expect("deserialize list request");
    assert!(decoded.target.is_none());
    assert!(json.get("availability").is_none());
    assert_eq!(
        decoded.availability,
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
    assert_eq!(json["mediaId"], "media-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_playlist_request_serializes_force_flag() {
    let request = DeletePlaylistRequest {
        playlist_id: "playlist-1".to_string(),
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete playlist request");
    assert_eq!(json["playlistId"], "playlist-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_entries_request_serializes_force_flag() {
    let request = DeleteEntriesRequest {
        playlist_ids: vec!["playlist-1".to_string()],
        media_ids: vec!["media-1".to_string()],
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete entries request");
    assert_eq!(json["playlistIds"][0], "playlist-1");
    assert_eq!(json["mediaIds"][0], "media-1");
    assert_eq!(json["force"], true);
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
    assert_eq!(json["playlistId"], "pl_1");
    synctv_proto::validate(&request).expect("request should be valid");
}

#[test]
fn test_start_playback_request_serializes_dynamic_playlist_target() {
    let request = StartPlaybackRequest {
        media_id: String::new(),
        playlist_id: "playlist-1".to_string(),
        target: emby_target("provider-item-1"),
        client_operation_id: None,
    };

    let json = serde_json::to_value(&request).expect("serialize start playback request");
    assert!(json.get("mediaId").is_none());
    assert_eq!(json["playlistId"], "playlist-1");
    assert_eq!(
        json["target"],
        serde_json::to_value(emby_target("provider-item-1")).expect("target should serialize")
    );
}

#[test]
fn test_start_playback_request_accepts_static_playlist_context() {
    let request = StartPlaybackRequest {
        media_id: "med_1".to_string(),
        playlist_id: "pl_1".to_string(),
        target: None,
        client_operation_id: None,
    };

    synctv_proto::validate(&request).expect("static playlist context should be valid");
}

#[test]
fn test_create_playlist_request_serializes_dynamic_fields_without_is_folder() {
    let request = synctv_proto::client::CreatePlaylistRequest {
        name: "Dyn".to_string(),
        parent_id: "playlist-root".to_string(),
        source_provider: SourceProvider::Alist as i32,
        source_config: alist_playlist_source_config("/tv"),
        provider_instance_name: "alist-main".to_string(),
        description: String::new(),
    };

    let json = serde_json::to_value(&request).expect("serialize create playlist request");
    assert!(json.get("is_folder").is_none());
    assert_eq!(json["sourceProvider"], SourceProvider::Alist as i32);
    assert_eq!(json["providerInstanceName"], "alist-main");
    assert_eq!(
        json["sourceConfig"],
        serde_json::to_value(alist_playlist_source_config("/tv"))
            .expect("source config should serialize")
    );
}

#[test]
fn test_playlist_source_config_oneof_only_contains_dynamic_playlist_providers() {
    let message = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.source_config.PlaylistSourceConfig")
        .expect("PlaylistSourceConfig descriptor should exist");
    let oneof = message
        .oneofs()
        .find(|oneof| oneof.name() == "provider")
        .expect("PlaylistSourceConfig.provider oneof should exist");
    let mut fields = oneof
        .fields()
        .map(|field| field.name().to_string())
        .collect::<Vec<_>>();
    fields.sort();

    assert_eq!(
        fields,
        [
            "alist",
            "bilibili",
            "cloudreve",
            "douyin",
            "emby",
            "fnos",
            "nextcloud",
            "qnap",
            "seafile",
            "synology",
            "tiktok",
            "truenas",
            "twitch",
            "youtube"
        ]
    );
}

#[test]
fn test_media_source_config_json_accepts_omitted_proto_default_fields() {
    let config: synctv_proto::source_config::MediaSourceConfig = serde_json::from_str(
        r#"{"directUrl":{"medias":[{"url":"https://example.com/video.mp4"}]}}"#,
    )
    .expect("directUrl source_config should allow omitted default fields");

    let provider = config
        .provider
        .expect("direct_url provider should be retained");
    match provider {
        synctv_proto::source_config::media_source_config::Provider::DirectUrl(config) => {
            assert_eq!(config.medias.len(), 1);
            assert_eq!(config.medias[0].url, "https://example.com/video.mp4");
            assert!(config.subtitles.is_empty());
            assert!(config.danmakus.is_empty());
        }
        other => panic!("unexpected provider: {other:?}"),
    }
}

#[test]
fn test_media_source_config_json_rejects_unknown_provider_key() {
    let error = serde_json::from_str::<synctv_proto::source_config::MediaSourceConfig>(
        r#"{"unknown_provider":{"url":"https://example.com/video.mp4"}}"#,
    )
    .expect_err("unknown provider key should be rejected");
    assert!(error.to_string().contains("unknown_provider"), "{error}");
}

#[test]
fn test_media_source_config_json_rejects_multiple_provider_keys() {
    let error = serde_json::from_str::<synctv_proto::source_config::MediaSourceConfig>(
        r#"{"directUrl":{"medias":[{"url":"https://example.com/video.mp4"}]},"rtmp":{}}"#,
    )
    .expect_err("media source_config should accept exactly one provider");
    assert!(error.to_string().contains("duplicate field"), "{error}");
}

#[test]
fn test_bilibili_source_config_json_accepts_single_source_key() {
    let config: synctv_proto::source_config::MediaSourceConfig =
        serde_json::from_str(r#"{"bilibili":{"video":{"bvid":"BV1234567890","cid":"42"}}}"#)
            .expect("bilibili source_config should deserialize");

    let provider = config
        .provider
        .expect("bilibili provider should be retained");
    match provider {
        synctv_proto::source_config::media_source_config::Provider::Bilibili(config) => {
            assert!(matches!(
                config.source,
                Some(synctv_proto::source_config::bilibili_media_source_config::Source::Video(_))
            ));
        }
        other => panic!("unexpected provider: {other:?}"),
    }
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
    let request = ListProviderBackendsRequest { provider_type: 99 };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider_type"), "{message}");
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
        provider: synctv_proto::client::OAuth2ProviderType::Oauth2ProviderTypeUnspecified as i32,
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
        provider: synctv_proto::client::OAuth2ProviderType::Oauth2ProviderTypeGithub as i32,
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
fn test_discover_rooms_request_rejects_too_long_search() {
    let request = synctv_proto::client::DiscoverRoomsRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        category_id: String::new(),
        label_ids: Vec::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_create_room_request_defaults_optional_taxonomy_fields_from_json() {
    let request: CreateRoomRequest = serde_json::from_str(r#"{"name":"room","settings":{}}"#)
        .expect("request should deserialize");

    assert!(request.category_id.is_empty());
    assert!(request.label_ids.is_empty());
}

#[test]
fn test_discover_rooms_request_defaults_taxonomy_filters_from_json() {
    let request: synctv_proto::client::DiscoverRoomsRequest =
        serde_json::from_str("{}").expect("request should deserialize");

    assert!(request.category_id.is_empty());
    assert!(request.label_ids.is_empty());
}

#[test]
fn test_list_room_labels_request_defaults_category_filter_from_json() {
    let request: ListRoomLabelsRequest =
        serde_json::from_str("{}").expect("request should deserialize");

    assert!(!request.include_disabled);
    assert!(request.category_id.is_empty());
}

#[test]
fn test_create_playlist_request_rejects_unknown_source_provider_json() {
    let error = serde_json::from_str::<synctv_proto::client::CreatePlaylistRequest>(
        r#"{"name":"Dyn","sourceProvider":999,"sourceConfig":{"alist":{"serverId":"alist-main","path":"/tv"}},"providerInstanceName":"alist-main"}"#,
    )
    .expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("999"), "{message}");
}

#[test]
fn test_list_playlists_request_rejects_invalid_provider_filters() {
    let request = ListPlaylistsRequest {
        parent_id: String::new(),
        page: 1,
        page_size: 20,
        search: String::new(),
        source_provider: 99,
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
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 20,
        search: String::new(),
        source_provider: 99,
        provider_instance_name: "bad name".to_string(),
        sort_by: synctv_proto::client::MediaListSortBy::Unspecified as i32,
        sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
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
        provider_instance_name: "bad name".to_string(),
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
        name: "Example".to_string(),
        description: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider_instance_name"), "{message}");
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
fn test_get_chat_history_request_rejects_invalid_limit() {
    let request = GetChatHistoryRequest {
        limit: 101,
        cursor: String::new(),
        include_message_types: Vec::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("limit"), "{message}");
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
        client_operation_id: None,
        client_time_millis: None,
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
        client_operation_id: None,
        client_time_millis: None,
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
fn test_mark_as_read_request_accepts_numeric_notification_ids() {
    let request = MarkAsReadRequest {
        notification_ids: vec![42, 43],
    };

    synctv_proto::validate(&request).expect("numeric notification IDs should be valid");
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
