use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAIN_PROTO_FILES: [&str; 5] = [
    "proto/common.proto",
    "proto/source_config.proto",
    "proto/client.proto",
    "proto/admin.proto",
    "proto/oauth2.proto",
];
const MAIN_PROTO_INCLUDES: [&str; 1] = ["."];
const PROVIDER_PROTO_FILES: [&str; 10] = [
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
];
const PROVIDER_PROTO_INCLUDES: [&str; 1] = ["."];
const PLAYBACK_PROVIDER_PROTO_FILES: [&str; 7] = [
    "proto/playback_provider/common.proto",
    "proto/playback_provider/direct_url.proto",
    "proto/playback_provider/alist.proto",
    "proto/playback_provider/emby.proto",
    "proto/playback_provider/bilibili.proto",
    "proto/playback_provider/rtmp.proto",
    "proto/playback_provider/live_proxy.proto",
];
const PLAYBACK_PROVIDER_PROTO_INCLUDES: [&str; 1] = ["."];

fn build_out_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    env::var_os("OUT_DIR").map(PathBuf::from).ok_or_else(|| {
        Box::new(io::Error::new(
            io::ErrorKind::NotFound,
            "OUT_DIR is not set by Cargo",
        )) as Box<dyn std::error::Error>
    })
}

fn emit_proto_rerun_if_changed(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            emit_proto_rerun_if_changed(&entry?.path())?;
        }
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("proto") {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    Ok(())
}

fn match_count_as_i32(line: &str, needle: char) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(line.matches(needle).count())?)
}

fn collect_proto_fields(
    proto_files: &[&str],
) -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let mut fields = HashSet::new();

    for proto_file in proto_files {
        let source = fs::read_to_string(proto_file)?;
        let package = source
            .lines()
            .map(str::trim)
            .find_map(|line| {
                line.strip_prefix("package ")
                    .and_then(|rest| rest.strip_suffix(';'))
            })
            .ok_or_else(|| format!("missing package declaration in {proto_file}"))?;

        let mut message_stack: Vec<(String, i32)> = Vec::new();
        let mut pending_field = String::new();
        let mut depth = 0_i32;
        for raw_line in source.lines() {
            let line = raw_line.trim();
            let message_name = line
                .strip_prefix("message ")
                .and_then(|rest| rest.split_whitespace().next());
            if let Some(message_name) = message_name {
                pending_field.clear();
                message_stack.push((message_name.to_string(), depth));
            } else if let Some((message_name, _)) = message_stack.last() {
                if let Some(oneof_name) = parse_proto_oneof_name(line) {
                    fields.insert(format!(".{package}.{message_name}.{oneof_name}"));
                    pending_field.clear();
                } else if let Some(field_name) = parse_proto_field_name(line, &mut pending_field) {
                    fields.insert(format!(".{package}.{message_name}.{field_name}"));
                }
            }

            depth += match_count_as_i32(raw_line, '{')?;
            depth -= match_count_as_i32(raw_line, '}')?;
            while message_stack
                .last()
                .is_some_and(|(_, message_depth)| depth <= *message_depth)
            {
                message_stack.pop();
                pending_field.clear();
            }
        }
    }

    Ok(fields)
}

fn collect_proto_64bit_integer_field_attributes(
    proto_files: &[&str],
) -> Result<Vec<(String, &'static str)>, Box<dyn std::error::Error>> {
    let mut fields = Vec::new();

    for proto_file in proto_files {
        let source = fs::read_to_string(proto_file)?;
        let package = source
            .lines()
            .map(str::trim)
            .find_map(|line| {
                line.strip_prefix("package ")
                    .and_then(|rest| rest.strip_suffix(';'))
            })
            .ok_or_else(|| format!("missing package declaration in {proto_file}"))?;

        let mut message_stack: Vec<(String, i32)> = Vec::new();
        let mut pending_field = String::new();
        let mut depth = 0_i32;
        for raw_line in source.lines() {
            let line = raw_line.trim();
            let message_name = line
                .strip_prefix("message ")
                .and_then(|rest| rest.split_whitespace().next());
            if let Some(message_name) = message_name {
                pending_field.clear();
                message_stack.push((message_name.to_string(), depth));
            } else if let Some((message_name, _)) = message_stack.last() {
                if let Some((field_type, field_name, repeated, optional)) =
                    parse_proto_field(line, &mut pending_field)
                {
                    if let Some(attribute) =
                        serde_attribute_for_64bit_integer(&field_type, repeated, optional)
                    {
                        fields.push((format!(".{package}.{message_name}.{field_name}"), attribute));
                    }
                }
            }

            depth += match_count_as_i32(raw_line, '{')?;
            depth -= match_count_as_i32(raw_line, '}')?;
            while message_stack
                .last()
                .is_some_and(|(_, message_depth)| depth <= *message_depth)
            {
                message_stack.pop();
                pending_field.clear();
            }
        }
    }

    fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    fields.dedup_by(|left, right| left.0 == right.0);
    Ok(fields)
}

fn serde_attribute_for_64bit_integer(
    field_type: &str,
    repeated: bool,
    optional: bool,
) -> Option<&'static str> {
    match (field_type, repeated, optional) {
        ("int64" | "sint64" | "sfixed64", true, _) => {
            Some("#[serde(with = \"crate::http_serde::int64_string_vec\")]")
        }
        ("uint64" | "fixed64", true, _) => {
            Some("#[serde(with = \"crate::http_serde::uint64_string_vec\")]")
        }
        ("int64" | "sint64" | "sfixed64", _, true) => {
            Some("#[serde(with = \"crate::http_serde::int64_string_option\")]")
        }
        ("uint64" | "fixed64", _, true) => {
            Some("#[serde(with = \"crate::http_serde::uint64_string_option\")]")
        }
        ("int64" | "sint64" | "sfixed64", _, _) => {
            Some("#[serde(with = \"crate::http_serde::int64_string\")]")
        }
        ("uint64" | "fixed64", _, _) => {
            Some("#[serde(with = \"crate::http_serde::uint64_string\")]")
        }
        _ => None,
    }
}

fn parse_proto_oneof_name(line: &str) -> Option<String> {
    line.strip_prefix("oneof ")
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
}

fn parse_proto_field_name<'a>(line: &'a str, pending_field: &'a mut String) -> Option<String> {
    parse_proto_field(line, pending_field).map(|(_, field_name, _, _)| field_name)
}

fn parse_proto_field<'a>(
    line: &'a str,
    pending_field: &'a mut String,
) -> Option<(String, String, bool, bool)> {
    if line.is_empty()
        || line.starts_with("//")
        || line.starts_with("option ")
        || line.starts_with("reserved ")
        || line.starts_with("oneof ")
        || line.starts_with("message ")
        || line.starts_with("enum ")
    {
        return None;
    }

    if !pending_field.is_empty() {
        pending_field.push(' ');
    }
    pending_field.push_str(line);

    if !pending_field.contains('=') {
        return None;
    }

    let candidate = std::mem::take(pending_field);
    let before_equals = candidate.split('=').next()?.trim();
    if before_equals.is_empty() || before_equals.ends_with(')') {
        return None;
    }

    let mut tokens = before_equals.split_whitespace().collect::<Vec<_>>();
    let repeated = tokens.first().is_some_and(|token| *token == "repeated");
    let optional = tokens.first().is_some_and(|token| *token == "optional");
    if repeated || optional {
        tokens.remove(0);
    }
    if tokens.len() < 2 {
        return None;
    }
    let field_name = tokens.pop()?.to_string();
    let field_type = tokens.pop()?.to_string();
    Some((field_type, field_name, repeated, optional))
}

fn validate_field_attributes(
    proto_files: &[&str],
    field_attributes: &HashMap<String, Vec<&'static str>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let fields = collect_proto_fields(proto_files)?;
    let mut missing = field_attributes
        .keys()
        .filter(|field| !fields.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort_unstable();

    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "build.rs has serde/OpenAPI attributes for missing proto field(s): {}",
        missing.join(", ")
    )
    .into())
}

fn add_field_attribute(
    field_attributes: &mut HashMap<String, Vec<&'static str>>,
    field: &'static str,
    attribute: &'static str,
) {
    field_attributes
        .entry(field.to_string())
        .or_default()
        .push(attribute);
}

fn add_field_attributes(
    field_attributes: &mut HashMap<String, Vec<&'static str>>,
    fields: &[&'static str],
    attribute: &'static str,
) {
    for field in fields {
        add_field_attribute(field_attributes, field, attribute);
    }
}

fn add_owned_field_attributes(
    field_attributes: &mut HashMap<String, Vec<&'static str>>,
    fields: Vec<(String, &'static str)>,
) {
    for (field, attribute) in fields {
        field_attributes.entry(field).or_default().push(attribute);
    }
}

fn apply_main_field_attributes(
    prost_config: &mut tonic_prost_build::Config,
    field_attributes: &HashMap<String, Vec<&'static str>>,
) {
    let mut fields = field_attributes.keys().collect::<Vec<_>>();
    fields.sort_unstable();
    for field in fields {
        if let Some(attributes) = field_attributes.get(field) {
            for attribute in attributes {
                prost_config.field_attribute(field, *attribute);
            }
        }
    }
}

fn apply_provider_field_attributes(
    provider_builder: tonic_prost_build::Builder,
    field_attributes: &HashMap<String, Vec<&'static str>>,
) -> tonic_prost_build::Builder {
    let mut builder = provider_builder;
    let mut fields = field_attributes.keys().collect::<Vec<_>>();
    fields.sort_unstable();
    for field in fields {
        if let Some(attributes) = field_attributes.get(field) {
            for attribute in attributes {
                builder = builder.field_attribute(field, *attribute);
            }
        }
    }
    builder
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

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let out_dir = build_out_dir()?;
    let main_out_dir = out_dir.join("main");
    let provider_out_dir = out_dir.join("providers");
    let playback_provider_out_dir = out_dir.join("playback_provider");
    fs::create_dir_all(&main_out_dir)?;
    fs::create_dir_all(&provider_out_dir)?;
    fs::create_dir_all(&playback_provider_out_dir)?;

    println!(
        "cargo:rustc-env=SYNCTV_PROTO_MAIN_OUT_DIR={}",
        main_out_dir.display()
    );
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_PROVIDERS_OUT_DIR={}",
        provider_out_dir.display()
    );
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR={}",
        playback_provider_out_dir.display()
    );
    emit_proto_rerun_if_changed(Path::new("proto"))?;

    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc.clone());
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::DESCRIPTOR_POOL")
        .file_descriptor_set_path(main_out_dir.join("descriptor.bin"))
        .configure(&mut prost_config, &MAIN_PROTO_FILES, &MAIN_PROTO_INCLUDES)?;
    let main_schema_aliases = MAIN_PROTO_FILES
        .into_iter()
        .map(collect_openapi_schema_aliases)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut main_field_attributes = HashMap::new();
    add_owned_field_attributes(
        &mut main_field_attributes,
        collect_proto_64bit_integer_field_attributes(&MAIN_PROTO_FILES)?,
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.ConfirmEmailLoginRequest.email",
            ".synctv.client.ConfirmEmailLoginRequest.email_token",
            ".synctv.client.StartOpaqueRegistrationRequest.username",
            ".synctv.client.StartOpaquePasswordResetRequest.email",
            ".synctv.client.StartPasskeyRegistrationRequest.email",
            ".synctv.client.StartPasskeyRegistrationRequest.name",
            ".synctv.client.StartPasskeyBindRequest.name",
            ".synctv.client.RejectRoomJoinReviewRequest.request_id",
            ".synctv.client.RejectRoomJoinReviewRequest.reason",
            ".synctv.client.GetRoomRequest.room_id",
            ".synctv.client.JoinRoomRequest.room_id",
            ".synctv.admin.GetUserRoomsRequest.user_id",
            ".synctv.admin.GetRoomMembersRequest.room_id",
            ".synctv.admin.ListContentReportsRequest.status",
            ".synctv.admin.ListContentReportsRequest.target_type",
            ".synctv.admin.ListContentReportsRequest.reporter_user_id",
            ".synctv.admin.ListContentReportsRequest.room_id",
            ".synctv.admin.ListContentReportsRequest.search",
            ".synctv.admin.ListContentReportsRequest.target_user_id",
            ".synctv.admin.ListContentReportsRequest.target_member_user_id",
            ".synctv.admin.ListContentReportsRequest.target_chat_message_id",
            ".synctv.admin.ListContentReportsRequest.target_room_id",
            ".synctv.admin.ListContentReportsRequest.target_member_room_id",
            ".synctv.admin.ListContentReportsRequest.scope",
            ".synctv.admin.RejectRoomJoinReviewRequest.request_id",
        ],
        "#[serde(default)]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.admin.GetUserRoomsRequest.user_id",
            ".synctv.admin.GetRoomMembersRequest.room_id",
        ],
        "#[cfg_attr(feature = \"openapi\", param(ignore))]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.CreateRoomRequest.description",
            ".synctv.client.CreateRoomRequest.password",
            ".synctv.client.CreateRoomRequest.category_id",
            ".synctv.client.CreateRoomRequest.label_ids",
            ".synctv.client.JoinRoomRequest.password",
            ".synctv.client.AddMediaRequest.source_provider",
            ".synctv.client.AddMediaRequest.provider_instance_name",
            ".synctv.client.AddMediaRequest.name",
            ".synctv.client.AddMediaRequest.description",
            ".synctv.client.KickMemberRequest.user_id",
            ".synctv.client.StartPlaybackRequest.media_id",
            ".synctv.client.StartPlaybackRequest.playlist_id",
            ".synctv.client.CreatePlaylistRequest.name",
            ".synctv.client.CreatePlaylistRequest.description",
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
            ".synctv.client.GetRoomMembersRequest.sort_by",
            ".synctv.client.GetRoomMembersRequest.sort_direction",
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
            ".synctv.client.ListRoomsRequest.category_id",
            ".synctv.client.ListRoomsRequest.label_ids",
            ".synctv.client.ListRoomCategoriesRequest.include_disabled",
            ".synctv.client.ListRoomLabelsRequest.include_disabled",
            ".synctv.client.ListRoomLabelsRequest.category_id",
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
            ".synctv.client.GetChatMessageRequest.message_id",
            ".synctv.client.GetChatMessageRequest.include_deleted",
            ".synctv.client.GetChatMessageContextRequest.message_id",
            ".synctv.client.GetChatMessageContextRequest.before_limit",
            ".synctv.client.GetChatMessageContextRequest.after_limit",
            ".synctv.client.GetChatMessageContextRequest.include_deleted",
            ".synctv.client.GetChatPlaybackMessagesRequest.playback_media_id",
            ".synctv.client.GetChatPlaybackMessagesRequest.playback_playlist_id",
            ".synctv.client.GetChatPlaybackMessagesRequest.playback_target",
            ".synctv.client.GetChatPlaybackMessagesRequest.position_seconds",
            ".synctv.client.GetChatPlaybackMessagesRequest.before_seconds",
            ".synctv.client.GetChatPlaybackMessagesRequest.after_seconds",
            ".synctv.client.GetChatPlaybackMessagesRequest.limit",
            ".synctv.client.GetChatPlaybackMessagesRequest.include_deleted",
            ".synctv.client.ListChatReactionUsersRequest.message_id",
            ".synctv.client.ListChatReactionUsersRequest.reaction_key",
            ".synctv.client.ListChatReactionUsersRequest.limit",
            ".synctv.client.ListChatReactionUsersRequest.cursor",
            ".synctv.client.ListRoomContentReportsRequest.page",
            ".synctv.client.ListRoomContentReportsRequest.page_size",
            ".synctv.client.ListRoomContentReportsRequest.status",
            ".synctv.client.ListRoomContentReportsRequest.target_type",
            ".synctv.client.ListRoomContentReportsRequest.target_member_user_id",
            ".synctv.client.ListRoomContentReportsRequest.target_chat_message_id",
            ".synctv.client.ListRoomContentReportsRequest.search",
            ".synctv.client.ChatMessageSend.mentions",
            ".synctv.client.SendChatMessageRequest.mentions",
            ".synctv.client.GetHotRoomsRequest.limit",
            ".synctv.client.SetUsernameRequest.new_username",
            ".synctv.client.UpdateUserPreferencesRequest.two_factor_enabled",
            ".synctv.client.WebSocketConnectRequest.ticket",
            ".synctv.client.DeletePlaylistQuery.force",
            ".synctv.client.MoveMediaRequest.media_ids",
            ".synctv.client.MoveMediaRequest.source_playlist_id",
            ".synctv.client.MoveMediaRequest.target_playlist_id",
            ".synctv.client.MoveMediaRequest.all_from_scope",
            ".synctv.client.MoveMediaRequest.before_media_id",
            ".synctv.client.MoveMediaRequest.after_media_id",
            ".synctv.client.UpdatePlaybackStateRequest.playing",
            ".synctv.client.UpdatePlaybackStateRequest.position",
            ".synctv.client.UpdatePlaybackStateRequest.speed",
            ".synctv.client.UpdatePlaybackStateRequest.version",
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
            ".synctv.client.UnlinkProviderRequest.provider_instance_name",
            ".synctv.client.StartRoomPasswordLoginRequest.room_id",
            ".synctv.client.CreateMediaCoverUploadSessionRequest.room_id",
            ".synctv.client.CreateMediaCoverUploadSessionRequest.media_id",
            ".synctv.client.UpdateMediaCoverRequest.room_id",
            ".synctv.client.UpdateMediaCoverRequest.media_id",
            ".synctv.client.CreateRoomCoverUploadSessionRequest.room_id",
            ".synctv.client.UpdateRoomCoverRequest.room_id",
            ".synctv.client.CreatePlaylistCoverUploadSessionRequest.room_id",
            ".synctv.client.CreatePlaylistCoverUploadSessionRequest.playlist_id",
            ".synctv.client.UpdatePlaylistCoverRequest.room_id",
            ".synctv.client.UpdatePlaylistCoverRequest.playlist_id",
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
            ".synctv.admin.ListRoomsRequest.category_id",
            ".synctv.admin.ListRoomsRequest.label_ids",
            ".synctv.admin.ListRoomCategoriesRequest.include_disabled",
            ".synctv.admin.ListRoomLabelsRequest.include_disabled",
            ".synctv.admin.ListRoomLabelsRequest.category_id",
            ".synctv.admin.UpsertRoomCategoryRequest.description",
            ".synctv.admin.UpsertRoomCategoryRequest.sort_order",
            ".synctv.admin.UpsertRoomCategoryRequest.is_enabled",
            ".synctv.admin.UpsertRoomLabelRequest.description",
            ".synctv.admin.UpsertRoomLabelRequest.color",
            ".synctv.admin.UpsertRoomLabelRequest.category_id",
            ".synctv.admin.UpsertRoomLabelRequest.sort_order",
            ".synctv.admin.UpsertRoomLabelRequest.is_enabled",
            ".synctv.admin.UpdateRoomTaxonomyRequest.room_id",
            ".synctv.admin.UpdateRoomTaxonomyRequest.category_id",
            ".synctv.admin.UpdateRoomTaxonomyRequest.label_ids",
            ".synctv.admin.UpdateRoomTaxonomyRequest.clear_category",
            ".synctv.admin.GetRoomMembersRequest.page",
            ".synctv.admin.GetRoomMembersRequest.page_size",
            ".synctv.admin.GetRoomMembersRequest.search",
            ".synctv.admin.GetRoomMembersRequest.role",
            ".synctv.admin.GetRoomMembersRequest.sort_by",
            ".synctv.admin.GetRoomMembersRequest.sort_direction",
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
            ".synctv.admin.CreateUserRequest.email",
            ".synctv.admin.CreateUserRequest.password",
            ".synctv.client.EditMediaRequest.media_id",
            ".synctv.client.EditMediaRequest.description",
            ".synctv.client.EditChatMessageRequest.message_id",
            ".synctv.client.EditChatMessageRequest.metadata",
            ".synctv.client.EditChatMessageRequest.client_operation_id",
            ".synctv.client.DeleteChatMessageRequest.message_id",
            ".synctv.client.DeleteChatMessageRequest.client_operation_id",
            ".synctv.client.DeleteEntriesRequest.playlist_ids",
            ".synctv.client.DeleteEntriesRequest.media_ids",
            ".synctv.client.DeleteEntriesRequest.force",
            ".synctv.client.ClearPlaylistRequest.playlist_id",
            ".synctv.client.UpdatePlaylistRequest.playlist_id",
            ".synctv.client.MovePlaylistRequest.playlist_id",
            ".synctv.client.UpdatePlaylistRequest.name",
            ".synctv.client.UpdatePlaylistRequest.description",
            ".synctv.client.UpdateMemberPermissionsRequest.user_id",
            ".synctv.client.UpdateMemberPermissionsRequest.role",
            ".synctv.client.UpdateMemberPermissionsRequest.added_permissions",
            ".synctv.client.UpdateMemberPermissionsRequest.removed_permissions",
            ".synctv.client.UpdateMemberPermissionsRequest.admin_added_permissions",
            ".synctv.client.UpdateMemberPermissionsRequest.admin_removed_permissions",
            ".synctv.admin.SetUserPasswordRequest.user_id",
            ".synctv.admin.SetUserPasswordRequest.password",
            ".synctv.admin.SetUserPasswordRequest.reason",
            ".synctv.admin.UpdateUserUsernameRequest.user_id",
            ".synctv.admin.UpdateUserRoleRequest.user_id",
            ".synctv.admin.UpdateUserPreferencesRequest.user_id",
            ".synctv.admin.UpdateUserPreferencesRequest.two_factor_enabled",
            ".synctv.admin.BanUserRequest.user_id",
            ".synctv.admin.UpdateRoomPasswordRequest.room_id",
            ".synctv.admin.UpdateRoomPasswordRequest.new_password",
            ".synctv.admin.UpdateRoomSettingsRequest.room_id",
            ".synctv.admin.AddMemberRequest.room_id",
            ".synctv.admin.AddMemberRequest.role",
            ".synctv.admin.AddMemberRequest.notify",
            ".synctv.admin.UpdateMemberPermissionsRequest.room_id",
            ".synctv.admin.UpdateMemberPermissionsRequest.user_id",
            ".synctv.admin.UpdateMemberPermissionsRequest.role",
            ".synctv.admin.UpdateMemberPermissionsRequest.added_permissions",
            ".synctv.admin.UpdateMemberPermissionsRequest.removed_permissions",
            ".synctv.admin.UpdateMemberPermissionsRequest.admin_added_permissions",
            ".synctv.admin.UpdateMemberPermissionsRequest.admin_removed_permissions",
            ".synctv.admin.KickMemberRequest.room_id",
            ".synctv.admin.KickMemberRequest.user_id",
            ".synctv.admin.BanRoomRequest.room_id",
        ],
        "#[serde(default)]",
    );
    prost_config.message_attribute(
        ".synctv.client.MovePlaylistRequest",
        "#[serde(try_from = \"crate::http_serde::MovePlaylistRequestDef\")]",
    );
    prost_config.message_attribute(
        ".synctv.client.StartOpaqueLoginRequest",
        "#[serde(try_from = \"crate::http_serde::StartOpaqueLoginRequestDef\")]",
    );
    prost_config.message_attribute(
        ".synctv.client.LoginWithDirectPasswordRequest",
        "#[serde(try_from = \"crate::http_serde::LoginWithDirectPasswordRequestDef\")]",
    );
    prost_config.message_attribute(
        ".synctv.client.StartPasskeyLoginRequest",
        "#[serde(try_from = \"crate::http_serde::StartPasskeyLoginRequestDef\")]",
    );
    for (path, attr) in [
        (
            ".synctv.source_config.BilibiliMediaSourceConfig",
            "#[serde(try_from = \"crate::http_serde::BilibiliMediaSourceConfigDef\")]",
        ),
        (
            ".synctv.source_config.BilibiliMediaSourceConfig",
            "#[serde(into = \"crate::http_serde::BilibiliMediaSourceConfigDef\")]",
        ),
        (
            ".synctv.source_config.MediaSourceConfig",
            "#[serde(try_from = \"crate::http_serde::MediaSourceConfigDef\")]",
        ),
        (
            ".synctv.source_config.MediaSourceConfig",
            "#[serde(into = \"crate::http_serde::MediaSourceConfigDef\")]",
        ),
        (
            ".synctv.source_config.PlaylistSourceConfig",
            "#[serde(try_from = \"crate::http_serde::PlaylistSourceConfigDef\")]",
        ),
        (
            ".synctv.source_config.PlaylistSourceConfig",
            "#[serde(into = \"crate::http_serde::PlaylistSourceConfigDef\")]",
        ),
    ] {
        prost_config.message_attribute(path, attr);
    }
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.StartOpaqueRegistrationRequest.registration_request",
            ".synctv.client.StartOpaqueRegistrationResponse.registration_response",
            ".synctv.client.FinishOpaqueRegistrationRequest.registration_upload",
            ".synctv.client.StartOpaqueLoginRequest.credential_request",
            ".synctv.client.StartOpaqueLoginResponse.credential_response",
            ".synctv.client.FinishOpaqueLoginRequest.credential_finalization",
            ".synctv.client.StartOpaquePasswordUpdateRequest.credential_request",
            ".synctv.client.StartOpaquePasswordUpdateRequest.registration_request",
            ".synctv.client.StartOpaquePasswordUpdateResponse.credential_response",
            ".synctv.client.StartOpaquePasswordUpdateResponse.registration_response",
            ".synctv.client.FinishOpaquePasswordUpdateRequest.credential_finalization",
            ".synctv.client.FinishOpaquePasswordUpdateRequest.registration_upload",
            ".synctv.client.StartOpaquePasswordResetRequest.registration_request",
            ".synctv.client.StartOpaquePasswordResetResponse.registration_response",
            ".synctv.client.FinishOpaquePasswordResetRequest.registration_upload",
            ".synctv.client.StartRoomPasswordRegistrationRequest.registration_request",
            ".synctv.client.StartRoomPasswordRegistrationResponse.registration_response",
            ".synctv.client.FinishRoomPasswordRegistrationRequest.registration_upload",
            ".synctv.client.StartRoomPasswordLoginRequest.credential_request",
            ".synctv.client.StartRoomPasswordLoginResponse.credential_response",
            ".synctv.client.FinishRoomPasswordLoginRequest.credential_finalization",
        ],
        "#[serde(with = \"crate::http_serde::base64_bytes\")]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.ApiErrorResponse.code",
            ".synctv.client.ApiErrorResponse.request_id",
            ".synctv.client.HealthResponse.details",
            ".synctv.client.HealthDetails.cluster",
            ".synctv.client.HealthDetails.ws_ticket",
            ".synctv.client.HealthDetails.email",
            ".synctv.client.HealthDetails.livestream",
            ".synctv.client.HealthDetails.memory",
            ".synctv.client.HealthDetails.message",
            ".synctv.client.HealthDetails.webrtc",
            ".synctv.client.GetIceServersResponse.webrtc",
            ".synctv.client.WebRtcStatus.local_addr",
            ".synctv.client.WebRtcStatus.external_addr",
            ".synctv.client.WebRtcStatus.message",
        ],
        "#[serde(skip_serializing_if = \"Option::is_none\")]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.CreateRoomRequest.settings",
            ".synctv.client.Room.settings",
            ".synctv.client.Media.metadata",
            ".synctv.client.PlaybackState.target",
            ".synctv.client.UpdateRoomSettingsRequest.settings",
            ".synctv.client.GetRoomSettingsResponse.settings",
            ".synctv.client.ResetRoomSettingsResponse.settings",
            ".synctv.client.UserPreferences.settings",
            ".synctv.client.StartPlaybackRequest.target",
            ".synctv.client.StartPasskeyLoginResponse.options",
            ".synctv.client.FinishPasskeyLoginRequest.credential",
            ".synctv.client.StartPasskeyRegistrationResponse.options",
            ".synctv.client.FinishPasskeyRegistrationRequest.credential",
            ".synctv.client.StartPasskeyBindResponse.options",
            ".synctv.client.FinishPasskeyBindRequest.credential",
            ".synctv.client.StartMfaPasskeyResponse.options",
            ".synctv.client.FinishMfaPasskeyRequest.credential",
            ".synctv.client.FinishSensitiveOperationVerificationRequest.passkey_credential",
            ".synctv.client.StartOpaquePasswordUpdateResponse.passkey_options",
            ".synctv.client.FinishOpaquePasswordUpdateRequest.passkey_credential",
            ".synctv.client.ListPlaylistItemsRequest.target",
            ".synctv.client.PlaylistItem.target",
            ".synctv.client.PlaylistBrowsePathNode.target",
            ".synctv.client.NotificationProto.data",
            ".synctv.client.UserAvatar.metadata",
            ".synctv.client.CreateUserAvatarUploadSessionRequest.metadata",
            ".synctv.client.FileCover.metadata",
            ".synctv.client.MediaCover.metadata",
            ".synctv.client.CreateMediaCoverUploadSessionRequest.metadata",
            ".synctv.client.CreateRoomCoverUploadSessionRequest.metadata",
            ".synctv.client.CreatePlaylistCoverUploadSessionRequest.metadata",
            ".synctv.client.ChatAttachment.metadata",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.metadata",
            ".synctv.client.SendChatMessageRequest.metadata",
            ".synctv.client.EditChatMessageRequest.metadata",
            ".synctv.admin.AdminRoom.settings",
            ".synctv.admin.SettingsGroup.settings",
            ".synctv.admin.GetRoomSettingsResponse.settings",
            ".synctv.admin.UpdateRoomSettingsRequest.settings",
            ".synctv.admin.GetSystemStatsResponse.additional_stats",
        ],
        "#[serde(with = \"crate::http_serde::json_bytes\")]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.CreateRoomRequest.settings",
            ".synctv.client.DeletePasskeyRequest.credential_id",
            ".synctv.client.StartPlaybackRequest.target",
            ".synctv.client.UpdatePlaybackStateRequest.type",
            ".synctv.client.Playback.is_live",
            ".synctv.client.ListPlaylistItemsRequest.target",
            ".synctv.client.ObserveResource.delivery_mode",
            ".synctv.client.FinishSensitiveOperationVerificationRequest.passkey_credential",
            ".synctv.client.ChatAttachment.filename",
            ".synctv.client.ChatAttachment.kind",
            ".synctv.client.ChatAttachment.metadata",
            ".synctv.client.ChatAttachment.reuse_token",
            ".synctv.client.ChatAttachmentReference.kind",
            ".synctv.client.ChatAttachmentUploadSession.upload_token",
            ".synctv.client.UserAvatarUploadSession.upload_token",
            ".synctv.client.MediaCoverUploadSession.upload_token",
            ".synctv.client.RoomCoverUploadSession.upload_token",
            ".synctv.client.PlaylistCoverUploadSession.upload_token",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.client_attachment_id",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.filename",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.width",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.height",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.duration_seconds",
            ".synctv.client.CreateChatAttachmentUploadSessionRequest.bitrate_bps",
            ".synctv.client.CreateUserAvatarUploadSessionRequest.duration_seconds",
            ".synctv.client.CreateUserAvatarUploadSessionRequest.bitrate_bps",
            ".synctv.client.CreateMediaCoverUploadSessionRequest.duration_seconds",
            ".synctv.client.CreateMediaCoverUploadSessionRequest.bitrate_bps",
            ".synctv.client.CreateRoomCoverUploadSessionRequest.duration_seconds",
            ".synctv.client.CreateRoomCoverUploadSessionRequest.bitrate_bps",
            ".synctv.client.CreatePlaylistCoverUploadSessionRequest.duration_seconds",
            ".synctv.client.CreatePlaylistCoverUploadSessionRequest.bitrate_bps",
            ".synctv.client.CompleteChatAttachmentUploadSessionRequest.file_id",
            ".synctv.client.CompleteChatAttachmentUploadSessionRequest.ownership_proof",
            ".synctv.client.CompleteUserAvatarUploadSessionRequest.file_id",
            ".synctv.client.CompleteUserAvatarUploadSessionRequest.ownership_proof",
            ".synctv.client.CompleteMediaCoverUploadSessionRequest.file_id",
            ".synctv.client.CompleteMediaCoverUploadSessionRequest.ownership_proof",
            ".synctv.client.CompleteRoomCoverUploadSessionRequest.file_id",
            ".synctv.client.CompleteRoomCoverUploadSessionRequest.ownership_proof",
            ".synctv.client.CompletePlaylistCoverUploadSessionRequest.file_id",
            ".synctv.client.CompletePlaylistCoverUploadSessionRequest.ownership_proof",
            ".synctv.client.SendChatMessageRequest.attachments",
            ".synctv.client.ListPinnedChatMessagesRequest.limit",
            ".synctv.client.PinChatMessageRequest.message_id",
            ".synctv.client.PinChatMessageRequest.note",
            ".synctv.client.PinChatMessageRequest.client_operation_id",
            ".synctv.client.UnpinChatMessageRequest.message_id",
            ".synctv.client.UnpinChatMessageRequest.client_operation_id",
            ".synctv.client.UpdateUserAvatarRequest.avatar_reference",
            ".synctv.client.UpdateMediaCoverRequest.cover_reference",
            ".synctv.client.UpdateRoomCoverRequest.cover_reference",
            ".synctv.client.UpdatePlaylistCoverRequest.cover_reference",
            ".synctv.client.SendChatMessageRequest.reply_to_message_id",
            ".synctv.client.SendChatMessageRequest.metadata",
            ".synctv.client.SendChatMessageRequest.display_position",
            ".synctv.client.SendChatMessageRequest.display_color",
            ".synctv.client.MarkAllAsReadRequest.before",
            ".synctv.admin.UpdateRoomSettingsRequest.settings",
        ],
        "#[serde(default)]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.source_config.DirectUrlMediaResourceConfig.name",
            ".synctv.source_config.DirectUrlMediaResourceConfig.headers",
            ".synctv.source_config.DirectUrlMediaResourceConfig.format",
            ".synctv.source_config.DirectUrlMediaSourceConfig.medias",
            ".synctv.source_config.DirectUrlMediaSourceConfig.subtitles",
            ".synctv.source_config.DirectUrlMediaSourceConfig.danmakus",
            ".synctv.source_config.DirectUrlMediaSourceConfig.is_live",
            ".synctv.source_config.DirectUrlMediaSourceConfig.duration_seconds",
            ".synctv.source_config.DirectUrlSubtitleSourceConfig.name",
            ".synctv.source_config.DirectUrlSubtitleSourceConfig.language",
            ".synctv.source_config.DirectUrlSubtitleSourceConfig.headers",
            ".synctv.source_config.DirectUrlSubtitleSourceConfig.format",
            ".synctv.source_config.DirectUrlDanmakuSourceConfig.name",
            ".synctv.source_config.DirectUrlDanmakuSourceConfig.headers",
            ".synctv.source_config.BilibiliVideoSourceConfig.bvid",
            ".synctv.source_config.BilibiliVideoSourceConfig.aid",
            ".synctv.source_config.BilibiliVideoSourceConfig.shared",
            ".synctv.source_config.BilibiliPgcSourceConfig.shared",
            ".synctv.source_config.BilibiliLiveSourceConfig.shared",
        ],
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.admin.BanUserRequest.reason",
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.admin.BanRoomRequest.reason",
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.admin.KickStreamRequest.reason",
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.admin.BatchBanUsersRequest.reason",
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.admin.BatchBanRoomsRequest.reason",
        "#[serde(default)]",
    );
    add_field_attribute(
        &mut main_field_attributes,
        ".synctv.client.GetPublicSettingsResponse.email_whitelist_domains",
        "#[serde(default, skip_serializing_if = \"Vec::is_empty\")]",
    );
    add_field_attributes(
        &mut main_field_attributes,
        &[
            ".synctv.client.Media.source_provider",
            ".synctv.client.Playlist.source_provider",
            ".synctv.client.CreatePlaylistRequest.source_provider",
            ".synctv.client.ListPlaylistsRequest.source_provider",
            ".synctv.client.AddMediaRequest.source_provider",
            ".synctv.client.ListPlaylistItemsRequest.source_provider",
        ],
        "#[serde(with = \"crate::http_serde::source_provider\")]",
    );
    validate_field_attributes(&MAIN_PROTO_FILES, &main_field_attributes)?;
    apply_main_field_attributes(&mut prost_config, &main_field_attributes);
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
        "synctv.source_config.BilibiliMediaSourceConfig.source",
        "synctv.source_config.MediaSourceConfig.provider",
        "synctv.source_config.PlaylistSourceConfig.provider",
    ] {
        main_builder = main_builder.field_attribute(path, "#[serde(skip)]");
    }
    main_builder = main_builder.enum_attribute(
        ".synctv.source_config.SourceProvider",
        "#[serde(rename_all = \"snake_case\")]",
    );
    for path in [
        ".synctv.client.ListMyRoomsRequest",
        ".synctv.client.ListNotificationsRequest",
        ".synctv.client.GetRoomMembersRequest",
        ".synctv.client.ListRoomStreamsRequest",
        ".synctv.client.ListRoomJoinReviewsRequest",
        ".synctv.client.ListRoomsRequest",
        ".synctv.client.ListRoomCategoriesRequest",
        ".synctv.client.ListRoomLabelsRequest",
        ".synctv.client.ListPlaylistsRequest",
        ".synctv.client.GetChatHistoryRequest",
        ".synctv.client.ListChatReactionUsersRequest",
        ".synctv.client.GetHotRoomsRequest",
        ".synctv.client.GetAuthorizationUrlRequest",
        ".synctv.client.GetAuthorizationUrlForBindRequest",
        ".synctv.client.UnlinkProviderRequest",
        ".synctv.admin.ListUsersRequest",
        ".synctv.admin.ListUserRegistrationReviewsRequest",
        ".synctv.admin.ListRoomCreationReviewsRequest",
        ".synctv.admin.ListRoomJoinReviewsRequest",
        ".synctv.admin.ListRoomCategoriesRequest",
        ".synctv.admin.ListRoomLabelsRequest",
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
        main_builder = main_builder.type_attribute(
            path,
            "#[cfg_attr(feature = \"openapi\", into_params(parameter_in = Query))]",
        );
    }
    for (path, attr) in &main_schema_aliases {
        main_builder = main_builder.type_attribute(path, attr);
    }
    main_builder.out_dir(&main_out_dir).compile_with_config(
        prost_config,
        &MAIN_PROTO_FILES,
        &MAIN_PROTO_INCLUDES,
    )?;

    let mut provider_prost_config = tonic_prost_build::Config::new();
    provider_prost_config.protoc_executable(protoc);
    provider_prost_config.extern_path(".synctv.source_config", "crate::source_config");
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PROVIDERS_DESCRIPTOR_POOL")
        .file_descriptor_set_path(provider_out_dir.join("descriptor.bin"))
        .configure(
            &mut provider_prost_config,
            &PROVIDER_PROTO_FILES,
            &PROVIDER_PROTO_INCLUDES,
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

    let mut provider_field_attributes = HashMap::new();
    add_owned_field_attributes(
        &mut provider_field_attributes,
        collect_proto_64bit_integer_field_attributes(&PROVIDER_PROTO_FILES)?,
    );
    add_field_attributes(
        &mut provider_field_attributes,
        &[
            ".synctv.provider.bilibili.ParseRequest.instance_name",
            ".synctv.provider.bilibili.LoginQRRequest.instance_name",
            ".synctv.provider.bilibili.CheckQRRequest.instance_name",
            ".synctv.provider.bilibili.StartSMSLoginRequest.instance_name",
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
            ".synctv.provider.common.ListAvailableProviderInstancesRequest.provider_type",
            ".synctv.provider.common.ListProviderInstancesRequest.page",
            ".synctv.provider.common.ListProviderInstancesRequest.page_size",
            ".synctv.provider.common.ListProviderInstancesRequest.provider_type",
            ".synctv.provider.common.ListProviderInstancesRequest.search",
            ".synctv.provider.common.ListProviderInstancesRequest.enabled",
            ".synctv.provider.common.ListProviderInstancesRequest.tls",
            ".synctv.provider.common.ListProviderInstancesRequest.sort_by",
            ".synctv.provider.common.ListProviderInstancesRequest.sort_direction",
            ".synctv.provider.common.ListProviderBackendsRequest.provider_type",
        ],
        "#[serde(default)]",
    );
    add_field_attributes(
        &mut provider_field_attributes,
        &[
            ".synctv.provider.common.ProviderInstance.providers",
            ".synctv.provider.common.AddProviderInstanceRequest.providers",
            ".synctv.provider.common.UpdateProviderInstanceRequest.providers",
        ],
        "#[serde(with = \"crate::http_serde::provider_type_vec\")]",
    );
    add_field_attributes(
        &mut provider_field_attributes,
        &[
            ".synctv.provider.common.ListAvailableProviderInstancesRequest.provider_type",
            ".synctv.provider.common.ListProviderInstancesRequest.provider_type",
            ".synctv.provider.common.ListProviderBackendsRequest.provider_type",
        ],
        "#[serde(with = \"crate::http_serde::provider_type\")]",
    );
    validate_field_attributes(&PROVIDER_PROTO_FILES, &provider_field_attributes)?;
    provider_builder =
        apply_provider_field_attributes(provider_builder, &provider_field_attributes);
    {
        let path = ".synctv.provider.common.ProviderInstanceQuery";
        provider_builder = provider_builder.type_attribute(
            path,
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::IntoParams))]",
        );
        provider_builder = provider_builder.type_attribute(
            path,
            "#[cfg_attr(feature = \"openapi\", into_params(parameter_in = Query))]",
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
            &PROVIDER_PROTO_FILES,
            &PROVIDER_PROTO_INCLUDES,
        )?;

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut playback_provider_prost_config = tonic_prost_build::Config::new();
    playback_provider_prost_config.protoc_executable(protoc);
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PLAYBACK_PROVIDER_DESCRIPTOR_POOL")
        .file_descriptor_set_path(playback_provider_out_dir.join("descriptor.bin"))
        .configure(
            &mut playback_provider_prost_config,
            &PLAYBACK_PROVIDER_PROTO_FILES,
            &PLAYBACK_PROVIDER_PROTO_INCLUDES,
        )?;
    let playback_provider_schema_aliases = PLAYBACK_PROVIDER_PROTO_FILES
        .into_iter()
        .map(collect_openapi_schema_aliases)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut playback_provider_builder = tonic_prost_build::configure();
    playback_provider_builder = playback_provider_builder
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(playback_provider_out_dir.join("descriptor.bin"))
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", allow(clippy::large_stack_arrays))]",
        )
        .type_attribute(
            ".",
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::ToSchema))]",
        );
    let mut playback_provider_field_attributes = HashMap::new();
    add_owned_field_attributes(
        &mut playback_provider_field_attributes,
        collect_proto_64bit_integer_field_attributes(&PLAYBACK_PROVIDER_PROTO_FILES)?,
    );
    add_field_attributes(
        &mut playback_provider_field_attributes,
        &[
            ".synctv.playback_provider.direct_url.GetDirectUrlStreamRequest.range",
            ".synctv.playback_provider.direct_url.GetDirectUrlHlsSegmentRequest.range",
            ".synctv.playback_provider.alist.GetAlistFileStreamRequest.range",
            ".synctv.playback_provider.alist.GetAlistTranscodedHlsSegmentRequest.range",
            ".synctv.playback_provider.emby.GetEmbyMediaStreamRequest.range",
            ".synctv.playback_provider.emby.GetEmbyHlsSegmentRequest.range",
            ".synctv.playback_provider.bilibili.GetBilibiliMediaStreamRequest.range",
            ".synctv.playback_provider.bilibili.GetBilibiliHlsSegmentRequest.range",
            ".synctv.playback_provider.bilibili.GetBilibiliDashSegmentRequest.range",
            ".synctv.playback_provider.rtmp.GetRtmpHlsSegmentRequest.range",
            ".synctv.playback_provider.live_proxy.GetLiveProxyHlsSegmentRequest.range",
        ],
        "#[serde(default)]",
    );
    validate_field_attributes(
        &PLAYBACK_PROVIDER_PROTO_FILES,
        &playback_provider_field_attributes,
    )?;
    playback_provider_builder = apply_provider_field_attributes(
        playback_provider_builder,
        &playback_provider_field_attributes,
    );
    for (path, attr) in &playback_provider_schema_aliases {
        playback_provider_builder = playback_provider_builder.type_attribute(path, attr);
    }
    playback_provider_builder
        .out_dir(&playback_provider_out_dir)
        .compile_with_config(
            playback_provider_prost_config,
            &PLAYBACK_PROVIDER_PROTO_FILES,
            &PLAYBACK_PROVIDER_PROTO_INCLUDES,
        )?;

    Ok(())
}
