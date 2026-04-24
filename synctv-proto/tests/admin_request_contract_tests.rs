use synctv_proto::admin::{
    AddAdminRequest, ApproveRoomRequest, ApproveUserRequest, BanRoomRequest, BanUserRequest,
    BatchBanRoomsRequest, BatchBanUsersRequest, BatchDeleteRoomsRequest, BatchDeleteUsersRequest,
    DeleteRoomRequest, DeleteUserRequest, GetRoomMembersRequest, GetRoomRequest,
    GetRoomSettingsRequest, GetSettingsGroupRequest, GetUserRequest, GetUserRoomsRequest,
    KickStreamRequest, ListActiveStreamsRequest, ListAdminsRequest, ListRoomsRequest,
    ListUsersRequest, RemoveAdminRequest, ResetRoomSettingsRequest, RoomPathRequest,
    UnbanRoomRequest, UnbanUserRequest, UpdateRoomPasswordRequest, UpdateRoomSettingsRequest,
    UpdateUserRoleRequest, UserPathRequest,
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
fn test_admin_user_path_request_rejects_invalid_user_id() {
    let request = UserPathRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_delete_user_request_rejects_invalid_user_id() {
    let request = DeleteUserRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_update_user_role_request_rejects_invalid_user_id() {
    let request = UpdateUserRoleRequest {
        user_id: "bad-user".to_string(),
        role: 1,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_ban_user_request_rejects_invalid_user_id() {
    let request = BanUserRequest {
        user_id: "bad-user".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_unban_user_request_rejects_invalid_user_id() {
    let request = UnbanUserRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_approve_user_request_rejects_invalid_user_id() {
    let request = ApproveUserRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_get_user_rooms_request_rejects_invalid_user_id() {
    let request = GetUserRoomsRequest {
        user_id: "bad-user".to_string(),
        page: 1,
        page_size: 20,
        status: 0,
        search: String::new(),
        is_banned: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_list_users_request_rejects_too_long_search() {
    let request = ListUsersRequest {
        page: 1,
        page_size: 20,
        status: 0,
        role: 0,
        search: "a".repeat(101),
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
        user_id: "ABCDEFGHIJKL".to_string(),
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
fn test_admin_add_admin_request_rejects_invalid_user_id() {
    let request = AddAdminRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
}

#[test]
fn test_admin_remove_admin_request_rejects_invalid_user_id() {
    let request = RemoveAdminRequest {
        user_id: "bad-user".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_id"), "{message}");
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
fn test_admin_room_path_request_rejects_invalid_room_id() {
    let request = RoomPathRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_get_room_settings_request_rejects_invalid_room_id() {
    let request = GetRoomSettingsRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_update_room_settings_request_rejects_invalid_room_id() {
    let request = UpdateRoomSettingsRequest {
        room_id: "bad-room".to_string(),
        settings: Vec::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_reset_room_settings_request_rejects_invalid_room_id() {
    let request = ResetRoomSettingsRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_update_room_password_request_rejects_invalid_room_id() {
    let request = UpdateRoomPasswordRequest {
        room_id: "bad-room".to_string(),
        new_password: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_delete_room_request_rejects_invalid_room_id() {
    let request = DeleteRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_ban_room_request_rejects_invalid_room_id() {
    let request = BanRoomRequest {
        room_id: "bad-room".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_unban_room_request_rejects_invalid_room_id() {
    let request = UnbanRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_approve_room_request_rejects_invalid_room_id() {
    let request = ApproveRoomRequest {
        room_id: "bad-room".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
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
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("creator_id"), "{message}");
}

#[test]
fn test_admin_list_rooms_request_rejects_too_long_search() {
    let request = ListRoomsRequest {
        page: 1,
        page_size: 20,
        status: 0,
        search: "a".repeat(101),
        creator_id: String::new(),
        is_banned: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_get_room_members_request_rejects_too_long_search() {
    let request = GetRoomMembersRequest {
        room_id: "ABCDEFGHIJKL".to_string(),
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        role: 0,
        status: 0,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_settings_group_request_rejects_invalid_group_name() {
    let request = GetSettingsGroupRequest {
        group: "bad group".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("group"), "{message}");
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
fn test_admin_list_admins_request_rejects_too_long_search() {
    let request = ListAdminsRequest {
        page: 1,
        page_size: 20,
        search: "a".repeat(101),
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_list_active_streams_request_rejects_too_long_search() {
    let request = ListActiveStreamsRequest {
        page: 1,
        page_size: 20,
        room_id: String::new(),
        user_id: String::new(),
        node_id: String::new(),
        search: "a".repeat(101),
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_admin_kick_stream_request_rejects_invalid_room_id() {
    let request = KickStreamRequest {
        room_id: "bad-room".to_string(),
        media_id: "ABCDEFGHIJKL".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_id"), "{message}");
}

#[test]
fn test_admin_kick_stream_request_rejects_invalid_media_id() {
    let request = KickStreamRequest {
        room_id: "ABCDEFGHIJKL".to_string(),
        media_id: "bad-media".to_string(),
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("media_id"), "{message}");
}

#[test]
fn test_admin_batch_ban_users_request_rejects_invalid_user_id() {
    let request = BatchBanUsersRequest {
        user_ids: vec!["ABCDEFGHIJKL".to_string(), "bad-user".to_string()],
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_ids"), "{message}");
}

#[test]
fn test_admin_batch_delete_users_request_rejects_invalid_user_id() {
    let request = BatchDeleteUsersRequest {
        user_ids: vec!["bad-user".to_string()],
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("user_ids"), "{message}");
}

#[test]
fn test_admin_batch_ban_rooms_request_rejects_invalid_room_id() {
    let request = BatchBanRoomsRequest {
        room_ids: vec!["ABCDEFGHIJKL".to_string(), "bad-room".to_string()],
        reason: String::new(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("room_ids"), "{message}");
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
