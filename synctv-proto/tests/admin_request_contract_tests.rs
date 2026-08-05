use synctv_proto::admin::{
    ApproveUserRegistrationReviewRequest, BatchDeleteRoomsRequest, GetRoomRequest, GetUserRequest,
    GetUserRoomsRequest, KickStreamRequest, ListActiveStreamsRequest, ListRoomLabelsRequest,
    ListRoomsRequest, ListUsersRequest, RejectRoomCreationReviewRequest, UpdateRoomTaxonomyRequest,
    UpsertRoomLabelRequest,
};

#[test]
fn test_admin_get_user_request_rejects_invalid_user_id() {
    let request = GetUserRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_approve_user_request_rejects_invalid_request_id() {
    let request = ApproveUserRegistrationReviewRequest {
        request_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("request_id"), "{message}");
}

#[test]
fn test_admin_list_users_request_rejects_too_long_search() {
    let request = ListUsersRequest {
        page: 1,
        page_size: 20,
        status: 0,
        role: 0,
        search: "a".repeat(101),
        is_banned: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_get_user_rooms_request_rejects_too_long_search() {
    let request = GetUserRoomsRequest {
        user_id: "usr_1".to_string(),
        page: 1,
        page_size: 20,
        status: 0,
        search: "a".repeat(101),
        is_banned: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_get_room_request_rejects_invalid_room_id() {
    let request = GetRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_approve_room_request_rejects_invalid_request_id() {
    let request = RejectRoomCreationReviewRequest {
        request_id: "bad-room".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("request_id"), "{message}");
}

#[test]
fn test_admin_delete_room_category_request_rejects_invalid_category_id() {
    let request = synctv_proto::admin::DeleteRoomCategoryRequest {
        category_id: "bad-category".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("category_id"), "{message}");
}

#[test]
fn test_admin_list_rooms_request_rejects_invalid_creator_id() {
    let request = ListRoomsRequest {
        page: 1,
        page_size: 20,
        status: 0,
        search: String::new(),
        creator_id: "bad-user".to_string(),
        is_banned: None,
        sort_by: 0,
        sort_direction: 0,
        category_id: String::new(),
        label_ids: Vec::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("creator_id"), "{message}");
}

#[test]
fn test_admin_list_rooms_request_defaults_taxonomy_filters_from_json() {
    let request: ListRoomsRequest = serde_json::from_str("{}").expect("request should deserialize");

    assert!(request.category_id.is_empty());
    assert!(request.label_ids.is_empty());
}

#[test]
fn test_admin_list_room_labels_request_defaults_category_filter_from_json() {
    let request: ListRoomLabelsRequest =
        serde_json::from_str("{}").expect("request should deserialize");

    assert!(!request.include_disabled);
    assert!(request.category_id.is_empty());
}

#[test]
fn test_admin_update_room_taxonomy_request_defaults_optional_fields_from_json() {
    let mut request: UpdateRoomTaxonomyRequest =
        serde_json::from_str(r"{}").expect("request should deserialize");
    request.room_id = "room_abc".to_string();

    assert_eq!(request.room_id, "room_abc");
    assert_eq!(request.category_id, None);
    assert!(request.label_ids.is_empty());
    assert!(!request.clear_category);
}

#[test]
fn test_admin_update_room_taxonomy_request_accepts_full_body() {
    let request: UpdateRoomTaxonomyRequest = serde_json::from_str(
        r#"{"roomId":"room_abc","categoryId":"roomcat_abc","clearCategory":true}"#,
    )
    .expect("request should deserialize");
    assert_eq!(request.room_id, "room_abc");
    assert_eq!(request.category_id.as_deref(), Some("roomcat_abc"));
    assert!(request.clear_category);
}

#[test]
fn test_admin_upsert_room_label_request_defaults_optional_category_from_json() {
    let request: UpsertRoomLabelRequest =
        serde_json::from_str(r#"{"key":"featured","name":"Featured"}"#)
            .expect("request should deserialize");

    assert!(request.category_id.is_empty());
    assert!(request.description.is_empty());
    assert!(request.color.is_empty());
}

#[test]
fn test_admin_list_active_streams_request_rejects_invalid_room_id() {
    let request = ListActiveStreamsRequest {
        page: 1,
        page_size: 20,
        room_id: "bad-room".to_string(),
        user_id: String::new(),
        node_id: String::new(),
        search: String::new(),
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_list_active_streams_request_rejects_invalid_user_id() {
    let request = ListActiveStreamsRequest {
        page: 1,
        page_size: 20,
        room_id: String::new(),
        user_id: "bad-user".to_string(),
        node_id: String::new(),
        search: String::new(),
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_kick_stream_request_rejects_invalid_media_id() {
    let request = KickStreamRequest {
        room_id: "room_1".to_string(),
        media_id: "bad-media".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_admin_batch_delete_rooms_request_rejects_invalid_room_id() {
    let request = BatchDeleteRoomsRequest {
        room_ids: vec!["bad-room".to_string()],
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_ids"), "{message}");
}

#[test]
fn test_runtime_settings_snapshot_rejects_unknown_format_version() {
    let snapshot = synctv_proto::admin::RuntimeSettingsSnapshot {
        format_version: 2,
        settings: Some(synctv_proto::admin::RuntimeSettings::default()),
    };

    let error = synctv_proto::validate(&snapshot).expect_err("snapshot should be invalid");
    let message = error.to_string();
    assert!(message.contains("format_version"), "{message}");
}
