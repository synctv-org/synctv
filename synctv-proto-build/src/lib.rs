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

const PROVIDER_PROTO_FILES: [&str; 38] = [
    "proto/providers/bilibili.proto",
    "proto/providers/bilibili_service.proto",
    "proto/providers/alist.proto",
    "proto/providers/alist_service.proto",
    "proto/providers/emby.proto",
    "proto/providers/emby_service.proto",
    "proto/providers/common.proto",
    "proto/providers/common_service.proto",
    "proto/providers/cloudreve.proto",
    "proto/providers/cloudreve_service.proto",
    "proto/providers/twitch.proto",
    "proto/providers/twitch_service.proto",
    "proto/providers/huya.proto",
    "proto/providers/huya_service.proto",
    "proto/providers/douyu.proto",
    "proto/providers/douyu_service.proto",
    "proto/providers/acfun.proto",
    "proto/providers/acfun_service.proto",
    "proto/providers/cctv.proto",
    "proto/providers/cctv_service.proto",
    "proto/providers/youtube.proto",
    "proto/providers/youtube_service.proto",
    "proto/providers/douyin.proto",
    "proto/providers/douyin_service.proto",
    "proto/providers/tiktok.proto",
    "proto/providers/tiktok_service.proto",
    "proto/providers/fnos.proto",
    "proto/providers/fnos_service.proto",
    "proto/providers/qnap.proto",
    "proto/providers/qnap_service.proto",
    "proto/providers/synology.proto",
    "proto/providers/synology_service.proto",
    "proto/providers/nextcloud.proto",
    "proto/providers/nextcloud_service.proto",
    "proto/providers/seafile.proto",
    "proto/providers/seafile_service.proto",
    "proto/providers/truenas.proto",
    "proto/providers/truenas_service.proto",
];
const PROVIDER_PROTO_INCLUDES: [&str; 1] = ["."];
const PROVIDER_PBJSON_PREFIXES: [&str; 19] = [
    ".synctv.provider.common",
    ".synctv.provider.bilibili",
    ".synctv.provider.alist",
    ".synctv.provider.emby",
    ".synctv.provider.cloudreve",
    ".synctv.provider.twitch",
    ".synctv.provider.huya",
    ".synctv.provider.douyu",
    ".synctv.provider.acfun",
    ".synctv.provider.cctv",
    ".synctv.provider.youtube",
    ".synctv.provider.douyin",
    ".synctv.provider.tiktok",
    ".synctv.provider.fnos",
    ".synctv.provider.qnap",
    ".synctv.provider.synology",
    ".synctv.provider.nextcloud",
    ".synctv.provider.seafile",
    ".synctv.provider.truenas",
];

const PLAYBACK_PROVIDER_PROTO_FILES: [&str; 22] = [
    "proto/playback_provider/common.proto",
    "proto/playback_provider/direct_url.proto",
    "proto/playback_provider/alist.proto",
    "proto/playback_provider/emby.proto",
    "proto/playback_provider/bilibili.proto",
    "proto/playback_provider/rtmp.proto",
    "proto/playback_provider/live_proxy.proto",
    "proto/playback_provider/twitch.proto",
    "proto/playback_provider/youtube.proto",
    "proto/playback_provider/huya.proto",
    "proto/playback_provider/douyu.proto",
    "proto/playback_provider/douyin.proto",
    "proto/playback_provider/tiktok.proto",
    "proto/playback_provider/acfun.proto",
    "proto/playback_provider/cctv.proto",
    "proto/playback_provider/fnos.proto",
    "proto/playback_provider/qnap.proto",
    "proto/playback_provider/synology.proto",
    "proto/playback_provider/cloudreve.proto",
    "proto/playback_provider/nextcloud.proto",
    "proto/playback_provider/seafile.proto",
    "proto/playback_provider/truenas.proto",
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

#[derive(Clone, Copy)]
enum OpenApiSchemaKind {
    Message,
    Enum,
}

struct OpenApiSchemaAlias {
    path: String,
    attribute: String,
    kind: OpenApiSchemaKind,
}

fn proto_type_declaration(line: &str) -> Option<(&str, OpenApiSchemaKind)> {
    line.strip_prefix("message ")
        .and_then(|rest| rest.split_whitespace().next())
        .map(|name| (name, OpenApiSchemaKind::Message))
        .or_else(|| {
            line.strip_prefix("enum ")
                .and_then(|rest| rest.split_whitespace().next())
                .map(|name| (name, OpenApiSchemaKind::Enum))
        })
}

fn proto_named_block(line: &str, keyword: &str) -> Option<String> {
    line.strip_prefix(keyword)
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
}

fn proto_json_name(name: &str) -> String {
    let mut segments = name.split('_');
    let mut result = segments.next().unwrap_or_default().to_string();
    for segment in segments {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            result.extend(first.to_uppercase());
            result.extend(chars);
        }
    }
    result
}

fn proto_field_name(line: &str) -> Option<&str> {
    let declaration = line.split_once('=')?.0.trim();
    let name = declaration.split_whitespace().last()?;
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric()))
    .then_some(name)
}

fn proto_bytes_field_name(line: &str) -> Option<&str> {
    let declaration = line.split_once('=')?.0.trim();
    let mut parts = declaration.split_whitespace();
    let first = parts.next()?;
    let field_type = match first {
        "optional" | "repeated" => parts.next()?,
        field_type => field_type,
    };
    (field_type == "bytes").then(|| parts.next()).flatten()
}

fn collect_proto_bytes_fields(
    proto_file: impl AsRef<Path>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
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
    let mut depth = 0_i32;
    let mut messages = Vec::<(String, i32)>::new();
    let mut fields = Vec::new();

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if let Some(message_name) = proto_named_block(line, "message ") {
            messages.push((message_name, depth));
        } else if let Some(field_name) = proto_bytes_field_name(line) {
            if !messages.is_empty() {
                let message_path = messages
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                fields.push(format!(".{package}.{message_path}.{field_name}"));
            }
        }

        depth += match_count_as_i32(raw_line, '{')?;
        depth -= match_count_as_i32(raw_line, '}')?;
        while messages
            .last()
            .is_some_and(|(_, parent_depth)| depth <= *parent_depth)
        {
            messages.pop();
        }
    }

    Ok(fields)
}

fn collect_proto_bytes_fields_for(
    proto_files: &[&str],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    proto_files
        .iter()
        .copied()
        .map(collect_proto_bytes_fields)
        .collect::<Result<Vec<_>, _>>()
        .map(|fields| fields.into_iter().flatten().collect())
}

#[derive(Default)]
struct OpenApiOneofAttributes {
    fields: Vec<String>,
    variants: Vec<(String, String)>,
}

fn collect_openapi_oneof_fields(
    proto_file: impl AsRef<Path>,
) -> Result<OpenApiOneofAttributes, Box<dyn std::error::Error>> {
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
    let mut depth = 0_i32;
    let mut messages = Vec::<(String, i32)>::new();
    let mut attributes = OpenApiOneofAttributes::default();
    let mut oneof: Option<(String, i32)> = None;

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if let Some(message_name) = proto_named_block(line, "message ") {
            messages.push((message_name, depth));
        } else if let Some(oneof_name) = proto_named_block(line, "oneof ") {
            if !messages.is_empty() {
                let message_path = messages
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                attributes
                    .fields
                    .push(format!(".{package}.{message_path}.{oneof_name}"));
                oneof = Some((oneof_name, depth));
            }
        } else if let Some((oneof_name, _)) = oneof.as_ref() {
            if let Some(field_name) = proto_field_name(line) {
                let message_path = messages
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                attributes.variants.push((
                    format!(".{package}.{message_path}.{oneof_name}.{field_name}"),
                    proto_json_name(field_name),
                ));
            }
        }

        depth += match_count_as_i32(raw_line, '{')?;
        depth -= match_count_as_i32(raw_line, '}')?;
        if oneof
            .as_ref()
            .is_some_and(|(_, parent_depth)| depth <= *parent_depth)
        {
            oneof = None;
        }
        while messages
            .last()
            .is_some_and(|(_, parent_depth)| depth <= *parent_depth)
        {
            messages.pop();
        }
    }

    Ok(attributes)
}

fn collect_openapi_oneof_fields_for(
    proto_files: &[&str],
) -> Result<OpenApiOneofAttributes, Box<dyn std::error::Error>> {
    if !feature_enabled("openapi") {
        return Ok(OpenApiOneofAttributes::default());
    }
    let collected = proto_files
        .iter()
        .copied()
        .map(collect_openapi_oneof_fields)
        .collect::<Result<Vec<_>, _>>()?;
    let mut merged = OpenApiOneofAttributes::default();
    for mut attributes in collected {
        merged.fields.append(&mut attributes.fields);
        merged.variants.append(&mut attributes.variants);
    }
    Ok(merged)
}

fn collect_openapi_schema_aliases(
    proto_file: impl AsRef<Path>,
) -> Result<Vec<OpenApiSchemaAlias>, Box<dyn std::error::Error>> {
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
    let mut current_type: Option<(String, OpenApiSchemaKind, bool)> = None;

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if depth == 0 {
            if let Some((type_name, kind)) = proto_type_declaration(line) {
                current_type = Some((type_name.to_string(), kind, false));
            }
        } else if proto_type_declaration(line).is_some() {
            if let Some((_, _, contains_nested_type)) = current_type.as_mut() {
                *contains_nested_type = true;
            }
        }

        depth += match_count_as_i32(raw_line, '{')?;
        depth -= match_count_as_i32(raw_line, '}')?;

        if depth == 0 {
            if let Some((type_name, kind, contains_nested_type)) = current_type.take() {
                if !contains_nested_type {
                    let path = format!(".{package}.{type_name}");
                    let schema_alias = format!("{package_alias}_{type_name}");
                    aliases.push(OpenApiSchemaAlias {
                        path,
                        attribute: format!(
                            "#[cfg_attr(feature = \"openapi\", schema(as = {schema_alias}))]"
                        ),
                        kind,
                    });
                }
            }
        }
    }

    Ok(aliases)
}

fn collect_openapi_schema_aliases_for(
    proto_files: &[&str],
) -> Result<Vec<OpenApiSchemaAlias>, Box<dyn std::error::Error>> {
    proto_files
        .iter()
        .copied()
        .map(collect_openapi_schema_aliases)
        .collect::<Result<Vec<_>, _>>()
        .map(|aliases| aliases.into_iter().flatten().collect())
}

fn collect_openapi_schema_aliases_when_enabled(
    proto_files: &[&str],
) -> Result<Vec<OpenApiSchemaAlias>, Box<dyn std::error::Error>> {
    if feature_enabled("openapi") {
        collect_openapi_schema_aliases_for(proto_files)
    } else {
        Ok(Vec::new())
    }
}

fn add_openapi_attrs(
    mut builder: tonic_prost_build::Builder,
    aliases: &[OpenApiSchemaAlias],
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
    for alias in aliases {
        if schema_prefixes
            .iter()
            .any(|prefix| alias.path.starts_with(*prefix))
        {
            builder = match alias.kind {
                OpenApiSchemaKind::Message => {
                    builder.message_attribute(&alias.path, &alias.attribute)
                }
                OpenApiSchemaKind::Enum => builder.enum_attribute(&alias.path, &alias.attribute),
            };
        }
    }
    builder
}

fn add_openapi_oneof_attrs(
    mut builder: tonic_prost_build::Builder,
    oneof_attributes: &OpenApiOneofAttributes,
) -> tonic_prost_build::Builder {
    for field in &oneof_attributes.fields {
        let schema_alias = field.trim_start_matches('.').replace('.', "_");
        builder = builder.type_attribute(
            field,
            format!("#[cfg_attr(feature = \"openapi\", schema(as = {schema_alias}))]"),
        );
    }
    for (field, json_name) in &oneof_attributes.variants {
        builder = builder.field_attribute(
            field,
            format!("#[cfg_attr(feature = \"openapi\", serde(rename = \"{json_name}\"))]"),
        );
    }
    builder
}

fn add_openapi_oneof_flatten_attrs(out_dir: &Path) -> io::Result<()> {
    if !feature_enabled("openapi") {
        return Ok(());
    }
    for entry in fs::read_dir(out_dir)? {
        let path = entry?.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() || !file_name.ends_with(".rs") || file_name.ends_with(".serde.rs") {
            continue;
        }

        let source = fs::read_to_string(&path)?;
        let mut output = Vec::new();
        let mut in_prost_attribute = false;
        let mut is_oneof_attribute = false;
        for line in source.lines() {
            output.push(line.to_string());
            let trimmed = line.trim();
            if trimmed.starts_with("#[prost(") {
                in_prost_attribute = true;
                is_oneof_attribute = trimmed.contains("oneof");
            } else if in_prost_attribute && trimmed.contains("oneof") {
                is_oneof_attribute = true;
            }

            if in_prost_attribute && trimmed.ends_with(")]") {
                if is_oneof_attribute {
                    let indent = &line[..line.len() - line.trim_start().len()];
                    output.push(format!(
                        "{indent}#[cfg_attr(feature = \"openapi\", serde(flatten))]"
                    ));
                }
                in_prost_attribute = false;
                is_oneof_attribute = false;
            }
        }
        let mut generated = output.join("\n");
        if source.ends_with('\n') {
            generated.push('\n');
        }
        if generated != source {
            fs::write(path, generated)?;
        }
    }
    Ok(())
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

fn add_bytes_openapi_attrs(
    mut builder: tonic_prost_build::Builder,
    fields: &[String],
) -> tonic_prost_build::Builder {
    for field in fields {
        builder = builder.field_attribute(
            field,
            "#[cfg_attr(feature = \"openapi\", schema(value_type = Vec<u8>))]",
        );
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

fn strict_pbjson_serde_source(
    source: &str,
    json_name_aliases: &[(String, String)],
    ignore_unknown_fields: bool,
) -> String {
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

        let mut generated_line =
            strict_generated_field_match_alias(line).unwrap_or_else(|| strict_generated_line(line));
        if ignore_unknown_fields {
            if generated_line.trim() == "enum GeneratedField {" {
                let indent = generated_line
                    [..generated_line.len() - generated_line.trim_start().len()]
                    .to_string();
                output.push(generated_line);
                output.push(format!("{indent}    __Ignore,"));
                index += 1;
                continue;
            }
            generated_line = generated_line.replace(
                "Err(serde::de::Error::unknown_field(value, FIELDS))",
                "Ok(GeneratedField::__Ignore)",
            );
            if generated_line.trim() == "match k {" {
                let indent = generated_line
                    [..generated_line.len() - generated_line.trim_start().len()]
                    .to_string();
                output.push(generated_line);
                output.push(format!("{indent}    GeneratedField::__Ignore => {{"));
                output.push(format!(
                    "{indent}        let _ = map_.next_value::<serde::de::IgnoredAny>()?;"
                ));
                output.push(format!("{indent}    }}"));
                index += 1;
                continue;
            }
        }
        output.push(generated_line);
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
        let ignore_unknown_fields = file_name == "synctv.source_config.serde.rs";
        let strict_source =
            strict_pbjson_serde_source(&source, &json_name_aliases, ignore_unknown_fields);
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
    builder.extern_path(".google.protobuf.FieldMask", "crate::FieldMask");
    for (proto_path, rust_path) in extern_paths {
        builder.extern_path(*proto_path, *rust_path);
    }
    builder.register_descriptors(&descriptor_set)?;
    builder.build(prefixes)?;
    stricten_pbjson_serde_files(out_dir)?;
    Ok(())
}

fn configure_well_known_types(config: &mut tonic_prost_build::Config) {
    config.bytes([".synctv"]);
    config.compile_well_known_types();
    config.extern_path(".google.protobuf", "::pbjson_types");
    config.extern_path(".google.protobuf.FieldMask", "crate::FieldMask");
}

fn build_main_protos(out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
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
    let oneof_fields = collect_openapi_oneof_fields_for(&MAIN_PROTO_FILES[2..])?;
    builder = add_openapi_oneof_attrs(builder, &oneof_fields);
    builder = add_query_params_attrs(
        builder,
        &[
            ".synctv.client.ListBlockedUsersRequest",
            ".synctv.client.ListMyRoomsRequest",
            ".synctv.client.ListNotificationsRequest",
            ".synctv.client.GetRoomMembersRequest",
            ".synctv.client.ListRoomStreamsRequest",
            ".synctv.client.ListRoomJoinReviewsRequest",
            ".synctv.client.DiscoverRoomsRequest",
            ".synctv.client.ListRoomCategoriesRequest",
            ".synctv.client.ListRoomLabelsRequest",
            ".synctv.client.ListPlaylistsRequest",
            ".synctv.client.GetChatHistoryRequest",
            ".synctv.client.SearchChatMessagesRequest",
            ".synctv.client.ListChatReactionUsersRequest",
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
    builder = add_field_attrs(
        builder,
        &[".synctv.admin.UpdateSettingsRequest.update_mask"],
        "#[cfg_attr(feature = \"openapi\", schema(value_type = String))]",
    );
    let bytes_fields = collect_proto_bytes_fields_for(&MAIN_PROTO_FILES)?;
    builder = add_bytes_openapi_attrs(builder, &bytes_fields);
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &MAIN_PROTO_FILES,
        &MAIN_PROTO_INCLUDES,
    )?;
    add_openapi_oneof_flatten_attrs(out_dir)?;
    build_pbjson(out_dir, &MAIN_PBJSON_PREFIXES, &[])?;
    Ok(())
}

fn build_provider_protos(out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
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

    let schema_files = PROVIDER_PROTO_FILES
        .iter()
        .copied()
        .filter(|file| !file.ends_with("_service.proto"))
        .collect::<Vec<_>>();
    let aliases = collect_openapi_schema_aliases_when_enabled(&schema_files)?;
    let mut builder = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"));
    builder = add_openapi_attrs(
        builder,
        &aliases,
        &[
            ".synctv.provider.common",
            ".synctv.provider.bilibili",
            ".synctv.provider.alist",
            ".synctv.provider.emby",
            ".synctv.provider.cloudreve",
            ".synctv.provider.twitch",
            ".synctv.provider.huya",
            ".synctv.provider.douyu",
            ".synctv.provider.acfun",
            ".synctv.provider.cctv",
            ".synctv.provider.youtube",
            ".synctv.provider.douyin",
            ".synctv.provider.tiktok",
            ".synctv.provider.fnos",
            ".synctv.provider.qnap",
            ".synctv.provider.synology",
            ".synctv.provider.nextcloud",
            ".synctv.provider.seafile",
            ".synctv.provider.truenas",
        ],
    );
    let oneof_fields = collect_openapi_oneof_fields_for(&PROVIDER_PROTO_FILES)?;
    builder = add_openapi_oneof_attrs(builder, &oneof_fields);
    let bytes_fields = collect_proto_bytes_fields_for(&PROVIDER_PROTO_FILES)?;
    builder = add_bytes_openapi_attrs(builder, &bytes_fields);
    builder = add_query_params_attrs(builder, &[".synctv.provider.common.ProviderInstanceQuery"]);
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &PROVIDER_PROTO_FILES,
        &PROVIDER_PROTO_INCLUDES,
    )?;
    add_openapi_oneof_flatten_attrs(out_dir)?;
    build_pbjson(
        out_dir,
        &PROVIDER_PBJSON_PREFIXES,
        &[(".synctv.source_config", "crate::source_config")],
    )?;
    Ok(())
}

fn build_playback_provider_protos(out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
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
    let oneof_fields = collect_openapi_oneof_fields_for(&PLAYBACK_PROVIDER_PROTO_FILES)?;
    builder = add_openapi_oneof_attrs(builder, &oneof_fields);
    let bytes_fields = collect_proto_bytes_fields_for(&PLAYBACK_PROVIDER_PROTO_FILES)?;
    builder = add_bytes_openapi_attrs(builder, &bytes_fields);
    builder.out_dir(out_dir).compile_with_config(
        prost_config,
        &PLAYBACK_PROVIDER_PROTO_FILES,
        &PLAYBACK_PROVIDER_PROTO_INCLUDES,
    )?;
    add_openapi_oneof_flatten_attrs(out_dir)?;
    build_pbjson(out_dir, &PLAYBACK_PROVIDER_PBJSON_PREFIXES, &[])?;
    Ok(())
}

fn prepare_build() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_dir = build_out_dir()?;
    // Cargo resolves relative rerun paths against the package manifest directory,
    // while the thin proto crates run this helper from the shared synctv-proto
    // directory. Emit absolute paths so Cargo watches the files we actually read.
    let proto_dir = fs::canonicalize("proto")?;
    emit_proto_rerun_if_changed(&proto_dir)?;
    Ok(out_dir)
}

pub fn build_main_crate() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = prepare_build()?;
    let main_out_dir = out_dir.join("main");
    fs::create_dir_all(&main_out_dir)?;
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_MAIN_OUT_DIR={}",
        main_out_dir.display()
    );
    build_main_protos(&main_out_dir)
}

pub fn build_providers_crate() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = prepare_build()?;
    let provider_out_dir = out_dir.join("providers");
    fs::create_dir_all(&provider_out_dir)?;
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_PROVIDERS_OUT_DIR={}",
        provider_out_dir.display()
    );
    build_provider_protos(&provider_out_dir)
}

pub fn build_playback_provider_crate() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = prepare_build()?;
    let playback_provider_out_dir = out_dir.join("playback_provider");
    fs::create_dir_all(&playback_provider_out_dir)?;
    println!(
        "cargo:rustc-env=SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR={}",
        playback_provider_out_dir.display()
    );
    build_playback_provider_protos(&playback_provider_out_dir)
}
