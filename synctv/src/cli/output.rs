use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value};

use super::human_output::ToHuman;

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ConfigOutputFormat {
    Yaml,
    Json,
    Toml,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RemoteOutputFormat {
    Human,
    Json,
    Yaml,
}

pub(super) fn print_json<T>(value: &T) -> Result<()>
where
    T: ?Sized + Serialize,
{
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub(super) fn print_structured_output<T>(format: RemoteOutputFormat, value: &T) -> Result<()>
where
    T: ?Sized + Serialize + ToHuman,
{
    match format {
        RemoteOutputFormat::Human => print_human(value),
        RemoteOutputFormat::Json => print_json(value),
        RemoteOutputFormat::Yaml => print_yaml(value),
    }
}

pub(super) fn print_humanized_structured_output<T>(
    format: RemoteOutputFormat,
    value: &T,
) -> Result<()>
where
    T: ?Sized + ToHuman,
    T::Human: Serialize,
{
    let human = value.to_human();
    match format {
        RemoteOutputFormat::Human | RemoteOutputFormat::Yaml => print_yaml(&human),
        RemoteOutputFormat::Json => print_json(&human),
    }
}

fn print_human<T>(value: &T) -> Result<()>
where
    T: ?Sized + ToHuman,
{
    print_yaml(&value.to_human())
}

pub(super) fn print_yaml<T>(value: &T) -> Result<()>
where
    T: ?Sized + Serialize,
{
    print!("{}", serde_yaml::to_string(value)?);
    Ok(())
}

pub(super) fn print_toml(value: &Value) -> Result<()> {
    let mut value = value.clone();
    prune_null_config_values(&mut value);
    print!("{}", toml::to_string_pretty(&value)?);
    Ok(())
}

pub(super) fn redact_config_for_display(config: &synctv_core::Config) -> Result<Value> {
    let mut value = serde_json::to_value(config)?;
    redact_config_value(&mut value);
    Ok(value)
}

fn redact_config_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            redact_known_secret_fields(map);
            for child in map.values_mut() {
                redact_config_value(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                redact_config_value(child);
            }
        }
        _ => {}
    }
}

fn prune_null_config_values(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, child| {
                prune_null_config_values(child);
                !child.is_null()
            });
        }
        Value::Array(values) => {
            for child in values.iter_mut() {
                prune_null_config_values(child);
            }
            values.retain(|child| !child.is_null());
        }
        _ => {}
    }
}

fn redact_known_secret_fields(map: &mut Map<String, Value>) {
    for key in [
        "secret",
        "cluster_secret",
        "auth_token",
        "bearer_token",
        "basic_password",
        "smtp_password",
        "root_password",
        "client_secret",
        "credential_encryption_key",
        "opaque_server_setup_secret",
        "api_key",
        "token",
        "access_token",
        "refresh_token",
        "private_key",
    ] {
        if let Some(value) = map.get_mut(key) {
            redact_scalar_secret(value);
        }
    }

    for key in ["url", "database_url", "redis_url"] {
        if let Some(Value::String(url)) = map.get_mut(key) {
            *url = mask_connection_url(url);
        }
    }
}

fn redact_scalar_secret(value: &mut Value) {
    match value {
        Value::Null => {}
        Value::String(secret) if secret.is_empty() => {}
        Value::String(secret) => *secret = "<redacted>".to_string(),
        other => *other = Value::String("<redacted>".to_string()),
    }
}

pub(super) fn mask_connection_url(url: &str) -> String {
    synctv_common::redaction::mask_url_credentials(url, "<redacted>", "<redacted>")
}
