use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use synctv_core::models::{RoomAdminPermissionBits, RoomMemberPermissionBits};
use synctv_management::proto as management_proto;

use crate::admin_client::AdminConnectionOptions;
use crate::config_loader::{public_id_config_extensions, LoadConfigOptions};

use super::commands::*;
use super::completion::CompletionArgs;
use super::human_output::ToHuman;
use super::output::{print_humanized_structured_output, print_json, RemoteOutputFormat};

pub(in crate::cli) const CLI_NAMED_PERMISSIONS: &[(&str, u64)] = &[
    (
        "send_chat_messages",
        RoomAdminPermissionBits::SEND_CHAT_MESSAGES,
    ),
    (
        "manage_own_media",
        RoomAdminPermissionBits::MANAGE_OWN_MEDIA,
    ),
    ("view_media", RoomAdminPermissionBits::VIEW_MEDIA),
    ("view_members", RoomAdminPermissionBits::VIEW_MEMBERS),
    (
        "view_chat_history",
        RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
    ),
    ("use_webrtc", RoomAdminPermissionBits::USE_WEBRTC),
    ("delete_media", RoomAdminPermissionBits::DELETE_MEDIA),
    ("reorder_media", RoomAdminPermissionBits::REORDER_MEDIA),
    ("clear_media", RoomAdminPermissionBits::CLEAR_MEDIA),
    (
        "manage_live_streams",
        RoomAdminPermissionBits::MANAGE_LIVE_STREAMS,
    ),
    (
        "control_playback_state",
        RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE,
    ),
    (
        "navigate_playback",
        RoomAdminPermissionBits::NAVIGATE_PLAYBACK,
    ),
    (
        "review_join_requests",
        RoomAdminPermissionBits::REVIEW_JOIN_REQUESTS,
    ),
    ("remove_members", RoomAdminPermissionBits::REMOVE_MEMBERS),
    (
        "manage_member_permissions",
        RoomAdminPermissionBits::MANAGE_MEMBER_PERMISSIONS,
    ),
    ("add_members", RoomAdminPermissionBits::ADD_MEMBERS),
    (
        "manage_room_settings",
        RoomAdminPermissionBits::MANAGE_ROOM_SETTINGS,
    ),
    (
        "delete_chat_messages",
        RoomAdminPermissionBits::DELETE_CHAT_MESSAGES,
    ),
    ("delete_room", RoomAdminPermissionBits::DELETE_ROOM),
];

pub(in crate::cli) const CLI_MEMBER_NAMED_PERMISSIONS: &[(&str, u64)] =
    RoomMemberPermissionBits::NAMES;
pub(in crate::cli) const CLI_ADMIN_NAMED_PERMISSIONS: &[(&str, u64)] =
    RoomAdminPermissionBits::NAMES;

#[derive(Debug, Parser)]
#[command(
    name = "synctv",
    about = "SyncTV operational command line",
    version,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run the SyncTV server
    Serve(ServeArgs),
    /// Stop the running SyncTV server through the management endpoint
    Stop(StopArgs),
    /// Configuration inspection and validation
    Config(ConfigCommand),
    /// Database migration and status operations
    Db(DbCommand),
    /// User lifecycle operations
    User(UserCommand),
    /// Room lifecycle operations
    Room(RoomCommand),
    /// Review workflow operations
    Review(ReviewCommand),
    /// Moderation ban record operations
    Ban(BanCommand),
    /// Playlist lifecycle operations within a room
    Playlist(PlaylistCommand),
    /// Media lifecycle operations within a room
    Media(MediaCommand),
    /// Remote media provider instance lifecycle management
    Provider(ProviderCommand),
    /// Runtime settings management through the management endpoint
    Settings(SettingsCommand),
    /// System inspection commands through the management endpoint
    System(SystemCommand),
    /// Proxy slice cache management through the management endpoint
    SliceCache(SliceCacheCommand),
    /// Runtime status for this instance or cluster nodes
    Status(StatusArgs),
    /// Generate shell completions
    Completion(CompletionArgs),
    /// Print version information
    Version,
}

#[derive(Debug, Clone, Args, Default)]
pub struct GlobalConfigArgs {
    /// Explicit config file path. Overrides SYNCTV_CONFIG_PATH discovery.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Shared local data directory for runtime-owned files such as the
    /// management socket, HLS storage, and proxy slice cache.
    /// Does not rebase static inputs like `*_file` secrets or metrics TLS files.
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,

    /// Do not load .env before resolving configuration.
    #[arg(long, global = true, default_value_t = false)]
    pub no_dotenv: bool,

    /// Emit verbose configuration-loading diagnostics to stderr.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// SyncTV management endpoint (`path.sock`, `unix:///path`, or `http://host:port`)
    #[arg(long, global = true)]
    pub endpoint: Option<String>,

    /// Management bearer token for remote management commands.
    #[arg(long = "auth-token", global = true, conflicts_with = "auth_token_file")]
    pub auth_token: Option<String>,

    /// Read the management bearer token for remote management commands from a file.
    #[arg(
        long = "auth-token-file",
        value_name = "PATH",
        global = true,
        conflicts_with = "auth_token"
    )]
    pub auth_token_file: Option<PathBuf>,
}

impl GlobalConfigArgs {
    pub fn load_options(&self, validate: bool) -> LoadConfigOptions {
        LoadConfigOptions {
            config_path: self.config.as_ref().map(|path| path.display().to_string()),
            data_dir: self
                .data_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            load_dotenv: !self.no_dotenv,
            validate,
            verbose: self.verbose > 0,
            extensions: public_id_config_extensions(),
        }
    }

    pub(super) fn merged_with_parent(&self, parent: &Self) -> Self {
        Self {
            config: self.config.clone().or_else(|| parent.config.clone()),
            data_dir: self.data_dir.clone().or_else(|| parent.data_dir.clone()),
            no_dotenv: self.no_dotenv || parent.no_dotenv,
            verbose: self.verbose.max(parent.verbose),
            endpoint: self.endpoint.clone().or_else(|| parent.endpoint.clone()),
            auth_token: self
                .auth_token
                .clone()
                .or_else(|| parent.auth_token.clone()),
            auth_token_file: self
                .auth_token_file
                .clone()
                .or_else(|| parent.auth_token_file.clone()),
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct RemoteAccessArgs {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    /// Output format for management command results
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}

impl RemoteAccessArgs {
    pub(super) fn connection_options(&self, endpoint: Option<String>) -> AdminConnectionOptions {
        AdminConnectionOptions {
            endpoint,
            auth_token: self.global.auth_token.clone(),
            auth_token_file: self
                .global
                .auth_token_file
                .as_ref()
                .map(|path| path.display().to_string()),
            config_path: self
                .global
                .config
                .as_ref()
                .map(|path| path.display().to_string()),
            data_dir: self
                .global
                .data_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            load_dotenv: !self.global.no_dotenv,
            verbose: self.global.verbose > 0,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        }
    }

    pub(super) fn print_output<T>(&self, value: &T) -> Result<()>
    where
        T: ?Sized + serde::Serialize + ToHuman,
    {
        match self.output {
            RemoteOutputFormat::Json => print_json(value),
            RemoteOutputFormat::Human | RemoteOutputFormat::Yaml => {
                print_humanized_structured_output(self.output, value)
            }
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct RoomScopedRemoteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliPlaybackStreamPreference {
    Auto,
    DirectPlay,
    Transcode,
}

impl CliPlaybackStreamPreference {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Auto => synctv_proto::client::PlaybackStreamPreference::Auto as i32,
            Self::DirectPlay => synctv_proto::client::PlaybackStreamPreference::DirectPlay as i32,
            Self::Transcode => synctv_proto::client::PlaybackStreamPreference::Transcode as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliPlaybackVideoCodec {
    H264,
    Hevc,
    Vp9,
    Av1,
}

impl CliPlaybackVideoCodec {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::H264 => synctv_proto::client::PlaybackVideoCodec::H264 as i32,
            Self::Hevc => synctv_proto::client::PlaybackVideoCodec::Hevc as i32,
            Self::Vp9 => synctv_proto::client::PlaybackVideoCodec::Vp9 as i32,
            Self::Av1 => synctv_proto::client::PlaybackVideoCodec::Av1 as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliPlaybackContainer {
    Mp4,
    Mkv,
    Webm,
}

impl CliPlaybackContainer {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Mp4 => synctv_proto::client::PlaybackContainer::Mp4 as i32,
            Self::Mkv => synctv_proto::client::PlaybackContainer::Mkv as i32,
            Self::Webm => synctv_proto::client::PlaybackContainer::Webm as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliPlaybackAudioCapability {
    Stereo,
    Surround,
    LosslessSurround,
}

impl CliPlaybackAudioCapability {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Stereo => synctv_proto::client::PlaybackAudioCapability::Stereo as i32,
            Self::Surround => synctv_proto::client::PlaybackAudioCapability::Surround as i32,
            Self::LosslessSurround => {
                synctv_proto::client::PlaybackAudioCapability::LosslessSurround as i32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliPlaybackSubtitlePreference {
    External,
    EmbeddedOrExternal,
    None,
}

impl CliPlaybackSubtitlePreference {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::External => synctv_proto::client::PlaybackSubtitlePreference::External as i32,
            Self::EmbeddedOrExternal => {
                synctv_proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal as i32
            }
            Self::None => synctv_proto::client::PlaybackSubtitlePreference::None as i32,
        }
    }
}

#[derive(Debug, Clone, Args, Default)]
pub struct PlaybackClientProfileArgs {
    #[arg(long = "stream", value_enum)]
    pub stream_preference: Option<CliPlaybackStreamPreference>,

    #[arg(long)]
    pub max_streaming_bitrate: Option<i64>,

    #[arg(long)]
    pub max_audio_channels: Option<i32>,

    #[arg(long = "video-codec", value_enum, value_delimiter = ',')]
    pub supported_video_codecs: Vec<CliPlaybackVideoCodec>,

    #[arg(long = "container", value_enum, value_delimiter = ',')]
    pub supported_containers: Vec<CliPlaybackContainer>,

    #[arg(long, value_enum)]
    pub audio_capability: Option<CliPlaybackAudioCapability>,

    #[arg(long = "subtitle", value_enum)]
    pub subtitle_preference: Option<CliPlaybackSubtitlePreference>,
}

impl PlaybackClientProfileArgs {
    pub(super) fn to_proto(&self) -> Option<synctv_proto::client::PlaybackClientProfile> {
        if self.stream_preference.is_none()
            && self.max_streaming_bitrate.is_none()
            && self.max_audio_channels.is_none()
            && self.supported_video_codecs.is_empty()
            && self.supported_containers.is_empty()
            && self.audio_capability.is_none()
            && self.subtitle_preference.is_none()
        {
            return None;
        }

        Some(synctv_proto::client::PlaybackClientProfile {
            stream_preference: self
                .stream_preference
                .map_or(0, CliPlaybackStreamPreference::to_proto),
            max_streaming_bitrate: self.max_streaming_bitrate,
            max_audio_channels: self.max_audio_channels,
            supported_video_codecs: self
                .supported_video_codecs
                .iter()
                .copied()
                .map(CliPlaybackVideoCodec::to_proto)
                .collect(),
            supported_containers: self
                .supported_containers
                .iter()
                .copied()
                .map(CliPlaybackContainer::to_proto)
                .collect(),
            audio_capability: self
                .audio_capability
                .map_or(0, CliPlaybackAudioCapability::to_proto),
            subtitle_preference: self
                .subtitle_preference
                .map_or(0, CliPlaybackSubtitlePreference::to_proto),
        })
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliUserRole {
    User,
    Admin,
    Root,
}

impl CliUserRole {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::User => synctv_proto::common::UserRole::User as i32,
            Self::Admin => synctv_proto::common::UserRole::Admin as i32,
            Self::Root => synctv_proto::common::UserRole::Root as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliUserStatus {
    Active,
    Banned,
}

impl CliUserStatus {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Active => synctv_proto::common::UserStatus::Active as i32,
            Self::Banned => synctv_proto::common::UserStatus::Banned as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliSortDirection {
    Asc,
    Desc,
}

impl CliSortDirection {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Asc => management_proto::SortDirection::Asc as i32,
            Self::Desc => management_proto::SortDirection::Desc as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliUserSortField {
    Username,
    Email,
    Status,
    Role,
    UpdatedAt,
    CreatedAt,
}

impl CliUserSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Username => management_proto::UserListSortBy::Username as i32,
            Self::Email => management_proto::UserListSortBy::Email as i32,
            Self::Status => management_proto::UserListSortBy::Status as i32,
            Self::Role => management_proto::UserListSortBy::Role as i32,
            Self::UpdatedAt => management_proto::UserListSortBy::UpdatedAt as i32,
            Self::CreatedAt => management_proto::UserListSortBy::CreatedAt as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliRoomStatus {
    Active,
    Closed,
}

impl CliRoomStatus {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Active => synctv_proto::common::RoomStatus::Active as i32,
            Self::Closed => synctv_proto::common::RoomStatus::Closed as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliRoomSortField {
    Name,
    UpdatedAt,
    LastActivityAt,
    CreatedAt,
}

impl CliRoomSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Name => management_proto::RoomListSortBy::Name as i32,
            Self::UpdatedAt => management_proto::RoomListSortBy::UpdatedAt as i32,
            Self::LastActivityAt => management_proto::RoomListSortBy::LastActivityAt as i32,
            Self::CreatedAt => management_proto::RoomListSortBy::CreatedAt as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliRoomMemberSortField {
    JoinedAt,
    Username,
    Role,
}

impl CliRoomMemberSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::JoinedAt => management_proto::RoomMemberListSortBy::JoinedAt as i32,
            Self::Username => management_proto::RoomMemberListSortBy::Username as i32,
            Self::Role => management_proto::RoomMemberListSortBy::Role as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliPlaylistSortField {
    Position,
    Name,
    CreatedAt,
    UpdatedAt,
}

impl CliPlaylistSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Position => management_proto::PlaylistListSortBy::Position as i32,
            Self::Name => management_proto::PlaylistListSortBy::Name as i32,
            Self::CreatedAt => management_proto::PlaylistListSortBy::CreatedAt as i32,
            Self::UpdatedAt => management_proto::PlaylistListSortBy::UpdatedAt as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliMediaSortField {
    Position,
    Name,
    AddedAt,
    UpdatedAt,
    SourceProvider,
    ProviderInstanceName,
}

impl CliMediaSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Position => management_proto::MediaListSortBy::Position as i32,
            Self::Name => management_proto::MediaListSortBy::Name as i32,
            Self::AddedAt => management_proto::MediaListSortBy::AddedAt as i32,
            Self::UpdatedAt => management_proto::MediaListSortBy::UpdatedAt as i32,
            Self::SourceProvider => management_proto::MediaListSortBy::SourceProvider as i32,
            Self::ProviderInstanceName => {
                management_proto::MediaListSortBy::ProviderInstanceName as i32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliResourceAvailabilityFilter {
    All,
    Available,
    Unavailable,
}

impl CliResourceAvailabilityFilter {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::All => synctv_proto::client::ResourceAvailabilityFilter::All as i32,
            Self::Available => synctv_proto::client::ResourceAvailabilityFilter::Available as i32,
            Self::Unavailable => {
                synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliProviderSortField {
    Name,
    Endpoint,
    CreatedAt,
    UpdatedAt,
}

impl CliProviderSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Name => synctv_proto::providers::common::ProviderInstanceListSortBy::Name as i32,
            Self::Endpoint => {
                synctv_proto::providers::common::ProviderInstanceListSortBy::Endpoint as i32
            }
            Self::CreatedAt => {
                synctv_proto::providers::common::ProviderInstanceListSortBy::CreatedAt as i32
            }
            Self::UpdatedAt => {
                synctv_proto::providers::common::ProviderInstanceListSortBy::UpdatedAt as i32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliActiveStreamSortField {
    StartedAt,
    RoomId,
    MediaId,
    UserId,
    NodeId,
}

impl CliActiveStreamSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::StartedAt => management_proto::ActiveStreamListSortBy::StartedAt as i32,
            Self::RoomId => management_proto::ActiveStreamListSortBy::RoomId as i32,
            Self::MediaId => management_proto::ActiveStreamListSortBy::MediaId as i32,
            Self::UserId => management_proto::ActiveStreamListSortBy::UserId as i32,
            Self::NodeId => management_proto::ActiveStreamListSortBy::NodeId as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliRoomStreamSortField {
    MediaId,
}

impl CliRoomStreamSortField {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::MediaId => management_proto::RoomStreamListSortBy::MediaId as i32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliSourceProvider {
    DirectUrl,
    Bilibili,
    Alist,
    Emby,
    Rtmp,
    LiveProxy,
    Cloudreve,
    Twitch,
    Huya,
    Douyu,
    Douyin,
    Acfun,
    Cctv,
    Fnos,
    Qnap,
    Synology,
    Nextcloud,
    Seafile,
    Truenas,
    Youtube,
    Tiktok,
}

impl CliSourceProvider {
    pub(super) fn to_proto(self) -> synctv_proto::source_config::SourceProvider {
        match self {
            Self::DirectUrl => synctv_proto::source_config::SourceProvider::DirectUrl,
            Self::Bilibili => synctv_proto::source_config::SourceProvider::Bilibili,
            Self::Alist => synctv_proto::source_config::SourceProvider::Alist,
            Self::Emby => synctv_proto::source_config::SourceProvider::Emby,
            Self::Rtmp => synctv_proto::source_config::SourceProvider::Rtmp,
            Self::LiveProxy => synctv_proto::source_config::SourceProvider::LiveProxy,
            Self::Cloudreve => synctv_proto::source_config::SourceProvider::Cloudreve,
            Self::Twitch => synctv_proto::source_config::SourceProvider::Twitch,
            Self::Huya => synctv_proto::source_config::SourceProvider::Huya,
            Self::Douyu => synctv_proto::source_config::SourceProvider::Douyu,
            Self::Douyin => synctv_proto::source_config::SourceProvider::Douyin,
            Self::Acfun => synctv_proto::source_config::SourceProvider::Acfun,
            Self::Cctv => synctv_proto::source_config::SourceProvider::Cctv,
            Self::Fnos => synctv_proto::source_config::SourceProvider::Fnos,
            Self::Qnap => synctv_proto::source_config::SourceProvider::Qnap,
            Self::Synology => synctv_proto::source_config::SourceProvider::Synology,
            Self::Nextcloud => synctv_proto::source_config::SourceProvider::Nextcloud,
            Self::Seafile => synctv_proto::source_config::SourceProvider::Seafile,
            Self::Truenas => synctv_proto::source_config::SourceProvider::Truenas,
            Self::Youtube => synctv_proto::source_config::SourceProvider::Youtube,
            Self::Tiktok => synctv_proto::source_config::SourceProvider::Tiktok,
        }
    }

    pub(super) fn to_proto_i32(self) -> i32 {
        self.to_proto() as i32
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliRoomMemberRole {
    Guest,
    Member,
    Admin,
    Creator,
}

impl CliRoomMemberRole {
    pub(super) const fn to_proto(self) -> i32 {
        match self {
            Self::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
            Self::Member => synctv_proto::common::RoomMemberRole::Member as i32,
            Self::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
            Self::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("user_ref")
        .args(["username", "user_id"])
        .required(true)
        .multiple(false)
))]
pub struct UserRefArgs {
    /// Username used as the primary human-friendly identifier
    #[arg(value_name = "USER", group = "user_ref")]
    pub username: Option<String>,

    /// Explicit public user ID
    #[arg(long, value_name = "USER_ID", group = "user_ref")]
    pub user_id: Option<String>,
}

impl UserRefArgs {
    pub(super) fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        let value = if let Some(user_id) = self.user_id.as_deref() {
            management_proto::user_ref::Value::UserId(user_id.to_string())
        } else if let Some(username) = self.username.as_deref() {
            management_proto::user_ref::Value::Username(username.to_string())
        } else {
            bail!("user reference requires USER or --user-id")
        };
        Ok(management_proto::UserRef { value: Some(value) })
    }

    pub(super) fn to_management_selector(&self) -> Result<(String, String)> {
        if let Some(user_id) = self.user_id.as_deref() {
            Ok((user_id.to_string(), String::new()))
        } else if let Some(username) = self.username.as_deref() {
            Ok((String::new(), username.to_string()))
        } else {
            bail!("user reference requires USER or --user-id")
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("actor_user_ref")
        .args(["username", "user_id", "email"])
        .required(true)
        .multiple(false)
))]
pub struct ActorUserArgs {
    /// Username of the real actor user that owns or performs this operation
    #[arg(long, value_name = "USERNAME", group = "actor_user_ref")]
    pub username: Option<String>,

    /// Explicit public user ID of the real actor user
    #[arg(long, value_name = "USER_ID", group = "actor_user_ref")]
    pub user_id: Option<String>,

    /// Email of the real actor user
    #[arg(long, value_name = "EMAIL", group = "actor_user_ref")]
    pub email: Option<String>,
}

impl ActorUserArgs {
    pub(super) fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        let value = if let Some(user_id) = self.user_id.as_deref() {
            management_proto::user_ref::Value::UserId(user_id.to_string())
        } else if let Some(username) = self.username.as_deref() {
            management_proto::user_ref::Value::Username(username.to_string())
        } else if let Some(email) = self.email.as_deref() {
            management_proto::user_ref::Value::Email(email.to_string())
        } else {
            bail!("actor reference requires --username, --user-id, or --email")
        };
        Ok(management_proto::UserRef { value: Some(value) })
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("optional_actor_user_ref")
        .args(["username", "user_id", "email"])
        .required(false)
        .multiple(false)
))]
pub struct OptionalActorUserArgs {
    /// Username of the real user associated with the operation
    #[arg(long, value_name = "USERNAME", group = "optional_actor_user_ref")]
    pub username: Option<String>,

    /// Public ID of the real user associated with the operation
    #[arg(long, value_name = "USER_ID", group = "optional_actor_user_ref")]
    pub user_id: Option<String>,

    /// Email of the real user associated with the operation
    #[arg(long, value_name = "EMAIL", group = "optional_actor_user_ref")]
    pub email: Option<String>,
}

impl OptionalActorUserArgs {
    pub(super) fn to_management_proto(&self) -> Result<Option<management_proto::UserRef>> {
        if self.username.is_none() && self.user_id.is_none() && self.email.is_none() {
            return Ok(None);
        }
        ActorUserArgs {
            username: self.username.clone(),
            user_id: self.user_id.clone(),
            email: self.email.clone(),
        }
        .to_management_proto()
        .map(Some)
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("room_creator_ref")
        .args(["creator", "creator_id"])
        .multiple(false)
))]
pub struct RoomCreatorRefArgs {
    /// Exact creator username used to filter rooms
    #[arg(
        long = "creator-username",
        value_name = "USERNAME",
        group = "room_creator_ref"
    )]
    pub creator: Option<String>,

    /// Explicit creator internal user ID used to filter rooms
    #[arg(long, value_name = "USER_ID", group = "room_creator_ref")]
    pub creator_id: Option<String>,
}

impl RoomCreatorRefArgs {
    pub(super) fn to_management_proto(&self) -> Result<Option<management_proto::UserRef>> {
        if self.creator.is_none() && self.creator_id.is_none() {
            return Ok(None);
        }

        UserRefArgs {
            username: self.creator.clone(),
            user_id: self.creator_id.clone(),
        }
        .to_management_proto()
        .map(Some)
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("stream_user_filter_ref")
        .args(["username", "user_id"])
        .multiple(false)
))]
pub struct StreamUserFilterArgs {
    /// Exact username used to filter active streams
    #[arg(long, value_name = "USERNAME", group = "stream_user_filter_ref")]
    pub username: Option<String>,

    /// Explicit internal user ID used to filter active streams
    #[arg(long, value_name = "USER_ID", group = "stream_user_filter_ref")]
    pub user_id: Option<String>,
}

impl StreamUserFilterArgs {
    pub(super) fn to_management_selector(&self) -> (String, String) {
        (
            self.user_id.clone().unwrap_or_default(),
            self.username.clone().unwrap_or_default(),
        )
    }
}
