use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAIN_GENERATED_FILES: &[&str] = &[
    "descriptor.bin",
    "buf.validate.rs",
    "synctv.common.rs",
    "synctv.client.rs",
    "synctv.admin.rs",
];

const PROVIDER_GENERATED_FILES: &[&str] = &[
    "descriptor.bin",
    "buf.validate.rs",
    "synctv.provider.alist.rs",
    "synctv.provider.bilibili.rs",
    "synctv.provider.common.rs",
    "synctv.provider.emby.rs",
    "synctv.provider.rtmp.rs",
];

fn build_out_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    env::var_os("OUT_DIR").map(PathBuf::from).ok_or_else(|| {
        Box::new(io::Error::new(
            io::ErrorKind::NotFound,
            "OUT_DIR is not set by Cargo",
        )) as Box<dyn std::error::Error>
    })
}

fn regen_proto_enabled() -> bool {
    env::var("SYNCTV_REGEN_PROTO")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
}

fn sync_generated_files(
    source_root: &Path,
    destination_root: &Path,
    files: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(destination_root)?;
    for file in files {
        let source = source_root.join(file);
        let destination = destination_root.join(file);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &destination)?;
    }
    Ok(())
}

fn match_count_as_i32(line: &str, needle: char) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(line.matches(needle).count())?)
}

fn collect_openapi_schema_aliases(
    proto_file: impl AsRef<Path>,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let proto_file = proto_file.as_ref();
    let source = fs::read_to_string(proto_file)?;

    let package = source
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("package ")
                .and_then(|rest| rest.strip_suffix(';'))
        })
        .ok_or_else(|| format!("missing package declaration in {}", proto_file.display()))?;

    let package_alias = package.replace('.', "_");
    let mut depth = 0_i32;
    let mut aliases = Vec::new();

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if depth == 0 {
            let type_name = line
                .strip_prefix("message ")
                .or_else(|| line.strip_prefix("enum "))
                .and_then(|rest| rest.split_whitespace().next());

            if let Some(type_name) = type_name {
                let fq_proto_type = format!(".{package}.{type_name}");
                let schema_alias = format!("{package_alias}_{type_name}");
                aliases.push((
                    fq_proto_type,
                    format!("#[cfg_attr(feature = \"openapi\", schema(as = {schema_alias}))]"),
                ));
            }
        }

        depth += match_count_as_i32(raw_line, '{')?;
        depth -= match_count_as_i32(raw_line, '}')?;
    }

    Ok(aliases)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let out_dir = build_out_dir()?;
    let main_out_dir = out_dir.join("main");
    let provider_out_dir = out_dir.join("providers");
    fs::create_dir_all(&main_out_dir)?;
    fs::create_dir_all(&provider_out_dir)?;

    println!(
        "cargo:rustc-env=SYNCTV_PROTO_MAIN_OUT_DIR={}",
        main_out_dir.display()
    );
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_PROVIDERS_OUT_DIR={}",
        provider_out_dir.display()
    );
    println!("cargo:rerun-if-env-changed=SYNCTV_REGEN_PROTO");

    let main_proto_files = [
        "proto/client.proto",
        "proto/admin.proto",
        "proto/oauth2.proto",
    ];
    let main_proto_includes = ["."];
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc.clone());
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::DESCRIPTOR_POOL")
        .file_descriptor_set_path(main_out_dir.join("descriptor.bin"))
        .configure(&mut prost_config, &main_proto_files, &main_proto_includes)?;
    let main_schema_aliases = [
        "proto/client.proto",
        "proto/admin.proto",
        "proto/oauth2.proto",
    ]
    .into_iter()
    .map(collect_openapi_schema_aliases)
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    for field in [
        ".synctv.client.LoginRequest.username",
        ".synctv.client.LoginRequest.password",
        ".synctv.client.LoginRequest.email",
        ".synctv.client.LoginRequest.email_token",
        ".synctv.client.StartOpaqueRegistrationRequest.username",
        ".synctv.client.StartOpaqueRegistrationRequest.email",
        ".synctv.client.StartOpaqueLoginRequest.username",
        ".synctv.client.StartOpaqueLoginRequest.email",
        ".synctv.client.StartPasskeyLoginRequest.username",
        ".synctv.client.StartPasskeyLoginRequest.email",
        ".synctv.client.StartPasskeyRegistrationRequest.email",
        ".synctv.client.StartPasskeyRegistrationRequest.name",
        ".synctv.client.StartPasskeyBindRequest.name",
        ".synctv.client.RejectRoomJoinReviewRequest.request_id",
        ".synctv.client.RejectRoomJoinReviewRequest.reason",
        ".synctv.client.GetRoomRequest.room_id",
        ".synctv.client.JoinRoomRequest.room_id",
        ".synctv.admin.GetUserRoomsRequest.user_id",
        ".synctv.admin.GetRoomMembersRequest.room_id",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    for field in [
        ".synctv.client.JoinRoomRequest.password",
        ".synctv.client.CreateRoomRequest.password",
        ".synctv.client.CreateRoomRequest.description",
        ".synctv.client.AddMediaRequest.source_provider",
        ".synctv.client.AddMediaRequest.provider_instance_name",
        ".synctv.client.AddMediaRequest.name",
        ".synctv.client.BanMemberRequest.reason",
        ".synctv.client.KickMemberRequest.user_id",
        ".synctv.client.StartPlaybackRequest.media_id",
        ".synctv.client.StartPlaybackRequest.playlist_id",
        ".synctv.client.CreatePlaylistRequest.name",
        ".synctv.client.CreatePlaylistRequest.parent_id",
        ".synctv.client.CreatePlaylistRequest.source_provider",
        ".synctv.client.CreatePlaylistRequest.provider_instance_name",
        ".synctv.client.ListPlaylistItemsRequest.playlist_id",
        ".synctv.client.ListPlaylistItemsRequest.page",
        ".synctv.client.ListPlaylistItemsRequest.page_size",
        ".synctv.client.ListPlaylistItemsRequest.search",
        ".synctv.client.ListPlaylistItemsRequest.source_provider",
        ".synctv.client.ListPlaylistItemsRequest.provider_instance_name",
        ".synctv.client.ListPlaylistItemsRequest.sort_by",
        ".synctv.client.ListPlaylistItemsRequest.sort_direction",
        ".synctv.client.ListPlaylistItemsRequest.availability",
        ".synctv.client.ListPlaylistItemsRequest.refresh",
        ".synctv.client.GetRoomMembersRequest.page",
        ".synctv.client.GetRoomMembersRequest.page_size",
        ".synctv.client.GetRoomMembersRequest.search",
        ".synctv.client.GetRoomMembersRequest.role",
        ".synctv.client.GetRoomMembersRequest.status",
        ".synctv.client.GetRoomMembersRequest.sort_by",
        ".synctv.client.GetRoomMembersRequest.sort_direction",
        ".synctv.client.GetRoomMembersRequest.is_banned",
        ".synctv.client.ListRoomStreamsRequest.page",
        ".synctv.client.ListRoomStreamsRequest.page_size",
        ".synctv.client.ListRoomStreamsRequest.search",
        ".synctv.client.ListRoomStreamsRequest.sort_by",
        ".synctv.client.ListRoomStreamsRequest.sort_direction",
        ".synctv.client.ListRoomJoinReviewsRequest.page",
        ".synctv.client.ListRoomJoinReviewsRequest.page_size",
        ".synctv.client.ListRoomJoinReviewsRequest.status",
        ".synctv.client.ListRoomJoinReviewsRequest.user_id",
        ".synctv.client.ListRoomsRequest.page",
        ".synctv.client.ListRoomsRequest.page_size",
        ".synctv.client.ListRoomsRequest.search",
        ".synctv.client.ListRoomsRequest.sort_by",
        ".synctv.client.ListRoomsRequest.sort_direction",
        ".synctv.client.ListPlaylistsRequest.parent_id",
        ".synctv.client.ListPlaylistsRequest.page",
        ".synctv.client.ListPlaylistsRequest.page_size",
        ".synctv.client.ListPlaylistsRequest.search",
        ".synctv.client.ListPlaylistsRequest.source_provider",
        ".synctv.client.ListPlaylistsRequest.provider_instance_name",
        ".synctv.client.ListPlaylistsRequest.dynamic_only",
        ".synctv.client.ListPlaylistsRequest.sort_by",
        ".synctv.client.ListPlaylistsRequest.sort_direction",
        ".synctv.client.ListPlaylistsRequest.availability",
        ".synctv.client.GetChatHistoryRequest.limit",
        ".synctv.client.GetChatHistoryRequest.cursor",
        ".synctv.client.GetHotRoomsRequest.limit",
        ".synctv.client.UpdateUserRequest.username",
        ".synctv.client.UpdateUserRequest.password",
        ".synctv.client.UpdateUserRequest.old_password",
        ".synctv.client.UpdateUserPreferencesRequest.two_factor_enabled",
        ".synctv.client.WebSocketConnectRequest.ticket",
        ".synctv.client.DeletePlaylistQuery.force",
        ".synctv.client.MoveMediaRequest.media_ids",
        ".synctv.client.MoveMediaRequest.source_playlist_id",
        ".synctv.client.MoveMediaRequest.target_playlist_id",
        ".synctv.client.MoveMediaRequest.all_from_scope",
        ".synctv.client.MoveMediaRequest.before_media_id",
        ".synctv.client.MoveMediaRequest.after_media_id",
        ".synctv.client.UpdatePlayback.playing",
        ".synctv.client.UpdatePlayback.position",
        ".synctv.client.UpdatePlayback.speed",
        ".synctv.client.UpdatePlayback.version",
        ".synctv.client.DeleteMediaQuery.force",
        ".synctv.client.ListMyRoomsRequest.page",
        ".synctv.client.ListMyRoomsRequest.page_size",
        ".synctv.client.ListMyRoomsRequest.search",
        ".synctv.client.ListMyRoomsRequest.status",
        ".synctv.client.ListMyRoomsRequest.is_banned",
        ".synctv.client.ListMyRoomsRequest.relation",
        ".synctv.client.ListMyRoomsRequest.sort_by",
        ".synctv.client.ListMyRoomsRequest.sort_direction",
        ".synctv.client.ListNotificationsRequest.page",
        ".synctv.client.ListNotificationsRequest.page_size",
        ".synctv.client.ListNotificationsRequest.is_read",
        ".synctv.client.ListNotificationsRequest.notification_type",
        ".synctv.client.ListNotificationsRequest.search",
        ".synctv.client.ListNotificationsRequest.sort_by",
        ".synctv.client.ListNotificationsRequest.sort_direction",
        ".synctv.client.GetAuthorizationUrlRequest.provider",
        ".synctv.client.GetAuthorizationUrlRequest.redirect_url",
        ".synctv.client.GetAuthorizationUrlForBindRequest.provider",
        ".synctv.client.GetAuthorizationUrlForBindRequest.redirect_url",
        ".synctv.client.ExchangeAuthorizationCodeRequest.provider",
        ".synctv.client.UnlinkProviderRequest.provider",
        ".synctv.client.UnlinkProviderRequest.provider_user_id",
        ".synctv.admin.ListUsersRequest.page",
        ".synctv.admin.ListUsersRequest.page_size",
        ".synctv.admin.ListUsersRequest.status",
        ".synctv.admin.ListUsersRequest.role",
        ".synctv.admin.ListUsersRequest.search",
        ".synctv.admin.ListUsersRequest.sort_by",
        ".synctv.admin.ListUsersRequest.sort_direction",
        ".synctv.admin.ListUsersRequest.is_banned",
        ".synctv.admin.ListUserRegistrationReviewsRequest.page",
        ".synctv.admin.ListUserRegistrationReviewsRequest.page_size",
        ".synctv.admin.ListUserRegistrationReviewsRequest.status",
        ".synctv.admin.ListUserRegistrationReviewsRequest.search",
        ".synctv.admin.ListRoomCreationReviewsRequest.page",
        ".synctv.admin.ListRoomCreationReviewsRequest.page_size",
        ".synctv.admin.ListRoomCreationReviewsRequest.status",
        ".synctv.admin.ListRoomCreationReviewsRequest.requested_by",
        ".synctv.admin.ListRoomCreationReviewsRequest.search",
        ".synctv.admin.ListRoomJoinReviewsRequest.page",
        ".synctv.admin.ListRoomJoinReviewsRequest.page_size",
        ".synctv.admin.ListRoomJoinReviewsRequest.status",
        ".synctv.admin.ListRoomJoinReviewsRequest.room_id",
        ".synctv.admin.ListRoomJoinReviewsRequest.user_id",
        ".synctv.admin.ListBanRecordsRequest.page",
        ".synctv.admin.ListBanRecordsRequest.page_size",
        ".synctv.admin.ListBanRecordsRequest.target_type",
        ".synctv.admin.ListBanRecordsRequest.active",
        ".synctv.admin.ListBanRecordsRequest.user_id",
        ".synctv.admin.ListBanRecordsRequest.room_id",
        ".synctv.admin.GetUserRoomsRequest.page",
        ".synctv.admin.GetUserRoomsRequest.page_size",
        ".synctv.admin.GetUserRoomsRequest.status",
        ".synctv.admin.GetUserRoomsRequest.search",
        ".synctv.admin.GetUserRoomsRequest.is_banned",
        ".synctv.admin.GetUserRoomsRequest.sort_by",
        ".synctv.admin.GetUserRoomsRequest.sort_direction",
        ".synctv.admin.ListRoomsRequest.page",
        ".synctv.admin.ListRoomsRequest.page_size",
        ".synctv.admin.ListRoomsRequest.status",
        ".synctv.admin.ListRoomsRequest.search",
        ".synctv.admin.ListRoomsRequest.creator_id",
        ".synctv.admin.ListRoomsRequest.is_banned",
        ".synctv.admin.ListRoomsRequest.sort_by",
        ".synctv.admin.ListRoomsRequest.sort_direction",
        ".synctv.admin.GetRoomMembersRequest.page",
        ".synctv.admin.GetRoomMembersRequest.page_size",
        ".synctv.admin.GetRoomMembersRequest.search",
        ".synctv.admin.GetRoomMembersRequest.role",
        ".synctv.admin.GetRoomMembersRequest.status",
        ".synctv.admin.GetRoomMembersRequest.sort_by",
        ".synctv.admin.GetRoomMembersRequest.sort_direction",
        ".synctv.admin.GetRoomMembersRequest.is_banned",
        ".synctv.admin.ListAdminsRequest.page",
        ".synctv.admin.ListAdminsRequest.page_size",
        ".synctv.admin.ListAdminsRequest.search",
        ".synctv.admin.ListAdminsRequest.sort_by",
        ".synctv.admin.ListAdminsRequest.sort_direction",
        ".synctv.admin.ListActiveStreamsRequest.page",
        ".synctv.admin.ListActiveStreamsRequest.page_size",
        ".synctv.admin.ListActiveStreamsRequest.room_id",
        ".synctv.admin.ListActiveStreamsRequest.user_id",
        ".synctv.admin.ListActiveStreamsRequest.node_id",
        ".synctv.admin.ListActiveStreamsRequest.search",
        ".synctv.admin.ListActiveStreamsRequest.sort_by",
        ".synctv.admin.ListActiveStreamsRequest.sort_direction",
        ".synctv.client.EditMediaRequest.media_id",
        ".synctv.client.DeleteEntriesRequest.playlist_ids",
        ".synctv.client.DeleteEntriesRequest.media_ids",
        ".synctv.client.DeleteEntriesRequest.force",
        ".synctv.client.UpdatePlaylistRequest.playlist_id",
        ".synctv.client.MovePlaylistRequest.playlist_id",
        ".synctv.client.UpdatePlaylistRequest.name",
        ".synctv.client.UpdateMemberPermissionsRequest.user_id",
        ".synctv.client.UpdateMemberPermissionsRequest.role",
        ".synctv.client.UpdateMemberPermissionsRequest.added_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.removed_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.admin_added_permissions",
        ".synctv.client.UpdateMemberPermissionsRequest.admin_removed_permissions",
        ".synctv.admin.UpdateUserPasswordRequest.user_id",
        ".synctv.admin.UpdateUserPasswordRequest.reason",
        ".synctv.admin.UpdateUserUsernameRequest.user_id",
        ".synctv.admin.UpdateUserRoleRequest.user_id",
        ".synctv.admin.UpdateUserPreferencesRequest.user_id",
        ".synctv.admin.UpdateUserPreferencesRequest.two_factor_enabled",
        ".synctv.admin.BanUserRequest.user_id",
        ".synctv.admin.UpdateRoomPasswordRequest.room_id",
        ".synctv.admin.UpdateRoomPasswordRequest.new_password",
        ".synctv.admin.UpdateRoomSettingsRequest.room_id",
        ".synctv.admin.BanRoomRequest.room_id",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    prost_config.message_attribute(
        ".synctv.client.MovePlaylistRequest",
        "#[serde(try_from = \"crate::http_serde::MovePlaylistRequestDef\")]",
    );
    for field in [
        ".synctv.client.ApiErrorResponse.code",
        ".synctv.client.ApiErrorResponse.request_id",
        ".synctv.client.HealthResponse.details",
        ".synctv.client.HealthDetails.cluster",
        ".synctv.client.HealthDetails.ws_ticket",
        ".synctv.client.HealthDetails.email",
        ".synctv.client.HealthDetails.livestream",
        ".synctv.client.HealthDetails.memory",
        ".synctv.client.HealthDetails.message",
    ] {
        prost_config.field_attribute(field, "#[serde(skip_serializing_if = \"Option::is_none\")]");
    }
    for field in [
        ".synctv.client.CreateRoomRequest.settings",
        ".synctv.client.Room.settings",
        ".synctv.client.Media.metadata",
        ".synctv.client.Media.source_config",
        ".synctv.client.Playlist.source_config",
        ".synctv.client.PlaybackState.target",
        ".synctv.client.UpdateRoomSettingsRequest.settings",
        ".synctv.client.GetRoomSettingsResponse.settings",
        ".synctv.client.ResetRoomSettingsResponse.settings",
        ".synctv.client.StartPlaybackRequest.target",
        ".synctv.client.StartPasskeyLoginResponse.options",
        ".synctv.client.FinishPasskeyLoginRequest.credential",
        ".synctv.client.StartPasskeyRegistrationResponse.options",
        ".synctv.client.FinishPasskeyRegistrationRequest.credential",
        ".synctv.client.StartPasskeyBindResponse.options",
        ".synctv.client.FinishPasskeyBindRequest.credential",
        ".synctv.client.StartMfaPasskeyResponse.options",
        ".synctv.client.FinishMfaPasskeyRequest.credential",
        ".synctv.client.StartOpaquePasswordUpdateResponse.passkey_options",
        ".synctv.client.FinishOpaquePasswordUpdateRequest.passkey_credential",
        ".synctv.client.AddMediaRequest.source_config",
        ".synctv.client.ListPlaylistItemsRequest.target",
        ".synctv.client.ObservePlaybackSnapshot.target",
        ".synctv.client.PlaylistItem.target",
        ".synctv.client.PlaylistBrowsePathNode.target",
        ".synctv.client.CreatePlaylistRequest.source_config",
        ".synctv.admin.AdminRoom.settings",
        ".synctv.admin.SettingsGroup.settings",
        ".synctv.admin.GetRoomSettingsResponse.settings",
        ".synctv.admin.UpdateRoomSettingsRequest.settings",
        ".synctv.admin.GetSystemStatsResponse.additional_stats",
    ] {
        prost_config.field_attribute(field, "#[serde(with = \"crate::http_serde::json_bytes\")]");
    }
    for field in [
        ".synctv.client.CreateRoomRequest.settings",
        ".synctv.client.StartPlaybackRequest.target",
        ".synctv.client.UpdatePlayback.type",
        ".synctv.client.ListPlaylistItemsRequest.target",
        ".synctv.client.ObserveResource.delivery_mode",
        ".synctv.client.CreatePlaylistRequest.source_config",
        ".synctv.admin.UpdateRoomSettingsRequest.settings",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }
    prost_config.field_attribute(".synctv.admin.BanUserRequest.reason", "#[serde(default)]");
    prost_config.field_attribute(".synctv.admin.BanRoomRequest.reason", "#[serde(default)]");
    prost_config.field_attribute(
        ".synctv.admin.KickStreamRequest.reason",
        "#[serde(default)]",
    );
    prost_config.field_attribute(
        ".synctv.admin.BatchBanUsersRequest.reason",
        "#[serde(default)]",
    );
    prost_config.field_attribute(
        ".synctv.admin.BatchBanRoomsRequest.reason",
        "#[serde(default)]",
    );
    let mut main_builder = tonic_prost_build::configure();
    main_builder = main_builder
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(main_out_dir.join("descriptor.bin"))
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", allow(clippy::large_stack_arrays))]",
        )
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::ToSchema))]",
        )
        .type_attribute(
            ".synctv.client.UpdateRoomSettingsRequest",
            "#[serde(from = \"crate::http_serde::ClientUpdateRoomSettingsRequestDef\")]",
        )
        .type_attribute(
            ".synctv.admin.UpdateRoomSettingsRequest",
            "#[serde(from = \"crate::http_serde::AdminUpdateRoomSettingsRequestDef\")]",
        );
    for path in [
        ".synctv.client.ListMyRoomsRequest",
        ".synctv.client.ListNotificationsRequest",
        ".synctv.client.GetRoomMembersRequest",
        ".synctv.client.ListRoomStreamsRequest",
        ".synctv.client.ListRoomJoinReviewsRequest",
        ".synctv.client.ListRoomsRequest",
        ".synctv.client.ListPlaylistsRequest",
        ".synctv.client.GetChatHistoryRequest",
        ".synctv.client.GetHotRoomsRequest",
        ".synctv.client.GetAuthorizationUrlRequest",
        ".synctv.client.GetAuthorizationUrlForBindRequest",
        ".synctv.client.UnlinkProviderRequest",
        ".synctv.admin.ListUsersRequest",
        ".synctv.admin.ListUserRegistrationReviewsRequest",
        ".synctv.admin.ListRoomCreationReviewsRequest",
        ".synctv.admin.ListRoomJoinReviewsRequest",
        ".synctv.admin.ListBanRecordsRequest",
        ".synctv.admin.GetUserRoomsRequest",
        ".synctv.admin.ListRoomsRequest",
        ".synctv.admin.GetRoomMembersRequest",
        ".synctv.admin.ListAdminsRequest",
        ".synctv.admin.ListActiveStreamsRequest",
    ] {
        main_builder = main_builder.type_attribute(
            path,
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::IntoParams))]",
        );
    }
    for (path, attr) in &main_schema_aliases {
        main_builder = main_builder.type_attribute(path, attr);
    }
    main_builder.out_dir(&main_out_dir).compile_with_config(
        prost_config,
        &main_proto_files,
        &main_proto_includes,
    )?;

    let mut provider_prost_config = tonic_prost_build::Config::new();
    provider_prost_config.protoc_executable(protoc);
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PROVIDERS_DESCRIPTOR_POOL")
        .file_descriptor_set_path(provider_out_dir.join("descriptor.bin"))
        .configure(
            &mut provider_prost_config,
            &[
                "proto/providers/bilibili.proto",
                "proto/providers/bilibili_service.proto",
                "proto/providers/alist.proto",
                "proto/providers/alist_service.proto",
                "proto/providers/emby.proto",
                "proto/providers/emby_service.proto",
                "proto/providers/common.proto",
                "proto/providers/common_service.proto",
                "proto/providers/rtmp.proto",
                "proto/providers/rtmp_service.proto",
            ],
            &["."],
        )?;
    let provider_schema_aliases = [
        "proto/providers/bilibili.proto",
        "proto/providers/alist.proto",
        "proto/providers/emby.proto",
        "proto/providers/common.proto",
        "proto/providers/rtmp.proto",
    ]
    .into_iter()
    .map(collect_openapi_schema_aliases)
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let mut provider_builder = tonic_prost_build::configure();
    provider_builder = provider_builder
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(provider_out_dir.join("descriptor.bin"))
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", allow(clippy::large_stack_arrays))]",
        )
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::ToSchema))]",
        );

    for field in [
        ".synctv.provider.bilibili.ParseRequest.instance_name",
        ".synctv.provider.bilibili.LoginQRRequest.instance_name",
        ".synctv.provider.bilibili.CheckQRRequest.instance_name",
        ".synctv.provider.bilibili.GetCaptchaRequest.instance_name",
        ".synctv.provider.bilibili.SendSMSRequest.instance_name",
        ".synctv.provider.bilibili.LoginSMSRequest.instance_name",
        ".synctv.provider.bilibili.UserInfoRequest.instance_name",
        ".synctv.provider.bilibili.LogoutRequest.instance_name",
        ".synctv.provider.alist.LoginRequest.instance_name",
        ".synctv.provider.alist.ListRequest.path",
        ".synctv.provider.alist.ListRequest.password",
        ".synctv.provider.alist.ListRequest.page",
        ".synctv.provider.alist.ListRequest.per_page",
        ".synctv.provider.alist.ListRequest.refresh",
        ".synctv.provider.alist.ListRequest.instance_name",
        ".synctv.provider.alist.SearchRequest.parent",
        ".synctv.provider.alist.SearchRequest.keywords",
        ".synctv.provider.alist.SearchRequest.scope",
        ".synctv.provider.alist.SearchRequest.page",
        ".synctv.provider.alist.SearchRequest.per_page",
        ".synctv.provider.alist.SearchRequest.password",
        ".synctv.provider.alist.SearchRequest.instance_name",
        ".synctv.provider.alist.GetMeRequest.instance_name",
        ".synctv.provider.alist.LogoutRequest.instance_name",
        ".synctv.provider.alist.GetBindsRequest.instance_name",
        ".synctv.provider.emby.LoginRequest.instance_name",
        ".synctv.provider.emby.ListRequest.path",
        ".synctv.provider.emby.ListRequest.start_index",
        ".synctv.provider.emby.ListRequest.limit",
        ".synctv.provider.emby.ListRequest.search_term",
        ".synctv.provider.emby.ListRequest.instance_name",
        ".synctv.provider.emby.GetMeRequest.instance_name",
        ".synctv.provider.emby.LogoutRequest.instance_name",
        ".synctv.provider.emby.GetBindsRequest.instance_name",
        ".synctv.provider.common.ProviderInstanceQuery.instance_name",
        ".synctv.provider.common.AddProviderInstanceRequest.comment",
        ".synctv.provider.common.AddProviderInstanceRequest.timeout_seconds",
        ".synctv.provider.common.AddProviderInstanceRequest.tls",
        ".synctv.provider.common.AddProviderInstanceRequest.insecure_tls",
        ".synctv.provider.common.AddProviderInstanceRequest.providers",
        ".synctv.provider.common.UpdateProviderInstanceRequest.name",
        ".synctv.provider.common.UpdateProviderInstanceRequest.providers",
        ".synctv.provider.common.ListProviderInstancesRequest.page",
        ".synctv.provider.common.ListProviderInstancesRequest.page_size",
        ".synctv.provider.common.ListProviderInstancesRequest.provider_type",
        ".synctv.provider.common.ListProviderInstancesRequest.search",
        ".synctv.provider.common.ListProviderInstancesRequest.sort_by",
        ".synctv.provider.common.ListProviderInstancesRequest.sort_direction",
        ".synctv.provider.common.ListProviderBackendsRequest.provider_type",
    ] {
        provider_builder = provider_builder.field_attribute(field, "#[serde(default)]");
    }
    {
        let path = ".synctv.provider.common.ProviderInstanceQuery";
        provider_builder = provider_builder.type_attribute(
            path,
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::IntoParams))]",
        );
    }
    provider_builder = provider_builder.type_attribute(
        ".synctv.provider.alist.LoginRequest",
        "#[serde(try_from = \"crate::http_serde::AlistLoginRequestDef\")]",
    );
    provider_builder = provider_builder.type_attribute(
        ".synctv.provider.emby.LoginRequest",
        "#[serde(try_from = \"crate::http_serde::EmbyLoginRequestDef\")]",
    );
    for (path, attr) in &provider_schema_aliases {
        provider_builder = provider_builder.type_attribute(path, attr);
    }
    provider_builder
        .out_dir(&provider_out_dir)
        .compile_with_config(
            provider_prost_config,
            &[
                "proto/providers/bilibili.proto",
                "proto/providers/bilibili_service.proto",
                "proto/providers/alist.proto",
                "proto/providers/alist_service.proto",
                "proto/providers/emby.proto",
                "proto/providers/emby_service.proto",
                "proto/providers/common.proto",
                "proto/providers/common_service.proto",
                "proto/providers/rtmp.proto",
                "proto/providers/rtmp_service.proto",
            ],
            &["."],
        )?;

    if regen_proto_enabled() {
        sync_generated_files(&main_out_dir, Path::new("src"), MAIN_GENERATED_FILES)?;
        sync_generated_files(
            &provider_out_dir,
            Path::new("src/providers"),
            PROVIDER_GENERATED_FILES,
        )?;
    }

    Ok(())
}
