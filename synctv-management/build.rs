use std::{fs, io, path::Path};

use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/management.proto");

    let out_dir = std::env::var("OUT_DIR")?;
    let out_dir = Path::new(&out_dir);
    let prost_config = prost_config();
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .out_dir(out_dir)
        .compile_with_config(
            prost_config,
            &["proto/management.proto"],
            &[".", "..", "../synctv-proto"],
        )?;
    build_pbjson(out_dir)?;

    Ok(())
}

fn prost_config() -> tonic_prost_build::Config {
    let mut config = tonic_prost_build::Config::new();
    config.extern_path(".synctv.admin", "::synctv_proto::admin");
    config.extern_path(".synctv.client", "::synctv_proto::client");
    config.extern_path(".synctv.common", "::synctv_proto::common");
    config.extern_path(".synctv.source_config", "::synctv_proto::source_config");
    config.extern_path(".synctv.provider.alist", "::synctv_proto::providers::alist");
    config.extern_path(
        ".synctv.provider.bilibili",
        "::synctv_proto::providers::bilibili",
    );
    config.extern_path(
        ".synctv.provider.common",
        "::synctv_proto::providers::common",
    );
    config.extern_path(
        ".synctv.provider.douyin",
        "::synctv_proto::providers::douyin",
    );
    config.extern_path(
        ".synctv.provider.tiktok",
        "::synctv_proto::providers::tiktok",
    );
    config.extern_path(
        ".synctv.provider.twitch",
        "::synctv_proto::providers::twitch",
    );
    config.extern_path(".synctv.provider.emby", "::synctv_proto::providers::emby");
    for provider in [
        "acfun",
        "cctv",
        "cloudreve",
        "douyu",
        "fnos",
        "huya",
        "nextcloud",
        "qnap",
        "seafile",
        "synology",
        "truenas",
        "youtube",
    ] {
        config.extern_path(
            format!(".synctv.provider.{provider}"),
            format!("::synctv_proto::providers::{provider}"),
        );
    }
    config
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

fn count_char(line: &str, needle: char) -> i32 {
    i32::try_from(line.matches(needle).count()).unwrap_or(i32::MAX)
}

fn is_enum_string_name(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
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

fn stricten_pbjson_serde_files(out_dir: &Path) -> io::Result<()> {
    let json_name_aliases = collect_json_name_aliases(out_dir)?;
    for entry in fs::read_dir(out_dir)? {
        let path = entry?.path();
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

fn build_pbjson(out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let descriptor_set = fs::read(out_dir.join("descriptor.bin"))?;
    let mut builder = pbjson_build::Builder::new();
    builder.out_dir(out_dir).use_integers_for_enums();
    builder.extern_path(".google.protobuf", "::pbjson_types");
    builder.extern_path(".synctv.admin", "::synctv_proto::admin");
    builder.extern_path(".synctv.client", "::synctv_proto::client");
    builder.extern_path(".synctv.common", "::synctv_proto::common");
    builder.extern_path(".synctv.source_config", "::synctv_proto::source_config");
    builder.extern_path(".synctv.provider.alist", "::synctv_proto::providers::alist");
    builder.extern_path(
        ".synctv.provider.bilibili",
        "::synctv_proto::providers::bilibili",
    );
    builder.extern_path(
        ".synctv.provider.common",
        "::synctv_proto::providers::common",
    );
    builder.extern_path(".synctv.provider.emby", "::synctv_proto::providers::emby");
    for provider in [
        "acfun",
        "cctv",
        "cloudreve",
        "douyu",
        "fnos",
        "huya",
        "nextcloud",
        "qnap",
        "seafile",
        "synology",
        "truenas",
        "youtube",
    ] {
        builder.extern_path(
            format!(".synctv.provider.{provider}"),
            format!("::synctv_proto::providers::{provider}"),
        );
    }
    builder.register_descriptors(&descriptor_set)?;
    builder.build(&[".synctv.management"])?;
    stricten_pbjson_serde_files(out_dir)?;
    Ok(())
}
