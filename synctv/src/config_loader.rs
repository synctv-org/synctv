//! Top-level configuration loading.
//!
//! File formats, dotenv, environment overrides, and unknown-key diagnostics are
//! application startup concerns. Keep this loader in the `synctv` crate so
//! `synctv-core` receives structured configuration values only.

use anyhow::Result;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::app_config::default_data_dir as app_default_data_dir;
use crate::app_config::AppConfig as Config;
use crate::path_util::absolute_display_path;
use config::{ConfigError, FileFormat};
use serde::Deserialize;
use synctv_common::time::set_default_timezone_name;

const CONFIG_KEY_PUBLIC_IDS: &str = "public_ids";
const ENV_PUBLIC_IDS_SQIDS_ALPHABET: &str = "SYNCTV_PUBLIC_IDS_SQIDS_ALPHABET";
const ENV_PUBLIC_IDS_SQIDS_MIN_LENGTH: &str = "SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH";

#[derive(Debug, Clone, Default)]
pub struct ConfigLoadExtensions {
    pub config_key_prefixes: Vec<String>,
    pub env_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnknownConfigDiagnostics {
    pub config_file: Option<String>,
    pub config_keys: Vec<String>,
    pub env_keys: Vec<String>,
}

impl UnknownConfigDiagnostics {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config_keys.is_empty() && self.env_keys.is_empty()
    }

    #[must_use]
    pub fn strict_error_message(&self) -> String {
        let mut parts = Vec::new();
        if !self.config_keys.is_empty() {
            let source = self.config_file.as_deref().map_or_else(
                || "config file".to_string(),
                |path| format!("config file {path}"),
            );
            parts.push(format!(
                "unsupported key(s) in {source}: {}",
                self.config_keys.join(", ")
            ));
        }
        if !self.env_keys.is_empty() {
            parts.push(format!(
                "unsupported SYNCTV_ environment variable(s): {}",
                self.env_keys.join(", ")
            ));
        }
        parts.join("; ")
    }
}

struct LoadedCoreConfig {
    config: Config,
    unknown: UnknownConfigDiagnostics,
}

impl ConfigLoadExtensions {
    pub fn new(
        config_key_prefixes: impl IntoIterator<Item = impl Into<String>>,
        env_keys: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            config_key_prefixes: config_key_prefixes.into_iter().map(Into::into).collect(),
            env_keys: env_keys.into_iter().map(Into::into).collect(),
        }
    }

    fn claims_config_key(&self, key: &str) -> bool {
        self.config_key_prefixes.iter().any(|prefix| {
            key == prefix
                || key
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }
}

pub fn public_id_config_extensions() -> ConfigLoadExtensions {
    ConfigLoadExtensions::new(
        [CONFIG_KEY_PUBLIC_IDS],
        [
            ENV_PUBLIC_IDS_SQIDS_ALPHABET,
            ENV_PUBLIC_IDS_SQIDS_MIN_LENGTH,
        ],
    )
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PublicIdRootConfig {
    public_ids: synctv_adapter::PublicIdConfig,
}

pub fn default_config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    push_config_path_variants(&mut paths, Path::new("synctv"));

    #[cfg(target_os = "macos")]
    if let Some(home) = user_home_dir() {
        push_config_path_variants(&mut paths, &home.join(".synctv").join("synctv"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg_config_home) = absolute_env_path("XDG_CONFIG_HOME") {
            push_config_path_variants(&mut paths, &xdg_config_home.join("synctv").join("synctv"));
        } else if let Some(home) = user_home_dir() {
            push_config_path_variants(
                &mut paths,
                &home.join(".config").join("synctv").join("synctv"),
            );
        }
        push_config_path_variants(&mut paths, Path::new("/etc/synctv/synctv"));
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            push_config_path_variants(&mut paths, &appdata.join("synctv").join("synctv"));
        } else if let Some(home) = user_home_dir() {
            push_config_path_variants(
                &mut paths,
                &home
                    .join("AppData")
                    .join("Roaming")
                    .join("synctv")
                    .join("synctv"),
            );
        }
    }

    push_config_path_variants(&mut paths, Path::new("/config/synctv"));
    paths
}

fn push_config_path_variants(paths: &mut Vec<PathBuf>, base_path_without_extension: &Path) {
    for extension in ["yaml", "yml", "json", "toml"] {
        push_unique_path(paths, base_path_without_extension.with_extension(extension));
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        })
}

pub(crate) fn default_data_dir() -> PathBuf {
    app_default_data_dir()
}

pub(crate) fn default_management_unix_socket_path() -> PathBuf {
    default_data_dir().join("run").join("synctv.sock")
}

pub(crate) fn default_runtime_socket_relative_path() -> PathBuf {
    PathBuf::from("run").join("synctv.sock")
}

pub(crate) fn default_proxy_slice_cache_relative_path() -> PathBuf {
    PathBuf::from("cache").join("proxy-slice")
}

pub(crate) fn resolve_relative_path_from(reference: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(reference.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn config_file_format_for_path(path: &Path) -> Result<FileFormat, ConfigError> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("yaml" | "yml") => Ok(FileFormat::Yaml),
        Some("json") => Ok(FileFormat::Json),
        Some("toml") => Ok(FileFormat::Toml),
        Some(ext) => Err(ConfigError::Message(format!(
            "unsupported config file extension '.{ext}' for {} (expected .yaml, .yml, .json, or .toml)",
            absolute_display_path(path)
        ))),
        None => Err(ConfigError::Message(format!(
            "config file {} is missing an extension (expected .yaml, .yml, .json, or .toml)",
            absolute_display_path(path)
        ))),
    }
}

fn join_config_key_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn resolve_config_file_reference_path(base_dir: &Path, reference: &str) -> PathBuf {
    let reference_path = Path::new(reference);
    if reference_path.is_absolute() {
        reference_path.to_path_buf()
    } else {
        base_dir.join(reference_path)
    }
}

pub(crate) fn load_config_string_from_file(
    config_path: &Path,
    base_dir: &Path,
    key_path: &str,
    reference: &str,
) -> Result<String, ConfigError> {
    let trimmed_reference = reference.trim();
    if trimmed_reference.is_empty() {
        return Err(ConfigError::Message(format!(
            "config key '{key_path}_file' in {} must not be empty",
            absolute_display_path(config_path)
        )));
    }

    let resolved_path = resolve_config_file_reference_path(base_dir, trimmed_reference);
    let contents = std::fs::read_to_string(&resolved_path).map_err(|error| {
        ConfigError::Message(format!(
            "failed to read config file reference '{key_path}_file' from {}: {error}",
            absolute_display_path(&resolved_path)
        ))
    })?;

    Ok(contents.trim().to_string())
}

fn is_secret_like_provider_key(base_key: &str) -> bool {
    matches!(
        base_key,
        "access_token"
            | "api_key"
            | "client_secret"
            | "password"
            | "refresh_token"
            | "secret"
            | "shared_secret"
            | "token"
    )
}

fn supports_secret_file_reference(current_path: &str, base_key: &str) -> bool {
    let key_path = join_config_key_path(current_path, base_key);
    matches!(
        key_path.as_str(),
        "cluster.secret"
            | "security.credential_encryption_key"
            | "security.totp_encryption_key"
            | "security.email_outbox_encryption_key"
            | "security.opaque_server_setup_secret"
            | "security.proxy_signing_key"
            | "security.media_swarm_signing_key"
            | "security.provider_session_encryption_key"
            | "security.login_discovery_key"
            | "security.webauthn_enumeration_key"
            | "management.auth_token"
            | "metrics.auth.basic_password"
            | "metrics.auth.bearer_token"
            | "database.password"
            | "database.url"
            | "database.read_url"
            | "redis.password"
            | "redis.url"
            | "jwt.secret"
            | "livestream.hls_storage.access_key_id"
            | "livestream.hls_storage.secret_access_key"
            | "file_storage.upload_token_secret"
            | "bootstrap.root_password"
    ) || (current_path.starts_with("file_storage.backends.")
        && matches!(base_key, "access_key_id" | "secret_access_key"))
        || (current_path.starts_with("media_providers.") && is_secret_like_provider_key(base_key))
}

fn resolve_secret_file_references_in_json_value(
    value: &mut serde_json::Value,
    config_path: &Path,
    current_path: &str,
    config_base_dir: &Path,
) -> Result<(), ConfigError> {
    match value {
        serde_json::Value::Object(map) => {
            let file_keys = map
                .keys()
                .filter(|key| key.ends_with("_file"))
                .cloned()
                .collect::<Vec<_>>();

            for file_key in file_keys {
                let Some(file_reference) = map.get(&file_key).and_then(serde_json::Value::as_str)
                else {
                    let key_path = join_config_key_path(current_path, &file_key);
                    return Err(ConfigError::Message(format!(
                        "config key '{key_path}' in {} must be a string path",
                        absolute_display_path(config_path)
                    )));
                };

                let Some(base_key) = file_key.strip_suffix("_file") else {
                    continue;
                };
                if base_key.is_empty() {
                    let key_path = join_config_key_path(current_path, &file_key);
                    return Err(ConfigError::Message(format!(
                        "config key '{key_path}' in {} has an invalid _file suffix",
                        absolute_display_path(config_path)
                    )));
                }

                if supports_secret_file_reference(current_path, base_key) {
                    let key_path = join_config_key_path(current_path, base_key);
                    let resolved_value = load_config_string_from_file(
                        config_path,
                        config_base_dir,
                        &key_path,
                        file_reference,
                    )?;
                    map.insert(
                        base_key.to_string(),
                        serde_json::Value::String(resolved_value),
                    );
                    map.remove(&file_key);
                }
            }

            let child_keys = map.keys().cloned().collect::<Vec<_>>();
            for key in child_keys {
                if let Some(child) = map.get_mut(&key) {
                    resolve_secret_file_references_in_json_value(
                        child,
                        config_path,
                        &join_config_key_path(current_path, &key),
                        config_base_dir,
                    )?;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                resolve_secret_file_references_in_json_value(
                    item,
                    config_path,
                    current_path,
                    config_base_dir,
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn normalize_split_database_config_value(value: &mut serde_json::Value) {
    let Some(database) = value
        .get_mut("database")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let has_explicit_url = database.contains_key("url");
    let has_split_config = ["host", "port", "username", "password", "name"]
        .iter()
        .any(|key| database.contains_key(*key));

    if has_split_config && !has_explicit_url {
        database.insert("url".to_string(), serde_json::Value::String(String::new()));
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadConfigOptions {
    pub config_path: Option<String>,
    pub data_dir: Option<String>,
    pub load_dotenv: bool,
    pub validate: bool,
    pub verbose: bool,
    pub extensions: ConfigLoadExtensions,
}

pub fn load_dotenv(verbose: bool) -> Result<()> {
    match dotenvy::dotenv() {
        Ok(path) => {
            if verbose {
                tracing::info!(path = %absolute_display_path(&path), "Loaded environment file");
            }
            Ok(())
        }
        Err(dotenvy::Error::Io(err)) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow::anyhow!("Failed to load .env: {err}")),
    }
}

fn apply_public_id_env_overrides(
    config: &mut synctv_adapter::PublicIdConfig,
    env: &HashMap<String, String>,
) -> Result<(), ConfigError> {
    if env.contains_key(ENV_PUBLIC_IDS_SQIDS_ALPHABET)
        || env.contains_key(ENV_PUBLIC_IDS_SQIDS_MIN_LENGTH)
    {
        let sqids = config.sqids.get_or_insert_with(Default::default);
        if let Some(alphabet) = env.get(ENV_PUBLIC_IDS_SQIDS_ALPHABET) {
            sqids.alphabet = Some(alphabet.clone());
        }
        if let Some(min_length) = env.get(ENV_PUBLIC_IDS_SQIDS_MIN_LENGTH) {
            sqids.min_length = min_length.parse::<u8>().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable {ENV_PUBLIC_IDS_SQIDS_MIN_LENGTH}: \
                     '{min_length}' ({error})"
                ))
            })?;
        }
    }
    Ok(())
}

fn load_public_id_config_file(path: &str) -> Result<synctv_adapter::PublicIdConfig, ConfigError> {
    let path = Path::new(path);
    let contents = std::fs::read_to_string(path).map_err(|error| {
        ConfigError::Message(format!(
            "failed to read public ID config file {}: {error}",
            absolute_display_path(path)
        ))
    })?;
    let root = match config_file_format_for_path(path)? {
        FileFormat::Yaml => serde_yaml::from_str::<PublicIdRootConfig>(&contents)
            .map_err(|error| ConfigError::Message(error.to_string()))?,
        FileFormat::Json => serde_json::from_str::<PublicIdRootConfig>(&contents)
            .map_err(|error| ConfigError::Message(error.to_string()))?,
        FileFormat::Toml => {
            let parsed = toml::from_str::<toml::Value>(&contents)
                .map_err(|error| ConfigError::Message(error.to_string()))?;
            let normalized = serde_json::to_value(parsed)
                .map_err(|error| ConfigError::Message(error.to_string()))?;
            serde_json::from_value::<PublicIdRootConfig>(normalized)
                .map_err(|error| ConfigError::Message(error.to_string()))?
        }
        _ => {
            return Err(ConfigError::Message(
                "unsupported config file format".to_string(),
            ));
        }
    };
    Ok(root.public_ids)
}

pub fn load_public_id_config_with_options(
    options: &LoadConfigOptions,
) -> Result<synctv_adapter::PublicIdConfig> {
    if options.load_dotenv {
        load_dotenv(options.verbose)?;
    }

    let explicit_config_path = options
        .config_path
        .clone()
        .or_else(|| std::env::var("SYNCTV_CONFIG_PATH").ok());

    let discovered_config_path = explicit_config_path
        .clone()
        .filter(|p| std::path::Path::new(p).exists())
        .or_else(|| {
            default_config_search_paths()
                .into_iter()
                .find(|path| path.exists())
                .map(|path| path.display().to_string())
        });

    let env: HashMap<String, String> = std::env::vars().collect();
    let mut config = if let Some(path) = discovered_config_path {
        let display_path = absolute_display_path(std::path::Path::new(&path));
        if options.verbose {
            tracing::info!(path = %display_path, "Loading public ID config");
        }
        load_public_id_config_file(&path).map_err(|error| {
            let source = if options.config_path.is_some() {
                "explicit CLI --config"
            } else if explicit_config_path.is_some() {
                "explicitly set SYNCTV_CONFIG_PATH"
            } else {
                "auto-discovered config file"
            };
            anyhow::anyhow!(
                "Failed to load public ID config from {source} '{display_path}': {error}"
            )
        })?
    } else if let Some(ref explicit_path) = explicit_config_path {
        let source = if options.config_path.is_some() {
            "CLI --config"
        } else {
            "SYNCTV_CONFIG_PATH"
        };
        return Err(anyhow::anyhow!(
            "Config file not found at explicitly set {source} '{}'",
            absolute_display_path(std::path::Path::new(explicit_path))
        ));
    } else {
        synctv_adapter::PublicIdConfig::default()
    };

    apply_public_id_env_overrides(&mut config, &env)?;
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(config)
}

/// Load configuration from config file or environment variables
///
/// Config file search order:
/// 1. `SYNCTV_CONFIG_PATH` environment variable (explicit path)
/// 2. platform default search paths with extension priority
///    `synctv.yaml`, `synctv.yml`, `synctv.json`, `synctv.toml`
///    for each location (for example current directory, user config dir,
///    macOS `~/.synctv/`, Linux `/etc/synctv/`, `/config/`)
/// 4. Fall back to environment variables only
#[cfg(test)]
pub fn load_config() -> Result<Config> {
    load_config_with_options(&LoadConfigOptions {
        config_path: None,
        data_dir: None,
        load_dotenv: true,
        validate: true,
        verbose: false,
        extensions: public_id_config_extensions(),
    })
}

fn load_core_config_with_env_lenient(
    config_file: Option<&str>,
    env: &HashMap<String, String>,
    data_dir_override: Option<&str>,
    extensions: &ConfigLoadExtensions,
) -> Result<LoadedCoreConfig, ConfigError> {
    let seen_env_keys = std::cell::RefCell::new(std::collections::HashSet::<String>::new());
    if config_file.is_some() && env.contains_key("SYNCTV_CONFIG_PATH") {
        seen_env_keys
            .borrow_mut()
            .insert("SYNCTV_CONFIG_PATH".to_string());
    }
    let get_env = |name: &str| {
        seen_env_keys.borrow_mut().insert(name.to_string());
        env.get(name).cloned()
    };
    let (mut config, mut unknown) = match config_file {
        Some(path) => load_core_config_file(path, extensions)?,
        None => (Config::default(), UnknownConfigDiagnostics::default()),
    };

    crate::config_env::apply_env_overrides_with(&mut config, &get_env)?;
    crate::config_env::resolve_owned_local_paths(
        &mut config,
        config_file.map(Path::new),
        env.contains_key("SYNCTV_DATA_DIR"),
        data_dir_override,
    );
    crate::config_env::resolve_time_defaults_with(&mut config, &get_env)?;
    let mut seen_env_keys = seen_env_keys.into_inner();
    seen_env_keys.extend(extensions.env_keys.iter().cloned());
    unknown.env_keys = collect_unknown_synctv_env_vars(env, &seen_env_keys);

    Ok(LoadedCoreConfig { config, unknown })
}

fn load_core_config_file(
    path: &str,
    extensions: &ConfigLoadExtensions,
) -> Result<(Config, UnknownConfigDiagnostics), ConfigError> {
    let path = Path::new(path);
    if !path.exists() {
        return Err(ConfigError::Message(format!(
            "config file not found: {}",
            absolute_display_path(path)
        )));
    }
    let (config, unknown_keys) = deserialize_core_config_file(path)?;
    let unknown_keys = filter_extension_config_keys(unknown_keys, extensions);
    Ok((
        config,
        UnknownConfigDiagnostics {
            config_file: Some(absolute_display_path(path)),
            config_keys: unknown_keys,
            env_keys: Vec::new(),
        },
    ))
}

fn deserialize_core_config_file(path: &Path) -> Result<(Config, Vec<String>), ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        ConfigError::Message(format!(
            "failed to read config file {}: {error}",
            absolute_display_path(path)
        ))
    })?;
    let format = config_file_format_for_path(path)?;
    let mut parsed_value = match format {
        FileFormat::Yaml => serde_yaml::from_str::<serde_json::Value>(&contents)
            .map_err(|error| ConfigError::Message(error.to_string()))?,
        FileFormat::Json => serde_json::from_str::<serde_json::Value>(&contents)
            .map_err(|error| ConfigError::Message(error.to_string()))?,
        FileFormat::Toml => {
            let parsed = toml::from_str::<toml::Value>(&contents)
                .map_err(|error| ConfigError::Message(error.to_string()))?;
            serde_json::to_value(parsed).map_err(|error| ConfigError::Message(error.to_string()))?
        }
        _ => {
            return Err(ConfigError::Message(
                "unsupported config file format".to_string(),
            ));
        }
    };
    let config_base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve_secret_file_references_in_json_value(&mut parsed_value, path, "", config_base_dir)?;
    normalize_split_database_config_value(&mut parsed_value);
    let normalized_contents = serde_json::to_string(&parsed_value)
        .map_err(|error| ConfigError::Message(error.to_string()))?;
    deserialize_core_config_contents(&normalized_contents, FileFormat::Json)
}

fn finalize_unknown_keys(mut unknown_keys: Vec<String>) -> Vec<String> {
    unknown_keys.sort_unstable();
    unknown_keys.dedup();
    unknown_keys
}

fn filter_extension_config_keys(
    unknown_keys: Vec<String>,
    extensions: &ConfigLoadExtensions,
) -> Vec<String> {
    unknown_keys
        .into_iter()
        .filter(|key| !extensions.claims_config_key(key))
        .collect()
}

fn deserialize_core_config_contents(
    contents: &str,
    format: FileFormat,
) -> Result<(Config, Vec<String>), ConfigError> {
    let mut unknown_keys = Vec::new();
    let config = match format {
        FileFormat::Yaml => {
            let deserializer = serde_yaml::Deserializer::from_str(contents);
            serde_ignored::deserialize::<_, _, Config>(deserializer, |path| {
                unknown_keys.push(path.to_string());
            })
            .map_err(|error| ConfigError::Message(error.to_string()))?
        }
        FileFormat::Json => {
            let mut deserializer = serde_json::Deserializer::from_str(contents);
            serde_ignored::deserialize::<_, _, Config>(&mut deserializer, |path| {
                unknown_keys.push(path.to_string());
            })
            .map_err(|error| ConfigError::Message(error.to_string()))?
        }
        FileFormat::Toml => {
            let deserializer = toml::Deserializer::parse(contents)
                .map_err(|error| ConfigError::Message(error.to_string()))?;
            serde_ignored::deserialize::<_, _, Config>(deserializer, |path| {
                unknown_keys.push(path.to_string());
            })
            .map_err(|error| ConfigError::Message(error.to_string()))?
        }
        _ => {
            return Err(ConfigError::Message(
                "unsupported config file format".to_string(),
            ));
        }
    };

    Ok((config, finalize_unknown_keys(unknown_keys)))
}

fn collect_unknown_synctv_env_vars(
    env: &HashMap<String, String>,
    seen_env_keys: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut unknown_keys: Vec<String> = env
        .keys()
        .filter(|key| {
            key.starts_with("SYNCTV_")
                && !seen_env_keys.contains(*key)
                && !is_cli_only_synctv_env_var(key)
        })
        .cloned()
        .collect();
    unknown_keys.sort_unstable();
    unknown_keys
}

fn is_cli_only_synctv_env_var(key: &str) -> bool {
    matches!(key, "SYNCTV_MANAGEMENT_ENDPOINT") || key.starts_with("SYNCTV_TEST_")
}

pub fn load_config_with_options(options: &LoadConfigOptions) -> Result<Config> {
    if options.load_dotenv {
        load_dotenv(options.verbose)?;
    }

    let explicit_config_path = options
        .config_path
        .clone()
        .or_else(|| std::env::var("SYNCTV_CONFIG_PATH").ok());

    let discovered_config_path = explicit_config_path
        .clone()
        .filter(|p| std::path::Path::new(p).exists())
        .or_else(|| {
            default_config_search_paths()
                .into_iter()
                .find(|path| path.exists())
                .map(|path| path.display().to_string())
        });

    let env: HashMap<String, String> = std::env::vars().collect();
    let config = if let Some(path) = discovered_config_path {
        let display_path = absolute_display_path(std::path::Path::new(&path));
        if options.verbose {
            tracing::info!(path = %display_path, "Loading config file");
        }
        match load_core_config_with_env_lenient(
            Some(&path),
            &env,
            options.data_dir.as_deref(),
            &options.extensions,
        ) {
            Ok(loaded) => {
                if !loaded.unknown.is_empty() {
                    let message = loaded.unknown.strict_error_message();
                    tracing::warn!("Ignoring unknown configuration setting(s): {message}");
                }
                if options.verbose {
                    tracing::info!(path = %display_path, "Config file loaded");
                }
                loaded.config
            }
            Err(e) => {
                let source = if options.config_path.is_some() {
                    "explicit CLI --config"
                } else if explicit_config_path.is_some() {
                    "explicitly set SYNCTV_CONFIG_PATH"
                } else {
                    "auto-discovered config file"
                };
                return Err(anyhow::anyhow!(
                    "Failed to load config from {source} '{display_path}': {e}"
                ));
            }
        }
    } else if let Some(ref explicit_path) = explicit_config_path {
        // CLI --config or SYNCTV_CONFIG_PATH was explicitly set but the file doesn't exist.
        let source = if options.config_path.is_some() {
            "CLI --config"
        } else {
            "SYNCTV_CONFIG_PATH"
        };
        return Err(anyhow::anyhow!(
            "Config file not found at explicitly set {source} '{}'",
            absolute_display_path(std::path::Path::new(explicit_path))
        ));
    } else {
        if options.verbose {
            tracing::info!("No config file found, using environment variables");
        }
        {
            let loaded = load_core_config_with_env_lenient(
                None,
                &env,
                options.data_dir.as_deref(),
                &options.extensions,
            )?;
            if !loaded.unknown.is_empty() {
                let message = loaded.unknown.strict_error_message();
                tracing::warn!("Ignoring unknown configuration setting(s): {message}");
            }
            loaded.config
        }
    };

    set_default_timezone_name(&config.time.timezone).map_err(|error| {
        anyhow::anyhow!(
            "Failed to initialize default timezone '{}': {error}",
            config.time.timezone
        )
    })?;

    if options.validate {
        // Validate configuration (fail fast on misconfigurations)
        if let Err(errors) = config.validate() {
            for error in &errors {
                tracing::error!(%error, "Config validation error");
            }
            return Err(anyhow::anyhow!(
                "Configuration validation failed with {} error(s): {}",
                errors.len(),
                errors.join("; ")
            ));
        }

        if options.verbose {
            tracing::info!(api_address = %config.api_address(), "Configuration loaded and validated");
        }
    } else if options.verbose {
        tracing::info!("Configuration loaded");
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::{default_config_search_paths, user_home_dir};
    use super::{
        load_config, load_config_with_options, load_public_id_config_with_options,
        public_id_config_extensions, ConfigLoadExtensions, LoadConfigOptions,
    };
    use std::fmt::Debug;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use synctv_common::time::{default_timezone_name, set_default_timezone_name};
    use tempfile::tempdir;

    static CONFIG_TEST_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    trait TestResultExt<T, E> {
        fn checked(self, context: &str) -> T;
        fn failed(self, context: &str) -> E;
    }

    impl<T, E> TestResultExt<T, E> for std::result::Result<T, E>
    where
        E: Debug,
    {
        fn checked(self, context: &str) -> T {
            match self {
                Ok(value) => value,
                Err(error) => panic!("{context}: {error:?}"),
            }
        }

        fn failed(self, context: &str) -> E {
            match self {
                Ok(_) => panic!("{context}: succeeded unexpectedly"),
                Err(error) => error,
            }
        }
    }

    fn acquire_process_config_test_lock() -> MutexGuard<'static, ()> {
        CONFIG_TEST_SERIAL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_default_config_search_paths_use_home_hidden_config_dir_on_macos() {
        let home = user_home_dir().expect("macOS test environment should expose HOME");
        let expected = [
            home.join(".synctv").join("synctv.yaml"),
            home.join(".synctv").join("synctv.yml"),
            home.join(".synctv").join("synctv.json"),
            home.join(".synctv").join("synctv.toml"),
        ];
        let paths = default_config_search_paths();
        assert!(
            expected.iter().all(|path| paths.contains(path)),
            "macOS default config search paths should include ~/.synctv variants, got: {paths:?}"
        );
    }

    struct EnvVarGuard {
        key: &'static str,
        value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl Into<String>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value.into());
            Self {
                key,
                value: previous,
            }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                value: previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.value.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().checked("current dir should be readable");
            std::env::set_current_dir(path).checked("current dir should be settable");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            if std::env::set_current_dir(&self.previous).is_err() {
                std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
                    .checked("crate root should be available as current dir fallback");
            }
        }
    }

    struct TimeZoneGuard {
        previous: String,
    }

    impl TimeZoneGuard {
        fn capture() -> Self {
            Self {
                previous: default_timezone_name(),
            }
        }
    }

    impl Drop for TimeZoneGuard {
        fn drop(&mut self) {
            set_default_timezone_name(&self.previous).checked("timezone should be restored");
        }
    }

    fn management_auth_token_guard() -> EnvVarGuard {
        EnvVarGuard::set("SYNCTV_MANAGEMENT_AUTH_TOKEN", "test-management-auth-token")
    }

    fn clear_secret_env_overrides() -> Vec<EnvVarGuard> {
        vec![
            EnvVarGuard::remove("SYNCTV_JWT_SECRET"),
            EnvVarGuard::remove("SYNCTV_JWT_SECRET_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET"),
            EnvVarGuard::remove("SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_PROXY_SIGNING_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_PROXY_SIGNING_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY"),
            EnvVarGuard::remove("SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY_FILE"),
            EnvVarGuard::remove("SYNCTV_FILE_UPLOAD_TOKEN_SECRET"),
            EnvVarGuard::remove("SYNCTV_FILE_UPLOAD_TOKEN_SECRET_FILE"),
        ]
    }

    fn required_secret_env_guards() -> Vec<EnvVarGuard> {
        vec![
            EnvVarGuard::set(
                "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY",
                "8181818181818181818181818181818181818181818181818181818181818181",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY",
                "8282828282828282828282828282828282828282828282828282828282828282",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY",
                "8383838383838383838383838383838383838383838383838383838383838383",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_PROXY_SIGNING_KEY",
                "test-proxy-signing-key-for-config-loader",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY",
                "test-media-swarm-signing-key-for-config-loader",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY",
                "test-provider-session-key-for-config-loader",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY",
                "test-login-discovery-key-for-config-loader",
            ),
            EnvVarGuard::set(
                "SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY",
                "test-webauthn-enumeration-key-for-config-loader",
            ),
            EnvVarGuard::set(
                "SYNCTV_FILE_UPLOAD_TOKEN_SECRET",
                "test-file-upload-token-secret-for-config-loader",
            ),
        ]
    }

    fn clear_public_id_env_overrides() -> Vec<EnvVarGuard> {
        vec![
            EnvVarGuard::remove("SYNCTV_PUBLIC_IDS_SQIDS_ALPHABET"),
            EnvVarGuard::remove("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH"),
        ]
    }

    #[test]
    fn test_load_config_fails_for_invalid_auto_discovered_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let config_path = dir.path().join("synctv.yaml");
        std::fs::write(&config_path, "not: [valid").checked("invalid config should be written");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");

        let err = load_config().failed("invalid auto-discovered config must fail closed");

        assert!(
            err.to_string().contains("auto-discovered config file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_config_prefers_first_supported_default_extension_in_order() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let _management_auth = management_auth_token_guard();
        let _required_secrets = required_secret_env_guards();

        std::fs::write(
            dir.path().join("synctv.yml"),
            "server:\n  port: 58082\njwt:\n  secret: \"12345678901234567890123456789012\"\nsecurity:\n  email_outbox_encryption_key: \"5757575757575757575757575757575757575757575757575757575757575757\"\n  opaque_server_setup_secret: \"opaque-server-setup-secret-123456789012\"\n",
        )
        .checked("yml config should be written");
        std::fs::write(
            dir.path().join("synctv.json"),
            "{\"server\":{\"port\":58083},\"jwt\":{\"secret\":\"12345678901234567890123456789012\"},\"security\":{\"email_outbox_encryption_key\":\"5757575757575757575757575757575757575757575757575757575757575757\",\"opaque_server_setup_secret\":\"opaque-server-setup-secret-123456789012\"}}",
        )
        .checked("json config should be written");

        let config = load_config().checked("first discovered config should load");

        assert_eq!(
            config.server.port, 58082,
            "default search must prefer .yml before .json"
        );
    }

    #[test]
    fn test_load_config_fails_for_invalid_explicit_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let config_path = dir.path().join("explicit-synctv.yaml");
        std::fs::write(&config_path, "not: [valid").checked("invalid config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let err = load_config().failed("invalid explicit config must fail closed");

        assert!(
            err.to_string()
                .contains("explicitly set SYNCTV_CONFIG_PATH"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_config_accepts_valid_explicit_file_when_synctv_config_path_is_set() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _management_auth = management_auth_token_guard();
        let _secret_env = clear_secret_env_overrides();
        let _required_secrets = required_secret_env_guards();
        let config_path = dir.path().join("explicit-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: "12345678901234567890123456789012"
security:
  email_outbox_encryption_key: "5757575757575757575757575757575757575757575757575757575757575757"
  opaque_server_setup_secret: "opaque-server-setup-secret-123456789012"
"#,
        )
        .checked("valid config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let config = load_config().checked("valid explicit config path should load successfully");

        assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
    }

    #[test]
    fn test_load_config_reads_dotenv_before_resolving_config() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _config_path = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let _management_auth = management_auth_token_guard();
        let _required_secrets = required_secret_env_guards();
        let _jwt = EnvVarGuard::remove("SYNCTV_JWT_SECRET");
        let _email_outbox_key = EnvVarGuard::remove("SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY");
        let _opaque_secret = EnvVarGuard::remove("SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET");
        let _port = EnvVarGuard::remove("SYNCTV_SERVER_PORT");
        std::fs::write(
            dir.path().join(".env"),
            "SYNCTV_JWT_SECRET=12345678901234567890123456789012\nSYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY=5757575757575757575757575757575757575757575757575757575757575757\nSYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET=opaque-server-setup-secret-123456789012\nSYNCTV_SERVER_PORT=50061\n",
        )
        .checked(".env should be written");

        let config = load_config().checked(".env-backed config should load successfully");

        assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
        assert_eq!(config.server.port, 50061);
    }

    #[test]
    fn test_load_config_with_options_accepts_explicit_cli_config_path() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _management_auth = management_auth_token_guard();
        let _required_secrets = required_secret_env_guards();
        let config_path = dir.path().join("cli-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: "12345678901234567890123456789012"
security:
  email_outbox_encryption_key: "5757575757575757575757575757575757575757575757575757575757575757"
  opaque_server_setup_secret: "opaque-server-setup-secret-123456789012"
server:
  port: 58080
"#,
        )
        .checked("valid config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: true,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("explicit CLI config path should load successfully");

        assert_eq!(config.server.port, 58080);
    }

    #[test]
    fn test_load_config_with_options_can_skip_validation() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _secret_env = clear_secret_env_overrides();
        let config_path = dir.path().join("invalid-but-loadable.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: ""
"#,
        )
        .checked("config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("loading without validation should succeed");

        assert!(config.jwt.secret.is_empty());
    }

    #[test]
    fn test_load_public_id_config_reads_env_overrides() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _config_path = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let _alphabet = EnvVarGuard::set(
            "SYNCTV_PUBLIC_IDS_SQIDS_ALPHABET",
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        );
        let _min_length = EnvVarGuard::set("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH", "8");

        let config = load_public_id_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("public ID env config should load");
        let sqids = config.sqids.expect("sqids should be enabled");

        assert_eq!(sqids.min_length, 8);
        assert!(sqids.alphabet.is_some());
    }

    #[test]
    fn test_load_public_id_config_reads_public_ids_file_section() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _public_id_env = clear_public_id_env_overrides();
        let config_path = dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            r"
public_ids:
  sqids:
    min_length: 9
",
        )
        .checked("public ID config should be written");

        let config = load_public_id_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("public ID config file should load");
        let sqids = config.sqids.expect("sqids should be enabled");

        assert_eq!(sqids.min_length, 9);
    }

    #[test]
    fn test_load_config_public_id_extension_accepts_public_ids_keys() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _public_id_env = clear_public_id_env_overrides();
        let config_path = dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            r"
public_ids:
  sqids:
    min_length: 9
",
        )
        .checked("config with public_ids should be written");

        load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: public_id_config_extensions(),
        })
        .checked("top-level config loader should accept public ID extension keys");
    }

    #[test]
    fn test_load_config_with_options_rejects_file_and_env_unknowns() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let _secret_env = clear_secret_env_overrides();
        let _unknown_env = EnvVarGuard::set("SYNCTV_UNKNOWN_SETTING", "1");
        let config_path = dir.path().join("unknown.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: "12345678901234567890123456789012"
metrics:
  enabled: true
  obsolete_token: "ignored"
"#,
        )
        .checked("config should be written");

        load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("default config loading should warn and continue for unsupported inputs");
    }

    #[test]
    fn test_load_config_with_options_honors_synctv_config_path_when_cli_path_absent() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let config_path = dir.path().join("env-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
management:
  transport: "tcp"
  port: 58081
"#,
        )
        .checked("config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("SYNCTV_CONFIG_PATH should be honored when CLI path is absent");

        assert_eq!(config.management.port, 58081);
    }

    #[test]
    fn test_load_config_with_options_initializes_default_timezone() {
        let _lock = acquire_process_config_test_lock();
        let _timezone = TimeZoneGuard::capture();
        let dir = tempdir().checked("temp dir should be created");
        let config_path = dir.path().join("timezone-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
time:
  timezone: "Asia/Shanghai"
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .checked("config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("timezone config should load");

        assert_eq!(config.time.timezone, "Asia/Shanghai");
        assert_eq!(default_timezone_name(), "Asia/Shanghai");
    }

    #[cfg(unix)]
    #[test]
    fn test_load_config_with_options_resolves_default_management_socket_from_cli_data_dir() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let data_dir = dir.path().join("state");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: Some(data_dir.to_string_lossy().to_string()),
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("cli data_dir should be applied to default runtime paths");

        assert_eq!(std::path::Path::new(&config.data_dir), data_dir.as_path());
        assert_eq!(
            std::path::Path::new(&config.management.unix_socket_path),
            data_dir.join("run").join("synctv.sock").as_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_load_config_with_options_cli_data_dir_overrides_env_data_dir() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().checked("temp dir should be created");
        let cli_data_dir = dir.path().join("cli-state");
        let _env = EnvVarGuard::set(
            "SYNCTV_DATA_DIR",
            dir.path().join("env-state").display().to_string(),
        );

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: Some(cli_data_dir.to_string_lossy().to_string()),
            load_dotenv: false,
            validate: false,
            verbose: false,
            extensions: ConfigLoadExtensions::default(),
        })
        .checked("cli data_dir should override SYNCTV_DATA_DIR");

        assert_eq!(
            std::path::Path::new(&config.data_dir),
            cli_data_dir.as_path()
        );
        assert_eq!(
            std::path::Path::new(&config.management.unix_socket_path),
            cli_data_dir.join("run").join("synctv.sock").as_path()
        );
    }
}
