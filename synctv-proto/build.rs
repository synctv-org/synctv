use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorSet};

const MAIN_PROTO_FILES: [&str; 8] = [
    "proto/google/rpc/status.proto",
    "proto/google/rpc/error_details.proto",
    "proto/common.proto",
    "proto/source_config.proto",
    "proto/passkey.proto",
    "proto/client.proto",
    "proto/admin.proto",
    "proto/oauth2.proto",
];
const MAIN_PROTO_INCLUDES: [&str; 1] = ["."];
const MAIN_PBJSON_PREFIXES: [&str; 5] = [
    ".google.rpc",
    ".synctv.common",
    ".synctv.source_config",
    ".synctv.client",
    ".synctv.admin",
];

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
const PROVIDER_PBJSON_PREFIXES: [&str; 5] = [
    ".synctv.provider.common",
    ".synctv.provider.rtmp",
    ".synctv.provider.bilibili",
    ".synctv.provider.alist",
    ".synctv.provider.emby",
];

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
const PLAYBACK_PROVIDER_PBJSON_PREFIXES: [&str; 1] = [".synctv.playback_provider"];

fn build_out_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    env::var_os("OUT_DIR").map(PathBuf::from).ok_or_else(|| {
        Box::new(io::Error::new(
            io::ErrorKind::NotFound,
            "OUT_DIR is not set by Cargo",
        )) as Box<dyn std::error::Error>
    })
}

fn feature_enabled(feature: &str) -> bool {
    let env_name = format!(
        "CARGO_FEATURE_{}",
        feature.replace('-', "_").to_ascii_uppercase()
    );
    env::var_os(env_name).is_some()
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

fn collect_openapi_schema_aliases_for(
    proto_files: &[&str],
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    proto_files
        .iter()
        .copied()
        .map(collect_openapi_schema_aliases)
        .collect::<Result<Vec<_>, _>>()
        .map(|aliases| aliases.into_iter().flatten().collect())
}

fn collect_openapi_schema_aliases_when_enabled(
    proto_files: &[&str],
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    if feature_enabled("openapi") {
        collect_openapi_schema_aliases_for(proto_files)
    } else {
        Ok(Vec::new())
    }
}

fn add_openapi_attrs(
    mut builder: tonic_prost_build::Builder,
    aliases: &[(String, String)],
    schema_prefixes: &[&str],
) -> tonic_prost_build::Builder {
    for prefix in schema_prefixes {
        builder = builder
            .type_attribute(
                *prefix,
                "#[cfg_attr(feature = \"openapi\", allow(clippy::large_stack_arrays))]",
            )
            .type_attribute(
                *prefix,
                "#[cfg_attr(feature = \"openapi\", derive(utoipa::ToSchema))]",
            )
            .type_attribute(
                *prefix,
                "#[cfg_attr(feature = \"openapi\", schema(rename_all = \"camelCase\"))]",
            );
    }
    for (path, attr) in aliases {
        if schema_prefixes
            .iter()
            .any(|prefix| path.starts_with(*prefix))
        {
            builder = builder.type_attribute(path, attr);
        }
    }
    builder
}

fn add_query_params_attrs(
    mut builder: tonic_prost_build::Builder,
    paths: &[&str],
) -> tonic_prost_build::Builder {
    for path in paths {
        builder = builder.type_attribute(
            *path,
            "#[cfg_attr(feature = \"openapi\", derive(utoipa::IntoParams))]",
        );
        builder = builder.type_attribute(
            *path,
            "#[cfg_attr(feature = \"openapi\", into_params(parameter_in = Query, rename_all = \"camelCase\"))]",
        );
    }
    builder
}

fn add_field_attrs(
    mut builder: tonic_prost_build::Builder,
    fields: &[&str],
    attr: &str,
) -> tonic_prost_build::Builder {
    for field in fields {
        builder = builder.field_attribute(*field, attr);
    }
    builder
}

fn count_char(line: &str, needle: char) -> i32 {
    i32::try_from(line.matches(needle).count()).unwrap_or(i32::MAX)
}

fn proto_snake_to_lower_camel(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut uppercase_next = false;
    for character in value.chars() {
        if character == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            output.extend(character.to_uppercase());
            uppercase_next = false;
        } else {
            output.push(character);
        }
    }
    output
}

fn collect_message_json_name_aliases(
    message: DescriptorProto,
    aliases: &mut Vec<(String, String)>,
) {
    for field in message.field {
        let Some(proto_name) = field.name else {
            continue;
        };
        let json_name = field
            .json_name
            .unwrap_or_else(|| proto_snake_to_lower_camel(&proto_name));
        if proto_name != json_name {
            aliases.push((proto_name, json_name));
        }
    }
    for nested_message in message.nested_type {
        collect_message_json_name_aliases(nested_message, aliases);
    }
}

fn collect_json_name_aliases(out_dir: &Path) -> io::Result<Vec<(String, String)>> {
    let descriptor_bytes = fs::read(out_dir.join("descriptor.bin"))?;
    let descriptor_set = FileDescriptorSet::decode(descriptor_bytes.as_slice())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut aliases = Vec::new();
    for file in descriptor_set.file {
        for message in file.message_type {
            collect_message_json_name_aliases(message, &mut aliases);
        }
    }
    Ok(aliases)
}

fn string_array_literal(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_suffix(',')?;
    let trimmed = trimmed.strip_prefix('"')?;
    trimmed.strip_suffix('"')
}

fn is_proto_json_name_alias(
    line: &str,
    next_line: Option<&str>,
    json_name_aliases: &[(String, String)],
) -> bool {
    let Some(value) = string_array_literal(line) else {
        return false;
    };
    let Some(next_value) = next_line.and_then(string_array_literal) else {
        return false;
    };
    json_name_aliases
        .iter()
        .any(|(proto_name, json_name)| value == proto_name && next_value == json_name)
}

fn strict_generated_field_match_alias(line: &str) -> Option<String> {
    let arrow_index = line.find("=> Ok(GeneratedField::")?;
    let (patterns, rest) = line.split_at(arrow_index);
    if !patterns.contains('|') {
        return None;
    }

    let indent_len = patterns.len() - patterns.trim_start().len();
    let indent = &patterns[..indent_len];
    let mut canonical_patterns = patterns[indent_len..]
        .split('|')
        .map(str::trim)
        .filter(|pattern| !pattern.trim_matches('"').contains('_'));
    let canonical_pattern = canonical_patterns.next()?;
    Some(format!("{indent}{canonical_pattern} {rest}"))
}

fn strict_generated_line(line: &str) -> String {
    line.replace(
        "deserializer.deserialize_any(GeneratedVisitor)",
        "deserializer.deserialize_i64(GeneratedVisitor)",
    )
    .replace(
        "expected one of: {:?}\", &FIELDS",
        "expected one of: {:?}\", FIELDS",
    )
    .replace(
        "write!(formatter, \"expected one of: {:?}\", FIELDS)",
        "formatter.write_str(\"integer enum value\")",
    )
}

fn strict_pbjson_serde_source(source: &str, json_name_aliases: &[(String, String)]) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut output = Vec::with_capacity(lines.len());
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let next_line = lines.get(index + 1).copied();

        if is_proto_json_name_alias(line, next_line, json_name_aliases) {
            index += 1;
            continue;
        }

        if line
            .contains("fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>")
        {
            let mut depth = 0_i32;
            let mut saw_body = false;
            while index < lines.len() {
                let skipped = lines[index];
                depth += count_char(skipped, '{');
                if skipped.contains('{') {
                    saw_body = true;
                }
                depth -= count_char(skipped, '}');
                index += 1;
                if saw_body && depth == 0 {
                    break;
                }
            }
            continue;
        }

        if line.contains("const FIELDS: &[&str] = &[")
            && lines
                .get(index + 1)
                .is_some_and(|next| string_array_literal(next).is_some_and(is_enum_string_name))
        {
            while index < lines.len() {
                let skipped = lines[index];
                index += 1;
                if skipped.trim() == "];" {
                    break;
                }
            }
            continue;
        }

        if let Some(strict_line) = strict_generated_field_match_alias(line) {
            output.push(strict_line);
        } else {
            output.push(strict_generated_line(line));
        }
        index += 1;
    }

    let mut strict_source = output.join("\n");
    if source.ends_with('\n') {
        strict_source.push('\n');
    }
    strict_source
}

fn verify_strict_pbjson_serde_source(
    source: &str,
    json_name_aliases: &[(String, String)],
) -> Result<(), String> {
    for forbidden in [
        "deserializer.deserialize_any(GeneratedVisitor)",
        "expected one of: {:?}\", &FIELDS",
        "write!(formatter, \"expected one of: {:?}\", FIELDS)",
        "fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>",
    ] {
        if source.contains(forbidden) {
            return Err(format!(
                "strict ProtoJSON output still contains `{forbidden}`"
            ));
        }
    }

    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if is_proto_json_name_alias(line, lines.get(index + 1).copied(), json_name_aliases) {
            let proto_name = string_array_literal(line).unwrap_or_default();
            return Err(format!(
                "strict ProtoJSON output still advertises proto field alias `{proto_name}`"
            ));
        }
    }

    Ok(())
}

fn is_enum_string_name(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn stricten_pbjson_serde_files(out_dir: &Path) -> io::Result<()> {
    let json_name_aliases = collect_json_name_aliases(out_dir)?;
    for entry in fs::read_dir(out_dir)? {
        let path = entry?.path();
        if path.is_dir() {
            stricten_pbjson_serde_files(&path)?;
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".serde.rs") {
            continue;
        }

        let source = fs::read_to_string(&path)?;
        let strict_source = strict_pbjson_serde_source(&source, &json_name_aliases);
        if strict_source != source {
            fs::write(&path, &strict_source)?;
        }
        verify_strict_pbjson_serde_source(&strict_source, &json_name_aliases).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
    }

    Ok(())
}

fn build_pbjson(
    out_dir: &Path,
    prefixes: &[&str],
    extern_paths: &[(&str, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    let descriptor_set = fs::read(out_dir.join("descriptor.bin"))?;
    let mut builder = pbjson_build::Builder::new();
    builder.out_dir(out_dir).use_integers_for_enums();
    builder.extern_path(".google.protobuf", "::pbjson_types");
    for (proto_path, rust_path) in extern_paths {
        builder.extern_path(*proto_path, *rust_path);
    }
    builder.register_descriptors(&descriptor_set)?;
    builder.build(prefixes)?;
    stricten_pbjson_serde_files(out_dir)?;
    Ok(())
}

fn configure_well_known_types(config: &mut tonic_prost_build::Config) {
    config.compile_well_known_types();
    config.extern_path(".google.protobuf", "::pbjson_types");
}

fn build_main_protos(protoc: PathBuf, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);
    configure_well_known_types(&mut prost_config);
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::DESCRIPTOR_POOL")
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .configure(&mut prost_config, &MAIN_PROTO_FILES, &MAIN_PROTO_INCLUDES)?;

    let aliases = collect_openapi_schema_aliases_when_enabled(&MAIN_PROTO_FILES)?;
    let mut builder = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"));
    builder = add_openapi_attrs(
        builder,
        &aliases,
        &[
            ".synctv.common",
            ".synctv.source_config",
            ".synctv.client",
            ".synctv.admin",
        ],
    );
    builder = add_query_params_attrs(
        builder,
        &[
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
            ".synctv.client.SearchChatMessagesRequest",
            ".synctv.client.ListChatReactionUsersRequest",
            ".synctv.client.GetHotRoomsRequest",
            ".synctv.client.GetServerTimeRequest",
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
        ],
    );
    builder = add_field_attrs(
        builder,
        &[
            ".synctv.admin.GetUserRoomsRequest.user_id",
            ".synctv.admin.GetRoomMembersRequest.room_id",
        ],
        "#[cfg_attr(feature = \"openapi\", param(ignore))]",
    );
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &MAIN_PROTO_FILES,
        &MAIN_PROTO_INCLUDES,
    )?;
    build_pbjson(out_dir, &MAIN_PBJSON_PREFIXES, &[])?;
    Ok(())
}

fn build_provider_protos(
    protoc: PathBuf,
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);
    configure_well_known_types(&mut prost_config);
    prost_config.extern_path(".synctv.source_config", "crate::source_config");
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PROVIDERS_DESCRIPTOR_POOL")
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .configure(
            &mut prost_config,
            &PROVIDER_PROTO_FILES,
            &PROVIDER_PROTO_INCLUDES,
        )?;

    let aliases = collect_openapi_schema_aliases_when_enabled(&[
        "proto/providers/bilibili.proto",
        "proto/providers/alist.proto",
        "proto/providers/emby.proto",
        "proto/providers/common.proto",
        "proto/providers/rtmp.proto",
    ])?;
    let mut builder = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"));
    builder = add_openapi_attrs(
        builder,
        &aliases,
        &[
            ".synctv.provider.common",
            ".synctv.provider.rtmp",
            ".synctv.provider.bilibili",
            ".synctv.provider.alist",
            ".synctv.provider.emby",
        ],
    );
    builder = add_query_params_attrs(builder, &[".synctv.provider.common.ProviderInstanceQuery"]);
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &PROVIDER_PROTO_FILES,
        &PROVIDER_PROTO_INCLUDES,
    )?;
    build_pbjson(
        out_dir,
        &PROVIDER_PBJSON_PREFIXES,
        &[(".synctv.source_config", "crate::source_config")],
    )?;
    Ok(())
}

fn build_playback_provider_protos(
    protoc: PathBuf,
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);
    configure_well_known_types(&mut prost_config);
    prost_reflect_build::Builder::new()
        .descriptor_pool("crate::PLAYBACK_PROVIDER_DESCRIPTOR_POOL")
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .configure(
            &mut prost_config,
            &PLAYBACK_PROVIDER_PROTO_FILES,
            &PLAYBACK_PROVIDER_PROTO_INCLUDES,
        )?;

    let aliases = collect_openapi_schema_aliases_when_enabled(&PLAYBACK_PROVIDER_PROTO_FILES)?;
    let mut builder = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"));
    builder = add_openapi_attrs(builder, &aliases, &[".synctv.playback_provider"]);
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &PLAYBACK_PROVIDER_PROTO_FILES,
        &PLAYBACK_PROVIDER_PROTO_INCLUDES,
    )?;
    build_pbjson(out_dir, &PLAYBACK_PROVIDER_PBJSON_PREFIXES, &[])?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let out_dir = build_out_dir()?;
    let build_main = feature_enabled("main") || feature_enabled("providers");
    let build_providers = feature_enabled("providers");
    let build_playback_provider = feature_enabled("playback-provider");

    emit_proto_rerun_if_changed(Path::new("proto"))?;

    if build_main {
        let main_out_dir = out_dir.join("main");
        fs::create_dir_all(&main_out_dir)?;
        println!(
            "cargo:rustc-env=SYNCTV_PROTO_MAIN_OUT_DIR={}",
            main_out_dir.display()
        );
        build_main_protos(protoc.clone(), &main_out_dir)?;
    }

    if build_providers {
        let provider_out_dir = out_dir.join("providers");
        fs::create_dir_all(&provider_out_dir)?;
        println!(
            "cargo:rustc-env=SYNCTV_PROTO_PROVIDERS_OUT_DIR={}",
            provider_out_dir.display()
        );
        build_provider_protos(protoc.clone(), &provider_out_dir)?;
    }

    if build_playback_provider {
        let playback_provider_out_dir = out_dir.join("playback_provider");
        fs::create_dir_all(&playback_provider_out_dir)?;
        println!(
            "cargo:rustc-env=SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR={}",
            playback_provider_out_dir.display()
        );
        build_playback_provider_protos(protoc, &playback_provider_out_dir)?;
    }

    Ok(())
}
