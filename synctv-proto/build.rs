fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc.clone());
    for field in [
        ".synctv.client.GetRoomRequest.room_id",
        ".synctv.client.JoinRoomRequest.room_id",
        ".synctv.client.LeaveRoomRequest.room_id",
        ".synctv.client.DeleteRoomRequest.room_id",
        ".synctv.client.CheckRoomPasswordRequest.room_id",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    for field in [
        ".synctv.client.JoinRoomRequest.password",
        ".synctv.client.AddMediaRequest.provider",
        ".synctv.client.AddMediaRequest.provider_instance_name",
        ".synctv.client.AddMediaRequest.title",
        ".synctv.client.BanMemberRequest.reason",
        ".synctv.client.StartPlaybackRequest.media_id",
        ".synctv.client.StartPlaybackRequest.playlist_id",
        ".synctv.client.CreatePlaylistRequest.name",
        ".synctv.client.CreatePlaylistRequest.parent_id",
        ".synctv.client.CreatePlaylistRequest.source_provider",
        ".synctv.client.CreatePlaylistRequest.provider_instance_name",
        ".synctv.client.ListPlaylistItemsRequest.playlist_id",
        ".synctv.client.ListPlaylistItemsRequest.page",
        ".synctv.client.ListPlaylistItemsRequest.page_size",
        ".synctv.client.EditMediaRequest.media_id",
        ".synctv.client.DeleteEntriesRequest.playlist_ids",
        ".synctv.client.DeleteEntriesRequest.media_ids",
        ".synctv.client.DeleteEntriesRequest.force",
        ".synctv.client.ReorderMediaBatchRequest.updates",
        ".synctv.client.UpdatePlaylistRequest.playlist_id",
        ".synctv.client.UpdatePlaylistRequest.name",
        ".synctv.client.UpdateMemberPermissionsRequest.user_id",
        ".synctv.client.UpdateMemberPermissionsRequest.added_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.removed_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.admin_added_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.admin_removed_permissions",
        ".synctv.admin.UpdateUserPasswordRequest.user_id",
        ".synctv.admin.UpdateUserPasswordRequest.reason",
        ".synctv.admin.UpdateUserUsernameRequest.user_id",
        ".synctv.admin.UpdateUserRoleRequest.user_id",
        ".synctv.admin.BanUserRequest.user_id",
        ".synctv.admin.UpdateRoomPasswordRequest.room_id",
        ".synctv.admin.UpdateRoomPasswordRequest.new_password",
        ".synctv.admin.UpdateRoomSettingsRequest.room_id",
        ".synctv.admin.BanRoomRequest.room_id",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    for field in [
        ".synctv.client.Room.settings",
        ".synctv.client.Media.metadata",
        ".synctv.client.Media.source_config",
        ".synctv.client.PlaybackState.target",
        ".synctv.client.UpdateRoomSettingsRequest.settings",
        ".synctv.client.GetRoomSettingsResponse.settings",
        ".synctv.client.ResetRoomSettingsResponse.settings",
        ".synctv.client.StartPlaybackRequest.target",
        ".synctv.client.AddMediaRequest.source_config",
        ".synctv.client.ListPlaylistItemsRequest.target",
        ".synctv.client.PlaylistItem.target",
        ".synctv.client.PlaylistBrowsePathNode.target",
        ".synctv.client.CreatePlaylistRequest.source_config",
        ".synctv.admin.AdminRoom.settings",
        ".synctv.admin.SettingsGroup.settings",
        ".synctv.admin.AddProviderInstanceRequest.config",
        ".synctv.admin.UpdateProviderInstanceRequest.config",
        ".synctv.admin.GetRoomSettingsResponse.settings",
        ".synctv.admin.UpdateRoomSettingsRequest.settings",
        ".synctv.admin.GetSystemStatsResponse.additional_stats",
    ] {
        prost_config.field_attribute(field, "#[serde(with = \"crate::http_serde::json_bytes\")]");
    }
    for field in [
        ".synctv.client.StartPlaybackRequest.target",
        ".synctv.client.ListPlaylistItemsRequest.target",
        ".synctv.client.CreatePlaylistRequest.source_config",
        ".synctv.admin.UpdateRoomSettingsRequest.settings",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    prost_config.field_attribute(
        ".synctv.client.UpdatePlaylistRequest.position",
        "#[serde(default = \"crate::serde_defaults::update_playlist_position\")]",
    );
    prost_config.field_attribute(
        ".synctv.admin.UpdateUserPasswordRequest.new_password",
        "#[serde(alias = \"password\")]",
    );
    prost_config.field_attribute(
        ".synctv.admin.UpdateUserUsernameRequest.new_username",
        "#[serde(alias = \"username\")]",
    );
    prost_config.field_attribute(
        ".synctv.admin.UpdateRoomPasswordRequest.new_password",
        "#[serde(alias = \"password\")]",
    );
    prost_config.field_attribute(".synctv.admin.BanUserRequest.reason", "#[serde(default)]");
    prost_config.field_attribute(".synctv.admin.BanRoomRequest.reason", "#[serde(default)]");
    prost_config.field_attribute(
        ".synctv.admin.BatchBanUsersRequest.reason",
        "#[serde(default)]",
    );
    prost_config.field_attribute(
        ".synctv.admin.BatchBanRoomsRequest.reason",
        "#[serde(default)]",
    );
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(
            ".synctv.client.UpdateRoomSettingsRequest",
            "#[serde(from = \"crate::http_serde::ClientUpdateRoomSettingsRequestDef\")]",
        )
        .type_attribute(
            ".synctv.admin.UpdateRoomSettingsRequest",
            "#[serde(from = \"crate::http_serde::AdminUpdateRoomSettingsRequestDef\")]",
        )
        .out_dir("src")
        .compile_with_config(
            prost_config,
            &[
                "proto/client.proto",
                "proto/admin.proto",
                "proto/oauth2.proto",
            ],
            &["."],
        )?;

    let mut provider_prost_config = tonic_prost_build::Config::new();
    provider_prost_config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/providers/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src/providers")
        .compile_with_config(
            provider_prost_config,
            &[
                "proto/providers/bilibili.proto",
                "proto/providers/alist.proto",
                "proto/providers/emby.proto",
            ],
            &["."],
        )?;

    Ok(())
}
