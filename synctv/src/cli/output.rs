use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

use super::{prune_null_config_values, ToHuman};

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
