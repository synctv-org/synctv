use super::*;
use crate::app_config::default_management_unix_socket_path;
use crate::cli::output::redact_config_value;
use clap::{CommandFactory, Parser};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use synctv_core::models::{RoomAdminPermissionBits, RoomMemberPermissionBits};
use synctv_management::proto as management_proto;
use synctv_proto::admin as admin_proto;

fn acquire_time_test_lock() -> MutexGuard<'static, ()> {
    static TIME_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TIME_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn acquire_current_dir_test_lock() -> MutexGuard<'static, ()> {
    static CURRENT_DIR_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    CURRENT_DIR_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn acquire_env_test_lock() -> MutexGuard<'static, ()> {
    static ENV_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn direct_url_media_source_config(
    url: &str,
) -> Option<synctv_proto::source_config::MediaSourceConfig> {
    Some(synctv_proto::source_config::MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::DirectUrl(
                synctv_proto::source_config::DirectUrlMediaSourceConfig {
                    is_live: None,
                    duration_seconds: None,
                    prefer_proxy: None,
                    proxy_only: None,
                    medias: vec![synctv_proto::source_config::DirectUrlMediaResourceConfig {
                        name: String::new(),
                        url: url.to_string(),
                        headers: Default::default(),
                        format: String::new(),
                    }],
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            ),
        ),
    })
}

fn alist_playlist_source_config(
    path: &str,
) -> Option<synctv_proto::source_config::PlaylistSourceConfig> {
    Some(synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(
            synctv_proto::source_config::playlist_source_config::Provider::Alist(
                synctv_proto::source_config::AlistPlaylistSourceConfig {
                    server_id: "alist-main".to_string(),
                    path: path.to_string(),
                    password: None,
                },
            ),
        ),
    })
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl Into<String>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value.into());
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct TimeZoneGuard {
    previous: String,
}

impl TimeZoneGuard {
    fn set(name: &str) -> Self {
        let previous = synctv_common::time::default_timezone_name();
        synctv_common::time::set_default_timezone_name(name).expect("timezone should be valid");
        Self { previous }
    }
}

impl Drop for TimeZoneGuard {
    fn drop(&mut self) {
        let _ = synctv_common::time::set_default_timezone_name(&self.previous);
    }
}

struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn change_to(path: &Path) -> Self {
        let previous = std::env::current_dir().expect("current dir should be readable");
        std::env::set_current_dir(path).expect("current dir should be settable");
        Self { previous }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        if std::env::set_current_dir(&self.previous).is_err() {
            std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
                .expect("crate root should be available as current dir fallback");
        }
    }
}

fn sample_config() -> crate::app_config::AppConfig {
    let mut config = crate::app_config::AppConfig::default();
    config.database.url = "postgresql://synctv:super-secret-db@db.internal:5432/synctv".into();
    config.redis.url = "redis://:redis-secret@redis.internal:6379/0".into();
    config.jwt.secret = "jwt-secret-123456789012345678901234".into();
    config.security.opaque_server_setup_secret =
        "opaque-server-setup-secret-123456789012345678901234".into();
    config.security.email_outbox_encryption_key =
        "5555555555555555555555555555555555555555555555555555555555555555".into();
    config.security.totp_encryption_key =
        "6666666666666666666666666666666666666666666666666666666666666666".into();
    config.security.proxy_signing_key = "proxy-signing-secret-value-1234567890".into();
    config.security.media_swarm_signing_key = "media-swarm-signing-secret-value-1234567890".into();
    config.security.provider_session_encryption_key =
        "provider-session-secret-value-1234567890".into();
    config.security.login_discovery_key = "login-discovery-secret-value-1234567890".into();
    config.security.webauthn_enumeration_key =
        "webauthn-enumeration-secret-value-1234567890".into();
    config.file_storage.upload_token_secret = "file-upload-token-secret-value-1234567890".into();
    config.cluster.secret = "cluster-secret-value".into();
    config.management.auth_token = "management-auth-token".into();
    config.metrics.auth.bearer_token = "metrics-bearer-token".into();
    config.metrics.auth.basic_password = "metrics-basic-password".into();
    config.bootstrap.root_password = "RootPass12345".into();
    config
}

fn string_values(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| Value::String((*value).to_string()))
            .collect(),
    )
}

#[test]
fn cli_requires_subcommand() {
    assert!(Cli::try_parse_from(["synctv"]).is_err());
}

#[test]
fn cli_parses_global_data_dir() {
    let cli = Cli::parse_from(["synctv", "--data-dir", "/tmp/synctv-state", "serve"]);
    match cli.command {
        Commands::Serve(args) => assert_eq!(
            args.global.data_dir,
            Some(PathBuf::from("/tmp/synctv-state"))
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn serve_config_context_warns_on_unknown_inputs() {
    let _env_lock = acquire_env_test_lock();
    let _unknown_env = EnvVarGuard::set("SYNCTV_UNKNOWN_BOOT_FLAG", "1");
    let context = CliConfigContext::new(GlobalConfigArgs {
        no_dotenv: true,
        ..GlobalConfigArgs::default()
    });

    context
        .validated_config()
        .expect("serve config loading should ignore unsupported SYNCTV_ inputs");
}

#[test]
fn switch_process_working_dir_to_data_dir_creates_and_enters_directory() {
    let _lock = acquire_current_dir_test_lock();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let _cwd = CurrentDirGuard::change_to(temp_dir.path());
    let target = temp_dir.path().join("state");

    let config = crate::app_config::AppConfig {
        data_dir: target.display().to_string(),
        ..crate::app_config::AppConfig::default()
    };

    switch_process_working_dir_to_data_dir(&config)
        .expect("data_dir working directory switch should succeed");

    assert!(
        target.is_dir(),
        "data_dir should be created before switching"
    );
    assert_eq!(
        std::env::current_dir()
            .expect("current dir should resolve")
            .canonicalize()
            .expect("current dir should canonicalize"),
        target
            .canonicalize()
            .expect("target dir should canonicalize")
    );
}

#[test]
fn cli_parses_stop_force_subcommand() {
    let socket_endpoint = format!("unix://{}", default_management_unix_socket_path().display());
    let cli = Cli::parse_from([
        "synctv",
        "stop",
        "--config",
        "/tmp/synctv.yaml",
        "--force",
        "--endpoint",
        &socket_endpoint,
    ]);
    match cli.command {
        Commands::Stop(args) => {
            assert!(args.force);
            assert_eq!(
                args.remote.global.config.as_deref(),
                Some(std::path::Path::new("/tmp/synctv.yaml"))
            );
            assert_eq!(
                args.remote.global.endpoint.as_deref(),
                Some(socket_endpoint.as_str())
            );
            assert_eq!(args.remote.output, RemoteOutputFormat::Human);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_remote_output_json() {
    let cli = Cli::parse_from(["synctv", "user", "list", "--output", "json"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => assert_eq!(args.remote.output, RemoteOutputFormat::Json),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_remote_management_auth_token() {
    let cli = Cli::parse_from(["synctv", "user", "list", "--auth-token", "token-123"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => assert_eq!(args.remote.global.auth_token.as_deref(), Some("token-123")),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_global_management_auth_token_before_subcommand() {
    let cli = Cli::parse_from(["synctv", "--auth-token", "token-123", "user", "list"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => assert_eq!(args.remote.global.auth_token.as_deref(), Some("token-123")),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_remote_management_auth_token_file() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "list",
        "--auth-token-file",
        "/run/secrets/management_auth_token",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => assert_eq!(
            args.remote.global.auth_token_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/management_auth_token"))
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_global_management_auth_token_file_before_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "--auth-token-file",
        "/run/secrets/management_auth_token",
        "user",
        "list",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => assert_eq!(
            args.remote.global.auth_token_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/management_auth_token"))
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_config_validate_strict() {
    let cli = Cli::parse_from(["synctv", "config", "validate", "--strict"]);
    match cli.command {
        Commands::Config(ConfigCommand {
            command: ConfigSubcommand::Validate(args),
            ..
        }) => assert!(args.strict),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_local_config_commands_accept_config_loading_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "config",
        "--config",
        "/tmp/synctv.yaml",
        "--no-dotenv",
        "validate",
    ]);
    match cli.command {
        Commands::Config(ConfigCommand { global, .. }) => {
            assert_eq!(
                global.config.as_deref(),
                Some(std::path::Path::new("/tmp/synctv.yaml"))
            );
            assert!(global.no_dotenv);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_root_verbose_flag() {
    let cli = Cli::parse_from(["synctv", "-v", "config", "show"]);
    assert_eq!(cli.global.verbose, 1);
    match cli.command {
        Commands::Config(ConfigCommand {
            command: ConfigSubcommand::Show(args),
            ..
        }) => assert_eq!(args.output, ConfigOutputFormat::Yaml),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn root_global_flags_propagate_to_local_subcommands() {
    let cli = Cli::parse_from([
        "synctv",
        "--config",
        "/tmp/root.yaml",
        "--no-dotenv",
        "-vv",
        "config",
        "show",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Config(ConfigCommand { global, .. }) => {
            assert_eq!(
                global.config.as_deref(),
                Some(std::path::Path::new("/tmp/root.yaml"))
            );
            assert!(global.no_dotenv);
            assert_eq!(global.verbose, 2);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn root_global_flags_propagate_to_remote_subcommands() {
    let cli = Cli::parse_from([
        "synctv",
        "--config",
        "/tmp/root.yaml",
        "--endpoint",
        "http://127.0.0.1:50052",
        "--no-dotenv",
        "-vvv",
        "user",
        "list",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => {
            assert_eq!(
                args.remote.global.config.as_deref(),
                Some(std::path::Path::new("/tmp/root.yaml"))
            );
            assert_eq!(
                args.remote.global.endpoint.as_deref(),
                Some("http://127.0.0.1:50052")
            );
            assert!(args.remote.global.no_dotenv);
            assert_eq!(args.remote.global.verbose, 3);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_root_global_endpoint_after_remote_subcommand_name() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "--endpoint",
        "http://127.0.0.1:50052",
        "list",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => {
            assert_eq!(
                args.remote.global.endpoint.as_deref(),
                Some("http://127.0.0.1:50052")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_remote_user_help_includes_config_loading_flags() {
    let mut command = Cli::command();
    let user = command
        .find_subcommand_mut("user")
        .expect("user subcommand should exist");
    let user_list = user
        .find_subcommand_mut("list")
        .expect("user list subcommand should exist");
    let mut help = Vec::new();
    user_list
        .write_long_help(&mut help)
        .expect("user help should render");
    let help = String::from_utf8(help).expect("user help should be utf-8");

    assert!(
        help.contains("--config"),
        "remote user help should expose config file flags: {help}"
    );
    assert!(
        help.contains("--no-dotenv"),
        "remote user help should expose dotenv flags: {help}"
    );
    assert!(
        help.contains("--endpoint"),
        "remote user help should expose management endpoint override: {help}"
    );
    assert!(
        help.contains("--output"),
        "remote user help should expose output selection: {help}"
    );
}

#[test]
fn cli_config_help_includes_config_loading_flags() {
    let mut command = Cli::command();
    let config = command
        .find_subcommand_mut("config")
        .expect("config subcommand should exist");
    let mut help = Vec::new();
    config
        .write_long_help(&mut help)
        .expect("config help should render");
    let help = String::from_utf8(help).expect("config help should be utf-8");

    assert!(
        help.contains("--config"),
        "config help should expose config file flag: {help}"
    );
    assert!(
        help.contains("--no-dotenv"),
        "config help should expose dotenv flag: {help}"
    );
}

#[test]
fn cli_room_playback_get_help_mentions_pull_urls() {
    let mut command = Cli::command();
    let room = command
        .find_subcommand_mut("room")
        .expect("room subcommand should exist");
    let playback = room
        .find_subcommand_mut("playback")
        .expect("room playback subcommand should exist");
    let get = playback
        .find_subcommand_mut("get")
        .expect("room playback get subcommand should exist");
    let mut help = Vec::new();
    get.write_long_help(&mut help)
        .expect("room playback get help should render");
    let help = String::from_utf8(help).expect("room playback get help should be utf-8");
    assert!(
        help.contains("signed pull URLs"),
        "room playback get help should mention pull URLs: {help}"
    );
}

#[test]
fn cli_parses_room_playback_patch_commands() {
    let pause = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "pause",
        "--room-id",
        "room-1",
        "--version",
        "7",
    ]);
    match pause.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Pause(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.version, Some(7));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let seek = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "seek",
        "--room-id",
        "room-1",
        "--position",
        "42.5",
    ]);
    match seek.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Seek(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert!((args.position - 42.5).abs() < f64::EPSILON);
            assert_eq!(args.version, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let speed = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "speed",
        "--room-id",
        "room-1",
        "--speed",
        "1.25",
    ]);
    match speed.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Speed(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert!((args.speed - 1.25).abs() < f64::EPSILON);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_user_refs_are_encoded_without_client_side_resolution() {
    let username_ref = UserRefArgs {
        username: Some("alice".to_string()),
        user_id: None,
    }
    .to_management_proto()
    .expect("username user ref should encode");
    assert!(matches!(
        username_ref.value,
        Some(management_proto::user_ref::Value::Username(ref username)) if username == "alice"
    ));

    let id_ref = UserRefArgs {
        username: None,
        user_id: Some("m6K3dSXiWUjU".to_string()),
    }
    .to_management_proto()
    .expect("id user ref should encode");
    assert!(matches!(
        id_ref.value,
        Some(management_proto::user_ref::Value::UserId(ref user_id)) if user_id == "m6K3dSXiWUjU"
    ));

    let email_ref = ActorUserArgs {
        username: None,
        user_id: None,
        email: Some("alice@example.com".to_string()),
    }
    .to_management_proto()
    .expect("email actor ref should encode");
    assert!(matches!(
        email_ref.value,
        Some(management_proto::user_ref::Value::Email(ref email)) if email == "alice@example.com"
    ));

    let batch_refs =
        batch_user_refs_to_proto(vec!["alice".to_string()], vec!["m6K3dSXiWUjU".to_string()]);
    assert!(matches!(
        batch_refs[0].value,
        Some(management_proto::user_ref::Value::Username(ref username)) if username == "alice"
    ));
    assert!(matches!(
        batch_refs[1].value,
        Some(management_proto::user_ref::Value::UserId(ref user_id)) if user_id == "m6K3dSXiWUjU"
    ));
}

#[test]
fn cli_room_stream_help_exposes_room_scoped_stream_management_only() {
    let mut command = Cli::command();
    let room = command
        .find_subcommand_mut("room")
        .expect("room subcommand should exist");
    let stream = room
        .find_subcommand_mut("stream")
        .expect("room stream subcommand should exist");

    let mut stream_help = Vec::new();
    stream
        .write_long_help(&mut stream_help)
        .expect("room stream help should render");
    let stream_help = String::from_utf8(stream_help).expect("room stream help should be utf-8");
    assert!(
        stream_help.contains("list"),
        "room stream help should show the room-scoped list command: {stream_help}"
    );
    assert!(
        stream_help.contains("kick"),
        "room stream help should show the room-scoped kick command: {stream_help}"
    );
    assert!(
        !stream_help.contains("publish-key") && !stream_help.contains("get"),
        "room stream should not duplicate provider rtmp key/info commands: {stream_help}"
    );
    assert!(
        stream.find_subcommand_mut("publish-key").is_none()
            && stream.find_subcommand_mut("get").is_none(),
        "room stream should only expose room-owned stream operations"
    );
}

#[test]
fn cli_parses_remote_user_list_without_explicit_management_identity() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "list",
        "--config",
        "/tmp/remote-synctv.yaml",
        "--no-dotenv",
        "--endpoint",
        "http://127.0.0.1:8080",
        "--page",
        "2",
        "--page-size",
        "20",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
            ..
        }) => {
            assert_eq!(
                args.remote.global.config.as_deref(),
                Some(std::path::Path::new("/tmp/remote-synctv.yaml"))
            );
            assert!(args.remote.global.no_dotenv);
            assert_eq!(
                args.remote.global.endpoint.as_deref(),
                Some("http://127.0.0.1:8080")
            );
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 20);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_list_sorting_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "list",
        "--sort-by",
        "username",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
            ..
        }) => {
            assert!(matches!(args.sort_by, Some(CliUserSortField::Username)));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_remote_room_members() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "list",
        "room-123",
        "--page",
        "3",
        "--page-size",
        "10",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.resolved_room_id(), "room-123");
            assert_eq!(args.page, 3);
            assert_eq!(args.page_size, 10);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_remote_room_members_room_id_flag() {
    let err = Cli::try_parse_from([
        "synctv",
        "room",
        "member",
        "list",
        "--room-id",
        "room-123",
        "--page",
        "2",
    ])
    .expect_err("room member list should use positional ROOM_ID");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn cli_parses_room_member_list_sorting_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "list",
        "room-123",
        "--search",
        "alice",
        "--role",
        "admin",
        "--sort-by",
        "username",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.search.as_deref(), Some("alice"));
            assert!(matches!(args.role, Some(CliRoomMemberRole::Admin)));
            assert!(matches!(
                args.sort_by,
                Some(CliRoomMemberSortField::Username)
            ));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_list_sorting_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "list",
        "--sort-by",
        "last-activity-at",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::List(args),
            ..
        }) => {
            assert!(matches!(
                args.sort_by,
                Some(CliRoomSortField::LastActivityAt)
            ));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_list_creator_username_filter() {
    let cli = Cli::parse_from(["synctv", "room", "list", "--creator-username", "alice"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.creator.creator.as_deref(), Some("alice"));
            assert_eq!(args.creator.creator_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_list_taxonomy_filters() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "list",
        "--category-id",
        "roomcat_anime",
        "--label-id",
        "roomlbl_hot,roomlbl_new",
        "--label-id",
        "roomlbl_weekly",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.category_id.as_deref(), Some("roomcat_anime"));
            assert_eq!(
                args.label_ids,
                ["roomlbl_hot", "roomlbl_new", "roomlbl_weekly"]
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_list_is_banned_as_bare_true_flag() {
    let cli = Cli::parse_from(["synctv", "room", "list", "--is-banned"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.is_banned, Some(true));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_settings_with_positional_room_id() {
    let cli = Cli::parse_from(["synctv", "room", "settings", "get", "room-123"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Settings(RoomSettingsCommand {
                    command: RoomSettingsSubcommand::Get(args),
                }),
            ..
        }) => {
            assert_eq!(args.room.resolved_room_id(), "room-123");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_room_settings_room_id_flag() {
    let err = Cli::try_parse_from(["synctv", "room", "settings", "get", "--room-id", "room-123"])
        .expect_err("room settings should use positional ROOM_ID");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn cli_parses_room_create_minimal() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "create",
        "CLI Room",
        "--username",
        "alice",
        "--description",
        "created from CLI",
        "--settings-json",
        "{\"chatEnabled\":false}",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.name, "CLI Room");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.description.as_deref(), Some("created from CLI"));
            assert_eq!(
                args.settings_json.as_deref(),
                Some("{\"chatEnabled\":false}")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn room_create_settings_json_is_applied_as_patch_to_defaults() {
    let patch: synctv_proto::client::RoomSettingsPatch =
        serde_json::from_str(r#"{"chatEnabled":false}"#)
            .expect("room settings patch JSON should parse");

    let settings = room_settings_patch_to_full_settings(patch);

    assert!(!settings.chat_enabled);
    assert_eq!(settings.max_members, 100);
    assert!(settings.allow_auto_join);
    assert!(!settings.allow_guest_join);
    assert!(settings.voice_chat_enabled);
    assert!(settings.p2p_media_enabled);
}

#[test]
fn cli_rejects_room_create_without_actor_user() {
    let result = Cli::try_parse_from(["synctv", "room", "create", "CLI Room"]);
    assert!(
        result.is_err(),
        "room create must require --username or --user-id"
    );
}

#[test]
fn cli_parses_room_create() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "create",
        "CLI Room",
        "--username",
        "alice",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.name, "CLI Room");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_create_taxonomy_fields() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "create",
        "CLI Room",
        "--username",
        "alice",
        "--category-id",
        "roomcat_anime",
        "--label-id",
        "roomlbl_hot",
        "--label-id",
        "roomlbl_new,roomlbl_weekly",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.name, "CLI Room");
            assert_eq!(args.category_id.as_deref(), Some("roomcat_anime"));
            assert_eq!(
                args.label_ids,
                ["roomlbl_hot", "roomlbl_new", "roomlbl_weekly"]
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_category_commands() {
    let list = Cli::parse_from(["synctv", "room", "category", "list", "--include-disabled"]);
    match list.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Category(RoomCategoryCommand {
                    command: RoomCategorySubcommand::List(args),
                }),
            ..
        }) => assert!(args.include_disabled),
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let upsert = Cli::parse_from([
        "synctv",
        "room",
        "category",
        "upsert",
        "anime",
        "--name",
        "Anime",
        "--description",
        "Animation rooms",
        "--sort-order",
        "10",
        "--enabled",
        "true",
    ]);
    match upsert.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Category(RoomCategoryCommand {
                    command: RoomCategorySubcommand::Upsert(args),
                }),
            ..
        }) => {
            assert_eq!(args.key, "anime");
            assert_eq!(args.name, "Anime");
            assert_eq!(args.description.as_deref(), Some("Animation rooms"));
            assert_eq!(args.sort_order, 10);
            assert_eq!(args.enabled, Some(true));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let delete = Cli::parse_from(["synctv", "room", "category", "delete", "roomcat_anime"]);
    match delete.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Category(RoomCategoryCommand {
                    command: RoomCategorySubcommand::Delete(args),
                }),
            ..
        }) => assert_eq!(args.category_id, "roomcat_anime"),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_label_commands() {
    let list = Cli::parse_from([
        "synctv",
        "room",
        "label",
        "list",
        "--include-disabled",
        "--category-id",
        "roomcat_anime",
    ]);
    match list.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Label(RoomLabelCommand {
                    command: RoomLabelSubcommand::List(args),
                }),
            ..
        }) => {
            assert!(args.include_disabled);
            assert_eq!(args.category_id.as_deref(), Some("roomcat_anime"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let upsert = Cli::parse_from([
        "synctv",
        "room",
        "label",
        "upsert",
        "featured",
        "--name",
        "Featured",
        "--description",
        "Featured rooms",
        "--color",
        "#ffcc00",
        "--category-id",
        "roomcat_anime",
        "--sort-order",
        "3",
        "--enabled",
        "false",
    ]);
    match upsert.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Label(RoomLabelCommand {
                    command: RoomLabelSubcommand::Upsert(args),
                }),
            ..
        }) => {
            assert_eq!(args.key, "featured");
            assert_eq!(args.name, "Featured");
            assert_eq!(args.description.as_deref(), Some("Featured rooms"));
            assert_eq!(args.color.as_deref(), Some("#ffcc00"));
            assert_eq!(args.category_id.as_deref(), Some("roomcat_anime"));
            assert_eq!(args.sort_order, 3);
            assert_eq!(args.enabled, Some(false));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let delete = Cli::parse_from(["synctv", "room", "label", "delete", "roomlbl_featured"]);
    match delete.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Label(RoomLabelCommand {
                    command: RoomLabelSubcommand::Delete(args),
                }),
            ..
        }) => assert_eq!(args.label_id, "roomlbl_featured"),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_taxonomy_set_commands() {
    let set = Cli::parse_from([
        "synctv",
        "room",
        "taxonomy",
        "set",
        "room_abc",
        "--category-id",
        "roomcat_anime",
        "--label-id",
        "roomlbl_hot,roomlbl_new",
    ]);
    match set.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Taxonomy(RoomTaxonomyCommand {
                    command: RoomTaxonomySubcommand::Set(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room_abc");
            assert_eq!(args.category_id.as_deref(), Some("roomcat_anime"));
            assert!(!args.clear_category);
            assert_eq!(args.label_ids, ["roomlbl_hot", "roomlbl_new"]);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let clear = Cli::parse_from([
        "synctv",
        "room",
        "taxonomy",
        "set",
        "room_abc",
        "--clear-category",
    ]);
    match clear.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Taxonomy(RoomTaxonomyCommand {
                    command: RoomTaxonomySubcommand::Set(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room_abc");
            assert!(args.clear_category);
            assert_eq!(args.category_id, None);
            assert!(args.label_ids.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_room_taxonomy_category_and_clear_category() {
    let result = Cli::try_parse_from([
        "synctv",
        "room",
        "taxonomy",
        "set",
        "room_abc",
        "--category-id",
        "roomcat_anime",
        "--clear-category",
    ]);
    assert!(
        result.is_err(),
        "room taxonomy set must reject conflicting category flags"
    );
}

#[test]
fn root_global_flags_propagate_to_room_taxonomy() {
    let cli = Cli::parse_from([
        "synctv",
        "--endpoint",
        "http://127.0.0.1:50052",
        "room",
        "taxonomy",
        "set",
        "room_abc",
        "--category-id",
        "roomcat_anime",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Taxonomy(RoomTaxonomyCommand {
                    command: RoomTaxonomySubcommand::Set(args),
                }),
            ..
        }) => assert_eq!(
            args.remote.global.endpoint.as_deref(),
            Some("http://127.0.0.1:50052")
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_chat_search() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "chat",
        "search",
        "--room-id",
        "room_abc",
        "--username",
        "alice",
        "--sender-username",
        "bob",
        "--cursor",
        "2026-06-23T10:00:00Z|msg_1",
        "--limit",
        "25",
        "--include-deleted",
        "hello",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Chat(RoomChatCommand {
                    command: RoomChatSubcommand::Search(args),
                }),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room_abc");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.sender.sender_username.as_deref(), Some("bob"));
            assert_eq!(args.sender.sender_user_id, None);
            assert_eq!(args.cursor.as_deref(), Some("2026-06-23T10:00:00Z|msg_1"));
            assert_eq!(args.limit, 25);
            assert!(args.include_deleted);
            assert_eq!(args.query, "hello");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_conflicting_room_chat_sender_filters() {
    let result = Cli::try_parse_from([
        "synctv",
        "room",
        "chat",
        "search",
        "--room-id",
        "room_abc",
        "--username",
        "alice",
        "--sender-username",
        "bob",
        "--sender-user-id",
        "usr_abc",
        "hello",
    ]);

    assert!(
        result.is_err(),
        "room chat search must accept a single sender filter"
    );
}

#[test]
fn root_global_flags_propagate_to_room_chat_search() {
    let cli = Cli::parse_from([
        "synctv",
        "--endpoint",
        "http://127.0.0.1:50052",
        "room",
        "chat",
        "search",
        "--room-id",
        "room_abc",
        "--username",
        "alice",
        "hello",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Chat(RoomChatCommand {
                    command: RoomChatSubcommand::Search(args),
                }),
            ..
        }) => assert_eq!(
            args.room.remote.global.endpoint.as_deref(),
            Some("http://127.0.0.1:50052")
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_transfer_owner() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "transfer-owner",
        "room-123",
        "--username",
        "alice",
        "bob",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::TransferOwner(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-123");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.new_owner.new_owner_username.as_deref(), Some("bob"));
            assert_eq!(args.new_owner.new_owner_user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_favorite_commands() {
    let add = Cli::parse_from([
        "synctv",
        "room",
        "favorite",
        "add",
        "room_abc",
        "--username",
        "alice",
    ]);
    match add.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Favorite(RoomFavoriteCommand {
                    command: RoomFavoriteSubcommand::Add(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room_abc");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let remove = Cli::parse_from([
        "synctv",
        "room",
        "favorite",
        "remove",
        "room_abc",
        "--user-id",
        "usr_abc",
    ]);
    match remove.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Favorite(RoomFavoriteCommand {
                    command: RoomFavoriteSubcommand::Remove(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room_abc");
            assert_eq!(args.actor.username, None);
            assert_eq!(args.actor.user_id.as_deref(), Some("usr_abc"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let list = Cli::parse_from([
        "synctv",
        "room",
        "favorite",
        "list",
        "--username",
        "alice",
        "--page",
        "2",
        "--page-size",
        "10",
        "--search",
        "movie",
    ]);
    match list.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Favorite(RoomFavoriteCommand {
                    command: RoomFavoriteSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 10);
            assert_eq!(args.search.as_deref(), Some("movie"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn root_global_flags_propagate_to_room_favorite() {
    let cli = Cli::parse_from([
        "synctv",
        "--endpoint",
        "http://127.0.0.1:50052",
        "room",
        "favorite",
        "add",
        "room_abc",
        "--username",
        "alice",
    ]);
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Favorite(RoomFavoriteCommand {
                    command: RoomFavoriteSubcommand::Add(args),
                }),
            ..
        }) => assert_eq!(
            args.remote.global.endpoint.as_deref(),
            Some("http://127.0.0.1:50052")
        ),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_room_transfer_owner_help_is_clear() {
    let mut command = Cli::command();
    let room = command
        .find_subcommand_mut("room")
        .expect("room subcommand should exist");
    let transfer_owner = room
        .find_subcommand_mut("transfer-owner")
        .expect("room transfer-owner subcommand should exist");
    let mut help = Vec::new();
    transfer_owner
        .write_long_help(&mut help)
        .expect("room transfer-owner help should render");
    let help = String::from_utf8(help).expect("room transfer-owner help should be utf-8");

    assert!(
        help.contains("--username <USERNAME>"),
        "room transfer-owner help should expose current owner flag: {help}"
    );
    assert!(
        help.contains("<USER|--new-owner-id <USER_ID>>"),
        "room transfer-owner help should label the new owner target as USER: {help}"
    );
    assert!(
        help.contains("<ROOM_ID>"),
        "room transfer-owner help should expose the room id positional: {help}"
    );
}

#[test]
fn cli_parses_room_settings_get() {
    let cli = Cli::parse_from(["synctv", "room", "settings", "get", "room-123"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Settings(RoomSettingsCommand {
                    command: RoomSettingsSubcommand::Get(args),
                }),
            ..
        }) => {
            assert_eq!(args.room.resolved_room_id(), "room-123");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_settings_update_with_set_and_unset() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "settings",
        "update",
        "room-123",
        "--set",
        "chatEnabled=false",
        "--unset",
        "autoPlay.mode",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Settings(RoomSettingsCommand {
                    command: RoomSettingsSubcommand::Update(args),
                }),
            ..
        }) => {
            assert_eq!(args.room.resolved_room_id(), "room-123");
            assert_eq!(args.set, ["chatEnabled=false"]);
            assert_eq!(args.unset, ["autoPlay.mode"]);
            assert_eq!(args.request_json, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_remote_admin_command() {
    let cli = Cli::parse_from(["synctv", "user", "get", "alice"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::Get(args),
            ..
        }) => {
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.user.user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_get_by_explicit_user_id() {
    let cli = Cli::parse_from(["synctv", "user", "get", "--user-id", "user-123"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::Get(args),
            ..
        }) => {
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-123"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_set_password_reason() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "set-password",
        "alice",
        "--password",
        "NewPassword123!",
        "--reason",
        "support reset",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::SetPassword(args),
            ..
        }) => {
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.password, "NewPassword123!");
            assert_eq!(args.reason.as_deref(), Some("support reset"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_set_username_with_username_flag() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "set-username",
        "--user-id",
        "user-123",
        "--username",
        "alice-renamed",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::SetUsername(args),
            ..
        }) => {
            assert_eq!(args.user.user_id.as_deref(), Some("user-123"));
            assert_eq!(args.new_username, "alice-renamed");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_set_role_with_role_flag() {
    let cli = Cli::parse_from(["synctv", "user", "set-role", "alice", "--role", "admin"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::SetRole(args),
            ..
        }) => {
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert!(matches!(args.resolved_role(), CliUserRole::Admin));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_user_set_role_positional_role() {
    assert!(Cli::try_parse_from(["synctv", "user", "set-role", "alice", "admin"]).is_err());
}

#[test]
fn cli_user_identity_mutation_help_uses_canonical_flag_names() {
    let mut command = Cli::command();
    let user = command
        .find_subcommand_mut("user")
        .expect("user subcommand should exist");

    let set_password = user
        .find_subcommand_mut("set-password")
        .expect("user set-password subcommand should exist");
    let mut set_password_help = Vec::new();
    set_password
        .write_long_help(&mut set_password_help)
        .expect("user set-password help should render");
    let set_password_help =
        String::from_utf8(set_password_help).expect("user set-password help should be utf-8");
    assert!(
        set_password_help.contains("--reason <REASON>"),
        "user set-password help should expose reset reason: {set_password_help}"
    );
    assert!(
        set_password_help.contains("--password <PASSWORD>"),
        "user set-password help should accept replacement passwords: {set_password_help}"
    );
    assert!(
        set_password_help.contains("<USER|--user-id <USER_ID>>"),
        "user set-password help should label the target user as USER: {set_password_help}"
    );

    let set_username = user
        .find_subcommand_mut("set-username")
        .expect("user set-username subcommand should exist");
    let mut set_username_help = Vec::new();
    set_username
        .write_long_help(&mut set_username_help)
        .expect("user set-username help should render");
    let set_username_help =
        String::from_utf8(set_username_help).expect("user set-username help should be utf-8");
    assert!(
        set_username_help.contains("--username <USERNAME>"),
        "user set-username help should use --username: {set_username_help}"
    );
    assert!(
        !set_username_help.contains("--new-username"),
        "user set-username help should not expose --new-username: {set_username_help}"
    );
    assert!(
        set_username_help.contains("<USER|--user-id <USER_ID>>"),
        "user set-username help should label the target user as USER: {set_username_help}"
    );
}

#[test]
fn cli_parses_config_flags_for_remote_user_commands() {
    let cli = Cli::parse_from(["synctv", "user", "list", "--config", "/tmp/synctv.yaml"]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::List(args),
        }) => {
            assert_eq!(
                args.remote.global.config.as_deref(),
                Some(std::path::Path::new("/tmp/synctv.yaml"))
            );
            assert!(!args.remote.global.no_dotenv);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_no_dotenv_for_remote_system_commands() {
    let cli = Cli::parse_from([
        "synctv",
        "system",
        "stats",
        "--config",
        "/tmp/system.yaml",
        "--no-dotenv",
    ]);
    match cli.command {
        Commands::System(SystemCommand {
            command: SystemSubcommand::Stats(args),
        }) => {
            assert_eq!(
                args.remote.global.config.as_deref(),
                Some(std::path::Path::new("/tmp/system.yaml"))
            );
            assert!(args.remote.global.no_dotenv);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_create() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "create",
        "alice",
        "--email",
        "alice@example.com",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.username, "alice");
            assert_eq!(args.email.as_deref(), Some("alice@example.com"));
            assert!(matches!(args.status, CliUserStatus::Active));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_user_create_with_positional_email() {
    let result = Cli::try_parse_from([
        "synctv",
        "user",
        "create",
        "alice",
        "alice@example.com",
        "--password",
        "StrongPass123",
    ]);
    assert!(
        result.is_err(),
        "user create must reject positional email input"
    );
}

#[test]
fn cli_user_create_help_uses_email_flag_not_positional_argument() {
    let mut command = Cli::command();
    let user = command
        .find_subcommand_mut("user")
        .expect("user subcommand should exist");
    let create = user
        .find_subcommand_mut("create")
        .expect("user create subcommand should exist");
    let mut help = Vec::new();
    create
        .write_long_help(&mut help)
        .expect("user create help should render");
    let help = String::from_utf8(help).expect("user create help should be utf-8");

    assert!(
        help.contains("--email <EMAIL>"),
        "user create help should expose --email flag: {help}"
    );
    assert!(
        !help.contains("[EMAIL]"),
        "user create help should no longer expose positional email argument: {help}"
    );
}

#[test]
fn cli_parses_user_admin_grant() {
    let cli = Cli::parse_from(["synctv", "user", "admin", "grant", "alice"]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Admin(UserAdminCommand {
                    command: UserAdminSubcommand::Grant(args),
                }),
            ..
        }) => {
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.user.user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_admin_revoke() {
    let cli = Cli::parse_from(["synctv", "user", "admin", "revoke", "--user-id", "user-123"]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Admin(UserAdminCommand {
                    command: UserAdminSubcommand::Revoke(args),
                }),
            ..
        }) => {
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-123"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_admin_list() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "admin",
        "list",
        "--page",
        "2",
        "--page-size",
        "25",
        "--search",
        "alice",
        "--sort-by",
        "username",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Admin(UserAdminCommand {
                    command: UserAdminSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 25);
            assert_eq!(args.search.as_deref(), Some("alice"));
            assert!(matches!(args.sort_by, Some(CliUserSortField::Username)));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_delete() {
    let cli = Cli::parse_from(["synctv", "room", "delete", "room-123"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Delete(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-123");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_review_approve_room_creation() {
    let cli = Cli::parse_from([
        "synctv",
        "review",
        "room-creation",
        "approve",
        "room-approve",
    ]);
    match cli.command {
        Commands::Review(ReviewCommand {
            command:
                ReviewSubcommand::RoomCreation(ReviewRoomCreationCommand {
                    command: ReviewRoomCreationSubcommand::Approve(args),
                }),
            ..
        }) => {
            assert_eq!(args.request_id, "room-approve");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_review_list_filters() {
    let cli = Cli::parse_from([
        "synctv",
        "review",
        "room-join",
        "list",
        "--status",
        "rejected",
        "--room-id",
        "room12345678",
        "--user-id",
        "user12345678",
        "--page",
        "2",
        "--page-size",
        "25",
    ]);
    match cli.command {
        Commands::Review(ReviewCommand {
            command:
                ReviewSubcommand::RoomJoin(ReviewRoomJoinCommand {
                    command: ReviewRoomJoinSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.status, CliReviewStatus::Rejected);
            assert_eq!(args.room_id.as_deref(), Some("room12345678"));
            assert_eq!(args.user_id.as_deref(), Some("user12345678"));
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 25);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_review_reject_user_registration() {
    let cli = Cli::parse_from([
        "synctv",
        "review",
        "user-registration",
        "reject",
        "request12345",
        "--reason",
        "duplicate account",
    ]);
    match cli.command {
        Commands::Review(ReviewCommand {
            command:
                ReviewSubcommand::UserRegistration(ReviewUserRegistrationCommand {
                    command: ReviewUserRegistrationSubcommand::Reject(args),
                }),
            ..
        }) => {
            assert_eq!(args.request_id, "request12345");
            assert_eq!(args.reason.as_deref(), Some("duplicate account"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_ban_list_filters() {
    let cli = Cli::parse_from([
        "synctv",
        "ban",
        "list",
        "--target",
        "room",
        "--active",
        "true",
        "--room-id",
        "room12345678",
        "--user-id",
        "user12345678",
        "--page",
        "3",
        "--page-size",
        "10",
    ]);
    match cli.command {
        Commands::Ban(BanCommand {
            command: BanSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.target, Some(CliBanTarget::Room));
            assert_eq!(args.active, Some(true));
            assert_eq!(args.room_id.as_deref(), Some("room12345678"));
            assert_eq!(args.user_id.as_deref(), Some("user12345678"));
            assert_eq!(args.page, 3);
            assert_eq!(args.page_size, 10);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_set_password_clear_mode() {
    let cli = Cli::parse_from(["synctv", "room", "set-password", "room-123", "--clear"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::SetPassword(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-123");
            assert!(args.clear);
            assert!(args.new_password.is_none());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_set_password_with_password_flag() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "set-password",
        "room-123",
        "--password",
        "room-secret",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::SetPassword(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-123");
            assert_eq!(args.new_password.as_deref(), Some("room-secret"));
            assert!(!args.clear);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_room_set_password_help_uses_password_flag() {
    let mut command = Cli::command();
    let room = command
        .find_subcommand_mut("room")
        .expect("room subcommand should exist");
    let set_password = room
        .find_subcommand_mut("set-password")
        .expect("room set-password subcommand should exist");
    let mut help = Vec::new();
    set_password
        .write_long_help(&mut help)
        .expect("room set-password help should render");
    let help = String::from_utf8(help).expect("room set-password help should be utf-8");

    assert!(
        help.contains("--password <PASSWORD>"),
        "room set-password help should use --password: {help}"
    );
    assert!(
        !help.contains("--new-password"),
        "room set-password help should not expose --new-password: {help}"
    );
}

#[test]
fn cli_parses_room_ban_with_reason() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "ban",
        "room-ban-1",
        "--reason",
        "moderation",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Ban(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-ban-1");
            assert_eq!(args.reason.as_deref(), Some("moderation"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_unban() {
    let cli = Cli::parse_from(["synctv", "room", "unban", "room-ban-1"]);
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Unban(args),
            ..
        }) => {
            assert_eq!(args.room_id, "room-ban-1");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_playlist_create_for_room_scope() {
    let cli = Cli::parse_from([
        "synctv",
        "playlist",
        "create",
        "Favorites",
        "--room-id",
        "room-123",
        "--username",
        "alice",
        "--parent-id",
        "folder-1",
        "--source-provider",
        "alist",
        "--source-config-json",
        "{\"path\":\"/movies\"}",
        "--provider-instance-name",
        "alist_main",
    ]);
    match cli.command {
        Commands::Playlist(PlaylistCommand {
            command: PlaylistSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.name, "Favorites");
            assert_eq!(args.parent_id.as_deref(), Some("folder-1"));
            assert_eq!(args.source_provider, Some(CliSourceProvider::Alist));
            assert_eq!(
                args.source_config_json.as_deref(),
                Some("{\"path\":\"/movies\"}")
            );
            assert_eq!(args.provider_instance_name.as_deref(), Some("alist_main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_playlist_list_dynamic_only_as_bare_true_flag() {
    let cli = Cli::parse_from([
        "synctv",
        "playlist",
        "list",
        "--room-id",
        "room-123",
        "--dynamic-only",
        "--availability",
        "available",
    ]);
    match cli.command {
        Commands::Playlist(PlaylistCommand {
            command: PlaylistSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.dynamic_only, Some(true));
            assert!(matches!(
                args.availability,
                CliResourceAvailabilityFilter::Available
            ));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_playlist_create_without_actor_user() {
    let result = Cli::try_parse_from([
        "synctv",
        "playlist",
        "create",
        "Favorites",
        "--room-id",
        "room-123",
    ]);
    assert!(
        result.is_err(),
        "playlist create must require --username or --user-id"
    );
}

#[test]
fn cli_parses_media_add_url_for_room_scope() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "add-url",
        "https://cdn.example.com/video.mp4",
        "--room-id",
        "room-123",
        "--username",
        "alice",
        "--playlist-id",
        "playlist-1",
        "--name",
        "Demo Video",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::AddUrl(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.url, "https://cdn.example.com/video.mp4");
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-1"));
            assert_eq!(args.name.as_deref(), Some("Demo Video"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_add_for_room_scope() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "add",
        "--room-id",
        "room-123",
        "--username",
        "alice",
        "--playlist-id",
        "playlist-1",
        "--source-provider",
        "alist",
        "--provider-instance-name",
        "alist-main",
        "--source-config-json",
        "{\"path\":\"/movies/demo.mp4\"}",
        "--name",
        "Demo Video",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Add(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-1"));
            assert_eq!(args.source_provider, CliSourceProvider::Alist);
            assert_eq!(args.provider_instance_name.as_deref(), Some("alist-main"));
            assert_eq!(args.source_config_json, "{\"path\":\"/movies/demo.mp4\"}");
            assert_eq!(args.name.as_deref(), Some("Demo Video"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_update() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "update",
        "--room-id",
        "room-123",
        "media-1",
        "--name",
        "Renamed",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Update(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.media_id, "media-1");
            assert_eq!(args.name, "Renamed");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_media_add_without_actor_user() {
    let result = Cli::try_parse_from([
        "synctv",
        "media",
        "add",
        "--room-id",
        "room-123",
        "--source-provider",
        "direct-url",
        "--source-config-json",
        "{\"medias\":[{\"url\":\"https://cdn.example.com/video.mp4\"}]}",
    ]);
    assert!(
        result.is_err(),
        "media add must require --username or --user-id"
    );
}

#[test]
fn cli_rejects_snake_case_source_provider_values() {
    for provider in ["direct_url", "live_proxy"] {
        let result = Cli::try_parse_from([
            "synctv",
            "media",
            "add",
            "--room-id",
            "room-123",
            "--username",
            "alice",
            "--source-provider",
            provider,
            "--source-config-json",
            "{\"medias\":[{\"url\":\"https://cdn.example.com/video.mp4\"}]}",
        ]);
        assert!(result.is_err(), "{provider} should be rejected");
    }
}

#[test]
fn cli_rejects_media_add_without_source_provider() {
    let result = Cli::try_parse_from([
        "synctv",
        "media",
        "add",
        "--room-id",
        "room-123",
        "--username",
        "alice",
        "--source-config-json",
        "{\"medias\":[{\"url\":\"https://cdn.example.com/video.mp4\"}]}",
    ]);
    assert!(result.is_err(), "media add must require --source-provider");
}

#[test]
fn cli_parses_media_list_availability_filter() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "list",
        "--room-id",
        "room-123",
        "--availability",
        "unavailable",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert!(matches!(
                args.availability,
                CliResourceAvailabilityFilter::Unavailable
            ));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_for_room_scope() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--before-media-id",
        "media-2",
        "--media-id",
        "media-1",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.media_ids, vec!["media-1".to_string()]);
            assert_eq!(args.before_media_id.as_deref(), Some("media-2"));
            assert!(args.after_media_id.is_none());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_add_with_provider_source() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "add",
        "--room-id",
        "room-123",
        "--username",
        "alice",
        "--playlist-id",
        "playlist-456",
        "--source-provider",
        "alist",
        "--provider-instance-name",
        "alist_main",
        "--source-config-json",
        "{\"path\":\"/movies/demo.mp4\"}",
        "--name",
        "Demo Media",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Add(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-456"));
            assert_eq!(args.source_provider, CliSourceProvider::Alist);
            assert_eq!(args.provider_instance_name.as_deref(), Some("alist_main"));
            assert_eq!(args.source_config_json, "{\"path\":\"/movies/demo.mp4\"}");
            assert_eq!(args.name.as_deref(), Some("Demo Media"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_after_anchor() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--after-media-id",
        "media-b",
        "--media-id",
        "media-a",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.media_ids, vec!["media-a".to_string()]);
            assert_eq!(args.after_media_id.as_deref(), Some("media-b"));
            assert!(args.before_media_id.is_none());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_with_hyphen_prefixed_media_id() {
    let cli = Cli::try_parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--before-media-id",
        "media-1",
        "--media-id",
        "-99tNxdXRosK",
    ])
    .expect("hyphen-prefixed media ids should be accepted as explicit media ids");
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.media_ids, vec!["-99tNxdXRosK".to_string()]);
            assert_eq!(args.before_media_id.as_deref(), Some("media-1"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_without_anchor_as_append() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--media-id",
        "media-1",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(args.media_ids, vec!["media-1".to_string()]);
            assert!(args.before_media_id.is_none());
            assert!(args.after_media_id.is_none());
            assert!(!args.all_from_scope);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_batch_to_playlist() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--to-playlist-id",
        "playlist-9",
        "--after-media-id",
        "media-anchor",
        "--media-id",
        "media-1",
        "--media-id",
        "media-2",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(
                args.media_ids,
                vec!["media-1".to_string(), "media-2".to_string()]
            );
            assert_eq!(args.to_playlist_id.as_deref(), Some("playlist-9"));
            assert_eq!(args.after_media_id.as_deref(), Some("media-anchor"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_move_all_from_scope() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--all-from-scope",
        "--from-playlist-id",
        "playlist-src",
        "--to-playlist-id",
        "playlist-dst",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert!(args.media_ids.is_empty());
            assert!(args.all_from_scope);
            assert_eq!(args.from_playlist_id.as_deref(), Some("playlist-src"));
            assert_eq!(args.to_playlist_id.as_deref(), Some("playlist-dst"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_media_move_from_playlist_without_all_from_scope() {
    let result = Cli::try_parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--from-playlist-id",
        "playlist-src",
        "--media-id",
        "media-1",
    ]);
    assert!(
        result.is_err(),
        "media move must reject --from-playlist-id without --all-from-scope"
    );
}

#[test]
fn cli_parses_media_move_without_consuming_trailing_global_output_flag() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "move",
        "--room-id",
        "room-123",
        "--before-media-id",
        "media-2",
        "--media-id",
        "media-1",
        "--output",
        "json",
    ]);
    match &cli.command {
        Commands::Media(MediaCommand {
            command: MediaSubcommand::Move(args),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-123");
            assert_eq!(args.media_ids, vec!["media-1".to_string()]);
            assert_eq!(args.before_media_id.as_deref(), Some("media-2"));
            assert_eq!(args.room.remote.output, RemoteOutputFormat::Json);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_media_move_help_describes_scope_and_append_semantics() {
    let mut command = Cli::command();
    let media = command
        .find_subcommand_mut("media")
        .expect("media subcommand should exist");
    let move_cmd = media
        .find_subcommand_mut("move")
        .expect("media move subcommand should exist");

    let mut help = Vec::new();
    move_cmd
        .write_long_help(&mut help)
        .expect("media move help should render");
    let help = String::from_utf8(help).expect("media move help should be utf-8");

    assert!(
        help.contains("--media-id <MEDIA_ID>"),
        "media move help should expose repeatable --media-id: {help}"
    );
    assert!(
        help.contains("Target static playlist. Omit to keep media in the current scope"),
        "media move help should explain optional target scope: {help}"
    );
    assert!(
        help.contains("Omit both anchors to append to the target scope"),
        "media move help should explain append semantics without anchors: {help}"
    );
}

#[test]
fn normalized_provider_types_maps_cli_values() {
    assert_eq!(
        normalized_provider_types(&[
            CliSourceProvider::Alist,
            CliSourceProvider::Emby,
            CliSourceProvider::Bilibili,
        ]),
        vec![
            synctv_proto::source_config::SourceProvider::Alist as i32,
            synctv_proto::source_config::SourceProvider::Emby as i32,
            synctv_proto::source_config::SourceProvider::Bilibili as i32,
        ]
    );
}

#[test]
fn cli_parses_provider_create_with_remote_auth() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "create",
        "alist-edge",
        "https://provider.example.com:50051",
        "--provider",
        "alist",
        "--provider",
        "emby",
        "--comment",
        "edge provider",
        "--timeout-seconds",
        "15",
        "--tls",
        "--jwt-secret",
        "provider-secret-12345678901234567890",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::Create(args),
            ..
        }) => {
            assert_eq!(args.name, "alist-edge");
            assert_eq!(args.provider_endpoint, "https://provider.example.com:50051");
            assert_eq!(
                args.providers,
                vec![CliSourceProvider::Alist, CliSourceProvider::Emby]
            );
            assert_eq!(args.comment.as_deref(), Some("edge provider"));
            assert_eq!(args.timeout_seconds, 15);
            assert!(args.tls);
            assert_eq!(
                args.jwt_secret.as_deref(),
                Some("provider-secret-12345678901234567890")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_update_with_optional_fields() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "update",
        "alist-edge",
        "--provider-endpoint",
        "https://provider-v2.example.com:50052",
        "--provider",
        "alist",
        "--timeout-seconds",
        "20",
        "--tls",
        "true",
        "--insecure-tls",
        "false",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::Update(args),
            ..
        }) => {
            assert_eq!(args.name, "alist-edge");
            assert_eq!(
                args.provider_endpoint.as_deref(),
                Some("https://provider-v2.example.com:50052")
            );
            assert_eq!(args.providers, vec![CliSourceProvider::Alist]);
            assert_eq!(args.timeout_seconds, Some(20));
            assert_eq!(args.tls, Some(true));
            assert_eq!(args.insecure_tls, Some(false));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_list_boolean_filters_as_bare_true_flags() {
    let cli = Cli::parse_from(["synctv", "provider", "list", "--enabled", "--tls"]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.enabled, Some(true));
            assert_eq!(args.tls, Some(true));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_available_command() {
    let cli = Cli::parse_from(["synctv", "provider", "available"]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::Available(_),
            ..
        }) => {}
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_backends_command() {
    let cli = Cli::parse_from(["synctv", "provider", "backends", "emby"]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::Backends(args),
            ..
        }) => assert_eq!(args.provider_type, CliSourceProvider::Emby),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_update_boolean_flags_as_bare_true_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "update",
        "alist-edge",
        "--tls",
        "--insecure-tls",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::Update(args),
            ..
        }) => {
            assert_eq!(args.tls, Some(true));
            assert_eq!(args.insecure_tls, Some(true));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_provider_create_help_disambiguates_provider_endpoint_from_management_endpoint() {
    let mut command = Cli::command();
    let provider = command
        .find_subcommand_mut("provider")
        .expect("provider subcommand should exist");
    let provider_create = provider
        .find_subcommand_mut("create")
        .expect("provider create subcommand should exist");
    let mut help = Vec::new();
    provider_create
        .write_long_help(&mut help)
        .expect("provider create help should render");
    let help = String::from_utf8(help).expect("provider create help should be utf-8");

    assert!(
        help.contains("<PROVIDER_ENDPOINT>"),
        "provider create help should expose provider endpoint positional name: {help}"
    );
    assert!(
        help.contains("--endpoint <ENDPOINT>"),
        "provider create help should still expose management endpoint override: {help}"
    );
    assert!(
        help.contains("--config"),
        "provider create help should expose config loading flags: {help}"
    );
}

#[test]
fn cli_user_batch_ban_help_uses_singular_user_id_metavar() {
    let mut command = Cli::command();
    let user = command
        .find_subcommand_mut("user")
        .expect("user subcommand should exist");
    let batch = user
        .find_subcommand_mut("batch")
        .expect("user batch subcommand should exist");
    let ban = batch
        .find_subcommand_mut("ban")
        .expect("user batch ban subcommand should exist");
    let mut help = Vec::new();
    ban.write_long_help(&mut help)
        .expect("user batch ban help should render");
    let help = String::from_utf8(help).expect("user batch ban help should be utf-8");

    assert!(
        help.contains("--user-id <USER_ID>..."),
        "user batch ban help should use singular user id metavar: {help}"
    );
}

#[test]
fn cli_parses_provider_list_with_filter() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "list",
        "--page",
        "2",
        "--page-size",
        "10",
        "--provider-type",
        "alist",
        "--search",
        "edge",
        "--enabled",
        "true",
        "--tls",
        "true",
        "--sort-by",
        "name",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 10);
            assert_eq!(args.provider_type, Some(CliSourceProvider::Alist));
            assert_eq!(args.search.as_deref(), Some("edge"));
            assert_eq!(args.enabled, Some(true));
            assert_eq!(args.tls, Some(true));
            assert!(matches!(args.sort_by, Some(CliProviderSortField::Name)));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_provider_update_help_uses_provider_type_metavar() {
    let mut command = Cli::command();
    let provider = command
        .find_subcommand_mut("provider")
        .expect("provider subcommand should exist");
    let update = provider
        .find_subcommand_mut("update")
        .expect("provider update subcommand should exist");
    let mut help = Vec::new();
    update
        .write_long_help(&mut help)
        .expect("provider update help should render");
    let help = String::from_utf8(help).expect("provider update help should be utf-8");

    assert!(
        help.contains("--provider <PROVIDER_TYPE>"),
        "provider update help should use provider type metavar: {help}"
    );
}

#[test]
fn cli_parses_provider_alist_login() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "alist",
        "login",
        "--username",
        "alice",
        "--host",
        "https://alist.example.com",
        "--account-username",
        "alist-user",
        "--password",
        "secret",
        "--otp-code",
        "123456",
        "--otp-secret",
        "JBSWY3DPEHPK3PXP",
        "--instance-name",
        "alist-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Alist(ProviderAlistCommand {
                    command: ProviderAlistSubcommand::Login(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.host, "https://alist.example.com");
            assert_eq!(args.account_username, "alist-user");
            assert_eq!(args.password.as_deref(), Some("secret"));
            assert_eq!(args.otp_code.as_deref(), Some("123456"));
            assert_eq!(args.otp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("alist-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_alist_list() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "alist",
        "list",
        "--username",
        "alice",
        "--server-id",
        "alist-srv",
        "--path",
        "/movies",
        "--password",
        "dir-pass",
        "--page",
        "2",
        "--per-page",
        "25",
        "--refresh",
        "--instance-name",
        "alist-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Alist(ProviderAlistCommand {
                    command: ProviderAlistSubcommand::List(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.bind.server_id, "alist-srv");
            assert_eq!(args.path, "/movies");
            assert_eq!(args.password.as_deref(), Some("dir-pass"));
            assert_eq!(args.page, 2);
            assert_eq!(args.per_page, 25);
            assert!(args.refresh);
            assert_eq!(
                args.bind.instance.instance_name.as_deref(),
                Some("alist-edge")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_alist_search() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "alist",
        "search",
        "--username",
        "alice",
        "--server-id",
        "alist-srv",
        "--parent",
        "/movies",
        "--keywords",
        "pilot",
        "--scope",
        "2",
        "--password",
        "dir-pass",
        "--page",
        "2",
        "--per-page",
        "25",
        "--instance-name",
        "alist-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Alist(ProviderAlistCommand {
                    command: ProviderAlistSubcommand::Search(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.bind.server_id, "alist-srv");
            assert_eq!(args.parent, "/movies");
            assert_eq!(args.keywords, "pilot");
            assert_eq!(args.scope, 2);
            assert_eq!(args.password.as_deref(), Some("dir-pass"));
            assert_eq!(args.page, 2);
            assert_eq!(args.per_page, 25);
            assert_eq!(
                args.bind.instance.instance_name.as_deref(),
                Some("alist-edge")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_alist_binds() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "alist",
        "binds",
        "--user-id",
        "user-1",
        "--instance-name",
        "alist-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Alist(ProviderAlistCommand {
                    command: ProviderAlistSubcommand::Binds(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("alist-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_emby_login_with_api_key() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "emby",
        "login",
        "--username",
        "alice",
        "--host",
        "https://emby.example.com",
        "--account-username",
        "emby-user",
        "--api-key",
        "emby-api-key",
        "--instance-name",
        "emby-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Emby(ProviderEmbyCommand {
                    command: ProviderEmbySubcommand::Login(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.host, "https://emby.example.com");
            assert_eq!(args.account_username, "emby-user");
            assert_eq!(args.password, None);
            assert_eq!(args.api_key.as_deref(), Some("emby-api-key"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("emby-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_emby_login_with_password() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "emby",
        "login",
        "--username",
        "alice",
        "--host",
        "https://emby.example.com",
        "--account-username",
        "emby-user",
        "--password",
        "secret",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Emby(ProviderEmbyCommand {
                    command: ProviderEmbySubcommand::Login(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.password.as_deref(), Some("secret"));
            assert_eq!(args.api_key, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_emby_list() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "emby",
        "list",
        "--user-id",
        "user-1",
        "--server-id",
        "emby-srv",
        "--path",
        "library-root",
        "--start-index",
        "10",
        "--limit",
        "20",
        "--search-term",
        "pilot",
        "--instance-name",
        "emby-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Emby(ProviderEmbyCommand {
                    command: ProviderEmbySubcommand::List(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.bind.server_id, "emby-srv");
            assert_eq!(args.path, "library-root");
            assert_eq!(args.start_index, 10);
            assert_eq!(args.limit, 20);
            assert_eq!(args.search_term.as_deref(), Some("pilot"));
            assert_eq!(
                args.bind.instance.instance_name.as_deref(),
                Some("emby-edge")
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_emby_binds() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "emby",
        "binds",
        "--username",
        "alice",
        "--instance-name",
        "emby-edge",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Emby(ProviderEmbyCommand {
                    command: ProviderEmbySubcommand::Binds(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("emby-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_provider_alist_login_without_credential() {
    let result = Cli::try_parse_from([
        "synctv",
        "provider",
        "alist",
        "login",
        "--username",
        "alice",
        "--host",
        "https://alist.example.com",
        "--account-username",
        "alist-user",
    ]);

    assert!(
        result.is_err(),
        "provider alist login must require password or hashed password"
    );
}

#[test]
fn cli_rejects_provider_emby_login_without_credential() {
    let result = Cli::try_parse_from([
        "synctv",
        "provider",
        "emby",
        "login",
        "--username",
        "alice",
        "--host",
        "https://emby.example.com",
        "--account-username",
        "emby-user",
    ]);

    assert!(
        result.is_err(),
        "provider emby login must require password or api key"
    );
}

#[test]
fn cli_rejects_provider_emby_login_with_both_password_and_api_key() {
    let result = Cli::try_parse_from([
        "synctv",
        "provider",
        "emby",
        "login",
        "--username",
        "alice",
        "--host",
        "https://emby.example.com",
        "--account-username",
        "emby-user",
        "--password",
        "secret",
        "--api-key",
        "emby-api-key",
    ]);

    assert!(
        result.is_err(),
        "provider emby login must reject simultaneous password and api key"
    );
}

#[test]
fn cli_parses_provider_bilibili_parse() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "parse",
        "--username",
        "alice",
        "https://www.bilibili.com/video/BV1xx411c7mD",
        "--instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::Parse(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.url, "https://www.bilibili.com/video/BV1xx411c7mD");
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_login_qr() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "login-qr",
        "--user-id",
        "user-1",
        "--instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::LoginQr(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_check_qr() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "check-qr",
        "--user-id",
        "user-1",
        "--key",
        "qr-key-1",
        "--instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::CheckQr(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.key, "qr-key-1");
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_start_sms_login() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "start-sms-login",
        "--username",
        "alice",
        "--instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::StartSmsLogin(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_send_sms() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "send-sms",
        "--username",
        "alice",
        "--phone",
        "13800138000",
        "--session-token",
        "sms-session-token",
        "--validate",
        "captcha-validate",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::SendSms(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.phone, "13800138000");
            assert_eq!(args.session_token, "sms-session-token");
            assert_eq!(args.validate, "captcha-validate");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_login_sms() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "login-sms",
        "--user-id",
        "user-1",
        "--session-token",
        "sms-session-token",
        "--code",
        "123456",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::LoginSms(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.session_token, "sms-session-token");
            assert_eq!(args.code, "123456");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_bilibili_me_logout_and_binds() {
    let cli_me = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "me",
        "--username",
        "alice",
        "--instance-name",
        "bili-main",
    ]);
    match cli_me.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::Me(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let cli_logout = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "logout",
        "--user-id",
        "user-1",
    ]);
    match cli_logout.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::Logout(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.instance.instance_name, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let cli_binds = Cli::parse_from([
        "synctv",
        "provider",
        "bilibili",
        "binds",
        "--user-id",
        "user-1",
        "--instance-name",
        "bili-main",
    ]);
    match cli_binds.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Bilibili(ProviderBilibiliCommand {
                    command: ProviderBilibiliSubcommand::Binds(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_rtmp_create_publish_key_and_get_stream_info() {
    let cli_publish_key = Cli::parse_from([
        "synctv",
        "provider",
        "rtmp",
        "create-publish-key",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "media-1",
    ]);
    match cli_publish_key.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Rtmp(ProviderRtmpCommand {
                    command: ProviderRtmpSubcommand::CreatePublishKey(args),
                }),
        }) => {
            assert_eq!(args.room_id, "room-1");
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(
                args.resolved_media_id().expect("media id should resolve"),
                "media-1"
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let cli_info = Cli::parse_from([
        "synctv",
        "provider",
        "rtmp",
        "get-stream-info",
        "--room-id",
        "room-1",
        "media-1",
    ]);
    match cli_info.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Rtmp(ProviderRtmpCommand {
                    command: ProviderRtmpSubcommand::GetStreamInfo(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room-1");
            assert_eq!(
                args.resolved_media_id().expect("media id should resolve"),
                "media-1"
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_rtmp_get_stream_info_media_id_flag() {
    let cli_info = Cli::parse_from([
        "synctv",
        "provider",
        "rtmp",
        "get-stream-info",
        "--room-id",
        "room-1",
        "--media-id",
        "media-1",
    ]);
    match cli_info.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Rtmp(ProviderRtmpCommand {
                    command: ProviderRtmpSubcommand::GetStreamInfo(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room-1");
            assert_eq!(
                args.resolved_media_id().expect("media id should resolve"),
                "media-1"
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_playlist_provider_alist() {
    let cli = Cli::parse_from([
        "synctv",
        "playlist",
        "provider",
        "alist",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "Movies",
        "--path",
        "/movies",
        "--server-id",
        "srv-1",
        "--provider-instance-name",
        "alist-edge",
    ]);
    match cli.command {
        Commands::Playlist(PlaylistCommand {
            command:
                PlaylistSubcommand::Provider(PlaylistProviderCommand {
                    command: PlaylistProviderSubcommand::Alist(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.name, "Movies");
            assert_eq!(args.path, "/movies");
            assert_eq!(args.server_id, "srv-1");
            assert_eq!(args.provider_instance_name.as_deref(), Some("alist-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_playlist_provider_emby() {
    let cli = Cli::parse_from([
        "synctv",
        "playlist",
        "provider",
        "emby",
        "--room-id",
        "room-1",
        "--user-id",
        "user-1",
        "Series",
        "--item-id",
        "library-root",
        "--server-id",
        "emby-srv",
        "--provider-instance-name",
        "emby-main",
    ]);
    match cli.command {
        Commands::Playlist(PlaylistCommand {
            command:
                PlaylistSubcommand::Provider(PlaylistProviderCommand {
                    command: PlaylistProviderSubcommand::Emby(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.name, "Series");
            assert_eq!(args.item_id, "library-root");
            assert_eq!(args.server_id, "emby-srv");
            assert_eq!(args.provider_instance_name.as_deref(), Some("emby-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_provider_alist() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "provider",
        "alist",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "--path",
        "/movies/demo.mp4",
        "--server-id",
        "alist-srv",
        "--playlist-id",
        "playlist-1",
        "--provider-instance-name",
        "alist-edge",
        "--name",
        "Demo",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command:
                MediaSubcommand::Provider(MediaProviderCommand {
                    command: MediaProviderSubcommand::Alist(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.path, "/movies/demo.mp4");
            assert_eq!(args.server_id, "alist-srv");
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-1"));
            assert_eq!(args.provider_instance_name.as_deref(), Some("alist-edge"));
            assert_eq!(args.name.as_deref(), Some("Demo"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_provider_emby() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "provider",
        "emby",
        "--room-id",
        "room-1",
        "--user-id",
        "user-1",
        "--item-id",
        "item-123",
        "--server-id",
        "srv-2",
        "--playlist-id",
        "playlist-1",
        "--name",
        "Pilot",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command:
                MediaSubcommand::Provider(MediaProviderCommand {
                    command: MediaProviderSubcommand::Emby(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.item_id, "item-123");
            assert_eq!(args.server_id, "srv-2");
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-1"));
            assert_eq!(args.name.as_deref(), Some("Pilot"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_provider_bilibili_pgc() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "provider",
        "bilibili",
        "pgc",
        "--room-id",
        "room-1",
        "--user-id",
        "user-1",
        "--epid",
        "1001",
        "--cid",
        "2002",
        "--playlist-id",
        "playlist-1",
        "--name",
        "Episode 1",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command:
                MediaSubcommand::Provider(MediaProviderCommand {
                    command:
                        MediaProviderSubcommand::Bilibili(MediaProviderBilibiliCommand {
                            command: MediaProviderBilibiliSubcommand::Pgc(args),
                        }),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.epid, 1001);
            assert_eq!(args.cid, 2002);
            assert_eq!(args.playlist_id.as_deref(), Some("playlist-1"));
            assert_eq!(args.name.as_deref(), Some("Episode 1"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_provider_bilibili_live() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "provider",
        "bilibili",
        "live",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "--room-live-id",
        "778899",
        "--provider-instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command:
                MediaSubcommand::Provider(MediaProviderCommand {
                    command:
                        MediaProviderSubcommand::Bilibili(MediaProviderBilibiliCommand {
                            command: MediaProviderBilibiliSubcommand::Live(args),
                        }),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.room_live_id, 778_899);
            assert_eq!(args.provider_instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_media_provider_bilibili_video() {
    let cli = Cli::parse_from([
        "synctv",
        "media",
        "provider",
        "bilibili",
        "video",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "--bvid",
        "BV1xx411c7mD",
        "--cid",
        "2333",
        "--provider-instance-name",
        "bili-main",
    ]);
    match cli.command {
        Commands::Media(MediaCommand {
            command:
                MediaSubcommand::Provider(MediaProviderCommand {
                    command:
                        MediaProviderSubcommand::Bilibili(MediaProviderBilibiliCommand {
                            command: MediaProviderBilibiliSubcommand::Video(args),
                        }),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.video.bvid.as_deref(), Some("BV1xx411c7mD"));
            assert_eq!(args.video.aid, None);
            assert_eq!(args.cid, 2333);
            assert_eq!(args.provider_instance_name.as_deref(), Some("bili-main"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_list_query_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "provider",
        "list",
        "--page",
        "2",
        "--page-size",
        "25",
        "--provider-type",
        "alist",
        "--search",
        "edge",
        "--enabled",
        "true",
        "--tls",
        "false",
        "--sort-by",
        "updated-at",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::Provider(ProviderCommand {
            command: ProviderSubcommand::List(args),
            ..
        }) => {
            assert_eq!(args.page, 2);
            assert_eq!(args.page_size, 25);
            assert_eq!(args.provider_type, Some(CliSourceProvider::Alist));
            assert_eq!(args.search.as_deref(), Some("edge"));
            assert_eq!(args.enabled, Some(true));
            assert_eq!(args.tls, Some(false));
            assert!(matches!(
                args.sort_by,
                Some(CliProviderSortField::UpdatedAt)
            ));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_admin_grant_subcommand() {
    let cli = Cli::parse_from(["synctv", "user", "admin", "grant", "alice"]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Admin(UserAdminCommand {
                    command: UserAdminSubcommand::Grant(args),
                }),
        }) => {
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.user.user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_batch_ban_with_multiple_ids() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "batch",
        "ban",
        "--user-id",
        "user-1",
        "--user-id",
        "user-2",
        "--reason",
        "abuse",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Batch(UserBatchCommand {
                    command: UserBatchSubcommand::Ban(args),
                }),
        }) => {
            assert!(args.usernames.is_empty());
            assert_eq!(args.user_ids, vec!["user-1", "user-2"]);
            assert_eq!(args.reason.as_deref(), Some("abuse"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_batch_ban_with_usernames_and_ids() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "batch",
        "ban",
        "alice",
        "bob",
        "--user-id",
        "user-1",
        "--reason",
        "abuse",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Batch(UserBatchCommand {
                    command: UserBatchSubcommand::Ban(args),
                }),
        }) => {
            assert_eq!(args.usernames, vec!["alice", "bob"]);
            assert_eq!(args.user_ids, vec!["user-1"]);
            assert_eq!(args.reason.as_deref(), Some("abuse"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_batch_delete_with_usernames() {
    let cli = Cli::parse_from(["synctv", "user", "batch", "delete", "alice", "bob"]);
    match cli.command {
        Commands::User(UserCommand {
            command:
                UserSubcommand::Batch(UserBatchCommand {
                    command: UserBatchSubcommand::Delete(args),
                }),
        }) => {
            assert_eq!(args.usernames, vec!["alice", "bob"]);
            assert!(args.user_ids.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_member_permissions_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "set-permissions",
        "--room-id",
        "room-1",
        "alice",
        "--role",
        "admin",
        "--admin-added-permissions",
        "7",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::SetPermissions(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.user.user_id, None);
            assert_eq!(args.role, Some(CliRoomMemberRole::Admin));
            assert_eq!(args.admin_added_permissions.map(u64::from), Some(7));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_member_add_display_fields() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "add",
        "--room-id",
        "room-1",
        "alice",
        "--role",
        "admin",
        "--notify",
        "--remark-name",
        "Alice",
        "--display-tag",
        "mod",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::Add(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.role, CliRoomMemberRole::Admin);
            assert!(args.notify);
            assert_eq!(args.remark_name.as_deref(), Some("Alice"));
            assert_eq!(args.display_tag.as_deref(), Some("mod"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_member_set_remark_name_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "set-remark-name",
        "--room-id",
        "room-1",
        "--user-id",
        "user-9",
        "--remark-name",
        "Alice",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::SetRemarkName(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-9"));
            assert_eq!(args.remark_name, "Alice");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_member_set_display_tag_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "set-display-tag",
        "--room-id",
        "room-1",
        "--user-id",
        "user-9",
        "--display-tag",
        "mod",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::SetDisplayTag(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-9"));
            assert_eq!(args.display_tag, "mod");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_member_permission_names() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "set-permissions",
        "--room-id",
        "room-1",
        "alice",
        "--added-permissions",
        "send-chat-messages,use-voice-chat,use-p2p-media",
        "--removed-permissions",
        r#"["send_chat_messages"]"#,
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::SetPermissions(args),
                }),
        }) => {
            assert_eq!(
                args.added_permissions.map(u64::from),
                Some(
                    RoomMemberPermissionBits::SEND_CHAT_MESSAGES
                        | RoomMemberPermissionBits::USE_VOICE_CHAT
                        | RoomMemberPermissionBits::USE_P2P_MEDIA
                )
            );
            assert_eq!(
                args.removed_permissions.map(u64::from),
                Some(RoomMemberPermissionBits::SEND_CHAT_MESSAGES)
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_accepts_send_chat_messages_permission_name() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "set-permissions",
        "--room-id",
        "room-1",
        "alice",
        "--removed-permissions",
        r#"["send_chat_messages"]"#,
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::SetPermissions(args),
                }),
        }) => {
            assert_eq!(
                args.removed_permissions.map(u64::from),
                Some(RoomMemberPermissionBits::SEND_CHAT_MESSAGES)
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_unknown_room_member_permission_name() {
    let error = Cli::try_parse_from([
        "synctv",
        "room",
        "member",
        "set-permissions",
        "--room-id",
        "room-1",
        "alice",
        "--added-permissions",
        "delete_room",
    ])
    .expect_err("room permission overrides must reject unknown permissions");

    let message = error.to_string();
    assert!(message.contains("unknown permission"));
    assert!(message.contains("delete_room"));
}

#[test]
fn cli_rejects_unknown_room_member_permission_bitmask() {
    let error = Cli::try_parse_from([
        "synctv",
        "room",
        "member",
        "set-permissions",
        "--room-id",
        "room-1",
        "alice",
        "--added-permissions",
        &(1_u64 << 21).to_string(),
    ])
    .expect_err("room permission overrides must reject unknown bitmasks");

    let message = error.to_string();
    assert!(message.contains("bits outside this role bitspace"));
    assert!(message.contains("2097152"));
}

#[test]
fn cli_parses_room_batch_ban_with_positional_room_ids() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "batch",
        "ban",
        "room-1",
        "room-2",
        "--reason",
        "moderation",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Batch(RoomBatchCommand {
                    command: RoomBatchSubcommand::Ban(args),
                }),
            ..
        }) => {
            assert_eq!(args.resolved_room_ids(), vec!["room-1", "room-2"]);
            assert_eq!(args.reason.as_deref(), Some("moderation"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_room_batch_delete_room_id_flags() {
    let err = Cli::try_parse_from([
        "synctv",
        "room",
        "batch",
        "delete",
        "--room-id",
        "room-1",
        "--room-id",
        "room-2",
    ])
    .expect_err("room batch delete should use positional ROOM_ID arguments");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn cli_parses_room_member_kick_with_explicit_user_id() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "member",
        "kick",
        "--room-id",
        "room-1",
        "--user-id",
        "user-9",
        "--kick-cooldown-seconds",
        "300",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Member(RoomMemberCommand {
                    command: RoomMemberSubcommand::Kick(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-9"));
            assert_eq!(args.kick_cooldown_seconds, 300);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_playback_start_with_media_id() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "start",
        "--room-id",
        "room-1",
        "--media-id",
        "media-1",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Start(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.media_id.as_deref(), Some("media-1"));
            assert_eq!(args.playlist_id, None);
            assert_eq!(args.actor.username, None);
            assert_eq!(args.actor.user_id, None);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_optional_room_playback_actor() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "start",
        "--room-id",
        "room-1",
        "--media-id",
        "media-1",
        "--email",
        "operator@example.com",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Start(args),
                }),
        }) => {
            assert_eq!(args.actor.username, None);
            assert_eq!(args.actor.user_id, None);
            assert_eq!(args.actor.email.as_deref(), Some("operator@example.com"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_playback_get_with_profile_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "playback",
        "get",
        "--room-id",
        "room-1",
        "--stream",
        "transcode",
        "--max-streaming-bitrate",
        "8000000",
        "--max-audio-channels",
        "2",
        "--video-codec",
        "h264,av1",
        "--video-codec",
        "hevc",
        "--container",
        "mp4,webm",
        "--audio-capability",
        "surround",
        "--subtitle",
        "embedded-or-external",
    ]);
    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Playback(RoomPlaybackCommand {
                    command: RoomPlaybackSubcommand::Get(args),
                }),
            ..
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(
                args.playback_client_profile.stream_preference,
                Some(CliPlaybackStreamPreference::Transcode)
            );
            assert_eq!(
                args.playback_client_profile.max_streaming_bitrate,
                Some(8_000_000)
            );
            assert_eq!(args.playback_client_profile.max_audio_channels, Some(2));
            assert_eq!(
                args.playback_client_profile.supported_video_codecs,
                vec![
                    CliPlaybackVideoCodec::H264,
                    CliPlaybackVideoCodec::Av1,
                    CliPlaybackVideoCodec::Hevc,
                ]
            );
            assert_eq!(
                args.playback_client_profile.supported_containers,
                vec![CliPlaybackContainer::Mp4, CliPlaybackContainer::Webm]
            );
            assert_eq!(
                args.playback_client_profile.audio_capability,
                Some(CliPlaybackAudioCapability::Surround)
            );
            assert_eq!(
                args.playback_client_profile.subtitle_preference,
                Some(CliPlaybackSubtitlePreference::EmbeddedOrExternal)
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn playback_client_profile_args_to_proto_omits_empty_profile() {
    let args = PlaybackClientProfileArgs::default();
    assert_eq!(args.to_proto(), None);
}

#[test]
fn playback_client_profile_args_to_proto_builds_profile() {
    let args = PlaybackClientProfileArgs {
        stream_preference: Some(CliPlaybackStreamPreference::Auto),
        max_streaming_bitrate: Some(10_000_000),
        max_audio_channels: Some(6),
        supported_video_codecs: vec![CliPlaybackVideoCodec::H264, CliPlaybackVideoCodec::Vp9],
        supported_containers: vec![CliPlaybackContainer::Mp4, CliPlaybackContainer::Mkv],
        audio_capability: Some(CliPlaybackAudioCapability::LosslessSurround),
        subtitle_preference: Some(CliPlaybackSubtitlePreference::External),
    };

    let profile = args.to_proto().expect("profile should be created");
    assert_eq!(
        profile.stream_preference,
        synctv_proto::client::PlaybackStreamPreference::Auto as i32
    );
    assert_eq!(profile.max_streaming_bitrate, Some(10_000_000));
    assert_eq!(profile.max_audio_channels, Some(6));
    assert_eq!(
        profile.supported_video_codecs,
        vec![
            synctv_proto::client::PlaybackVideoCodec::H264 as i32,
            synctv_proto::client::PlaybackVideoCodec::Vp9 as i32,
        ]
    );
    assert_eq!(
        profile.supported_containers,
        vec![
            synctv_proto::client::PlaybackContainer::Mp4 as i32,
            synctv_proto::client::PlaybackContainer::Mkv as i32,
        ]
    );
    assert_eq!(
        profile.audio_capability,
        synctv_proto::client::PlaybackAudioCapability::LosslessSurround as i32
    );
    assert_eq!(
        profile.subtitle_preference,
        synctv_proto::client::PlaybackSubtitlePreference::External as i32
    );
}

#[test]
fn cli_parses_settings_test_email_subcommand() {
    let cli = Cli::parse_from(["synctv", "settings", "test-email", "ops@example.com"]);
    match cli.command {
        Commands::Settings(SettingsCommand {
            command: SettingsSubcommand::TestEmail(args),
            ..
        }) => {
            assert_eq!(args.to, "ops@example.com");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_user_create_without_email_and_with_explicit_status_and_role() {
    let cli = Cli::parse_from([
        "synctv",
        "user",
        "create",
        "zjr",
        "--password",
        "StrongPwd12345!",
        "--role",
        "admin",
        "--status",
        "active",
    ]);
    match cli.command {
        Commands::User(UserCommand {
            command: UserSubcommand::Create(args),
        }) => {
            assert_eq!(args.username, "zjr");
            assert_eq!(args.email, None);
            assert_eq!(
                args.role.to_proto(),
                synctv_proto::common::UserRole::Admin as i32
            );
            assert_eq!(
                args.status.to_proto(),
                synctv_proto::common::UserStatus::Active as i32
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_system_stream_kick_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "system",
        "stream",
        "kick",
        "--room-id",
        "room-1",
        "--media-id",
        "media-1",
        "--reason",
        "manual-stop",
    ]);
    match cli.command {
        Commands::System(SystemCommand {
            command:
                SystemSubcommand::Stream(SystemStreamCommand {
                    command: SystemStreamSubcommand::Kick(args),
                }),
        }) => {
            assert_eq!(args.room_id, "room-1");
            assert_eq!(args.media_id, "media-1");
            assert_eq!(args.reason.as_deref(), Some("manual-stop"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_room_get_with_hyphen_prefixed_room_id() {
    let cli = Cli::try_parse_from(["synctv", "room", "get", "-3meH069FhrA"])
        .expect("hyphen-prefixed room ids should be accepted as positional values");
    match cli.command {
        Commands::Room(RoomCommand {
            command: RoomSubcommand::Get(args),
            ..
        }) => {
            assert_eq!(args.room_id, "-3meH069FhrA");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_provider_rtmp_get_stream_info_with_hyphen_prefixed_media_id() {
    let cli = Cli::try_parse_from([
        "synctv",
        "provider",
        "rtmp",
        "get-stream-info",
        "--room-id",
        "room-123",
        "-99tNxdXRosK",
    ])
    .expect("hyphen-prefixed media ids should be accepted as positional values");
    match cli.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Rtmp(ProviderRtmpCommand {
                    command: ProviderRtmpSubcommand::GetStreamInfo(args),
                }),
            ..
        }) => {
            assert_eq!(args.room_id, "room-123");
            assert_eq!(
                args.resolved_media_id().expect("media id should resolve"),
                "-99tNxdXRosK"
            );
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_settings_get() {
    let cli = Cli::parse_from(["synctv", "settings", "get", "roomDefaults"]);
    match cli.command {
        Commands::Settings(SettingsCommand {
            command: SettingsSubcommand::Get(args),
            ..
        }) => {
            assert_eq!(args.group, "roomDefaults");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_settings_update_with_standard_proto_json_request() {
    let request_json = r#"{"settings":{"user":{"enablePasswordSignup":true}},"updateMask":"user.enablePasswordSignup"}"#;
    let cli = Cli::parse_from([
        "synctv",
        "settings",
        "update",
        "--request-json",
        request_json,
    ]);
    match cli.command {
        Commands::Settings(SettingsCommand {
            command: SettingsSubcommand::Update(args),
            ..
        }) => assert_eq!(args.request_json.as_deref(), Some(request_json)),
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn settings_update_parser_accepts_field_mask_proto_json() {
    let request: synctv_proto::admin::UpdateSettingsRequest = parse_cli_json(
        "settings update request",
        r#"{"settings":{"email":{"smtpProxy":{"url":"socks5://proxy.example.com:1080"}}},"updateMask":"email.smtpProxy"}"#,
    )
    .expect("settings update request should parse");
    let proxy = request
        .settings
        .expect("settings")
        .email
        .expect("email")
        .smtp_proxy
        .expect("smtp proxy");
    assert_eq!(proxy.url, "socks5://proxy.example.com:1080");
    assert_eq!(
        request.update_mask.expect("update mask").paths,
        ["email.smtp_proxy"]
    );
}

#[test]
fn settings_update_parser_accepts_field_mask_clear_request() {
    let request: synctv_proto::admin::UpdateSettingsRequest = parse_cli_json(
        "settings update request",
        r#"{"settings":{"email":{}},"updateMask":"email.smtpProxy"}"#,
    )
    .expect("settings clear request should parse");
    assert!(request
        .settings
        .expect("settings")
        .email
        .expect("email")
        .smtp_proxy
        .is_none());
    assert_eq!(
        request.update_mask.expect("update mask").paths,
        ["email.smtp_proxy"]
    );
}

#[test]
fn settings_update_set_and_unset_build_standard_proto_json_request() {
    let request: synctv_proto::admin::UpdateSettingsRequest = parse_masked_settings_request(
        "settings update request",
        None,
        &[
            "server.name=Family TV".to_string(),
            "email.enabled=true".to_string(),
            "email.whitelistDomains=[\"example.com\"]".to_string(),
            "roomCreation.passwordPolicy=required".to_string(),
        ],
        &["email.smtpProxy".to_string()],
    )
    .expect("set and unset should build an update request");

    let settings = request.settings.expect("settings");
    assert_eq!(
        settings.server.expect("server").name.as_deref(),
        Some("Family TV")
    );
    let email = settings.email.expect("email");
    assert_eq!(email.enabled, Some(true));
    assert_eq!(email.whitelist_domains, ["example.com"]);
    assert_eq!(email.smtp_proxy, None);
    assert_eq!(
        settings
            .room_creation
            .expect("room creation")
            .password_policy,
        Some(synctv_proto::admin::RoomPasswordPolicy::Required as i32)
    );
    assert_eq!(
        request.update_mask.expect("update mask").paths,
        [
            "server.name",
            "email.enabled",
            "email.whitelist_domains",
            "room_creation.password_policy",
            "email.smtp_proxy"
        ]
    );
}

#[test]
fn room_settings_update_set_and_unset_build_field_mask_request() {
    let request: synctv_proto::admin::UpdateRoomSettingsRequest = parse_masked_settings_request(
        "room settings update request",
        None,
        &[
            "requireApproval=true".to_string(),
            "autoPlay.mode=shuffle".to_string(),
        ],
        &["autoPlay.delay".to_string()],
    )
    .expect("room set and unset should build an update request");

    let settings = request.settings.expect("settings");
    assert_eq!(settings.require_approval, Some(true));
    assert_eq!(
        settings.auto_play.expect("auto play").mode,
        Some(synctv_proto::client::PlayMode::Shuffle as i32)
    );
    assert_eq!(
        request.update_mask.expect("update mask").paths,
        ["require_approval", "auto_play.mode", "auto_play.delay"]
    );
}

#[test]
fn settings_update_rejects_conflicting_input_modes() {
    Cli::try_parse_from([
        "synctv",
        "settings",
        "update",
        "--set",
        "email.enabled=true",
        "--request-json",
        r#"{"settings":{"email":{"enabled":true}},"updateMask":"email.enabled"}"#,
    ])
    .expect_err("set and request-json must be mutually exclusive");
}

#[test]
fn cli_parses_system_stats() {
    let cli = Cli::parse_from(["synctv", "system", "stats"]);
    match cli.command {
        Commands::System(SystemCommand {
            command: SystemSubcommand::Stats(_args),
            ..
        }) => {}
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_status_defaults_to_connected_node() {
    let cli = Cli::parse_from(["synctv", "status"]);
    match cli.command {
        Commands::Status(args) => {
            assert!(args.node_id.is_none());
            assert!(!args.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_status_cluster_target_flags() {
    let cli = Cli::parse_from(["synctv", "status", "--node-id", "node-a"]);
    match cli.command {
        Commands::Status(args) => {
            assert_eq!(args.node_id.as_deref(), Some("node-a"));
            assert!(!args.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let cli = Cli::parse_from(["synctv", "status", "--all-nodes"]);
    match cli.command {
        Commands::Status(args) => {
            assert!(args.node_id.is_none());
            assert!(args.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_status_conflicting_cluster_target_flags() {
    let error = Cli::try_parse_from(["synctv", "status", "--node-id", "node-a", "--all-nodes"])
        .expect_err("conflicting status target flags should be rejected");
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn cli_parses_slice_cache_stats() {
    let cli = Cli::parse_from(["synctv", "slice-cache", "stats"]);
    match cli.command {
        Commands::SliceCache(SliceCacheCommand {
            command: SliceCacheSubcommand::Stats(args),
            ..
        }) => {
            assert!(args.target.node_id.is_none());
            assert!(!args.target.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_slice_cache_cluster_target_flags() {
    let cli = Cli::parse_from(["synctv", "slice-cache", "stats", "--node-id", "node-a"]);
    match cli.command {
        Commands::SliceCache(SliceCacheCommand {
            command: SliceCacheSubcommand::Stats(args),
        }) => {
            assert_eq!(args.target.node_id.as_deref(), Some("node-a"));
            assert!(!args.target.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let cli = Cli::parse_from(["synctv", "slice-cache", "purge", "--all-nodes"]);
    match cli.command {
        Commands::SliceCache(SliceCacheCommand {
            command: SliceCacheSubcommand::Purge(args),
        }) => {
            assert!(args.target.node_id.is_none());
            assert!(args.target.all_nodes);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_slice_cache_conflicting_cluster_target_flags() {
    let error = Cli::try_parse_from([
        "synctv",
        "slice-cache",
        "stats",
        "--node-id",
        "node-a",
        "--all-nodes",
    ])
    .expect_err("node-id and all-nodes must conflict");
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn cli_parses_slice_cache_maintenance_subcommands() {
    for (raw_command, expected) in [("purge", "purge"), ("evict-expired", "evict-expired")] {
        let cli = Cli::parse_from(["synctv", "slice-cache", raw_command]);
        let parsed = match cli.command {
            Commands::SliceCache(SliceCacheCommand { command }) => match command {
                SliceCacheSubcommand::Purge(_) => "purge",
                SliceCacheSubcommand::EvictExpired(_) => "evict-expired",
                SliceCacheSubcommand::Stats(_) => "stats",
            },
            other => panic!("unexpected command parsed: {other:?}"),
        };
        assert_eq!(parsed, expected);
    }
}

#[test]
fn cli_parses_system_stream_list_query_flags() {
    let cli = Cli::parse_from([
        "synctv",
        "system",
        "stream",
        "list",
        "--page",
        "3",
        "--page-size",
        "10",
        "--room-id",
        "room-1",
        "--username",
        "alice",
        "--node-id",
        "node-a",
        "--search",
        "media",
        "--sort-by",
        "user-id",
        "--sort-dir",
        "asc",
    ]);
    match cli.command {
        Commands::System(SystemCommand {
            command:
                SystemSubcommand::Stream(SystemStreamCommand {
                    command: SystemStreamSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.page, 3);
            assert_eq!(args.page_size, 10);
            assert_eq!(args.room_id.as_deref(), Some("room-1"));
            assert_eq!(args.user.username.as_deref(), Some("alice"));
            assert_eq!(args.user.user_id, None);
            assert_eq!(args.node_id.as_deref(), Some("node-a"));
            assert_eq!(args.search.as_deref(), Some("media"));
            assert!(matches!(
                args.sort_by,
                Some(CliActiveStreamSortField::UserId)
            ));
            assert!(matches!(args.sort_dir, CliSortDirection::Asc));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_system_stream_list_with_explicit_user_id_filter() {
    let cli = Cli::parse_from(["synctv", "system", "stream", "list", "--user-id", "user-1"]);
    match cli.command {
        Commands::System(SystemCommand {
            command:
                SystemSubcommand::Stream(SystemStreamCommand {
                    command: SystemStreamSubcommand::List(args),
                }),
            ..
        }) => {
            assert_eq!(args.user.username, None);
            assert_eq!(args.user.user_id.as_deref(), Some("user-1"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_accepts_room_stream_list_query_flags() {
    let result = Cli::try_parse_from([
        "synctv",
        "room",
        "stream",
        "list",
        "--room-id",
        "room-1",
        "--page",
        "2",
        "--page-size",
        "10",
        "--search",
        "beta",
        "--sort-by",
        "media-id",
        "--sort-dir",
        "desc",
    ]);

    assert!(
        result.is_ok(),
        "room stream list should accept pagination, search, and sort flags"
    );
}

#[test]
fn cli_parses_room_stream_kick_subcommand() {
    let cli = Cli::parse_from([
        "synctv",
        "room",
        "stream",
        "kick",
        "--room-id",
        "room-1",
        "--media-id",
        "media-1",
        "--reason",
        "moderation",
    ]);

    match cli.command {
        Commands::Room(RoomCommand {
            command:
                RoomSubcommand::Stream(RoomStreamCommand {
                    command: RoomStreamSubcommand::Kick(args),
                }),
        }) => {
            assert_eq!(args.room.room_id, "room-1");
            assert_eq!(args.media_id, "media-1");
            assert_eq!(args.reason.as_deref(), Some("moderation"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_rejects_room_scoped_commands_without_room_id() {
    let result = Cli::try_parse_from(["synctv", "playlist", "list"]);
    assert!(
        result.is_err(),
        "room-scoped commands must require --room-id"
    );
}

#[test]
fn version_string_contains_package_name_and_version() {
    let version = version_string();
    assert!(version.contains(env!("CARGO_PKG_NAME")));
    assert!(version.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn rendered_config_redacts_secrets_and_masks_connection_urls() {
    let rendered = render_config_for_display(&sample_config())
        .expect("sample config should serialize for display");
    let rendered_text = serde_json::to_string(&rendered).expect("rendered config should serialize");

    for secret in [
        "super-secret-db",
        "redis-secret",
        "jwt-secret-123456789012345678901234",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "6666666666666666666666666666666666666666666666666666666666666666",
        "proxy-signing-secret-value-1234567890",
        "media-swarm-signing-secret-value-1234567890",
        "provider-session-secret-value-1234567890",
        "login-discovery-secret-value-1234567890",
        "webauthn-enumeration-secret-value-1234567890",
        "file-upload-token-secret-value-1234567890",
        "opaque-server-setup-secret-123456789012345678901234",
        "cluster-secret-value",
        "management-auth-token",
        "metrics-bearer-token",
        "metrics-basic-password",
        "RootPass12345",
        "oauth-client-secret",
    ] {
        assert!(
            !rendered_text.contains(secret),
            "rendered config leaked secret {secret}: {rendered_text}"
        );
    }

    assert!(
        rendered_text.contains("***"),
        "rendered config should contain redaction markers: {rendered_text}"
    );
    assert!(
        rendered_text.contains("db.internal:5432/synctv"),
        "rendered config should retain non-secret db address context: {rendered_text}"
    );
}

#[test]
fn config_redaction_covers_object_storage_credentials() {
    let mut value = serde_json::json!({
        "backend": {
            "access_key_id": "storage-access-key",
            "secret_access_key": "storage-secret-key"
        }
    });

    redact_config_value(&mut value);

    assert_eq!(value["backend"]["access_key_id"], "<redacted>");
    assert_eq!(value["backend"]["secret_access_key"], "<redacted>");
}

#[test]
fn config_json_output_uses_lower_camel_case_keys() {
    let rendered =
        config_json_for_display(&sample_config()).expect("sample config should render as JSON");
    let rendered_text = serde_json::to_string(&rendered).expect("rendered config should serialize");

    assert_eq!(rendered["management"]["authToken"], "<redacted>");
    assert_eq!(rendered["metrics"]["auth"]["bearerToken"], "<redacted>");
    assert_eq!(
        rendered["security"]["emailOutboxEncryptionKey"],
        "<redacted>"
    );
    assert_eq!(
        rendered["security"]["opaqueServerSetupSecret"],
        "<redacted>"
    );
    assert_eq!(rendered["security"]["mediaSwarmSigningKey"], "<redacted>");
    let database_url = rendered["database"]["url"]
        .as_str()
        .expect("database url should render as a string");
    assert!(database_url.contains("***"));
    assert!(database_url.contains("db.internal:5432/synctv"));
    assert!(
        rendered.get("public_ids").is_none(),
        "config JSON output should expose lowerCamelCase keys: {rendered_text}"
    );
    assert!(
        rendered.get("publicIds").is_none(),
        "config JSON output should keep public id config outside AppConfig display: {rendered_text}"
    );
    assert!(
        rendered.get("externalIds").is_none(),
        "config JSON output should keep external id config outside AppConfig display: {rendered_text}"
    );
}

#[test]
fn database_status_summary_masks_credentials() {
    let output = DatabaseCliOutput::status(
        &sample_config(),
        &crate::migrations::EmbeddedMigrationsStatus::Ready,
    );
    let summary = database_summary(&output);

    assert!(
        !summary.contains("super-secret-db"),
        "database status summary leaked password: {summary}"
    );
    assert!(
        summary.contains("Migration status: ready"),
        "database status summary should report migration readiness: {summary}"
    );
    assert!(
        summary.contains("postgresql://***:***@db.internal:5432/synctv"),
        "database status summary should print masked url: {summary}"
    );
}

#[test]
fn database_status_summary_reports_broken_migration_history() {
    let output = DatabaseCliOutput::status(
        &sample_config(),
        &crate::migrations::EmbeddedMigrationsStatus::Broken(
            crate::migrations::MigrationHistoryIssue::Dirty(20_260_426_004),
        ),
    );
    let summary = database_summary(&output);

    assert!(
        summary.contains("Migration status: broken"),
        "database status summary should flag broken migration history: {summary}"
    );
    assert!(
        summary.contains("Migration detail: migration 20260426004 is marked dirty"),
        "database status summary should explain the broken migration detail: {summary}"
    );
}

#[test]
fn render_human_output_converts_user_timestamps_role_and_status() {
    let _lock = acquire_time_test_lock();
    let _timezone = TimeZoneGuard::set("UTC");
    let rendered = render_human_output(&synctv_proto::admin::AdminUser {
        id: "I9jXL5s61FPV".into(),
        username: "root".into(),
        email: String::new(),
        role: synctv_proto::common::UserRole::Root as i32,
        status: synctv_proto::common::UserStatus::Banned as i32,
        created_at: 1_775_144_583_i64,
        updated_at: 1_775_291_071_i64,
        is_banned: true,
        banned_at: 1_775_291_071_i64,
        banned_by: "admin-1".into(),
        banned_reason: "test".into(),
        avatar_url: String::new(),
        presence: None,
    })
    .expect("human output should render");

    assert_eq!(rendered["role"], "root");
    assert_eq!(rendered["status"], "banned");
    assert_eq!(
        rendered["createdAt"],
        "2026-04-02 15:43:03 +00:00 (UTC) (1775144583)"
    );
    assert_eq!(
        rendered["updatedAt"],
        "2026-04-04 08:24:31 +00:00 (UTC) (1775291071)"
    );
}

#[test]
fn render_human_output_uses_room_and_member_enums_by_context() {
    let _lock = acquire_time_test_lock();
    let _timezone = TimeZoneGuard::set("UTC");
    let rendered = render_human_output(&synctv_proto::client::JoinRoomResponse {
        room: Some(synctv_proto::client::Room {
            id: "room-1".into(),
            name: "room".into(),
            created_by: "owner-1".into(),
            status: synctv_proto::common::RoomStatus::Closed as i32,
            settings: Some(synctv_proto::client::RoomSettings {
                chat_enabled: true,
                ..Default::default()
            }),
            created_at: 1_775_144_583_i64,
            member_count: 1,
            description: String::new(),
            updated_at: 1_775_291_071_i64,
            is_banned: false,
            availability: synctv_proto::client::ResourceAvailability::CreatorInactive as i32,
            version: 78,
            cover: None,
            presence: None,
            creator: None,
            category: None,
            labels: Vec::new(),
        }),
        playback_state: None,
        membership_status: synctv_proto::common::MemberStatus::Active as i32,
        requires_approval: false,
        members: vec![synctv_proto::common::RoomMember {
            room_id: "room-1".into(),
            user_id: "user-1".into(),
            username: "root".into(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_proto::common::RoomMemberRole::Creator as i32,
            permissions: RoomAdminPermissionBits::SEND_CHAT_MESSAGES
                | RoomAdminPermissionBits::BROWSE_LIBRARY,
            added_permissions: RoomMemberPermissionBits::MANAGE_OWN_MEDIA,
            removed_permissions: RoomMemberPermissionBits::SEND_CHAT_MESSAGES,
            admin_added_permissions: RoomAdminPermissionBits::REMOVE_MEMBERS,
            admin_removed_permissions: RoomAdminPermissionBits::REMOVE_MEMBERS,
            joined_at: 1_775_291_657_i64,
            is_online: true,
            connection_count: 1,
        }],
    })
    .expect("human output should render");
    let instances = render_human_output(
        &synctv_proto::providers::common::ListProviderInstancesResponse {
            instances: vec![synctv_proto::providers::common::ProviderInstance {
                name: "provider-1".into(),
                endpoint: "http://127.0.0.1:50052".into(),
                comment: String::new(),
                timeout_seconds: 30,
                tls: false,
                insecure_tls: false,
                providers: vec![synctv_proto::source_config::SourceProvider::DirectUrl as i32],
                enabled: true,
                status: synctv_proto::providers::common::ProviderInstanceStatus::Disconnected
                    as i32,
                created_at: 1_775_144_583_i64,
                updated_at: 1_775_291_071_i64,
            }],
            total: 1,
        },
    )
    .expect("human output should render");

    assert_eq!(rendered["room"]["status"], "closed");
    assert_eq!(rendered["room"]["availability"], "creatorInactive");
    assert_eq!(rendered["members"][0]["role"], "creator");
    assert_eq!(
        rendered["members"][0]["permissionNames"],
        string_values(&["send_chat_messages", "browse_library"])
    );
    assert_eq!(
        rendered["members"][0]["addedPermissionNames"],
        string_values(&["manage_own_media"])
    );
    assert_eq!(
        rendered["members"][0]["removedPermissionNames"],
        string_values(&["send_chat_messages"])
    );
    assert_eq!(
        rendered["members"][0]["adminAddedPermissionNames"],
        string_values(&["remove_members"])
    );
    assert_eq!(
        rendered["members"][0]["adminRemovedPermissionNames"],
        string_values(&["remove_members"])
    );
    assert_eq!(
        rendered["members"][0]["joinedAt"],
        "2026-04-04 08:34:17 +00:00 (UTC) (1775291657)"
    );
    assert_eq!(instances["instances"][0]["status"], "disconnected");
}

#[test]
fn render_human_output_converts_room_listing_without_context_inference() {
    let _lock = acquire_time_test_lock();
    let _timezone = TimeZoneGuard::set("UTC");
    let rendered = render_human_output(&synctv_proto::admin::ListRoomsResponse {
        rooms: vec![synctv_proto::admin::Room {
            id: "room-1".into(),
            name: "General".into(),
            creator_id: "user-1".into(),
            creator_username: "root".into(),
            creator_status: synctv_proto::common::UserStatus::Banned as i32,
            status: synctv_proto::common::RoomStatus::Active as i32,
            settings: Some(synctv_proto::client::RoomSettings {
                allow_guest_join: true,
                ..Default::default()
            }),
            member_count: 3,
            created_at: 1_775_144_583_i64,
            updated_at: 1_775_291_071_i64,
            description: "main room".into(),
            is_banned: false,
            version: 56,
            creator_avatar_url: String::new(),
            presence: None,
            cover: None,
            category: None,
            labels: Vec::new(),
        }],
        total: 1,
    })
    .expect("human output should render");

    assert_eq!(rendered["rooms"][0]["status"], "active");
    assert_eq!(rendered["rooms"][0]["creatorStatus"], "banned");
    assert_eq!(rendered["rooms"][0]["version"], 56);
    assert!(
        rendered["rooms"][0]["createdAt"]
            .as_str()
            .is_some_and(|value| value.ends_with("(1775144583)")),
        "createdAt should be humanized while preserving the source epoch"
    );
}

#[test]
fn render_human_output_includes_media_and_playlist_availability() {
    let rendered_media = render_human_output(&synctv_proto::client::Media {
        id: "media-1".into(),
        room_id: "room-1".into(),
        source_provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
        name: "Example".into(),
        metadata: Some(synctv_proto::client::ResourceMetadata {
            source: Some("direct_url".to_string()),
        }),
        position: 1.0,
        added_at: 1_775_291_657_i64,
        creator_id: "user-1".into(),
        provider_instance_name: "default".into(),
        source_config: direct_url_media_source_config("https://example.com"),
        availability: synctv_proto::client::ResourceAvailability::CreatorInactive as i32,
        version: 12,
        cover: None,
        description: String::new(),
        thumbnail: None,
    })
    .expect("media human output should render");
    let rendered_playlist = render_human_output(&synctv_proto::client::Playlist {
        id: "playlist-1".into(),
        room_id: "room-1".into(),
        name: "Default".into(),
        parent_id: String::new(),
        position: 1.0,
        is_dynamic: false,
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        item_count: 1,
        creator_id: "user-1".into(),
        created_at: 1_775_144_583_i64,
        updated_at: 1_775_291_071_i64,
        availability: synctv_proto::client::ResourceAvailability::Available as i32,
        version: 34,
        source_config: alist_playlist_source_config("/shows"),
        description: String::new(),
        cover: None,
    })
    .expect("playlist human output should render");

    assert_eq!(rendered_media["availability"], "creatorInactive");
    assert_eq!(rendered_playlist["availability"], "available");
    assert_eq!(rendered_media["version"], 12);
    assert_eq!(rendered_playlist["version"], 34);
    assert_eq!(
        rendered_playlist["sourceConfig"],
        serde_json::to_value(alist_playlist_source_config("/shows"))
            .expect("source config should serialize")
    );
}

#[test]
fn render_human_output_includes_playlist_items_snapshot_version() {
    let rendered = render_human_output(&synctv_proto::client::ListPlaylistItemsResponse {
        playlists: vec![synctv_proto::client::Playlist {
            id: "playlist-1".into(),
            room_id: "room-1".into(),
            name: "Folder".into(),
            parent_id: String::new(),
            position: 1.0,
            is_dynamic: false,
            source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            item_count: 0,
            creator_id: "user-1".into(),
            created_at: 1,
            updated_at: 2,
            availability: synctv_proto::client::ResourceAvailability::Available as i32,
            version: 10,
            source_config: None,
            description: String::new(),
            cover: None,
        }],
        media: vec![synctv_proto::client::Media {
            id: "media-1".into(),
            room_id: "room-1".into(),
            source_provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
            name: "Example".into(),
            metadata: Some(synctv_proto::client::ResourceMetadata {
                source: Some("direct_url".to_string()),
            }),
            position: 1.0,
            added_at: 3,
            creator_id: "user-1".into(),
            provider_instance_name: "default".into(),
            source_config: direct_url_media_source_config("https://example.com"),
            availability: synctv_proto::client::ResourceAvailability::Available as i32,
            version: 11,
            cover: None,
            description: String::new(),
            thumbnail: None,
        }],
        total: Some(2),
        folder_count: 1,
        file_count: 1,
        dynamic_items: Vec::new(),
        current_path: Vec::new(),
        version: "items-v42".into(),
        pagination: None,
    })
    .expect("playlist items human output should render");

    assert_eq!(rendered["version"], "items-v42");
    assert_eq!(rendered["playlists"][0]["version"], 10);
    assert_eq!(rendered["media"][0]["version"], 11);
}

#[test]
fn render_human_output_includes_room_members_snapshot_version() {
    let rendered = render_human_output(&synctv_proto::client::GetRoomMembersResponse {
        members: vec![synctv_proto::common::RoomMember {
            room_id: "room-1".into(),
            user_id: "user-1".into(),
            username: "alice".into(),
            remark_name: "Alice".into(),
            display_tag: "mod".into(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            permissions: 7,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: 123,
            is_online: true,
            connection_count: 1,
        }],
        total: 1,
        version: "members-v7".into(),
        presence: None,
    })
    .expect("room members human output should render");

    assert_eq!(rendered["version"], "members-v7");
    assert_eq!(rendered["total"], 1);
    assert_eq!(rendered["members"][0]["username"], "alice");
    assert_eq!(rendered["members"][0]["remarkName"], "Alice");
    assert_eq!(rendered["members"][0]["displayTag"], "mod");
}

#[test]
fn render_human_output_uses_camel_case_for_slice_cache_responses() {
    let stats = admin_proto::SliceCacheStatsNode {
        node_id: "node-a".into(),
        config: Some(admin_proto::SliceCacheConfigInfo {
            engine_enabled: true,
            backend: "file".into(),
            file_cache_dir: "/tmp/synctv-slices".into(),
            slice_size: 1024,
            max_cache_size: 4096,
            segment_ttl_secs: 30,
            stale_max_age_secs: 60,
            stale_while_revalidate: true,
            eviction_interval_secs: 10,
            watermark_ratio: 0.8,
        }),
        current_size_bytes: 2048,
        entry_count: 3,
        metadata_entries: 4,
        updating_entries: 5,
        lock_count: 6,
        usage_ratio: 0.5,
    };
    let rendered_stats = render_human_output(&admin_proto::GetSliceCacheStatsResponse {
        nodes: vec![stats.clone()],
        failures: vec![admin_proto::SliceCacheNodeFailure {
            node_id: "node-b".into(),
            error: "offline".into(),
        }],
    })
    .expect("slice cache stats output should render");
    let rendered_purge = render_human_output(&admin_proto::PurgeSliceCacheResponse {
        success: true,
        removed_entries: 7,
        freed_bytes: 8192,
        stats: Some(stats.clone()),
        nodes: vec![admin_proto::PurgeSliceCacheNodeResult {
            node_id: "node-a".into(),
            success: true,
            removed_entries: 7,
            freed_bytes: 8192,
            stats: Some(stats.clone()),
        }],
        failures: Vec::new(),
    })
    .expect("slice cache purge output should render");
    let rendered_evict = render_human_output(&admin_proto::EvictExpiredSliceCacheResponse {
        success: true,
        removed_expired_entries: 2,
        stats: Some(stats),
        nodes: vec![admin_proto::EvictExpiredSliceCacheNodeResult {
            node_id: "node-a".into(),
            success: true,
            removed_expired_entries: 2,
            stats: None,
        }],
        failures: Vec::new(),
    })
    .expect("slice cache evict output should render");

    assert_eq!(rendered_stats["nodes"][0]["nodeId"], "node-a");
    assert_eq!(rendered_stats["nodes"][0]["currentSizeBytes"], 2048);
    assert_eq!(rendered_stats["nodes"][0]["config"]["engineEnabled"], true);
    assert_eq!(
        rendered_stats["nodes"][0]["config"]["fileCacheDir"],
        "/tmp/synctv-slices"
    );
    assert_eq!(rendered_stats["failures"][0]["nodeId"], "node-b");
    assert!(rendered_stats["nodes"][0].get("node_id").is_none());
    assert!(rendered_stats["nodes"][0]["config"]
        .get("engine_enabled")
        .is_none());
    assert_eq!(rendered_purge["removedEntries"], 7);
    assert_eq!(rendered_purge["freedBytes"], 8192);
    assert!(rendered_purge.get("removed_entries").is_none());
    assert_eq!(rendered_evict["removedExpiredEntries"], 2);
    assert_eq!(rendered_evict["nodes"][0]["removedExpiredEntries"], 2);
    assert!(rendered_evict.get("removed_expired_entries").is_none());
}

#[test]
fn render_human_output_uses_proto_json_for_admin_settings() {
    let rendered_room = render_human_output(&synctv_proto::admin::RuntimeSettings {
        room_creation: Some(synctv_proto::admin::RoomCreationSettings {
            enabled: true,
            approval_required: false,
            password_policy: synctv_proto::admin::RoomPasswordPolicy::Required as i32,
            max_rooms_per_user: 10,
        }),
        ..Default::default()
    })
    .expect("room settings output should render");
    let rendered_oauth2 = render_human_output(&synctv_proto::admin::RuntimeSettings {
        oauth2: Some(synctv_proto::admin::OAuth2Settings {
            allowed_redirect_urls: vec!["https://syncs.tv/oauth2/callback".into()],
            providers: vec![synctv_proto::admin::OAuth2ProviderSettings {
                name: "github-main".into(),
                enable_signup: true,
                signup_need_review: true,
                config: Some(
                    synctv_proto::admin::o_auth2_provider_settings::Config::Github(
                        synctv_proto::admin::OAuth2GithubProviderConfig {
                            client_id: "client-id".into(),
                            client_secret: "client-secret".into(),
                            redirect_url: "https://example.com/callback".into(),
                        },
                    ),
                ),
            }],
        }),
        ..Default::default()
    })
    .expect("oauth2 settings output should render");

    assert_eq!(rendered_room["roomCreation"]["enabled"], true);
    assert_eq!(rendered_room["roomCreation"]["passwordPolicy"], 2);
    assert!(rendered_room["roomCreation"]
        .get("password_policy")
        .is_none());
    assert_eq!(
        rendered_oauth2["oauth2"]["providers"][0]["name"],
        "github-main"
    );
    assert_eq!(
        rendered_oauth2["oauth2"]["allowedRedirectUrls"][0],
        "https://syncs.tv/oauth2/callback"
    );
    assert_eq!(
        rendered_oauth2["oauth2"]["providers"][0]["github"]["clientId"],
        "client-id"
    );
}

#[test]
fn build_get_playback_cli_output_omits_absolute_urls_for_explicit_endpoint_mode() {
    let output = build_get_playback_cli_output(
        synctv_proto::client::GetPlaybackResponse {
            playback_state: None,
            playback: Some(synctv_proto::client::Playback {
                media_id: "media-1".into(),
                playlist_id: String::new(),
                room_id: "room-1".into(),
                name: "Example".into(),
                playlist_position: 1.0,
                provider: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                provider_instance_name: String::new(),
                playback_infos: std::collections::HashMap::from([(
                    "direct".to_string(),
                    synctv_proto::client::PlaybackInfo {
                        thumbnail: None,
                        medias: vec![synctv_proto::client::PlaybackMedia {
                            name: String::new(),
                            url: "/api/playback-providers/direct-url/abc/streams/direct/0".into(),
                            headers: std::collections::HashMap::new(),
                            format: "mp4".into(),
                            expire_at: None,
                            metadata: None,
                            p2p_delivery: None,
                        }],
                        default_media_index: Some(0),
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                    },
                )]),
                default_mode: "direct".into(),
                metadata: None,
                expires_at: None,
                duration_seconds: None,
                is_live: false,
                target: None,
            }),
        },
        &GlobalConfigArgs {
            endpoint: Some("http://127.0.0.1:50339".into()),
            ..GlobalConfigArgs::default()
        },
    );

    assert_eq!(
        output.default_pull_url.as_deref(),
        Some("/api/playback-providers/direct-url/abc/streams/direct/0")
    );
    assert_eq!(output.default_absolute_pull_url, None);
    assert_eq!(output.pull_urls.len(), 1);
    assert_eq!(output.pull_urls[0].absolute_url, None);
}

#[test]
fn build_get_playback_cli_output_prefers_default_hls_media() {
    let media = |url: &str| synctv_proto::client::PlaybackMedia {
        name: String::new(),
        url: url.into(),
        headers: std::collections::HashMap::new(),
        format: "m3u8".into(),
        expire_at: None,
        metadata: None,
        p2p_delivery: None,
    };
    let output = build_get_playback_cli_output(
        synctv_proto::client::GetPlaybackResponse {
            playback_state: None,
            playback: Some(synctv_proto::client::Playback {
                media_id: "media-1".into(),
                playlist_id: String::new(),
                room_id: "room-1".into(),
                name: "CCTV example".into(),
                playlist_position: 1.0,
                provider: synctv_proto::source_config::SourceProvider::Cctv as i32,
                provider_instance_name: String::new(),
                playback_infos: std::collections::HashMap::from([
                    (
                        "audio_audio".to_string(),
                        synctv_proto::client::PlaybackInfo {
                            thumbnail: None,
                            medias: vec![media("/resources/audio_audio/0")],
                            default_media_index: Some(0),
                            subtitles: Vec::new(),
                            default_subtitle_index: None,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        },
                    ),
                    (
                        "hls_hls".to_string(),
                        synctv_proto::client::PlaybackInfo {
                            thumbnail: None,
                            medias: vec![media("/resources/hls_hls/0")],
                            default_media_index: Some(0),
                            subtitles: Vec::new(),
                            default_subtitle_index: None,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        },
                    ),
                ]),
                default_mode: "hls_hls".into(),
                metadata: None,
                expires_at: None,
                duration_seconds: None,
                is_live: false,
                target: None,
            }),
        },
        &GlobalConfigArgs {
            endpoint: Some("http://127.0.0.1:50339".into()),
            ..GlobalConfigArgs::default()
        },
    );

    assert_eq!(output.hls_pull_url.as_deref(), Some("/resources/hls_hls/0"));
    assert_eq!(output.hls_pull_url, output.default_pull_url);
}

#[test]
fn build_get_playback_cli_output_uses_the_first_media_when_default_index_is_omitted() {
    let output = build_get_playback_cli_output(
        synctv_proto::client::GetPlaybackResponse {
            playback_state: None,
            playback: Some(synctv_proto::client::Playback {
                playback_infos: HashMap::from([(
                    "direct".to_string(),
                    synctv_proto::client::PlaybackInfo {
                        medias: vec![
                            synctv_proto::client::PlaybackMedia {
                                url: "https://media.example/first.mp4".to_string(),
                                format: "mp4".to_string(),
                                ..Default::default()
                            },
                            synctv_proto::client::PlaybackMedia {
                                url: "https://media.example/second.mp4".to_string(),
                                format: "mp4".to_string(),
                                ..Default::default()
                            },
                        ],
                        default_media_index: None,
                        ..Default::default()
                    },
                )]),
                default_mode: "direct".to_string(),
                ..Default::default()
            }),
        },
        &GlobalConfigArgs::default(),
    );

    assert!(output.pull_urls[0].default);
    assert!(!output.pull_urls[1].default);
    assert_eq!(
        output.default_pull_url.as_deref(),
        Some("https://media.example/first.mp4")
    );
}

#[test]
fn resolve_remote_endpoint_returns_none_when_cli_endpoint_is_absent() {
    let _env_lock = acquire_env_test_lock();
    let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_ENDPOINT");
    let endpoint = resolve_remote_endpoint(&GlobalConfigArgs::default());

    assert_eq!(endpoint, None);
}

#[test]
fn resolve_remote_endpoint_preserves_explicit_unix_socket_endpoint() {
    let _env_lock = acquire_env_test_lock();
    let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_ENDPOINT");
    let raw = format!("unix://{}", default_management_unix_socket_path().display());
    let endpoint = resolve_remote_endpoint(&GlobalConfigArgs {
        endpoint: Some(raw.clone()),
        ..GlobalConfigArgs::default()
    });

    assert_eq!(endpoint.as_deref(), Some(raw.as_str()));
}

#[test]
fn resolve_remote_endpoint_preserves_explicit_tcp_endpoint() {
    let _env_lock = acquire_env_test_lock();
    let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_ENDPOINT");
    let endpoint = resolve_remote_endpoint(&GlobalConfigArgs {
        endpoint: Some("http://192.0.2.10:50099".to_string()),
        ..GlobalConfigArgs::default()
    });

    assert_eq!(endpoint.as_deref(), Some("http://192.0.2.10:50099"));
}

#[test]
fn resolve_remote_endpoint_reads_environment_without_clap_env_feature() {
    let _env_lock = acquire_env_test_lock();
    let _env_guard = EnvVarGuard::set("SYNCTV_MANAGEMENT_ENDPOINT", "http://127.0.0.1:59052");
    let endpoint = resolve_remote_endpoint(&GlobalConfigArgs::default());

    assert_eq!(endpoint.as_deref(), Some("http://127.0.0.1:59052"));
}

#[test]
fn resolve_remote_endpoint_prefers_cli_over_environment() {
    let _env_lock = acquire_env_test_lock();
    let _env_guard = EnvVarGuard::set("SYNCTV_MANAGEMENT_ENDPOINT", "http://127.0.0.1:59052");
    let endpoint = resolve_remote_endpoint(&GlobalConfigArgs {
        endpoint: Some("http://127.0.0.1:59053".to_string()),
        ..GlobalConfigArgs::default()
    });

    assert_eq!(endpoint.as_deref(), Some("http://127.0.0.1:59053"));
}

#[test]
fn remote_cli_context_caches_config_derived_management_endpoint() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = dir.path().join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
time:
  timezone: "Asia/Shanghai"
management:
  transport: "tcp"
  port: 50123
"#,
    )
    .expect("config should be written");

    let args = RemoteAccessArgs {
        global: GlobalConfigArgs {
            config: Some(config_path.clone()),
            data_dir: None,
            no_dotenv: true,
            verbose: 0,
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
        },
        output: RemoteOutputFormat::Human,
    };
    let context = RemoteCliContext::new(&args);

    let first = context
        .resolved_config_endpoint()
        .expect("initial endpoint should resolve");
    assert_eq!(first.as_deref(), Some("http://127.0.0.1:50123"));

    std::fs::write(
        &config_path,
        r#"
management:
  transport: "tcp"
  port: 50124
"#,
    )
    .expect("config should be rewritten");

    let second = context
        .resolved_config_endpoint()
        .expect("cached endpoint should still resolve");
    assert_eq!(second.as_deref(), Some("http://127.0.0.1:50123"));
}

#[test]
fn remote_cli_context_with_explicit_endpoint_does_not_require_local_config() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let missing_config_path = dir.path().join("missing-synctv.yaml");
    let args = RemoteAccessArgs {
        global: GlobalConfigArgs {
            config: Some(missing_config_path),
            data_dir: None,
            no_dotenv: true,
            verbose: 0,
            endpoint: Some("http://127.0.0.1:50052".to_string()),
            auth_token: None,
            auth_token_file: None,
        },
        output: RemoteOutputFormat::Human,
    };
    let context = RemoteCliContext::new(&args);

    context
        .initialize_output_state()
        .expect("explicit endpoint mode should not require loading local config");
    assert_eq!(
        context
            .resolved_config_endpoint()
            .expect("explicit endpoint mode should not resolve a config endpoint"),
        None
    );
}

#[test]
fn write_completion_output_ignores_broken_pipe() {
    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = BrokenPipeWriter;
    write_completion_output(&mut writer, b"synctv completion")
        .expect("broken pipe should be treated as a clean completion exit");
}

#[tokio::test]
async fn management_unary_response_times_out_when_rpc_never_returns() {
    let result = management_unary_response_with_timeout::<()>(
        "test unary timeout",
        Duration::from_millis(10),
        std::future::pending::<std::result::Result<tonic::Response<()>, tonic::Status>>(),
    )
    .await;

    let error = result.expect_err("pending management RPC must time out");
    assert!(
        error.to_string().contains("timed out"),
        "timeout error should mention timeout: {error}"
    );
}

#[tokio::test]
async fn management_stream_item_times_out_when_stop_stream_stalls() {
    let result = management_stream_item::<management_proto::StopServerEvent>(
        "test stop stream timeout",
        Duration::from_millis(10),
        std::future::pending::<
            std::result::Result<Option<management_proto::StopServerEvent>, tonic::Status>,
        >(),
    )
    .await;

    let error = result.expect_err("pending management stream item must time out");
    assert!(
        error.to_string().contains("timed out"),
        "timeout error should mention timeout: {error}"
    );
}

#[test]
fn format_management_status_error_makes_permission_denied_human_readable() {
    let error = format_management_status_error(
        "create room",
        &tonic::Status::permission_denied("actor user 'root' is banned"),
    );

    assert_eq!(
        error.to_string(),
        "management create room failed: permission denied: actor user 'root' is banned"
    );
}

#[test]
fn format_management_status_error_makes_invalid_argument_human_readable() {
    let error = format_management_status_error(
        "reorder media",
        &tonic::Status::invalid_argument("position must be an integer"),
    );

    assert_eq!(
        error.to_string(),
        "management reorder media failed: invalid request: position must be an integer"
    );
}

#[test]
fn format_management_status_error_hides_internal_details() {
    let error = format_management_status_error(
        "kick active stream",
        &tonic::Status::internal("redis://user:secret@localhost:6379 failed"),
    );

    assert_eq!(
        error.to_string(),
        "management kick active stream failed: internal error"
    );
}

#[test]
fn format_management_status_error_keeps_service_unavailable_context() {
    let error = format_management_status_error(
        "list active streams",
        &tonic::Status::unavailable("live streaming backend unavailable"),
    );

    assert_eq!(
            error.to_string(),
            "management list active streams failed: service unavailable: live streaming backend unavailable"
        );
}

#[test]
fn stop_stream_disconnect_is_treated_as_success_after_finalizing() {
    let error = anyhow::anyhow!(
            "code: 'Unknown error', message: \"h2 protocol error: error reading a body from connection\""
        );

    assert!(stop_stream_disconnect_can_be_treated_as_success(
        Some(management_proto::StopServerStage::Finalizing),
        &error
    ));
    assert!(!stop_stream_disconnect_can_be_treated_as_success(
        Some(management_proto::StopServerStage::ConnectionDraining),
        &error
    ));
}

#[test]
fn stop_stream_end_is_only_treated_as_success_after_finalizing() {
    assert!(stop_stream_end_can_be_treated_as_success(Some(
        management_proto::StopServerStage::Finalizing
    )));
    assert!(!stop_stream_end_can_be_treated_as_success(Some(
        management_proto::StopServerStage::RuntimeDraining
    )));
    assert!(!stop_stream_end_can_be_treated_as_success(None));
}

#[test]
fn synthesize_stop_completion_appends_terminal_completed_event_after_finalizing() {
    let mut last_stage = Some(management_proto::StopServerStage::Finalizing);
    let mut saw_terminal = false;
    let mut events = vec![StopServerEventOutput {
        stage: management_proto::StopServerStage::Finalizing as i32,
        message: "final shutdown tasks in progress".to_string(),
        terminal: true,
    }];

    synthesize_stop_completion_if_needed(
        RemoteOutputFormat::Json,
        &mut last_stage,
        &mut saw_terminal,
        &mut events,
    );

    assert_eq!(
        last_stage,
        Some(management_proto::StopServerStage::Completed)
    );
    assert!(saw_terminal);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].message, "shutdown complete");
    assert!(events[1].terminal);
}

#[test]
fn synthesize_stop_completion_is_noop_when_not_finalizing() {
    let mut last_stage = Some(management_proto::StopServerStage::ConnectionDraining);
    let mut saw_terminal = false;
    let mut events = Vec::new();

    synthesize_stop_completion_if_needed(
        RemoteOutputFormat::Json,
        &mut last_stage,
        &mut saw_terminal,
        &mut events,
    );

    assert_eq!(
        last_stage,
        Some(management_proto::StopServerStage::ConnectionDraining)
    );
    assert!(!saw_terminal);
    assert!(events.is_empty());
}

#[test]
fn print_stop_output_json_is_machine_readable() {
    let output = StopServerOutput {
        success: true,
        terminal_received: false,
        final_stage: Some(management_proto::StopServerStage::Finalizing as i32),
        events: vec![StopServerEventOutput {
            stage: management_proto::StopServerStage::RuntimeDraining as i32,
            message: "runtime draining".to_string(),
            terminal: false,
        }],
    };

    let rendered = serde_json::to_value(&output).expect("stop output should serialize");
    assert_eq!(rendered["success"], true);
    assert_eq!(rendered["terminalReceived"], false);
    assert_eq!(
        rendered["finalStage"],
        management_proto::StopServerStage::Finalizing as i32
    );
    assert_eq!(
        rendered["events"][0]["stage"],
        management_proto::StopServerStage::RuntimeDraining as i32
    );
    assert_eq!(rendered["events"][0]["message"], "runtime draining");
}

#[test]
fn cli_parses_douyin_provider_commands() {
    let bind = Cli::parse_from([
        "synctv",
        "provider",
        "douyin",
        "bind",
        "--username",
        "alice",
        "--label",
        "main",
        "--cookie",
        "sessionid=secret",
        "--instance-name",
        "douyin-edge",
    ]);
    match bind.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Douyin(ProviderDouyinCommand {
                    command: ProviderDouyinSubcommand::Bind(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.label, "main");
            assert_eq!(args.cookie, "sessionid=secret");
            assert_eq!(args.instance.instance_name.as_deref(), Some("douyin-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let posts = Cli::parse_from([
        "synctv",
        "provider",
        "douyin",
        "posts",
        "--user-id",
        "user-1",
        "--sec-uid",
        "MS4wLjABAAAAexample",
        "--cursor",
        "123456",
        "--page-size",
        "30",
    ]);
    match posts.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Douyin(ProviderDouyinCommand {
                    command: ProviderDouyinSubcommand::Posts(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.cursor.as_deref(), Some("123456"));
            assert_eq!(args.page_size, 30);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_tiktok_provider_commands() {
    let bind = Cli::parse_from([
        "synctv",
        "provider",
        "tiktok",
        "bind",
        "--username",
        "alice",
        "--label",
        "main",
        "--cookie",
        "sessionid=secret",
        "--instance-name",
        "tiktok-edge",
    ]);
    match bind.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Tiktok(ProviderTikTokCommand {
                    command: ProviderTikTokSubcommand::Bind(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.label, "main");
            assert_eq!(args.cookie, "sessionid=secret");
            assert_eq!(args.instance.instance_name.as_deref(), Some("tiktok-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let user = Cli::parse_from([
        "synctv",
        "provider",
        "tiktok",
        "user",
        "creator_name",
        "--user-id",
        "user-1",
    ]);
    match user.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Tiktok(ProviderTikTokCommand {
                    command: ProviderTikTokSubcommand::User(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.unique_id, "creator_name");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let posts = Cli::parse_from([
        "synctv",
        "provider",
        "tiktok",
        "posts",
        "--user-id",
        "user-1",
        "--sec-uid",
        "MS4wLjABAAAAexample",
        "--cursor",
        "1712345678",
        "--page-size",
        "35",
    ]);
    match posts.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Tiktok(ProviderTikTokCommand {
                    command: ProviderTikTokSubcommand::Posts(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.sec_uid, "MS4wLjABAAAAexample");
            assert_eq!(args.cursor.as_deref(), Some("1712345678"));
            assert_eq!(args.page_size, 35);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parses_twitch_provider_commands() {
    let bind = Cli::parse_from([
        "synctv",
        "provider",
        "twitch",
        "bind",
        "--username",
        "alice",
        "--oauth-token",
        "oauth-secret",
        "--device-id",
        "device-1",
        "--client-integrity",
        "integrity-secret",
        "--instance-name",
        "twitch-edge",
    ]);
    match bind.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Twitch(ProviderTwitchCommand {
                    command: ProviderTwitchSubcommand::Bind(args),
                }),
        }) => {
            assert_eq!(args.access.actor.username.as_deref(), Some("alice"));
            assert_eq!(args.oauth_token, "oauth-secret");
            assert_eq!(args.device_id.as_deref(), Some("device-1"));
            assert_eq!(args.client_integrity.as_deref(), Some("integrity-secret"));
            assert_eq!(args.instance.instance_name.as_deref(), Some("twitch-edge"));
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let resolve = Cli::parse_from([
        "synctv",
        "provider",
        "twitch",
        "resolve",
        "https://www.twitch.tv/videos/1234",
        "--user-id",
        "user-1",
    ]);
    match resolve.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Twitch(ProviderTwitchCommand {
                    command: ProviderTwitchSubcommand::Resolve(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.resource, "https://www.twitch.tv/videos/1234");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }

    let items = Cli::parse_from([
        "synctv",
        "provider",
        "twitch",
        "items",
        "streamer",
        "--user-id",
        "user-1",
        "--content",
        "clips",
        "--cursor",
        "next-page",
        "--page-size",
        "30",
    ]);
    match items.command {
        Commands::Provider(ProviderCommand {
            command:
                ProviderSubcommand::Twitch(ProviderTwitchCommand {
                    command: ProviderTwitchSubcommand::Items(args),
                }),
        }) => {
            assert_eq!(args.access.actor.user_id.as_deref(), Some("user-1"));
            assert_eq!(args.channel, "streamer");
            assert!(matches!(args.content, ProviderTwitchContent::Clips));
            assert_eq!(args.cursor.as_deref(), Some("next-page"));
            assert_eq!(args.page_size, 30);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}
