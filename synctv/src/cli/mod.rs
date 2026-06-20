use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use synctv_common::time as app_time;
use synctv_core::bootstrap::LoadConfigOptions;
use synctv_core::config::absolute_display_path;
#[cfg(test)]
use synctv_core::config::default_management_unix_socket_path;
use synctv_core::models::{RoomAdminPermissionBits, RoomMemberPermissionBits};
use synctv_management::proto as management_proto;

use crate::admin_client::{AdminConnectionOptions, RemoteAdminSession};
use crate::app::Application;

mod completion;
mod context;
mod output;

pub use completion::{CompletionArgs, CompletionShell};
pub use output::{ConfigOutputFormat, RemoteOutputFormat};

use completion::execute_completion;
use context::{CliConfigContext, RemoteCliContext};
use output::{
    print_humanized_structured_output, print_json, print_structured_output, print_toml, print_yaml,
};

#[cfg(test)]
use completion::write_completion_output;

const MANAGEMENT_UNARY_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

const CLI_NAMED_PERMISSIONS: &[(&str, u64)] = &[
    ("chat", RoomAdminPermissionBits::CHAT),
    (
        "create_media_resource",
        RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    ),
    (
        "view_media_resources",
        RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES,
    ),
    (
        "view_member_list",
        RoomAdminPermissionBits::VIEW_MEMBER_LIST,
    ),
    (
        "view_chat_history",
        RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
    ),
    ("use_webrtc", RoomAdminPermissionBits::USE_WEBRTC),
    (
        "delete_media_resource_any",
        RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY,
    ),
    (
        "reorder_media_resources",
        RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES,
    ),
    (
        "clear_media_resources",
        RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES,
    ),
    ("live_control", RoomAdminPermissionBits::LIVE_CONTROL),
    ("play_control", RoomAdminPermissionBits::PLAY_CONTROL),
    (
        "change_current_media",
        RoomAdminPermissionBits::CHANGE_CURRENT_MEDIA,
    ),
    (
        "change_playback_rate",
        RoomAdminPermissionBits::CHANGE_PLAYBACK_RATE,
    ),
    ("approve_member", RoomAdminPermissionBits::APPROVE_MEMBER),
    ("kick_member", RoomAdminPermissionBits::KICK_MEMBER),
    (
        "set_member_permissions",
        RoomAdminPermissionBits::SET_MEMBER_PERMISSIONS,
    ),
    ("add_member", RoomAdminPermissionBits::ADD_MEMBER),
    (
        "set_room_settings",
        RoomAdminPermissionBits::SET_ROOM_SETTINGS,
    ),
    ("delete_chat", RoomAdminPermissionBits::DELETE_CHAT),
    ("delete_room", RoomAdminPermissionBits::DELETE_ROOM),
];

const CLI_MEMBER_NAMED_PERMISSIONS: &[(&str, u64)] = RoomMemberPermissionBits::NAMES;
const CLI_ADMIN_NAMED_PERMISSIONS: &[(&str, u64)] = RoomAdminPermissionBits::NAMES;

macro_rules! management_unary_call {
    ($session:expr, $operation:literal, $method:ident, $request:expr) => {{
        let mut client = $session.management_client();
        management_unary_response($operation, client.$method($request)).await
    }};
}

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

    /// SyncTV management endpoint (`unix:///path` or `http://host:port`)
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
        }
    }

    fn merged_with_parent(&self, parent: &Self) -> Self {
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

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct StopArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    /// Request an immediate shutdown path with minimal draining
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ConfigCommand {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[command(subcommand)]
    pub command: ConfigSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
    /// Validate resolved configuration
    Validate(ConfigValidateArgs),
    /// Print the resolved configuration with secrets redacted
    Show(ConfigShowArgs),
}

#[derive(Debug, Args)]
pub struct ConfigValidateArgs {
    /// Reject unknown config-file keys and unsupported SYNCTV_ environment variables
    #[arg(long, default_value_t = false)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// Output format for the rendered configuration
    #[arg(long, short = 'o', value_enum, default_value_t = ConfigOutputFormat::Yaml)]
    pub output: ConfigOutputFormat,
}

#[derive(Debug, Args)]
pub struct DbCommand {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[command(subcommand)]
    pub command: DbSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum DbSubcommand {
    /// Run startup migrations without starting the server
    Migrate(DbMigrateArgs),
    /// Check connectivity and report migration readiness
    Status(DbStatusArgs),
}

#[derive(Debug, Args)]
pub struct DbMigrateArgs {
    /// Output format for migration result
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}

#[derive(Debug, Args)]
pub struct DbStatusArgs {
    /// Output format for database status
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}

#[derive(Debug, Args)]
pub struct ReviewCommand {
    #[command(subcommand)]
    pub command: ReviewSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewSubcommand {
    /// User registration review workflow
    UserRegistration(ReviewUserRegistrationCommand),
    /// Room creation review workflow
    RoomCreation(ReviewRoomCreationCommand),
    /// Room join review workflow
    RoomJoin(ReviewRoomJoinCommand),
}

#[derive(Debug, Args)]
pub struct ReviewUserRegistrationCommand {
    #[command(subcommand)]
    pub command: ReviewUserRegistrationSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewUserRegistrationSubcommand {
    List(ReviewUserRegistrationListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Debug, Args)]
pub struct ReviewRoomCreationCommand {
    #[command(subcommand)]
    pub command: ReviewRoomCreationSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewRoomCreationSubcommand {
    List(ReviewRoomCreationListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Debug, Args)]
pub struct ReviewRoomJoinCommand {
    #[command(subcommand)]
    pub command: ReviewRoomJoinSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewRoomJoinSubcommand {
    List(ReviewRoomJoinListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliReviewStatus {
    Pending,
    Approved,
    Rejected,
}

impl CliReviewStatus {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Pending => synctv_proto::common::ReviewStatus::Pending as i32,
            Self::Approved => synctv_proto::common::ReviewStatus::Approved as i32,
            Self::Rejected => synctv_proto::common::ReviewStatus::Rejected as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct ReviewUserRegistrationListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long)]
    pub search: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewRoomCreationListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long, allow_hyphen_values = true)]
    pub requested_by: Option<String>,
    #[arg(long)]
    pub search: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewRoomJoinListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long, allow_hyphen_values = true)]
    pub room_id: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    pub user_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewApproveArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(allow_hyphen_values = true)]
    pub request_id: String,
}

#[derive(Debug, Args)]
pub struct ReviewRejectArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(allow_hyphen_values = true)]
    pub request_id: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct BanCommand {
    #[command(subcommand)]
    pub command: BanSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum BanSubcommand {
    /// List ban records
    List(BanListArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliBanTarget {
    User,
    Room,
}

impl CliBanTarget {
    const fn to_proto(self) -> i32 {
        match self {
            Self::User => synctv_proto::admin::BanTargetType::User as i32,
            Self::Room => synctv_proto::admin::BanTargetType::Room as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct BanListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, value_enum)]
    pub target: Option<CliBanTarget>,

    #[arg(long)]
    pub active: Option<bool>,

    #[arg(long, allow_hyphen_values = true)]
    pub room_id: Option<String>,

    #[arg(long, allow_hyphen_values = true)]
    pub user_id: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct UserCommand {
    #[command(subcommand)]
    pub command: UserSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum UserSubcommand {
    /// List users
    List(UserListArgs),
    /// Get a user by username or explicit user ID
    Get(UserGetArgs),
    /// Inspect or update user preferences
    Preferences(UserPreferencesCommand),
    /// Create a user
    Create(UserAddArgs),
    /// Delete a user
    Delete(UserDeleteArgs),
    /// Ban a user
    Ban(UserBanArgs),
    /// Unban a user
    Unban(UserUnbanArgs),
    /// Update a user's global role
    SetRole(UserSetRoleArgs),
    /// Set a user's direct password credential
    SetPassword(UserSetPasswordArgs),
    /// Update a user's username
    SetUsername(UserSetUsernameArgs),
    /// List rooms owned or joined by the user
    Rooms(UserRoomsArgs),
    /// Global administrator management
    Admin(UserAdminCommand),
    /// Batch user operations
    Batch(UserBatchCommand),
}

#[derive(Debug, Args)]
pub struct UserAdminCommand {
    #[command(subcommand)]
    pub command: UserAdminSubcommand,
}

#[derive(Debug, Args)]
pub struct UserPreferencesCommand {
    #[command(subcommand)]
    pub command: UserPreferencesSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum UserPreferencesSubcommand {
    /// Get user preferences and available authentication factors
    Get(UserPreferencesGetArgs),
    /// Update user preferences
    Set(Box<UserPreferencesSetArgs>),
}

#[derive(Debug, Subcommand)]
pub enum UserAdminSubcommand {
    /// Grant global admin role to a user by username or explicit user ID
    Grant(UserAdminAddArgs),
    /// Revoke global admin role from a user by username or explicit user ID
    Revoke(UserAdminRemoveArgs),
    /// List all global admins and root users
    List(UserAdminListArgs),
}

#[derive(Debug, Args)]
pub struct UserBatchCommand {
    #[command(subcommand)]
    pub command: UserBatchSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum UserBatchSubcommand {
    /// Ban multiple users
    Ban(UserBatchBanArgs),
    /// Delete multiple users
    Delete(UserBatchDeleteArgs),
}

#[derive(Debug, Args)]
pub struct RoomCommand {
    #[command(subcommand)]
    pub command: RoomSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomSubcommand {
    /// Create a room as a specific real user
    Create(RoomCreateArgs),
    /// List rooms
    List(RoomListArgs),
    /// Get a room by ID
    Get(RoomGetArgs),
    /// Manage room settings
    Settings(RoomSettingsCommand),
    /// Transfer room ownership to another existing member
    TransferOwner(RoomTransferOwnerArgs),
    /// Manage room members
    Member(RoomMemberCommand),
    /// Playback lifecycle operations
    Playback(RoomPlaybackCommand),
    /// Live stream lifecycle operations
    Stream(RoomStreamCommand),
    /// Batch room operations
    Batch(RoomBatchCommand),
    /// Set or clear a room password
    SetPassword(RoomSetPasswordArgs),
    /// Ban a room
    Ban(RoomBanArgs),
    /// Unban a room
    Unban(RoomUnbanArgs),
    /// Delete a room
    Delete(RoomDeleteArgs),
}

#[derive(Debug, Args)]
pub struct RoomSettingsCommand {
    #[command(subcommand)]
    pub command: RoomSettingsSubcommand,
}

#[derive(Debug, Args)]
pub struct RoomCreateArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    pub name: String,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(long)]
    pub description: Option<String>,

    /// Partial JSON object patch merged onto default room settings before creation
    #[arg(long)]
    pub settings_json: Option<String>,

    /// Room password
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomTransferOwnerArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    pub room_id: String,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[command(flatten)]
    pub new_owner: RoomTransferTargetUserArgs,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("room_transfer_target_ref")
        .args(["new_owner_username", "new_owner_user_id"])
        .required(true)
        .multiple(false)
))]
pub struct RoomTransferTargetUserArgs {
    /// Username of the member that will become the new room owner
    #[arg(value_name = "USER", group = "room_transfer_target_ref")]
    pub new_owner_username: Option<String>,

    /// Explicit internal user ID of the member that will become the new room owner
    #[arg(
        long = "new-owner-id",
        value_name = "USER_ID",
        group = "room_transfer_target_ref"
    )]
    pub new_owner_user_id: Option<String>,
}

impl RoomTransferTargetUserArgs {
    fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        UserRefArgs {
            username: self.new_owner_username.clone(),
            user_id: self.new_owner_user_id.clone(),
        }
        .to_management_proto()
    }
}

#[derive(Debug, Subcommand)]
pub enum RoomSettingsSubcommand {
    /// Get room settings
    Get(RoomSettingsGetArgs),
    /// Patch room settings with a partial JSON object
    Update(RoomSettingsUpdateArgs),
    /// Reset room settings to defaults
    Reset(RoomSettingsResetArgs),
}

#[derive(Debug, Args)]
pub struct RoomMemberCommand {
    #[command(subcommand)]
    pub command: RoomMemberSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomMemberSubcommand {
    /// List room members
    List(RoomMembersArgs),
    /// Add an existing user as an active room member using a management override
    Add(RoomMemberAddArgs),
    /// Update a room member's role or permission bitmasks
    SetPermissions(RoomMemberSetPermissionsArgs),
    /// Kick a room member
    Kick(RoomMemberKickArgs),
}

#[derive(Debug, Args)]
pub struct RoomPlaybackCommand {
    #[command(subcommand)]
    pub command: RoomPlaybackSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomPlaybackSubcommand {
    /// Get the current playback state and signed pull URLs for the room's active item
    Get(RoomPlaybackGetArgs),
    /// Start playback for a static media item or dynamic playlist target
    Start(RoomPlaybackStartArgs),
    /// Resume playback for the room's current playback item
    Play(RoomPlaybackStateUpdateArgs),
    /// Pause playback for the room's current playback item
    Pause(RoomPlaybackStateUpdateArgs),
    /// Seek the room's current playback item to a position
    Seek(RoomPlaybackSeekArgs),
    /// Change playback speed for the room's current playback item
    Speed(RoomPlaybackSpeedArgs),
    /// Stop the room's current playback item
    Stop(RoomPlaybackStopArgs),
}

#[derive(Debug, Args)]
pub struct RoomStreamCommand {
    #[command(subcommand)]
    pub command: RoomStreamSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomStreamSubcommand {
    /// List active RTMP publish sessions in a room
    List(RoomStreamListArgs),
    /// Kick an active RTMP publish session in a room
    Kick(RoomStreamKickArgs),
}

#[derive(Debug, Args)]
pub struct RoomBatchCommand {
    #[command(subcommand)]
    pub command: RoomBatchSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomBatchSubcommand {
    /// Ban multiple rooms
    Ban(RoomBatchBanArgs),
    /// Delete multiple rooms
    Delete(RoomBatchDeleteArgs),
}

#[derive(Debug, Args)]
pub struct SettingsCommand {
    #[command(subcommand)]
    pub command: SettingsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsSubcommand {
    /// List effective runtime settings grouped by category
    List(SettingsListArgs),
    /// Get one effective settings group
    Get(SettingsGetArgs),
    /// Update one settings group using repeated --set key=value entries
    Update(SettingsUpdateArgs),
    /// Send a test email using the current runtime email settings
    TestEmail(SettingsTestEmailArgs),
}

#[derive(Debug, Args)]
pub struct SystemCommand {
    #[command(subcommand)]
    pub command: SystemSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SystemSubcommand {
    /// Show system statistics
    Stats(SystemStatsArgs),
    /// Active stream inspection and control
    Stream(SystemStreamCommand),
}

#[derive(Debug, Args)]
pub struct SystemStreamCommand {
    #[command(subcommand)]
    pub command: SystemStreamSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SystemStreamSubcommand {
    /// List active streams across the cluster
    List(SystemStreamListArgs),
    /// Kick an active stream
    Kick(SystemStreamKickArgs),
}

#[derive(Debug, Args)]
pub struct SliceCacheCommand {
    #[command(subcommand)]
    pub command: SliceCacheSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SliceCacheSubcommand {
    /// Show proxy slice cache runtime statistics and configuration
    Stats(SliceCacheStatsArgs),
    /// Remove all cached slice entries and metadata
    Purge(SliceCachePurgeArgs),
    /// Remove only expired cached slice entries
    EvictExpired(SliceCacheEvictExpiredArgs),
}

#[derive(Debug, Args)]
pub struct ProviderCommand {
    #[command(subcommand)]
    pub command: ProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderSubcommand {
    /// List enabled remote provider instance names available to app clients
    Available(ProviderAvailableArgs),
    /// List backends for one provider type, including the default backend when present
    Backends(ProviderBackendsArgs),
    /// List provider instances
    List(ProviderListArgs),
    /// Create a provider instance
    Create(ProviderAddArgs),
    /// Update a provider instance
    Update(ProviderUpdateArgs),
    /// Delete a provider instance
    Delete(ProviderDeleteArgs),
    /// Reconnect a provider instance
    Reconnect(ProviderReconnectArgs),
    /// Enable a provider instance
    Enable(ProviderEnableArgs),
    /// Disable a provider instance
    Disable(ProviderDisableArgs),
    /// Alist provider service operations
    Alist(ProviderAlistCommand),
    /// Emby provider service operations
    Emby(ProviderEmbyCommand),
    /// Bilibili provider service operations
    Bilibili(ProviderBilibiliCommand),
    /// RTMP provider service operations
    Rtmp(ProviderRtmpCommand),
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
    fn connection_options(&self, endpoint: Option<String>) -> AdminConnectionOptions {
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

    fn print_output<T>(&self, value: &T) -> Result<()>
    where
        T: ?Sized + serde::Serialize + ToHuman,
    {
        print_structured_output(self.output, value)
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    fn to_proto(&self) -> Option<synctv_proto::client::PlaybackClientProfile> {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
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
    const fn to_proto(self) -> i32 {
        match self {
            Self::MediaId => management_proto::RoomStreamListSortBy::MediaId as i32,
        }
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
    const fn to_proto(self) -> i32 {
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

    /// Explicit internal user ID
    #[arg(long, value_name = "USER_ID", group = "user_ref")]
    pub user_id: Option<String>,
}

impl UserRefArgs {
    fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        let value = if let Some(user_id) = self.user_id.as_deref() {
            management_proto::user_ref::Value::UserId(user_id.to_string())
        } else if let Some(username) = self.username.as_deref() {
            management_proto::user_ref::Value::Username(username.to_string())
        } else {
            bail!("user reference requires USER or --user-id")
        };
        Ok(management_proto::UserRef { value: Some(value) })
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("actor_user_ref")
        .args(["username", "user_id"])
        .required(true)
        .multiple(false)
))]
pub struct ActorUserArgs {
    /// Username of the real actor user that owns or performs this operation
    #[arg(long, value_name = "USERNAME", group = "actor_user_ref")]
    pub username: Option<String>,

    /// Explicit internal user ID of the real actor user
    #[arg(long, value_name = "USER_ID", group = "actor_user_ref")]
    pub user_id: Option<String>,
}

impl ActorUserArgs {
    fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        UserRefArgs {
            username: self.username.clone(),
            user_id: self.user_id.clone(),
        }
        .to_management_proto()
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
    fn to_management_proto(&self) -> Result<Option<management_proto::UserRef>> {
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
    fn to_management_proto(&self) -> Result<Option<management_proto::UserRef>> {
        if self.username.is_none() && self.user_id.is_none() {
            return Ok(None);
        }

        UserRefArgs {
            username: self.username.clone(),
            user_id: self.user_id.clone(),
        }
        .to_management_proto()
        .map(Some)
    }
}

#[derive(Debug, Args)]
pub struct UserListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long, value_enum)]
    pub status: Option<CliUserStatus>,

    #[arg(long, value_enum)]
    pub role: Option<CliUserRole>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliUserSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct UserGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserPreferencesGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserPreferencesSetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    /// Enable or disable user-level two-factor authentication
    #[arg(long)]
    pub two_factor_enabled: Option<bool>,

    /// Replace notification preferences with a full JSON object
    #[arg(long)]
    pub notifications_json: Option<String>,
}

#[derive(Debug, Args)]
pub struct UserAdminAddArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserAdminRemoveArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserAdminListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliUserSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("user_batch_targets")
        .args(["usernames", "user_ids"])
        .required(true)
        .multiple(true)
))]
pub struct UserBatchBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "USERNAME", group = "user_batch_targets")]
    pub usernames: Vec<String>,

    #[arg(
        long = "user-id",
        value_name = "USER_ID",
        required = false,
        num_args = 1..,
        group = "user_batch_targets"
    )]
    pub user_ids: Vec<String>,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("user_batch_targets")
        .args(["usernames", "user_ids"])
        .required(true)
        .multiple(true)
))]
pub struct UserBatchDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "USERNAME", group = "user_batch_targets")]
    pub usernames: Vec<String>,

    #[arg(
        long = "user-id",
        value_name = "USER_ID",
        required = false,
        num_args = 1..,
        group = "user_batch_targets"
    )]
    pub user_ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UserAddArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    pub username: String,

    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    #[arg(long, value_name = "PASSWORD")]
    pub password: Option<String>,

    #[arg(long, value_enum, default_value_t = CliUserRole::User)]
    pub role: CliUserRole,

    #[arg(long, value_enum, default_value_t = CliUserStatus::Active)]
    pub status: CliUserStatus,
}

#[derive(Debug, Args)]
pub struct UserDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct UserUnbanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("user_role_input")
        .args(["role", "role_arg"])
        .required(true)
        .multiple(false)
))]
pub struct UserSetRoleArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum, group = "user_role_input")]
    pub role: Option<CliUserRole>,

    #[arg(value_enum, hide = true, group = "user_role_input")]
    pub role_arg: Option<CliUserRole>,
}

impl UserSetRoleArgs {
    fn resolved_role(&self) -> Result<CliUserRole> {
        self.role
            .or(self.role_arg)
            .ok_or_else(|| anyhow!("user set-role requires ROLE or --role"))
    }
}

#[derive(Debug, Args)]
pub struct UserSetPasswordArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long)]
    pub password: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct UserSetUsernameArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long = "username", value_name = "USERNAME")]
    pub new_username: String,
}

#[derive(Debug, Args)]
pub struct UserRoomsArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long, value_enum)]
    pub status: Option<CliRoomStatus>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub is_banned: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct RoomListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long, value_enum)]
    pub status: Option<CliRoomStatus>,

    #[arg(long)]
    pub search: Option<String>,

    #[command(flatten)]
    pub creator: RoomCreatorRefArgs,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub is_banned: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct RoomGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_member_scope")
        .args(["room_id", "room_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct RoomMembersArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: Option<String>,

    #[arg(long = "room-id", value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id_flag: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomMemberSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,
}

impl RoomMembersArgs {
    fn resolved_room_id(&self) -> Result<&str> {
        self.room_id
            .as_deref()
            .or(self.room_id_flag.as_deref())
            .ok_or_else(|| anyhow!("room member list requires ROOM_ID or --room-id"))
    }
}

#[derive(Debug, Args)]
pub struct RoomMemberSetPermissionsArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(
        long,
        value_parser = parse_member_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub added_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_member_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub removed_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_admin_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Admin permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub admin_added_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_admin_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Admin permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub admin_removed_permissions: Option<PermissionOverrideBits>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionOverrideBits(u64);

impl From<PermissionOverrideBits> for u64 {
    fn from(value: PermissionOverrideBits) -> Self {
        value.0
    }
}

fn parse_member_permission_bits_arg(
    raw: &str,
) -> std::result::Result<PermissionOverrideBits, String> {
    parse_permission_bits_from_named_set(raw, CLI_MEMBER_NAMED_PERMISSIONS)
}

fn parse_admin_permission_bits_arg(
    raw: &str,
) -> std::result::Result<PermissionOverrideBits, String> {
    parse_permission_bits_from_named_set(raw, CLI_ADMIN_NAMED_PERMISSIONS)
}

fn parse_permission_bits_from_named_set(
    raw: &str,
    named_permissions: &[(&str, u64)],
) -> std::result::Result<PermissionOverrideBits, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("permission override must not be empty".to_string());
    }

    if let Ok(bits) = trimmed.parse::<u64>() {
        reject_unknown_permission_bits(bits, named_permissions)?;
        return Ok(PermissionOverrideBits(bits));
    }

    let names = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed).map_err(|error| {
            format!("permission JSON array must contain permission names: {error}")
        })?
    } else {
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    if names.is_empty() {
        return Err("permission override must include at least one permission name".to_string());
    }

    let mut bits = 0_u64;
    for name in names {
        let canonical = name.replace('-', "_").to_ascii_lowercase();
        let Some((_, bit)) = named_permissions
            .iter()
            .find(|(permission_name, _)| *permission_name == canonical)
        else {
            let allowed = named_permissions
                .iter()
                .map(|(permission_name, _)| *permission_name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "unknown permission name '{name}'. Allowed: {allowed}"
            ));
        };
        bits |= *bit;
    }

    reject_unknown_permission_bits(bits, named_permissions)?;

    Ok(PermissionOverrideBits(bits))
}

fn reject_unknown_permission_bits(
    bits: u64,
    named_permissions: &[(&str, u64)],
) -> std::result::Result<(), String> {
    let allowed_mask = named_permissions
        .iter()
        .fold(0_u64, |mask, (_, bit)| mask | *bit);
    let invalid = bits & !allowed_mask;
    if invalid == 0 {
        return Ok(());
    }

    let allowed = named_permissions
        .iter()
        .map(|(permission_name, _)| *permission_name)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "permission override contains bits outside this role bitspace (unknown bits 0x{invalid:x}). Allowed: {allowed}"
    ))
}

#[derive(Debug, Args)]
pub struct RoomMemberAddArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum, default_value_t = CliRoomMemberRole::Member)]
    pub role: CliRoomMemberRole,

    #[arg(long)]
    pub notify: bool,
}

#[derive(Debug, Args)]
pub struct RoomMemberKickArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_name = "SECONDS")]
    pub kick_cooldown_seconds: i64,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_settings_scope")
        .args(["room_id", "room_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct RoomSettingsScopeArgs {
    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: Option<String>,

    #[arg(long = "room-id", value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id_flag: Option<String>,
}

impl RoomSettingsScopeArgs {
    fn resolved_room_id(&self) -> Result<&str> {
        self.room_id
            .as_deref()
            .or(self.room_id_flag.as_deref())
            .ok_or_else(|| anyhow!("room settings requires ROOM_ID or --room-id"))
    }
}

#[derive(Debug, Args)]
pub struct RoomSettingsGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,
}

#[derive(Debug, Args)]
pub struct RoomSettingsUpdateArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,

    /// Partial JSON object patch merged onto the current room settings before submission
    #[arg(long)]
    pub settings_json: String,
}

#[derive(Debug, Args)]
pub struct RoomSettingsResetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_password_mode")
        .args(["new_password", "clear"])
        .required(true)
        .multiple(false)
))]
pub struct RoomSetPasswordArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(
        long = "password",
        value_name = "PASSWORD",
        group = "room_password_mode"
    )]
    pub new_password: Option<String>,

    #[arg(long, default_value_t = false, group = "room_password_mode")]
    pub clear: bool,
}

#[derive(Debug, Args)]
pub struct RoomBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomUnbanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Debug, Args)]
pub struct RoomDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_batch_ids")
        .args(["room_ids", "room_id_flags"])
        .required(true)
        .multiple(true)
))]
pub struct RoomBatchBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", num_args = 1.., group = "room_batch_ids")]
    pub room_ids: Vec<String>,

    #[arg(long = "room-id", value_name = "ROOM_ID", num_args = 1.., group = "room_batch_ids")]
    pub room_id_flags: Vec<String>,

    #[arg(long)]
    pub reason: Option<String>,
}

impl RoomBatchBanArgs {
    fn resolved_room_ids(&self) -> Vec<String> {
        self.room_ids
            .iter()
            .chain(self.room_id_flags.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_batch_delete_ids")
        .args(["room_ids", "room_id_flags"])
        .required(true)
        .multiple(true)
))]
pub struct RoomBatchDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", num_args = 1.., group = "room_batch_delete_ids")]
    pub room_ids: Vec<String>,

    #[arg(
        long = "room-id",
        value_name = "ROOM_ID",
        num_args = 1..,
        group = "room_batch_delete_ids"
    )]
    pub room_id_flags: Vec<String>,
}

impl RoomBatchDeleteArgs {
    fn resolved_room_ids(&self) -> Vec<String> {
        self.room_ids
            .iter()
            .chain(self.room_id_flags.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Args)]
pub struct RoomPlaybackGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub playback_client_profile: PlaybackClientProfileArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("playback_target")
        .args(["media_id", "playlist_id"])
        .required(true)
        .multiple(false)
))]
pub struct RoomPlaybackStartArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, group = "playback_target")]
    pub media_id: Option<String>,

    #[arg(long, group = "playback_target")]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub target_json: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackStopArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliPlaybackStateUpdateType {
    Play,
    Pause,
    Seek,
    Speed,
}

impl CliPlaybackStateUpdateType {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Play => synctv_proto::client::PlaybackUpdateType::Play as i32,
            Self::Pause => synctv_proto::client::PlaybackUpdateType::Pause as i32,
            Self::Seek => synctv_proto::client::PlaybackUpdateType::Seek as i32,
            Self::Speed => synctv_proto::client::PlaybackUpdateType::Speed as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct RoomPlaybackStateUpdateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback position in seconds. Required for `seek`.
    #[arg(long, value_name = "SECONDS")]
    pub position: Option<f64>,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackSeekArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Playback position in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub position: f64,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackSpeedArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: f64,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback position in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub position: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}

#[derive(Debug, Args)]
pub struct RoomStreamListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomStreamSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct RoomStreamKickArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub media_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistCommand {
    #[command(subcommand)]
    pub command: PlaylistSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistSubcommand {
    /// List playlists under a room or parent playlist
    List(PlaylistListArgs),
    /// Get a playlist by ID
    Get(PlaylistGetArgs),
    /// Create a playlist as a specific real user
    Create(PlaylistCreateArgs),
    /// Update playlist name
    Update(PlaylistUpdateArgs),
    /// Move a playlist before or after a sibling
    Move(PlaylistMoveArgs),
    /// Delete a playlist
    Delete(PlaylistDeleteArgs),
    /// Create provider-backed dynamic playlists with typed provider arguments
    Provider(PlaylistProviderCommand),
}

#[derive(Debug, Args)]
pub struct PlaylistListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub parent_id: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long)]
    pub source_provider: Option<String>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub dynamic_only: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliPlaylistSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,

    #[arg(long, value_enum, default_value_t = CliResourceAvailabilityFilter::All)]
    pub availability: CliResourceAvailabilityFilter,
}

#[derive(Debug, Args)]
pub struct PlaylistGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,
}

#[derive(Debug, Args)]
pub struct PlaylistCreateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    #[arg(long)]
    pub source_provider: Option<String>,

    #[arg(long)]
    pub source_config_json: Option<String>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistUpdateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistMoveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long, conflicts_with = "after_playlist_id")]
    pub before_playlist_id: Option<String>,

    #[arg(long, conflicts_with = "before_playlist_id")]
    pub after_playlist_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistDeleteArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MediaCommand {
    #[command(subcommand)]
    pub command: MediaSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaSubcommand {
    /// List media and child playlists under the room root or a playlist
    List(MediaListArgs),
    /// Add media as a specific real user using any configured provider instance or direct_url source config
    Add(MediaAddArgs),
    /// Add a direct HTTP(S) media URL
    AddUrl(MediaAddUrlArgs),
    /// Update media name
    Update(MediaEditArgs),
    /// Delete a media item
    Delete(MediaDeleteArgs),
    /// Reorder media in place or move media into another static playlist
    Move(MediaMoveArgs),
    /// Add provider-backed media with typed provider arguments
    Provider(MediaProviderCommand),
}

#[derive(Debug, Args)]
pub struct MediaListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub target_json: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long)]
    pub source_provider: Option<String>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliMediaSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,

    /// Force upstream provider directory cache refresh when listing a dynamic playlist
    #[arg(long, default_value_t = false)]
    pub refresh: bool,

    #[arg(long, value_enum, default_value_t = CliResourceAvailabilityFilter::All)]
    pub availability: CliResourceAvailabilityFilter,
}

#[derive(Debug, Args)]
pub struct MediaAddUrlArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub url: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaAddArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub source_provider: String,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub source_config_json: String,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaEditArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct MediaDeleteArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MediaMoveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Media ID to move. Repeat this flag to move multiple media items in order.
    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        action = ArgAction::Append,
        required_unless_present = "all_from_scope"
    )]
    pub media_ids: Vec<String>,

    /// Move every media item from the room root or the source playlist scope.
    #[arg(long, default_value_t = false)]
    pub all_from_scope: bool,

    /// Source static playlist when using --all-from-scope. Omit for the room root scope.
    #[arg(long, requires = "all_from_scope")]
    pub from_playlist_id: Option<String>,

    /// Target static playlist. Omit to keep media in the current scope.
    #[arg(long)]
    pub to_playlist_id: Option<String>,

    /// Insert before this media in the target scope. Omit both anchors to append to the target scope.
    #[arg(long, conflicts_with = "after_media_id")]
    pub before_media_id: Option<String>,

    /// Insert after this media in the target scope. Omit both anchors to append to the target scope.
    #[arg(long, conflicts_with = "before_media_id")]
    pub after_media_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct SettingsListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsGetArgs {
    pub group: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsUpdateArgs {
    pub group: String,

    /// Setting override in key=value form. Repeat for multiple fields.
    #[arg(long = "set", value_name = "KEY=VALUE", required = true, num_args = 1..)]
    pub entries: Vec<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsTestEmailArgs {
    pub to: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SystemStatsArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SliceCacheStatsArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Args)]
pub struct SliceCachePurgeArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Args)]
pub struct SliceCacheEvictExpiredArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Clone, Default, Args)]
pub struct SliceCacheTargetArgs {
    /// Query or manage slice cache on a specific cluster node through the connected management endpoint
    #[arg(long, value_name = "NODE_ID", conflicts_with = "all_nodes")]
    pub node_id: Option<String>,

    /// Query or manage slice cache on the connected node and all reachable cluster nodes
    #[arg(long)]
    pub all_nodes: bool,
}

#[derive(Debug, Args)]
pub struct SystemStreamListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub room_id: Option<String>,

    #[command(flatten)]
    pub user: StreamUserFilterArgs,

    #[arg(long)]
    pub node_id: Option<String>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliActiveStreamSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct SystemStreamKickArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long)]
    pub room_id: String,

    #[arg(long)]
    pub media_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProviderAvailableArgs {
    #[arg(long)]
    pub provider_type: Option<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBackendsArgs {
    pub provider_type: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderListArgs {
    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub provider_type: Option<String>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub enabled: Option<bool>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub tls: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliProviderSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAddArgs {
    pub name: String,
    #[arg(value_name = "PROVIDER_ENDPOINT")]
    pub provider_endpoint: String,

    #[arg(long)]
    pub comment: Option<String>,

    #[arg(long, default_value_t = 10)]
    pub timeout_seconds: u32,

    #[arg(long, default_value_t = false)]
    pub tls: bool,

    #[arg(long, default_value_t = false)]
    pub insecure_tls: bool,

    #[arg(long = "provider", value_name = "PROVIDER_TYPE", required = true, num_args = 1..)]
    pub providers: Vec<String>,

    /// Shared secret used to authenticate against a remote provider server
    #[arg(long)]
    pub jwt_secret: Option<String>,

    /// Custom PEM CA bundle used when connecting to a TLS-enabled provider endpoint
    #[arg(long)]
    pub custom_ca: Option<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderUpdateArgs {
    pub name: String,

    #[arg(long = "provider-endpoint")]
    pub provider_endpoint: Option<String>,

    #[arg(long)]
    pub comment: Option<String>,

    #[arg(long, default_value_t = false)]
    pub clear_comment: bool,

    #[arg(long)]
    pub timeout_seconds: Option<u32>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub tls: Option<bool>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub insecure_tls: Option<bool>,

    #[arg(long = "provider", value_name = "PROVIDER_TYPE")]
    pub providers: Vec<String>,

    /// Replace the shared secret used to authenticate against a remote provider server
    #[arg(long, conflicts_with = "clear_jwt_secret")]
    pub jwt_secret: Option<String>,

    /// Clear the stored remote provider shared secret
    #[arg(long, default_value_t = false, conflicts_with = "jwt_secret")]
    pub clear_jwt_secret: bool,

    /// Replace the custom PEM CA bundle for TLS provider endpoints
    #[arg(long, conflicts_with = "clear_custom_ca")]
    pub custom_ca: Option<String>,

    /// Clear the stored custom PEM CA bundle
    #[arg(long, default_value_t = false, conflicts_with = "custom_ca")]
    pub clear_custom_ca: bool,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDeleteArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderReconnectArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderEnableArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDisableArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistCommand {
    #[command(subcommand)]
    pub command: ProviderAlistSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderAlistSubcommand {
    /// Log a user into Alist and persist the credential bind
    Login(ProviderAlistLoginArgs),
    /// List directory contents using a saved Alist bind
    List(ProviderAlistListArgs),
    /// Search files and directories using a saved Alist bind
    Search(ProviderAlistSearchArgs),
    /// Show the current Alist account info for a saved bind
    Me(ProviderAlistGetMeArgs),
    /// Remove a saved Alist bind
    Logout(ProviderAlistLogoutArgs),
    /// List saved Alist binds for a user
    Binds(ProviderAlistBindsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderEmbyCommand {
    #[command(subcommand)]
    pub command: ProviderEmbySubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderEmbySubcommand {
    /// Log a user into Emby/Jellyfin and persist the credential bind
    Login(ProviderEmbyLoginArgs),
    /// List Emby-compatible library items using a saved bind
    List(ProviderEmbyListArgs),
    /// Show the current Emby-compatible account info for a saved bind
    Me(ProviderEmbyGetMeArgs),
    /// Remove a saved Emby-compatible bind
    Logout(ProviderEmbyLogoutArgs),
    /// List saved Emby-compatible binds for a user
    Binds(ProviderEmbyBindsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliCommand {
    #[command(subcommand)]
    pub command: ProviderBilibiliSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderBilibiliSubcommand {
    /// Parse a Bilibili URL, using the user's global bind when available
    Parse(ProviderBilibiliParseArgs),
    /// Generate a QR code for Bilibili login
    LoginQr(ProviderBilibiliLoginQrArgs),
    /// Poll the QR code login status and persist the bind on success
    CheckQr(ProviderBilibiliCheckQrArgs),
    /// Start a Bilibili SMS login session and return Geetest parameters
    StartSmsLogin(ProviderBilibiliStartSmsLoginArgs),
    /// Send a Bilibili SMS verification code
    SendSms(ProviderBilibiliSendSmsArgs),
    /// Log in with Bilibili SMS and persist the bind
    LoginSms(ProviderBilibiliLoginSmsArgs),
    /// Show the current Bilibili account info for the user's global bind
    Me(ProviderBilibiliGetUserInfoArgs),
    /// Remove a saved Bilibili bind
    Logout(ProviderBilibiliLogoutArgs),
    /// List saved Bilibili binds for a user
    Binds(ProviderBilibiliBindsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderRtmpCommand {
    #[command(subcommand)]
    pub command: ProviderRtmpSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderRtmpSubcommand {
    /// Create a single-use RTMP publish key for a media item as a specific real user
    CreatePublishKey(ProviderRtmpPublishKeyArgs),
    /// Get the active RTMP stream state for one room media item
    GetStreamInfo(ProviderRtmpGetStreamInfoArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ProviderServiceInstanceArgs {
    /// Explicit provider instance name. Omit to use the default backend for that provider type.
    #[arg(long = "instance-name")]
    pub instance_name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderServiceRemoteActorArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderBoundCredentialArgs {
    /// Stored provider credential server identifier
    #[arg(long)]
    pub server_id: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("alist_login_credential")
        .args(["password", "hashed_password"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderAlistLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Alist server base URL
    #[arg(long)]
    pub host: String,

    /// Alist account username used for the remote login
    #[arg(long = "account-username", value_name = "ACCOUNT_USERNAME")]
    pub account_username: String,

    /// Plaintext Alist password. Prefer --hashed-password when available.
    #[arg(long, group = "alist_login_credential")]
    pub password: Option<String>,

    /// Pre-hashed Alist password accepted by the Alist login API
    #[arg(long, group = "alist_login_credential")]
    pub hashed_password: Option<String>,

    /// Current Alist TOTP/2FA code. This is not persisted.
    #[arg(long = "otp-code")]
    pub otp_code: Option<String>,

    /// Alist TOTP secret used to generate future 2FA codes for automatic token refresh.
    #[arg(long = "otp-secret")]
    pub otp_secret: Option<String>,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Directory path to list. Use `/` for the root.
    #[arg(long)]
    pub path: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: u64,

    #[arg(long, default_value_t = 50)]
    pub per_page: u64,

    #[arg(long, default_value_t = false)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct ProviderAlistSearchArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Parent directory path to search under. Use `/` for the root.
    #[arg(long)]
    pub parent: String,

    /// Search keywords.
    #[arg(long)]
    pub keywords: String,

    /// Search scope: 0 = all, 1 = directories, 2 = files.
    #[arg(long, default_value_t = 0)]
    pub scope: u64,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: u64,

    #[arg(long, default_value_t = 50)]
    pub per_page: u64,
}

#[derive(Debug, Args)]
pub struct ProviderAlistGetMeArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistLogoutArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("emby_login_credential")
        .args(["password", "api_key"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderEmbyLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Emby/Jellyfin server base URL
    #[arg(long)]
    pub host: String,

    /// Target Emby/Jellyfin account username to bind
    #[arg(long)]
    pub account_username: String,

    /// Emby/Jellyfin account password. Conflicts with --api-key.
    #[arg(long, group = "emby_login_credential")]
    pub password: Option<String>,

    /// Emby/Jellyfin API key. Conflicts with --password.
    #[arg(long, group = "emby_login_credential")]
    pub api_key: Option<String>,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

fn alist_login_credential(
    args: &ProviderAlistLoginArgs,
) -> Result<synctv_proto::providers::alist::login_request::Credential> {
    if let Some(password) = &args.password {
        return Ok(
            synctv_proto::providers::alist::login_request::Credential::Password(password.clone()),
        );
    }
    if let Some(hashed_password) = &args.hashed_password {
        return Ok(
            synctv_proto::providers::alist::login_request::Credential::HashedPassword(
                hashed_password.clone(),
            ),
        );
    }
    bail!("Alist login requires --password or --hashed-password")
}

fn emby_login_credential(
    args: &ProviderEmbyLoginArgs,
) -> Result<synctv_proto::providers::emby::login_request::Credential> {
    if let Some(password) = &args.password {
        return Ok(
            synctv_proto::providers::emby::login_request::Credential::Password(password.clone()),
        );
    }
    if let Some(api_key) = &args.api_key {
        return Ok(
            synctv_proto::providers::emby::login_request::Credential::ApiKey(api_key.clone()),
        );
    }
    bail!("Emby login requires --password or --api-key")
}

#[derive(Debug, Args)]
pub struct ProviderEmbyListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Library path or parent item identifier to list. Use an empty string for the root if needed.
    #[arg(long)]
    pub path: String,

    #[arg(long, default_value_t = 0)]
    pub start_index: u64,

    #[arg(long, default_value_t = 50)]
    pub limit: u64,

    /// Optional fuzzy search term applied by the provider backend
    #[arg(long)]
    pub search_term: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProviderEmbyGetMeArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderEmbyLogoutArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderEmbyBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliParseArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// Bilibili page URL to parse
    pub url: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliCheckQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// QR login polling key returned by `login-qr`
    #[arg(long)]
    pub key: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliStartSmsLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliSendSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Mobile phone number in the format expected by the backend provider
    #[arg(long)]
    pub phone: String,

    /// Signed SMS login session token returned by `start-sms-login`
    #[arg(long)]
    pub session_token: String,

    /// Geetest validate result produced by a frontend captcha widget
    #[arg(long)]
    pub validate: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// SMS verification code
    #[arg(long)]
    pub code: String,

    /// Signed SMS login session token returned by `send-sms`
    #[arg(long)]
    pub session_token: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliGetUserInfoArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLogoutArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("provider_rtmp_media_ref")
        .args(["media_id", "media_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderRtmpPublishKeyArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long = "room-id")]
    pub room_id: String,

    #[arg(
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_media_ref"
    )]
    pub media_id: Option<String>,

    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_media_ref"
    )]
    pub media_id_flag: Option<String>,
}

impl ProviderRtmpPublishKeyArgs {
    fn resolved_media_id(&self) -> Result<&str> {
        self.media_id
            .as_deref()
            .or(self.media_id_flag.as_deref())
            .ok_or_else(|| {
                anyhow!("provider rtmp create-publish-key requires MEDIA_ID or --media-id")
            })
    }
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("provider_rtmp_stream_media_ref")
        .args(["media_id", "media_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderRtmpGetStreamInfoArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long = "room-id")]
    pub room_id: String,

    #[arg(
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_stream_media_ref"
    )]
    pub media_id: Option<String>,

    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_stream_media_ref"
    )]
    pub media_id_flag: Option<String>,
}

impl ProviderRtmpGetStreamInfoArgs {
    fn resolved_media_id(&self) -> Result<&str> {
        self.media_id
            .as_deref()
            .or(self.media_id_flag.as_deref())
            .ok_or_else(|| anyhow!("provider rtmp get-stream-info requires MEDIA_ID or --media-id"))
    }
}

#[derive(Debug, Args)]
pub struct PlaylistProviderCommand {
    #[command(subcommand)]
    pub command: PlaylistProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistProviderSubcommand {
    /// Create an Alist-backed dynamic playlist
    Alist(PlaylistProviderAlistArgs),
    /// Create an Emby-compatible dynamic playlist
    Emby(PlaylistProviderEmbyArgs),
}

#[derive(Debug, Args)]
pub struct MediaProviderCommand {
    #[command(subcommand)]
    pub command: MediaProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaProviderSubcommand {
    /// Add an Alist-backed media item
    Alist(MediaProviderAlistArgs),
    /// Add an Emby-compatible media item
    Emby(MediaProviderEmbyArgs),
    /// Add a Bilibili-backed media item
    Bilibili(MediaProviderBilibiliCommand),
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliCommand {
    #[command(subcommand)]
    pub command: MediaProviderBilibiliSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaProviderBilibiliSubcommand {
    /// Add a regular Bilibili video or multi-part page
    Video(MediaProviderBilibiliVideoArgs),
    /// Add a Bilibili PGC episode
    Pgc(MediaProviderBilibiliPgcArgs),
    /// Add a Bilibili live room
    Live(MediaProviderBilibiliLiveArgs),
}

#[derive(Debug, Args)]
pub struct PlaylistProviderAlistArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    /// Alist folder path used as the dynamic playlist root
    #[arg(long)]
    pub path: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    /// Saved Alist credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    /// Explicit provider instance name to store alongside the playlist
    #[arg(long)]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistProviderEmbyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    /// Root Emby-compatible item identifier used as the dynamic playlist source
    #[arg(long)]
    pub item_id: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    /// Saved Emby-compatible credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Explicit provider instance name to store alongside the playlist
    #[arg(long)]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderAlistArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Alist file path
    #[arg(long)]
    pub path: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Saved Alist credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderEmbyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Emby-compatible media item identifier
    #[arg(long)]
    pub item_id: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Saved Emby-compatible credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("bilibili_video_ref")
        .args(["bvid", "aid"])
        .required(true)
        .multiple(false)
))]
pub struct BilibiliVideoRefArgs {
    #[arg(long, group = "bilibili_video_ref")]
    pub bvid: Option<String>,

    #[arg(long, group = "bilibili_video_ref")]
    pub aid: Option<u64>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliVideoArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[command(flatten)]
    pub video: BilibiliVideoRefArgs,

    /// Bilibili content page `cid`
    #[arg(long)]
    pub cid: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliPgcArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Bilibili PGC episode identifier
    #[arg(long)]
    pub epid: u64,

    /// Bilibili content page `cid`
    #[arg(long)]
    pub cid: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliLiveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Bilibili live room identifier
    #[arg(long)]
    pub room_live_id: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

pub async fn execute(cli: Cli) -> Result<()> {
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Serve(serve) => Box::pin(execute_serve(serve)).await,
        Commands::Stop(stop) => execute_stop(stop).await,
        Commands::Config(config) => execute_config(config),
        Commands::Db(db) => execute_db(db).await,
        Commands::User(user) => execute_user(user).await,
        Commands::Room(room) => execute_room(room).await,
        Commands::Review(review) => execute_review(review).await,
        Commands::Ban(ban) => execute_ban(ban).await,
        Commands::Playlist(playlist) => execute_playlist(playlist).await,
        Commands::Media(media) => execute_media(media).await,
        Commands::Provider(provider) => execute_provider(provider).await,
        Commands::Settings(settings) => execute_settings(settings).await,
        Commands::System(system) => execute_system(system).await,
        Commands::SliceCache(slice_cache) => execute_slice_cache(slice_cache).await,
        Commands::Completion(args) => execute_completion(&args),
        Commands::Version => {
            println!("{}", version_string());
            Ok(())
        }
    }
}

fn apply_root_global_overrides(mut cli: Cli) -> Cli {
    let root = cli.global.clone();
    match &mut cli.command {
        Commands::Serve(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::Stop(args) => merge_remote_access_args(&mut args.remote, &root),
        Commands::Config(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::Db(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::User(command) => merge_user_command_globals(command, &root),
        Commands::Room(command) => merge_room_command_globals(command, &root),
        Commands::Review(command) => merge_review_command_globals(command, &root),
        Commands::Ban(command) => merge_ban_command_globals(command, &root),
        Commands::Playlist(command) => merge_playlist_command_globals(command, &root),
        Commands::Media(command) => merge_media_command_globals(command, &root),
        Commands::Provider(command) => merge_provider_command_globals(command, &root),
        Commands::Settings(command) => merge_settings_command_globals(command, &root),
        Commands::System(command) => merge_system_command_globals(command, &root),
        Commands::SliceCache(command) => merge_slice_cache_command_globals(command, &root),
        Commands::Completion(_) | Commands::Version => {}
    }
    cli
}

fn merge_remote_access_args(remote: &mut RemoteAccessArgs, root: &GlobalConfigArgs) {
    remote.global = remote.global.merged_with_parent(root);
}

fn merge_review_command_globals(command: &mut ReviewCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ReviewSubcommand::UserRegistration(command) => match &mut command.command {
            ReviewUserRegistrationSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewUserRegistrationSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewUserRegistrationSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        ReviewSubcommand::RoomCreation(command) => match &mut command.command {
            ReviewRoomCreationSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomCreationSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomCreationSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        ReviewSubcommand::RoomJoin(command) => match &mut command.command {
            ReviewRoomJoinSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomJoinSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomJoinSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
    }
}

fn merge_ban_command_globals(command: &mut BanCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        BanSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
    }
}

fn merge_room_scoped_remote_args(room: &mut RoomScopedRemoteArgs, root: &GlobalConfigArgs) {
    merge_remote_access_args(&mut room.remote, root);
}

fn merge_user_command_globals(command: &mut UserCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        UserSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Unban(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetRole(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetUsername(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Rooms(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Preferences(command) => match &mut command.command {
            UserPreferencesSubcommand::Get(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            UserPreferencesSubcommand::Set(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        UserSubcommand::Admin(command) => match &mut command.command {
            UserAdminSubcommand::Grant(args) => merge_remote_access_args(&mut args.remote, root),
            UserAdminSubcommand::Revoke(args) => merge_remote_access_args(&mut args.remote, root),
            UserAdminSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        },
        UserSubcommand::Batch(command) => match &mut command.command {
            UserBatchSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
            UserBatchSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_room_command_globals(command: &mut RoomCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        RoomSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::TransferOwner(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Unban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Settings(command) => match &mut command.command {
            RoomSettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
            RoomSettingsSubcommand::Update(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            RoomSettingsSubcommand::Reset(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::Member(command) => match &mut command.command {
            RoomMemberSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomMemberSubcommand::Add(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomMemberSubcommand::SetPermissions(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomMemberSubcommand::Kick(args) => merge_room_scoped_remote_args(&mut args.room, root),
        },
        RoomSubcommand::Playback(command) => match &mut command.command {
            RoomPlaybackSubcommand::Get(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Start(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Play(args) | RoomPlaybackSubcommand::Pause(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Seek(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Speed(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Stop(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
        },
        RoomSubcommand::Stream(command) => match &mut command.command {
            RoomStreamSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomStreamSubcommand::Kick(args) => merge_room_scoped_remote_args(&mut args.room, root),
        },
        RoomSubcommand::Batch(command) => match &mut command.command {
            RoomBatchSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
            RoomBatchSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_playlist_command_globals(command: &mut PlaylistCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        PlaylistSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Get(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Create(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Update(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Move(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Delete(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Provider(command) => match &mut command.command {
            PlaylistProviderSubcommand::Alist(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            PlaylistProviderSubcommand::Emby(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
        },
    }
}

fn merge_media_command_globals(command: &mut MediaCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        MediaSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Add(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::AddUrl(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Update(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Delete(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Move(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Provider(command) => match &mut command.command {
            MediaProviderSubcommand::Alist(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            MediaProviderSubcommand::Emby(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            MediaProviderSubcommand::Bilibili(command) => match &mut command.command {
                MediaProviderBilibiliSubcommand::Video(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
                MediaProviderBilibiliSubcommand::Pgc(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
                MediaProviderBilibiliSubcommand::Live(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
            },
        },
    }
}

fn merge_provider_command_globals(command: &mut ProviderCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ProviderSubcommand::Available(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Backends(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Reconnect(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Enable(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Disable(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Alist(command) => match &mut command.command {
            ProviderAlistSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Search(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Emby(command) => match &mut command.command {
            ProviderEmbySubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Bilibili(command) => match &mut command.command {
            ProviderBilibiliSubcommand::Parse(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::LoginQr(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::CheckQr(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::StartSmsLogin(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::SendSms(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::LoginSms(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Rtmp(command) => match &mut command.command {
            ProviderRtmpSubcommand::CreatePublishKey(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderRtmpSubcommand::GetStreamInfo(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
    }
}

fn merge_settings_command_globals(command: &mut SettingsCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SettingsSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::TestEmail(args) => merge_remote_access_args(&mut args.remote, root),
    }
}

fn merge_system_command_globals(command: &mut SystemCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SystemSubcommand::Stats(args) => merge_remote_access_args(&mut args.remote, root),
        SystemSubcommand::Stream(command) => match &mut command.command {
            SystemStreamSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            SystemStreamSubcommand::Kick(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_slice_cache_command_globals(command: &mut SliceCacheCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SliceCacheSubcommand::Stats(args) => merge_remote_access_args(&mut args.remote, root),
        SliceCacheSubcommand::Purge(args) => merge_remote_access_args(&mut args.remote, root),
        SliceCacheSubcommand::EvictExpired(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
    }
}

async fn execute_serve(args: ServeArgs) -> Result<()> {
    let context = CliConfigContext::new(args.global.clone());
    let config = context.strict_validated_config()?;
    switch_process_working_dir_to_data_dir(&config)?;

    crate::install_panic_hook(config.logging.backtrace);
    let _log_guard = synctv_core::logging::init_logging(&config.logging)?;

    if args.dry_run {
        tracing::info!("Configuration and logging initialized successfully");
        tracing::info!("Dry run requested, not starting server");
        return Ok(());
    }

    tracing::info!("SyncTV server starting...");
    tracing::info!("API address: {}", config.api_address());

    let app = Box::pin(Application::build(config)).await?;
    Box::pin(app.run()).await
}

fn switch_process_working_dir_to_data_dir(config: &synctv_core::Config) -> Result<()> {
    let data_dir = PathBuf::from(config.data_dir.trim());

    std::fs::create_dir_all(&data_dir).with_context(|| {
        format!(
            "failed to create data_dir {} before switching working directory",
            absolute_display_path(&data_dir)
        )
    })?;

    std::env::set_current_dir(&data_dir).with_context(|| {
        format!(
            "failed to switch working directory to data_dir {}",
            absolute_display_path(&data_dir)
        )
    })?;

    Ok(())
}

fn local_api_probe_address(config: &synctv_core::Config) -> String {
    let host = match config.server.host.trim() {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "::1".to_string(),
        host if host.contains(':') && !host.starts_with('[') => format!("[{host}]"),
        host => host.to_string(),
    };
    format!("{host}:{}", config.server.port)
}

async fn execute_stop(args: StopArgs) -> Result<()> {
    let session = connect_remote_access(&args.remote).await?;
    let mut client = session.management_client();
    let mut stream = management_unary_response(
        "stop server",
        client.stop_server(management_proto::StopServerRequest {
            mode: if args.force {
                management_proto::ShutdownMode::Force as i32
            } else {
                management_proto::ShutdownMode::Graceful as i32
            },
        }),
    )
    .await?;

    let mut events = Vec::new();
    let mut saw_terminal = false;
    let mut last_stage = None;
    loop {
        match management_stream_item(
            "stop server",
            MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT,
            stream.message(),
        )
        .await
        {
            Ok(Some(event)) => {
                let stage = management_proto::StopServerStage::try_from(event.stage).ok();
                let message = event.message.trim().to_string();
                if args.remote.output == RemoteOutputFormat::Human && !message.is_empty() {
                    println!("{message}");
                }
                events.push(StopServerEventOutput {
                    stage: stage.map_or_else(
                        || format!("UNKNOWN_STAGE_{}", event.stage),
                        stop_server_stage_name,
                    ),
                    message,
                    terminal: event.terminal,
                });
                last_stage = stage;
                if event.terminal {
                    saw_terminal = true;
                    break;
                }
            }
            Ok(None) => {
                synthesize_stop_completion_if_needed(
                    args.remote.output,
                    &mut last_stage,
                    &mut saw_terminal,
                    &mut events,
                );
                if stop_stream_end_can_be_treated_as_success(last_stage) {
                    print_stop_output(
                        args.remote.output,
                        &StopServerOutput {
                            success: true,
                            terminal_received: saw_terminal,
                            final_stage: last_stage.map(stop_server_stage_name),
                            events,
                        },
                    )?;
                    return Ok(());
                }
                break;
            }
            Err(error) => {
                synthesize_stop_completion_if_needed(
                    args.remote.output,
                    &mut last_stage,
                    &mut saw_terminal,
                    &mut events,
                );
                if stop_stream_disconnect_can_be_treated_as_success(last_stage, &error) {
                    print_stop_output(
                        args.remote.output,
                        &StopServerOutput {
                            success: true,
                            terminal_received: saw_terminal,
                            final_stage: last_stage.map(stop_server_stage_name),
                            events,
                        },
                    )?;
                    return Ok(());
                }
                return Err(error);
            }
        }
    }

    synthesize_stop_completion_if_needed(
        args.remote.output,
        &mut last_stage,
        &mut saw_terminal,
        &mut events,
    );

    if !saw_terminal {
        bail!("management stop stream ended before terminal shutdown status")
    }

    print_stop_output(
        args.remote.output,
        &StopServerOutput {
            success: true,
            terminal_received: saw_terminal,
            final_stage: last_stage.map(stop_server_stage_name),
            events,
        },
    )?;

    Ok(())
}

const fn stop_stream_end_can_be_treated_as_success(
    last_stage: Option<management_proto::StopServerStage>,
) -> bool {
    matches!(
        last_stage,
        Some(management_proto::StopServerStage::Finalizing)
    )
}

fn synthesize_stop_completion_if_needed(
    format: RemoteOutputFormat,
    last_stage: &mut Option<management_proto::StopServerStage>,
    saw_terminal: &mut bool,
    events: &mut Vec<StopServerEventOutput>,
) {
    if !matches!(
        *last_stage,
        Some(management_proto::StopServerStage::Finalizing)
    ) {
        return;
    }

    if format == RemoteOutputFormat::Human {
        println!("shutdown complete");
    }

    events.push(StopServerEventOutput {
        stage: stop_server_stage_name(management_proto::StopServerStage::Completed),
        message: "shutdown complete".to_string(),
        terminal: true,
    });
    *last_stage = Some(management_proto::StopServerStage::Completed);
    *saw_terminal = true;
}

fn stop_stream_disconnect_can_be_treated_as_success(
    last_stage: Option<management_proto::StopServerStage>,
    error: &anyhow::Error,
) -> bool {
    if !stop_stream_end_can_be_treated_as_success(last_stage) {
        return false;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("broken pipe")
        || message.contains("connection closed")
        || message.contains("error reading a body from connection")
        || message.contains("transport error")
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct StopServerEventOutput {
    stage: String,
    message: String,
    terminal: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct StopServerOutput {
    success: bool,
    terminal_received: bool,
    final_stage: Option<String>,
    events: Vec<StopServerEventOutput>,
}

fn stop_server_stage_name(stage: management_proto::StopServerStage) -> String {
    stage.as_str_name().to_ascii_lowercase()
}

fn print_stop_output(format: RemoteOutputFormat, output: &StopServerOutput) -> Result<()> {
    match format {
        RemoteOutputFormat::Human => Ok(()),
        RemoteOutputFormat::Json => print_json(output),
        RemoteOutputFormat::Yaml => print_yaml(output),
    }
}

fn execute_config(config_command: ConfigCommand) -> Result<()> {
    let context = CliConfigContext::new(config_command.global.clone());
    match config_command.command {
        ConfigSubcommand::Validate(args) => {
            let config = if args.strict {
                context.strict_validated_config()?
            } else {
                context.validated_config()?
            };
            println!("Configuration is valid");
            println!("API address: {}", config.api_address());
            Ok(())
        }
        ConfigSubcommand::Show(args) => {
            let config = context.config()?;
            let rendered = render_config_for_display(&config)?;
            match args.output {
                ConfigOutputFormat::Yaml => print_yaml(&rendered)?,
                ConfigOutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&rendered)?);
                }
                ConfigOutputFormat::Toml => print_toml(&rendered)?,
            }
            Ok(())
        }
    }
}

async fn execute_db(db_command: DbCommand) -> Result<()> {
    let context = CliConfigContext::new(db_command.global.clone());
    let config = context.validated_config()?;
    crate::install_panic_hook(config.logging.backtrace);
    let _log_guard = synctv_core::logging::init_logging(&config.logging)?;

    match db_command.command {
        DbSubcommand::Migrate(args) => {
            let pool = synctv_core::bootstrap::init_database(&config).await?.pool;

            crate::migrations::run_migrations(&pool).await?;

            let migrations_status = crate::migrations::inspect_embedded_migrations(&pool).await?;
            let output = DatabaseCliOutput::migrate(&config, &migrations_status);
            print_database_output(args.output, &output)?;
            pool.close().await;
            Ok(())
        }
        DbSubcommand::Status(args) => {
            let pool = synctv_core::bootstrap::init_database(&config).await?.pool;
            sqlx::query!("SELECT 1 AS ok").fetch_one(&pool).await?;
            let migrations_status = crate::migrations::inspect_embedded_migrations(&pool).await?;
            let output = DatabaseCliOutput::status(&config, &migrations_status);
            print_database_output(args.output, &output)?;
            pool.close().await;
            Ok(())
        }
    }
}

async fn execute_review(review_command: ReviewCommand) -> Result<()> {
    match review_command.command {
        ReviewSubcommand::UserRegistration(command) => match command.command {
            ReviewUserRegistrationSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list user registration reviews",
                    list_user_registration_reviews,
                    management_proto::ListUserRegistrationReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        search: args.search.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewUserRegistrationSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve user registration review",
                    approve_user_registration_review,
                    management_proto::ApproveUserRegistrationReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewUserRegistrationSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject user registration review",
                    reject_user_registration_review,
                    management_proto::RejectUserRegistrationReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        ReviewSubcommand::RoomCreation(command) => match command.command {
            ReviewRoomCreationSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room creation reviews",
                    list_room_creation_reviews,
                    management_proto::ListRoomCreationReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        requested_by: args.requested_by.unwrap_or_default(),
                        search: args.search.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomCreationSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve room creation review",
                    approve_room_creation_review,
                    management_proto::ApproveRoomCreationReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomCreationSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject room creation review",
                    reject_room_creation_review,
                    management_proto::RejectRoomCreationReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        ReviewSubcommand::RoomJoin(command) => match command.command {
            ReviewRoomJoinSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room join reviews",
                    list_room_join_reviews,
                    management_proto::ListRoomJoinReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        room_id: args.room_id.unwrap_or_default(),
                        user_id: args.user_id.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomJoinSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve room join review",
                    approve_room_join_review,
                    management_proto::ApproveRoomJoinReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomJoinSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject room join review",
                    reject_room_join_review,
                    management_proto::RejectRoomJoinReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
    }
}

async fn execute_ban(ban_command: BanCommand) -> Result<()> {
    match ban_command.command {
        BanSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list ban records",
                list_ban_records,
                management_proto::ListBanRecordsRequest {
                    page: args.page,
                    page_size: args.page_size,
                    target_type: args.target.map_or(
                        synctv_proto::admin::BanTargetType::Unspecified as i32,
                        CliBanTarget::to_proto,
                    ),
                    active: args.active,
                    user_id: args.user_id.unwrap_or_default(),
                    room_id: args.room_id.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}

async fn execute_user(user_command: UserCommand) -> Result<()> {
    let UserCommand { command } = user_command;
    match command {
        UserSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list users",
                list_users,
                management_proto::ListUsersRequest {
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::UserStatus::Unspecified as i32,
                        CliUserStatus::to_proto,
                    ),
                    role: args.role.map_or(
                        synctv_proto::common::UserRole::Unspecified as i32,
                        CliUserRole::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    sort_by: args.sort_by.map_or(
                        management_proto::UserListSortBy::CreatedAt as i32,
                        CliUserSortField::to_proto
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    is_banned: None,
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get user",
                get_user,
                management_proto::GetUserRequest {
                    user: Some(args.user.to_management_proto()?),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Preferences(preferences_command) => match preferences_command.command {
            UserPreferencesSubcommand::Get(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get user preferences",
                    get_user_preferences,
                    management_proto::GetUserPreferencesRequest {
                        user: Some(args.user.to_management_proto()?),
                    }
                )?;
                print_humanized_structured_output(args.remote.output, &response)
            }
            UserPreferencesSubcommand::Set(args) => {
                let args = *args;
                let notifications = parse_cli_optional_json(
                    "notification preferences",
                    args.notifications_json.as_deref(),
                )?;
                if args.two_factor_enabled.is_none() && notifications.is_none() {
                    bail!("No user preference fields provided");
                }

                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "update user preferences",
                    update_user_preferences,
                    management_proto::UpdateUserPreferencesRequest {
                        user: Some(args.user.to_management_proto()?),
                        two_factor_enabled: args.two_factor_enabled,
                        notifications,
                    }
                )?;
                print_humanized_structured_output(args.remote.output, &response)
            }
        },
        UserSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create user",
                create_user,
                management_proto::CreateUserRequest {
                    username: args.username,
                    email: args.email.unwrap_or_default(),
                    role: args.role.to_proto(),
                    status: args.status.to_proto(),
                    password: args.password.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete user",
                delete_user,
                management_proto::DeleteUserRequest {
                    user: Some(args.user.to_management_proto()?),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Ban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "ban user",
                ban_user,
                management_proto::BanUserRequest {
                    user: Some(args.user.to_management_proto()?),
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Unban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "unban user",
                unban_user,
                management_proto::UnbanUserRequest {
                    user: Some(args.user.to_management_proto()?),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::SetRole(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let role = args.resolved_role()?;
            let response = management_unary_call!(
                session,
                "update user role",
                update_user_role,
                management_proto::UpdateUserRoleRequest {
                    user: Some(args.user.to_management_proto()?),
                    role: role.to_proto(),
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: true,
                user: response.user,
            })
        }
        UserSubcommand::SetPassword(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "set user password",
                set_user_password,
                management_proto::SetUserPasswordRequest {
                    user: Some(args.user.to_management_proto()?),
                    password: args.password,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: response.success,
                user: response.user,
            })
        }
        UserSubcommand::SetUsername(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update user username",
                update_user_username,
                management_proto::UpdateUserUsernameRequest {
                    user: Some(args.user.to_management_proto()?),
                    new_username: args.new_username,
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: true,
                user: response.user,
            })
        }
        UserSubcommand::Rooms(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get user rooms",
                get_user_rooms,
                management_proto::GetUserRoomsRequest {
                    user: Some(args.user.to_management_proto()?),
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::RoomStatus::Unspecified as i32,
                        CliRoomStatus::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    is_banned: args.is_banned,
                    sort_by: args.sort_by.map_or(
                        management_proto::RoomListSortBy::CreatedAt as i32,
                        CliRoomSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Admin(admin_command) => match admin_command.command {
            UserAdminSubcommand::Grant(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "add admin",
                    add_admin,
                    management_proto::AddAdminRequest {
                        user: Some(args.user.to_management_proto()?),
                    }
                )?;
                args.remote.print_output(&response)
            }
            UserAdminSubcommand::Revoke(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "remove admin",
                    remove_admin,
                    management_proto::RemoveAdminRequest {
                        user: Some(args.user.to_management_proto()?),
                    }
                )?;
                args.remote.print_output(&response)
            }
            UserAdminSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list admins",
                    list_admins,
                    management_proto::ListAdminsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::UserListSortBy::CreatedAt as i32,
                            CliUserSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        UserSubcommand::Batch(batch_command) => match batch_command.command {
            UserBatchSubcommand::Ban(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch ban users",
                    batch_ban_users,
                    management_proto::BatchBanUsersRequest {
                        users: batch_user_refs_to_proto(args.usernames, args.user_ids),
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            UserBatchSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch delete users",
                    batch_delete_users,
                    management_proto::BatchDeleteUsersRequest {
                        users: batch_user_refs_to_proto(args.usernames, args.user_ids),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
    }
}

async fn execute_room(room_command: RoomCommand) -> Result<()> {
    let RoomCommand { command } = room_command;
    match command {
        RoomSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create room",
                create_room,
                management_proto::CreateRoomRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    name: args.name,
                    settings_json: raw_optional_bytes(args.settings_json.as_deref()),
                    description: args.description.unwrap_or_default(),
                    password: args.password.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list rooms",
                list_rooms,
                management_proto::ListRoomsRequest {
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::RoomStatus::Unspecified as i32,
                        CliRoomStatus::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    creator: args.creator.to_management_proto()?,
                    is_banned: args.is_banned,
                    sort_by: args.sort_by.map_or(
                        management_proto::RoomListSortBy::CreatedAt as i32,
                        CliRoomSortField::to_proto
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get room",
                get_room,
                management_proto::GetRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::TransferOwner(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "transfer room ownership",
                transfer_room_ownership,
                management_proto::TransferRoomOwnershipRequest {
                    room_id: args.room_id,
                    actor: Some(args.actor.to_management_proto()?),
                    new_owner: Some(args.new_owner.to_management_proto()?),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Settings(settings_command) => match settings_command.command {
            RoomSettingsSubcommand::Get(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room settings",
                    get_room_settings,
                    management_proto::GetRoomSettingsRequest {
                        room_id: args.room.resolved_room_id()?.to_string(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomSettingsSubcommand::Update(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "update room settings",
                    update_room_settings,
                    management_proto::UpdateRoomSettingsRequest {
                        room_id: args.room.resolved_room_id()?.to_string(),
                        settings_json: args.settings_json.into_bytes(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomSettingsSubcommand::Reset(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reset room settings",
                    reset_room_settings,
                    management_proto::ResetRoomSettingsRequest {
                        room_id: args.room.resolved_room_id()?.to_string(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Member(member_command) => match member_command.command {
            RoomMemberSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room members",
                    get_room_members,
                    management_proto::GetRoomMembersRequest {
                        room_id: args.resolved_room_id()?.to_string(),
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        sort_by: args.sort_by.map_or(
                            management_proto::RoomMemberListSortBy::JoinedAt as i32,
                            CliRoomMemberSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomMemberSubcommand::Add(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "add room member",
                    add_member,
                    management_proto::AddMemberRequest {
                        room_id: args.room.room_id,
                        user: Some(args.user.to_management_proto()?),
                        role: args.role.to_proto(),
                        notify: args.notify,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::SetPermissions(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "update room member permissions",
                    update_member_permissions,
                    management_proto::UpdateMemberPermissionsRequest {
                        room_id: args.room.room_id,
                        user: Some(args.user.to_management_proto()?),
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        added_permissions: args.added_permissions.map_or(0, Into::into),
                        removed_permissions: args.removed_permissions.map_or(0, Into::into),
                        admin_added_permissions: args.admin_added_permissions.map_or(0, Into::into),
                        admin_removed_permissions: args
                            .admin_removed_permissions
                            .map_or(0, Into::into),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "kick room member",
                    kick_member,
                    management_proto::KickMemberRequest {
                        room_id: args.room.room_id,
                        user: Some(args.user.to_management_proto()?),
                        kick_cooldown_seconds: args.kick_cooldown_seconds,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
        },
        RoomSubcommand::Playback(playback_command) => match playback_command.command {
            RoomPlaybackSubcommand::Get(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room playback",
                    get_playback,
                    management_proto::GetPlaybackRequest {
                        room_id: args.room.room_id,
                        playback_client_profile: args.playback_client_profile.to_proto(),
                    }
                )?;
                let output = build_get_playback_cli_output(response, &args.room.remote.global);
                args.room.remote.print_output(&output)
            }
            RoomPlaybackSubcommand::Start(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let room_id = args.room.room_id;
                let media_id = args.media_id;
                let playlist_id = args.playlist_id;
                let target_json = args.target_json;
                management_unary_call!(
                    session,
                    "start room playback",
                    start_playback,
                    management_proto::StartPlaybackRequest {
                        room_id: room_id.clone(),
                        media_id: media_id.clone().unwrap_or_default(),
                        playlist_id: playlist_id.clone().unwrap_or_default(),
                        target_json: raw_optional_bytes(target_json.as_deref()),
                    }
                )?;
                args.room.remote.print_output(&PlaybackStartCliOutput {
                    success: true,
                    room_id,
                    media_id,
                    playlist_id,
                })
            }
            RoomPlaybackSubcommand::Play(args) => {
                let playing = Some(args.playing.unwrap_or(true));
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Play,
                    playing,
                    args.position,
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Pause(args) => {
                let playing = Some(args.playing.unwrap_or(false));
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Pause,
                    playing,
                    args.position,
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Seek(args) => {
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Seek,
                    args.playing,
                    Some(args.position),
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Speed(args) => {
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Speed,
                    args.playing,
                    args.position,
                    Some(args.speed),
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Stop(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let room_id = args.room.room_id;
                management_unary_call!(
                    session,
                    "stop room playback",
                    stop_playback,
                    management_proto::StopPlaybackRequest {
                        room_id: room_id.clone(),
                    }
                )?;
                args.room.remote.print_output(&PlaybackStopCliOutput {
                    success: true,
                    room_id,
                })
            }
        },
        RoomSubcommand::Stream(stream_command) => match stream_command.command {
            RoomStreamSubcommand::List(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room streams",
                    list_room_streams,
                    management_proto::ListRoomStreamsRequest {
                        room_id: args.room.room_id,
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::RoomStreamListSortBy::MediaId as i32,
                            CliRoomStreamSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomStreamSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "kick room stream",
                    kick_room_stream,
                    management_proto::KickRoomStreamRequest {
                        room_id: args.room.room_id,
                        media_id: args.media_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
        },
        RoomSubcommand::Batch(batch_command) => match batch_command.command {
            RoomBatchSubcommand::Ban(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch ban rooms",
                    batch_ban_rooms,
                    management_proto::BatchBanRoomsRequest {
                        room_ids: args.resolved_room_ids(),
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomBatchSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch delete rooms",
                    batch_delete_rooms,
                    management_proto::BatchDeleteRoomsRequest {
                        room_ids: args.resolved_room_ids(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::SetPassword(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update room password",
                update_room_password,
                management_proto::UpdateRoomPasswordRequest {
                    room_id: args.room_id,
                    new_password: args.new_password,
                    clear: args.clear,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Ban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "ban room",
                ban_room,
                management_proto::BanRoomRequest {
                    room_id: args.room_id,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Unban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "unban room",
                unban_room,
                management_proto::UnbanRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete room",
                delete_room,
                management_proto::DeleteRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}

async fn execute_playlist(playlist_command: PlaylistCommand) -> Result<()> {
    let PlaylistCommand { command } = playlist_command;
    match command {
        PlaylistSubcommand::List(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "list playlists",
                list_playlists,
                management_proto::ListPlaylistsRequest {
                    room_id: args.room.room_id,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    page: args.page,
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider:
                        normalized_optional_cli_value(args.source_provider.as_deref(),)
                            .unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    dynamic_only: args.dynamic_only,
                    sort_by: args.sort_by.map_or(
                        management_proto::PlaylistListSortBy::Position as i32,
                        CliPlaylistSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    availability: args.availability.to_proto(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Get(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "get playlist",
                get_playlist,
                management_proto::GetPlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Create(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "create playlist",
                create_playlist,
                management_proto::CreatePlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    source_provider:
                        normalized_optional_cli_value(args.source_provider.as_deref(),)
                            .unwrap_or_default(),
                    source_config_json: raw_optional_bytes(args.source_config_json.as_deref()),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Update(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "update playlist",
                update_playlist,
                management_proto::UpdatePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    name: args.name,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Move(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let anchor = match (args.before_playlist_id, args.after_playlist_id) {
                (Some(id), None) if !id.trim().is_empty() => {
                    Some(management_proto::move_playlist_request::Anchor::BeforePlaylistId(id))
                }
                (None, Some(id)) if !id.trim().is_empty() => {
                    Some(management_proto::move_playlist_request::Anchor::AfterPlaylistId(id))
                }
                _ => None,
            };
            let response = management_unary_call!(
                session,
                "move playlist",
                move_playlist,
                management_proto::MovePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    anchor,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "delete playlist",
                delete_playlist,
                management_proto::DeletePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    force: args.force,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Provider(command) => execute_playlist_provider(command).await,
    }
}

async fn execute_media(media_command: MediaCommand) -> Result<()> {
    let MediaCommand { command } = media_command;
    match command {
        MediaSubcommand::List(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "list media",
                list_media,
                management_proto::ListMediaRequest {
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    target_json: raw_optional_bytes(args.target_json.as_deref()),
                    page: args.page,
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider:
                        normalized_optional_cli_value(args.source_provider.as_deref(),)
                            .unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    sort_by: args.sort_by.map_or(
                        management_proto::MediaListSortBy::Position as i32,
                        CliMediaSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    refresh: args.refresh,
                    availability: args.availability.to_proto(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Add(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add media",
                add_media,
                management_proto::AddMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    source_provider: args.source_provider,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref()
                    ),
                    source_config_json: args.source_config_json.into_bytes(),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::AddUrl(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add direct url media",
                add_direct_url_media,
                management_proto::AddDirectUrlMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    url: args.url,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Update(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "edit media",
                edit_media,
                management_proto::EditMediaRequest {
                    room_id: args.room.room_id,
                    media_id: args.media_id,
                    name: args.name,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "delete media",
                delete_media,
                management_proto::DeleteMediaRequest {
                    room_id: args.room.room_id,
                    media_id: args.media_id,
                    force: args.force,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Move(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let anchor = match (args.before_media_id, args.after_media_id) {
                (Some(id), None) if !id.trim().is_empty() => Some(
                    management_proto::move_media_request::Anchor::BeforeMediaId(id),
                ),
                (None, Some(id)) if !id.trim().is_empty() => Some(
                    management_proto::move_media_request::Anchor::AfterMediaId(id),
                ),
                _ => None,
            };
            let response = management_unary_call!(
                session,
                "move media",
                move_media,
                management_proto::MoveMediaRequest {
                    room_id: args.room.room_id,
                    media_ids: args.media_ids,
                    source_playlist_id: normalized_optional_cli_value(
                        args.from_playlist_id.as_deref(),
                    ),
                    target_playlist_id: normalized_optional_cli_value(
                        args.to_playlist_id.as_deref(),
                    ),
                    all_from_scope: args.all_from_scope,
                    anchor,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Provider(command) => execute_media_provider(command).await,
    }
}

async fn execute_room_playback_state_update(
    room: RoomScopedRemoteArgs,
    update_type: CliPlaybackStateUpdateType,
    playing: Option<bool>,
    position: Option<f64>,
    speed: Option<f64>,
    version: Option<i64>,
) -> Result<()> {
    let session = connect_remote_access(&room.remote).await?;
    let response = management_unary_call!(
        session,
        "update room playback state",
        update_playback_state,
        management_proto::UpdatePlaybackStateRequest {
            room_id: room.room_id,
            update: Some(synctv_proto::client::UpdatePlaybackStateRequest {
                r#type: update_type.to_proto(),
                playing,
                position,
                speed,
                version,
                expected_media_id: None,
                expected_playlist_id: None,
                expected_target_hash: None,
            }),
        }
    )?;
    room.remote.print_output(&response)
}

async fn execute_provider(provider_command: ProviderCommand) -> Result<()> {
    let ProviderCommand { command } = provider_command;
    match command {
        ProviderSubcommand::Available(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list available provider instances",
                list_available_provider_instances,
                synctv_proto::providers::common::ListAvailableProviderInstancesRequest {
                    provider_type: normalized_optional_cli_value(args.provider_type.as_deref())
                        .unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Backends(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list provider backends",
                list_provider_backends,
                synctv_proto::providers::common::ListProviderBackendsRequest {
                    provider_type: args.provider_type,
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list provider instances",
                list_provider_instances,
                synctv_proto::providers::common::ListProviderInstancesRequest {
                    page: args.page,
                    page_size: args.page_size,
                    provider_type: normalized_optional_cli_value(args.provider_type.as_deref())
                        .unwrap_or_default(),
                    search: args.search.unwrap_or_default(),
                    enabled: args.enabled,
                    tls: args.tls,
                    sort_by: args.sort_by.map_or(
                        synctv_proto::providers::common::ProviderInstanceListSortBy::CreatedAt
                            as i32,
                        CliProviderSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "add provider instance",
                add_provider_instance,
                synctv_proto::providers::common::AddProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment: args.comment.unwrap_or_default(),
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: normalized_provider_types(&args.providers),
                    jwt_secret: normalized_optional_cli_value(args.jwt_secret.as_deref()),
                    custom_ca: normalized_optional_cli_value(args.custom_ca.as_deref()),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update provider instance",
                update_provider_instance,
                synctv_proto::providers::common::UpdateProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment: args.comment,
                    clear_comment: args.clear_comment.then_some(true),
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: normalized_provider_types(&args.providers),
                    jwt_secret: normalized_optional_cli_value(args.jwt_secret.as_deref()),
                    custom_ca: normalized_optional_cli_value(args.custom_ca.as_deref()),
                    clear_jwt_secret: args.clear_jwt_secret.then_some(true),
                    clear_custom_ca: args.clear_custom_ca.then_some(true),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete provider instance",
                delete_provider_instance,
                synctv_proto::providers::common::DeleteProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Reconnect(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "reconnect provider instance",
                reconnect_provider_instance,
                synctv_proto::providers::common::ReconnectProviderInstanceRequest {
                    name: args.name,
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Enable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "enable provider instance",
                enable_provider_instance,
                synctv_proto::providers::common::EnableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Disable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "disable provider instance",
                disable_provider_instance,
                synctv_proto::providers::common::DisableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Alist(command) => execute_provider_alist(command).await,
        ProviderSubcommand::Emby(command) => execute_provider_emby(command).await,
        ProviderSubcommand::Bilibili(command) => execute_provider_bilibili(command).await,
        ProviderSubcommand::Rtmp(command) => execute_provider_rtmp(command).await,
    }
}

async fn execute_playlist_provider(command: PlaylistProviderCommand) -> Result<()> {
    match command.command {
        PlaylistProviderSubcommand::Alist(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "create alist dynamic playlist",
                create_alist_playlist,
                management_proto::CreateAlistPlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    path: args.path,
                    password: args.password.unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistProviderSubcommand::Emby(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "create emby dynamic playlist",
                create_emby_playlist,
                management_proto::CreateEmbyPlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    item_id: args.item_id,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                }
            )?;
            args.room.remote.print_output(&response)
        }
    }
}

async fn execute_media_provider(command: MediaProviderCommand) -> Result<()> {
    match command.command {
        MediaProviderSubcommand::Alist(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add alist media",
                add_alist_media,
                management_proto::AddAlistMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    path: args.path,
                    password: args.password.unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderSubcommand::Emby(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add emby media",
                add_emby_media,
                management_proto::AddEmbyMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    item_id: args.item_id,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderSubcommand::Bilibili(command) => {
            execute_media_provider_bilibili(command).await
        }
    }
}

async fn execute_media_provider_bilibili(command: MediaProviderBilibiliCommand) -> Result<()> {
    match command.command {
        MediaProviderBilibiliSubcommand::Video(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili video media",
                add_bilibili_video_media,
                management_proto::AddBilibiliVideoMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    bvid: args.video.bvid.unwrap_or_default(),
                    aid: args.video.aid,
                    cid: args.cid,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderBilibiliSubcommand::Pgc(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili pgc media",
                add_bilibili_pgc_media,
                management_proto::AddBilibiliPgcMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    epid: args.epid,
                    cid: args.cid,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderBilibiliSubcommand::Live(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili live media",
                add_bilibili_live_media,
                management_proto::AddBilibiliLiveMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    room_live_id: args.room_live_id,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
    }
}

async fn execute_provider_alist(command: ProviderAlistCommand) -> Result<()> {
    match command.command {
        ProviderAlistSubcommand::Login(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let credential = alist_login_credential(&args)?;
            let response = management_unary_call!(
                session,
                "alist login",
                alist_login,
                management_proto::AlistLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::LoginRequest {
                        host: args.host,
                        username: args.account_username,
                        credential: Some(credential),
                        otp_code: args.otp_code.unwrap_or_default(),
                        otp_secret: args.otp_secret.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::List(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist list",
                alist_list,
                management_proto::AlistListRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::ListRequest {
                        server_id: args.bind.server_id,
                        path: args.path,
                        password: args.password.unwrap_or_default(),
                        page: args.page,
                        per_page: args.per_page,
                        refresh: args.refresh,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Search(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist search",
                alist_search,
                management_proto::AlistSearchRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::SearchRequest {
                        server_id: args.bind.server_id,
                        parent: args.parent,
                        keywords: args.keywords,
                        scope: args.scope,
                        page: args.page,
                        per_page: args.per_page,
                        password: args.password.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist me",
                alist_get_me,
                management_proto::AlistGetMeRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::GetMeRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist logout",
                alist_logout,
                management_proto::AlistLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::LogoutRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist get binds",
                alist_get_binds,
                management_proto::AlistGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}

async fn execute_provider_emby(command: ProviderEmbyCommand) -> Result<()> {
    match command.command {
        ProviderEmbySubcommand::Login(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let credential = emby_login_credential(&args)?;
            let response = management_unary_call!(
                session,
                "emby login",
                emby_login,
                management_proto::EmbyLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::LoginRequest {
                        host: args.host,
                        username: args.account_username,
                        credential: Some(credential),
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::List(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby list",
                emby_list,
                management_proto::EmbyListRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::ListRequest {
                        server_id: args.bind.server_id,
                        path: args.path,
                        start_index: args.start_index,
                        limit: args.limit,
                        search_term: args.search_term.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby me",
                emby_get_me,
                management_proto::EmbyGetMeRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::GetMeRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby logout",
                emby_logout,
                management_proto::EmbyLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::LogoutRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby get binds",
                emby_get_binds,
                management_proto::EmbyGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}

async fn execute_provider_bilibili(command: ProviderBilibiliCommand) -> Result<()> {
    match command.command {
        ProviderBilibiliSubcommand::Parse(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili parse",
                bilibili_parse,
                management_proto::BilibiliParseRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::ParseRequest {
                        url: args.url,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::LoginQr(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili login qr",
                bilibili_login_qr,
                management_proto::BilibiliLoginQrRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LoginQrRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::CheckQr(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili check qr",
                bilibili_check_qr,
                management_proto::BilibiliCheckQrRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::CheckQrRequest {
                        key: args.key,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::StartSmsLogin(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili start sms login",
                bilibili_start_sms_login,
                management_proto::BilibiliStartSmsLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::StartSmsLoginRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::SendSms(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili send sms",
                bilibili_send_sms,
                management_proto::BilibiliSendSmsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::SendSmsRequest {
                        session_token: args.session_token,
                        phone: args.phone,
                        validate: args.validate,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::LoginSms(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili login sms",
                bilibili_login_sms,
                management_proto::BilibiliLoginSmsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LoginSmsRequest {
                        session_token: args.session_token,
                        code: args.code,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili me",
                bilibili_get_user_info,
                management_proto::BilibiliGetUserInfoRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::UserInfoRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili logout",
                bilibili_logout,
                management_proto::BilibiliLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LogoutRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili get binds",
                bilibili_get_binds,
                management_proto::BilibiliGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}

async fn execute_provider_rtmp(command: ProviderRtmpCommand) -> Result<()> {
    match command.command {
        ProviderRtmpSubcommand::CreatePublishKey(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let room_id = args.room_id.clone();
            let media_id = args.resolved_media_id()?.to_string();
            let response = management_unary_call!(
                session,
                "create rtmp publish key",
                create_publish_key,
                management_proto::CreatePublishKeyRequest {
                    actor: Some(actor_user_id),
                    room_id,
                    media_id,
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderRtmpSubcommand::GetStreamInfo(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let media_id = args.resolved_media_id()?.to_string();
            let response = management_unary_call!(
                session,
                "get rtmp stream info",
                get_stream_info,
                management_proto::GetStreamInfoRequest {
                    room_id: args.room_id,
                    media_id,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}

async fn execute_settings(settings_command: SettingsCommand) -> Result<()> {
    let SettingsCommand { command } = settings_command;
    match command {
        SettingsSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings",
                get_settings,
                management_proto::GetSettingsRequest {}
            )?;
            args.remote.print_output(&response)
        }
        SettingsSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings group",
                get_settings_group,
                management_proto::GetSettingsGroupRequest { group: args.group }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management settings group response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let settings = parse_setting_entries(&args.entries)?;
            let response = management_unary_call!(
                session,
                "update settings",
                update_settings,
                management_proto::UpdateSettingsRequest {
                    group: args.group,
                    settings,
                }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management update settings response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::TestEmail(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "send test email",
                send_test_email,
                management_proto::SendTestEmailRequest { to: args.to }
            )?;
            args.remote.print_output(&response)
        }
    }
}

async fn execute_system(system_command: SystemCommand) -> Result<()> {
    let SystemCommand { command } = system_command;
    match command {
        SystemSubcommand::Stats(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get system stats",
                get_system_stats,
                management_proto::GetSystemStatsRequest {}
            )?;
            args.remote.print_output(&response)
        }
        SystemSubcommand::Stream(stream_command) => match stream_command.command {
            SystemStreamSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list active streams",
                    list_active_streams,
                    management_proto::ListActiveStreamsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        room_id: args.room_id.unwrap_or_default(),
                        user: args.user.to_management_proto()?,
                        node_id: args.node_id.unwrap_or_default(),
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::ActiveStreamListSortBy::StartedAt as i32,
                            CliActiveStreamSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            SystemStreamSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let room_id = args.room_id;
                let media_id = args.media_id;
                let reason = args.reason;
                management_unary_call!(
                    session,
                    "kick active stream",
                    kick_stream,
                    management_proto::KickStreamRequest {
                        room_id: room_id.clone(),
                        media_id: media_id.clone(),
                        reason: reason.clone().unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&KickStreamCliOutput {
                    success: true,
                    room_id,
                    media_id,
                    reason,
                })
            }
        },
    }
}

async fn execute_slice_cache(slice_cache_command: SliceCacheCommand) -> Result<()> {
    let SliceCacheCommand { command } = slice_cache_command;
    match command {
        SliceCacheSubcommand::Stats(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get slice cache stats",
                get_slice_cache_stats,
                management_proto::GetSliceCacheStatsRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
        SliceCacheSubcommand::Purge(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "purge slice cache",
                purge_slice_cache,
                management_proto::PurgeSliceCacheRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
        SliceCacheSubcommand::EvictExpired(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "evict expired slice cache entries",
                evict_expired_slice_cache,
                management_proto::EvictExpiredSliceCacheRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}

async fn management_unary_response<T>(
    operation: &'static str,
    future: impl std::future::Future<Output = std::result::Result<tonic::Response<T>, tonic::Status>>,
) -> Result<T> {
    management_unary_response_with_timeout(operation, MANAGEMENT_UNARY_RPC_TIMEOUT, future).await
}

async fn management_unary_response_with_timeout<T>(
    operation: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = std::result::Result<tonic::Response<T>, tonic::Status>>,
) -> Result<T> {
    let response = tokio::time::timeout(timeout, future)
        .await
        .with_context(|| {
            format!(
                "management operation '{operation}' timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|status| format_management_status_error(operation, &status))?;
    Ok(response.into_inner())
}

async fn management_stream_item<T>(
    operation: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = std::result::Result<Option<T>, tonic::Status>>,
) -> Result<Option<T>> {
    tokio::time::timeout(timeout, future)
        .await
        .with_context(|| {
            format!(
                "management stream '{operation}' timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|status| format_management_status_error(operation, &status))
}

fn format_management_status_error(
    operation: &'static str,
    status: &tonic::Status,
) -> anyhow::Error {
    let message = status.message().trim();
    let detail = if message.is_empty() {
        operation.to_string()
    } else {
        message.to_string()
    };

    let rendered = match status.code() {
        tonic::Code::InvalidArgument => {
            format!("management {operation} failed: invalid request: {detail}")
        }
        tonic::Code::NotFound | tonic::Code::AlreadyExists | tonic::Code::Unknown => {
            format!("management {operation} failed: {detail}")
        }
        tonic::Code::PermissionDenied => {
            format!("management {operation} failed: permission denied: {detail}")
        }
        tonic::Code::Unauthenticated => {
            format!("management {operation} failed: authentication failed: {detail}")
        }
        tonic::Code::Unavailable => {
            format!("management {operation} failed: service unavailable: {detail}")
        }
        tonic::Code::DeadlineExceeded => {
            format!("management {operation} failed: deadline exceeded: {detail}")
        }
        tonic::Code::Aborted => {
            format!("management {operation} failed: operation aborted: {detail}")
        }
        tonic::Code::ResourceExhausted => {
            format!("management {operation} failed: resource exhausted: {detail}")
        }
        tonic::Code::Internal => format!("management {operation} failed: internal error"),
        _ => format!("management {operation} failed: {}: {detail}", status.code()),
    };

    anyhow!(rendered)
}

fn resolve_remote_endpoint(global: &GlobalConfigArgs) -> Option<String> {
    global
        .endpoint
        .as_deref()
        .map(str::to_string)
        .or_else(|| std::env::var("SYNCTV_MANAGEMENT_ENDPOINT").ok())
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn connect_remote_access(args: &RemoteAccessArgs) -> Result<RemoteAdminSession> {
    let context = RemoteCliContext::new(args);
    context.initialize_output_state()?;
    let options = context.connection_options(args)?;
    let session = RemoteAdminSession::connect(options).await?;
    if args.global.verbose > 0 {
        eprintln!(
            "Connected to remote management endpoint {}",
            session.endpoint()
        );
    }
    Ok(session)
}

async fn connect_provider_actor_access(
    access: &ProviderServiceRemoteActorArgs,
) -> Result<(RemoteAdminSession, management_proto::UserRef)> {
    let session = connect_remote_access(&access.remote).await?;
    Ok((session, access.actor.to_management_proto()?))
}

fn batch_user_refs_to_proto(
    usernames: Vec<String>,
    user_ids: Vec<String>,
) -> Vec<management_proto::UserRef> {
    usernames
        .into_iter()
        .map(|username| management_proto::UserRef {
            value: Some(management_proto::user_ref::Value::Username(username)),
        })
        .chain(
            user_ids
                .into_iter()
                .map(|user_id| management_proto::UserRef {
                    value: Some(management_proto::user_ref::Value::UserId(user_id)),
                }),
        )
        .collect()
}

fn infer_cli_api_base_url(global: &GlobalConfigArgs) -> Option<String> {
    if global.endpoint.is_some() {
        return None;
    }
    let config = CliConfigContext::new(global.clone()).config().ok()?;
    Some(format!("http://{}", local_api_probe_address(&config)))
}

fn absolutize_cli_url(raw: &str, api_base_url: Option<&str>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(parsed) = url::Url::parse(trimmed) {
        return Some(parsed.to_string());
    }

    let base = api_base_url?;
    let base = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    };
    let parsed_base = url::Url::parse(&base).ok()?;
    parsed_base.join(trimmed).ok().map(|url| url.to_string())
}

fn build_get_playback_cli_output(
    response: synctv_proto::client::GetPlaybackResponse,
    global: &GlobalConfigArgs,
) -> GetPlaybackCliOutput {
    let synctv_proto::client::GetPlaybackResponse {
        playback_state,
        playback,
    } = response;
    let api_base_url = infer_cli_api_base_url(global);
    let mut pull_urls = Vec::new();
    let mut default_pull_url = None;
    let mut default_absolute_pull_url = None;
    let mut hls_pull_url = None;
    let mut hls_absolute_pull_url = None;
    let mut flv_pull_url = None;
    let mut flv_absolute_pull_url = None;

    if let Some(playback) = playback.as_ref() {
        let mut modes = playback.playback_infos.iter().collect::<Vec<_>>();
        modes.sort_by_key(|(mode, _)| *mode);

        for (mode, info) in modes {
            for (index, playback_media) in info.medias.iter().enumerate() {
                let is_default = mode == &playback.default_mode
                    && i32::try_from(index)
                        .is_ok_and(|index| info.default_media_index == Some(index));
                let absolute_url = absolutize_cli_url(&playback_media.url, api_base_url.as_deref());
                let output = PlaybackPullUrlCliOutput {
                    mode: mode.clone(),
                    format: playback_media.format.clone(),
                    name: playback_media.name.clone(),
                    url: playback_media.url.clone(),
                    absolute_url: absolute_url.clone(),
                    default: is_default,
                    headers: playback_media.headers.clone(),
                    expire_at: playback_media.expire_at,
                };

                if is_default {
                    default_pull_url = Some(output.url.clone());
                    default_absolute_pull_url.clone_from(&output.absolute_url);
                }

                match playback_media.format.as_str() {
                    "m3u8" if hls_pull_url.is_none() => {
                        hls_pull_url = Some(output.url.clone());
                        hls_absolute_pull_url.clone_from(&output.absolute_url);
                    }
                    "flv" if flv_pull_url.is_none() => {
                        flv_pull_url = Some(output.url.clone());
                        flv_absolute_pull_url.clone_from(&output.absolute_url);
                    }
                    _ => {}
                }

                pull_urls.push(output);
            }
        }
    }

    let default_mode = playback
        .as_ref()
        .map(|playback| playback.default_mode.clone())
        .filter(|mode| !mode.is_empty());

    GetPlaybackCliOutput {
        playback_state,
        playback,
        default_mode,
        pull_urls,
        default_pull_url,
        default_absolute_pull_url,
        hls_pull_url,
        hls_absolute_pull_url,
        flv_pull_url,
        flv_absolute_pull_url,
    }
}

fn normalized_optional_cli_value(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn provider_service_instance_name(args: &ProviderServiceInstanceArgs) -> String {
    provider_instance_name_string(args.instance_name.as_deref())
}

fn provider_instance_name_string(raw: Option<&str>) -> String {
    normalized_optional_cli_value(raw).unwrap_or_default()
}

fn raw_optional_bytes(raw: Option<&str>) -> Vec<u8> {
    raw.map(str::as_bytes)
        .map_or_else(Vec::new, ToOwned::to_owned)
}

fn parse_cli_json<T>(label: &str, raw: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(raw).with_context(|| format!("Invalid {label} JSON"))
}

fn parse_cli_optional_json<T>(label: &str, raw: Option<&str>) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    raw.map(|value| parse_cli_json(label, value)).transpose()
}

fn parse_setting_entries(entries: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut settings = std::collections::HashMap::with_capacity(entries.len());
    for entry in entries {
        let Some((key, value)) = entry.split_once('=') else {
            bail!("invalid --set entry '{entry}': expected key=value");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("invalid --set entry '{entry}': key must not be empty");
        }
        if settings
            .insert(key.to_string(), value.to_string())
            .is_some()
        {
            bail!("duplicate --set entry for key '{key}'");
        }
    }
    Ok(settings)
}

fn normalized_provider_types(providers: &[String]) -> Vec<String> {
    providers
        .iter()
        .filter_map(|provider| normalized_optional_cli_value(Some(provider)))
        .collect()
}

pub fn version_string() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseCliAction {
    Status,
    Migrate,
}

#[derive(Debug, Clone, Serialize)]
struct DatabaseCliOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    database_connection: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration: Option<&'static str>,
    migration_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_detail: Option<String>,
    database_url: String,
    #[serde(skip)]
    action: DatabaseCliAction,
}

impl DatabaseCliOutput {
    fn status(
        config: &synctv_core::Config,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self::new(DatabaseCliAction::Status, config, migrations_status)
    }

    fn migrate(
        config: &synctv_core::Config,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self::new(DatabaseCliAction::Migrate, config, migrations_status)
    }

    fn new(
        action: DatabaseCliAction,
        config: &synctv_core::Config,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self {
            database_connection: (action == DatabaseCliAction::Status).then_some("ok"),
            migration: (action == DatabaseCliAction::Migrate).then_some("completed"),
            migration_status: migrations_status.label(),
            migration_detail: migrations_status.detail(),
            database_url: mask_connection_url(&config.database.url),
            action,
        }
    }

    fn human_header(&self) -> &'static str {
        match self.action {
            DatabaseCliAction::Status => "Database connection: OK",
            DatabaseCliAction::Migrate => "Database migration: completed",
        }
    }
}

fn print_database_output(format: RemoteOutputFormat, output: &DatabaseCliOutput) -> Result<()> {
    match format {
        RemoteOutputFormat::Human => {
            print!("{}", database_summary(output));
            Ok(())
        }
        RemoteOutputFormat::Json => print_json(output),
        RemoteOutputFormat::Yaml => print_yaml(output),
    }
}

fn database_summary(output: &DatabaseCliOutput) -> String {
    let mut lines = vec![
        output.human_header().to_string(),
        format!("Migration status: {}", output.migration_status),
    ];
    if let Some(detail) = output.migration_detail.as_deref() {
        lines.push(format!("Migration detail: {detail}"));
    }
    lines.push(format!("Database URL: {}", output.database_url));
    lines.push(String::new());
    lines.join("\n")
}

fn render_config_for_display(config: &synctv_core::Config) -> Result<Value> {
    let mut value = serde_json::to_value(config).context("config should serialize for display")?;
    redact_config_value(&mut value);
    Ok(value)
}

trait ToHuman {
    type Human: Serialize;

    fn to_human(&self) -> Self::Human;
}

impl ToHuman for String {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        self.clone()
    }
}

impl ToHuman for bool {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i32 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u32 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for f64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl<T> ToHuman for Option<T>
where
    T: ToHuman,
{
    type Human = Option<T::Human>;

    fn to_human(&self) -> Self::Human {
        self.as_ref().map(ToHuman::to_human)
    }
}

impl<T> ToHuman for Vec<T>
where
    T: ToHuman,
{
    type Human = Vec<T::Human>;

    fn to_human(&self) -> Self::Human {
        self.iter().map(ToHuman::to_human).collect()
    }
}

impl<K, V> ToHuman for std::collections::HashMap<K, V>
where
    K: Clone + Eq + std::hash::Hash + Serialize,
    V: ToHuman,
{
    type Human = std::collections::HashMap<K, V::Human>;

    fn to_human(&self) -> Self::Human {
        self.iter()
            .map(|(key, value)| (key.clone(), value.to_human()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
struct HumanAdminUser {
    id: String,
    username: String,
    email: String,
    role: String,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoom {
    id: String,
    name: String,
    created_by: String,
    status: String,
    settings: Value,
    created_at: String,
    member_count: i32,
    description: String,
    updated_at: String,
    is_banned: bool,
    availability: String,
    version: i64,
}

#[derive(Debug, Clone, Serialize)]
struct HumanAdminRoom {
    id: String,
    name: String,
    creator_id: String,
    creator_username: String,
    creator_status: String,
    status: String,
    settings: Value,
    member_count: i32,
    created_at: String,
    updated_at: String,
    description: String,
    is_banned: bool,
    version: i64,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomMember {
    room_id: String,
    user_id: String,
    username: String,
    role: String,
    permissions: u64,
    permission_names: Vec<String>,
    added_permissions: u64,
    added_permission_names: Vec<String>,
    removed_permissions: u64,
    removed_permission_names: Vec<String>,
    admin_added_permissions: u64,
    admin_added_permission_names: Vec<String>,
    admin_removed_permissions: u64,
    admin_removed_permission_names: Vec<String>,
    joined_at: String,
    is_online: bool,
    connection_count: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanProviderInstance {
    name: String,
    endpoint: String,
    comment: String,
    timeout_seconds: u32,
    tls: bool,
    insecure_tls: bool,
    providers: Vec<String>,
    enabled: bool,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaylist {
    id: String,
    room_id: String,
    name: String,
    parent_id: String,
    position: f64,
    is_dynamic: bool,
    source_provider: String,
    provider_instance_name: String,
    item_count: i32,
    created_at: String,
    updated_at: String,
    availability: String,
    version: i64,
    source_config: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanMedia {
    id: String,
    room_id: String,
    source_provider: String,
    name: String,
    metadata: Value,
    position: f64,
    added_at: String,
    creator_id: String,
    provider_instance_name: String,
    source_config: Value,
    availability: String,
    version: i64,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaybackState {
    room_id: String,
    playing_media_id: String,
    position: f64,
    speed: f64,
    is_playing: bool,
    updated_at: String,
    version: i64,
    playing_playlist_id: String,
    target_hash: String,
    target: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanSettingsGroup {
    name: String,
    settings: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanReviewRequest {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    username: String,
    email: String,
    signup_method: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomCreationReview {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    requested_by: String,
    requested_by_username: String,
    name: String,
    description: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomJoinReview {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    room_id: String,
    room_name: String,
    user_id: String,
    username: String,
    requested_role: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanBanRecord {
    id: String,
    target_type: String,
    user_id: String,
    username: String,
    room_id: String,
    room_name: String,
    banned_by: String,
    banned_by_username: String,
    reason: String,
    starts_at: String,
    ends_at: String,
    revoked_at: String,
    revoked_by: String,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HumanCreatePublishKeyResponse {
    publish_key: String,
    rtmp_url: String,
    stream_key: String,
    expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanStreamPublisherInfo {
    user_id: String,
    started_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanGetStreamInfoResponse {
    active: bool,
    publisher: Option<HumanStreamPublisherInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanStreamEntry {
    media_id: String,
    active: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HumanListRoomStreamsResponse {
    streams: Vec<HumanStreamEntry>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserResponse<T> {
    user: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserAuthFactors {
    password: bool,
    webauthn: bool,
    email: bool,
    eligible_count: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserPreferences {
    two_factor_enabled: bool,
    notifications: Option<HumanUserNotificationPreferences>,
    settings: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserNotificationPreferences {
    room_invitation_in_app: bool,
    room_event_in_app: bool,
    system_announcement_in_app: bool,
    room_invitation_email: bool,
    room_event_email: bool,
    system_announcement_email: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserPreferencesResponse<T> {
    user: Option<T>,
    preferences: Option<HumanUserPreferences>,
    auth_factors: Option<HumanUserAuthFactors>,
}

#[derive(Debug, Clone, Serialize)]
struct UserMutationCliOutput {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<synctv_proto::admin::AdminUser>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUserMutationCliOutput {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<HumanAdminUser>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUsersResponse<T> {
    users: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanAdminsResponse<T> {
    admins: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomResponse<T> {
    room: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomsResponse<T> {
    rooms: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomMembersResponse<T> {
    members: Vec<T>,
    total: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanMemberResponse<T> {
    member: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanProviderInstancesResponse<T> {
    instances: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanProviderNamesResponse {
    instances: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanProviderBackendsResponse {
    backends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanProviderInstanceResponse<T> {
    instance: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaylistResponse<T> {
    playlist: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaylistsResponse<T> {
    playlists: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanMediaResponse<T> {
    media: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanMediaBatchResponse<T> {
    moved_count: i32,
    media: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaylistItemsResponse<P, M> {
    playlists: Vec<P>,
    media: Vec<M>,
    total: i32,
    folder_count: i32,
    file_count: i32,
    dynamic_items: Vec<synctv_proto::client::PlaylistItem>,
    current_path: Vec<synctv_proto::client::PlaylistBrowsePathNode>,
    version: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanGetPlaybackResponse<T> {
    playback_state: Option<T>,
    playback: Option<synctv_proto::client::Playback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_mode: Option<String>,
    pull_urls: Vec<PlaybackPullUrlCliOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_absolute_pull_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanUpdatePlaybackStateResponse<T> {
    playback_state: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct PlaybackPullUrlCliOutput {
    mode: String,
    format: String,
    name: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    absolute_url: Option<String>,
    default: bool,
    headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expire_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct GetPlaybackCliOutput {
    playback_state: Option<synctv_proto::client::PlaybackState>,
    playback: Option<synctv_proto::client::Playback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_mode: Option<String>,
    pull_urls: Vec<PlaybackPullUrlCliOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_absolute_pull_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PlaybackStartCliOutput {
    success: bool,
    room_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    playlist_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PlaybackStopCliOutput {
    success: bool,
    room_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct KickStreamCliOutput {
    success: bool,
    room_id: String,
    media_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanSettingsGroupsResponse<T> {
    groups: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanSettingsGroupResponse<T> {
    group: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanGetRoomWithPlaybackResponse<R, P> {
    room: Option<R>,
    playback_state: Option<P>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanJoinRoomResponse<R, P, M> {
    room: Option<R>,
    playback_state: Option<P>,
    members: Vec<M>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanGetPlaylistResponse<T> {
    playlist: Option<T>,
    child_folder_count: i32,
    media_count: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanReviewRequestsResponse<T> {
    reviews: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
struct HumanApproveReviewRequestResponse<R, T> {
    review: Option<R>,
    result: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanApproveRoomJoinReviewResponse<R, M> {
    review: Option<R>,
    member: Option<M>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanApproveUserRegistrationReviewResponse<R, U> {
    review: Option<R>,
    user: Option<U>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRejectReviewRequestResponse<R> {
    review: Option<R>,
    success: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HumanBanRecordsResponse<T> {
    bans: Vec<T>,
    total: i32,
}

fn parse_json_bytes(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).unwrap_or_else(|_| Value::String("<invalid json>".into()))
    }
}

fn humanize_timestamp(raw: i64) -> String {
    if raw <= 0 {
        return "unset".to_string();
    }

    app_time::format_timestamp_secs_display(raw).unwrap_or_else(|| raw.to_string())
}

impl ToHuman for synctv_proto::admin::AdminUser {
    type Human = HumanAdminUser;

    fn to_human(&self) -> Self::Human {
        HumanAdminUser {
            id: self.id.clone(),
            username: self.username.clone(),
            email: self.email.clone(),
            role: humanize_user_role(i64::from(self.role)).unwrap_or_else(|| self.role.to_string()),
            status: humanize_user_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::client::Room {
    type Human = HumanRoom;

    fn to_human(&self) -> Self::Human {
        HumanRoom {
            id: self.id.clone(),
            name: self.name.clone(),
            created_by: self.created_by.clone(),
            status: humanize_room_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            settings: parse_json_bytes(&self.settings),
            created_at: humanize_timestamp(self.created_at),
            member_count: self.member_count,
            description: self.description.clone(),
            updated_at: humanize_timestamp(self.updated_at),
            is_banned: self.is_banned,
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
        }
    }
}

impl ToHuman for synctv_proto::admin::AdminRoom {
    type Human = HumanAdminRoom;

    fn to_human(&self) -> Self::Human {
        HumanAdminRoom {
            id: self.id.clone(),
            name: self.name.clone(),
            creator_id: self.creator_id.clone(),
            creator_username: self.creator_username.clone(),
            creator_status: humanize_user_status(i64::from(self.creator_status))
                .unwrap_or_else(|| self.creator_status.to_string()),
            status: humanize_room_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            settings: parse_json_bytes(&self.settings),
            member_count: self.member_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
            description: self.description.clone(),
            is_banned: self.is_banned,
            version: self.version,
        }
    }
}

impl ToHuman for synctv_proto::admin::UserRegistrationReview {
    type Human = HumanReviewRequest;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequest {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            username: self.username.clone(),
            email: self.email.clone(),
            signup_method: self.signup_method,
        }
    }
}

impl ToHuman for synctv_proto::admin::RoomCreationReview {
    type Human = HumanRoomCreationReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomCreationReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            requested_by: self.requested_by.clone(),
            requested_by_username: self.requested_by_username.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
        }
    }
}

impl ToHuman for synctv_proto::admin::RoomJoinReview {
    type Human = HumanRoomJoinReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomJoinReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            room_id: self.room_id.clone(),
            room_name: self.room_name.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            requested_role: humanize_room_member_role(i64::from(self.requested_role))
                .unwrap_or_else(|| self.requested_role.to_string()),
        }
    }
}

impl ToHuman for synctv_proto::client::RoomJoinReview {
    type Human = HumanRoomJoinReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomJoinReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            room_id: self.room_id.clone(),
            room_name: String::new(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            requested_role: humanize_room_member_role(i64::from(self.requested_role))
                .unwrap_or_else(|| self.requested_role.to_string()),
        }
    }
}

impl ToHuman for synctv_proto::admin::BanRecord {
    type Human = HumanBanRecord;

    fn to_human(&self) -> Self::Human {
        HumanBanRecord {
            id: self.id.clone(),
            target_type: synctv_proto::admin::BanTargetType::try_from(self.target_type)
                .map_or_else(
                    |_| self.target_type.to_string(),
                    |value| format!("{value:?}"),
                ),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            room_id: self.room_id.clone(),
            room_name: self.room_name.clone(),
            banned_by: self.banned_by.clone(),
            banned_by_username: self.banned_by_username.clone(),
            reason: self.reason.clone(),
            starts_at: humanize_timestamp(self.starts_at),
            ends_at: humanize_timestamp(self.ends_at),
            revoked_at: humanize_timestamp(self.revoked_at),
            revoked_by: self.revoked_by.clone(),
            is_active: self.is_active,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListUserRegistrationReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanReviewRequest>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveUserRegistrationReviewResponse {
    type Human = HumanApproveUserRegistrationReviewResponse<HumanReviewRequest, HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanApproveUserRegistrationReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            user: self.user.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::RejectUserRegistrationReviewResponse {
    type Human = HumanRejectReviewRequestResponse<HumanReviewRequest>;

    fn to_human(&self) -> Self::Human {
        HumanRejectReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomCreationReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomCreationReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveRoomCreationReviewResponse {
    type Human = HumanApproveReviewRequestResponse<HumanRoomCreationReview, HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanApproveReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            result: self.room.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::RejectRoomCreationReviewResponse {
    type Human = HumanRejectReviewRequestResponse<HumanRoomCreationReview>;

    fn to_human(&self) -> Self::Human {
        HumanRejectReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomJoinReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveRoomJoinReviewResponse {
    type Human = HumanApproveRoomJoinReviewResponse<HumanRoomJoinReview, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanApproveRoomJoinReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            member: self.member.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::RejectRoomJoinReviewResponse {
    type Human = HumanRejectReviewRequestResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanRejectReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomJoinReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::ApproveRoomJoinReviewResponse {
    type Human = HumanApproveRoomJoinReviewResponse<HumanRoomJoinReview, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanApproveRoomJoinReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            member: self.member.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::client::RejectRoomJoinReviewResponse {
    type Human = HumanRejectReviewRequestResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanRejectReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListBanRecordsResponse {
    type Human = HumanBanRecordsResponse<HumanBanRecord>;

    fn to_human(&self) -> Self::Human {
        HumanBanRecordsResponse {
            bans: self.bans.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::common::RoomMember {
    type Human = HumanRoomMember;

    fn to_human(&self) -> Self::Human {
        HumanRoomMember {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            role: humanize_room_member_role(i64::from(self.role))
                .unwrap_or_else(|| self.role.to_string()),
            permissions: self.permissions,
            permission_names: humanize_permission_bits(self.permissions),
            added_permissions: self.added_permissions,
            added_permission_names: humanize_named_permission_bits(
                self.added_permissions,
                CLI_MEMBER_NAMED_PERMISSIONS,
            ),
            removed_permissions: self.removed_permissions,
            removed_permission_names: humanize_named_permission_bits(
                self.removed_permissions,
                CLI_MEMBER_NAMED_PERMISSIONS,
            ),
            admin_added_permissions: self.admin_added_permissions,
            admin_added_permission_names: humanize_named_permission_bits(
                self.admin_added_permissions,
                CLI_ADMIN_NAMED_PERMISSIONS,
            ),
            admin_removed_permissions: self.admin_removed_permissions,
            admin_removed_permission_names: humanize_named_permission_bits(
                self.admin_removed_permissions,
                CLI_ADMIN_NAMED_PERMISSIONS,
            ),
            joined_at: humanize_timestamp(self.joined_at),
            is_online: self.is_online,
            connection_count: self.connection_count,
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderInstance {
    type Human = HumanProviderInstance;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstance {
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            comment: self.comment.clone(),
            timeout_seconds: self.timeout_seconds,
            tls: self.tls,
            insecure_tls: self.insecure_tls,
            providers: self.providers.clone(),
            enabled: self.enabled,
            status: humanize_provider_instance_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::StreamPublisherInfo {
    type Human = HumanStreamPublisherInfo;

    fn to_human(&self) -> Self::Human {
        HumanStreamPublisherInfo {
            user_id: self.user_id.clone(),
            started_at: humanize_timestamp(self.started_at),
        }
    }
}

impl ToHuman for synctv_proto::client::StreamEntry {
    type Human = HumanStreamEntry;

    fn to_human(&self) -> Self::Human {
        HumanStreamEntry {
            media_id: self.media_id.clone(),
            active: self.active,
        }
    }
}

impl ToHuman for synctv_proto::client::Playlist {
    type Human = HumanPlaylist;

    fn to_human(&self) -> Self::Human {
        HumanPlaylist {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            name: self.name.clone(),
            parent_id: self.parent_id.clone(),
            position: self.position,
            is_dynamic: self.is_dynamic,
            source_provider: self.source_provider.clone(),
            provider_instance_name: self.provider_instance_name.clone(),
            item_count: self.item_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
            source_config: parse_json_bytes(&self.source_config),
        }
    }
}

impl ToHuman for synctv_proto::client::Media {
    type Human = HumanMedia;

    fn to_human(&self) -> Self::Human {
        HumanMedia {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            source_provider: self.source_provider.clone(),
            name: self.name.clone(),
            metadata: parse_json_bytes(&self.metadata),
            position: self.position,
            added_at: humanize_timestamp(self.added_at),
            creator_id: self.creator_id.clone(),
            provider_instance_name: self.provider_instance_name.clone(),
            source_config: parse_json_bytes(&self.source_config),
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
        }
    }
}

impl ToHuman for synctv_proto::client::PlaybackState {
    type Human = HumanPlaybackState;

    fn to_human(&self) -> Self::Human {
        HumanPlaybackState {
            room_id: self.room_id.clone(),
            playing_media_id: self.playing_media_id.clone(),
            position: self.position,
            speed: self.speed,
            is_playing: self.is_playing,
            updated_at: humanize_timestamp(self.updated_at),
            version: self.version,
            playing_playlist_id: self.playing_playlist_id.clone(),
            target_hash: self.target_hash.clone(),
            target: parse_json_bytes(&self.target),
        }
    }
}

impl ToHuman for synctv_proto::admin::SettingsGroup {
    type Human = HumanSettingsGroup;

    fn to_human(&self) -> Self::Human {
        HumanSettingsGroup {
            name: self.name.clone(),
            settings: parse_json_bytes(&self.settings),
        }
    }
}

macro_rules! impl_identity_to_human {
    ($($ty:path),+ $(,)?) => {
        $(
            impl ToHuman for $ty {
                type Human = Self;

                fn to_human(&self) -> Self::Human {
                    self.clone()
                }
            }
        )+
    };
}

impl ToHuman for synctv_proto::admin::CreateUserResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetUserResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::UserAuthFactors {
    type Human = HumanUserAuthFactors;

    fn to_human(&self) -> Self::Human {
        HumanUserAuthFactors {
            password: self.password,
            webauthn: self.webauthn,
            email: self.email,
            eligible_count: self.eligible_count,
        }
    }
}

impl ToHuman for synctv_proto::client::UserPreferences {
    type Human = HumanUserPreferences;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferences {
            two_factor_enabled: self.two_factor_enabled,
            notifications: self.notifications.map(|notifications| {
                HumanUserNotificationPreferences {
                    room_invitation_in_app: notifications.room_invitation_in_app,
                    room_event_in_app: notifications.room_event_in_app,
                    system_announcement_in_app: notifications.system_announcement_in_app,
                    room_invitation_email: notifications.room_invitation_email,
                    room_event_email: notifications.room_event_email,
                    system_announcement_email: notifications.system_announcement_email,
                }
            }),
            settings: parse_json_bytes(&self.settings),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetUserPreferencesResponse {
    type Human = HumanUserPreferencesResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferencesResponse {
            user: self.user.to_human(),
            preferences: self.preferences.to_human(),
            auth_factors: self.auth_factors.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateUserPreferencesResponse {
    type Human = HumanUserPreferencesResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferencesResponse {
            user: self.user.to_human(),
            preferences: self.preferences.to_human(),
            auth_factors: self.auth_factors.to_human(),
        }
    }
}

impl ToHuman for UserMutationCliOutput {
    type Human = HumanUserMutationCliOutput;

    fn to_human(&self) -> Self::Human {
        HumanUserMutationCliOutput {
            success: self.success,
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListUsersResponse {
    type Human = HumanUsersResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUsersResponse {
            users: self.users.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateUserUsernameResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateUserRoleResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::BanUserResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UnbanUserResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetUserRoomsResponse {
    type Human = HumanRoomsResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomsResponse {
    type Human = HumanRoomsResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::GetRoomResponse {
    type Human = HumanRoomResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateRoomSettingsResponse {
    type Human = HumanRoomResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ResetRoomSettingsResponse {
    type Human = HumanRoomResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::BanRoomResponse {
    type Human = HumanRoomResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UnbanRoomResponse {
    type Human = HumanRoomResponse<HumanAdminRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetRoomMembersResponse {
    type Human = HumanRoomMembersResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanRoomMembersResponse {
            members: self.members.to_human(),
            total: self.total,
            version: None,
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ListProviderInstancesResponse {
    type Human = HumanProviderInstancesResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstancesResponse {
            instances: self.instances.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderInstancesResponse {
    type Human = HumanProviderNamesResponse;

    fn to_human(&self) -> Self::Human {
        HumanProviderNamesResponse {
            instances: self.instances.clone(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderBackendsResponse {
    type Human = HumanProviderBackendsResponse;

    fn to_human(&self) -> Self::Human {
        HumanProviderBackendsResponse {
            backends: self.backends.clone(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::AddProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::UpdateProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ReconnectProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::EnableProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::DisableProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetSettingsResponse {
    type Human = HumanSettingsGroupsResponse<HumanSettingsGroup>;

    fn to_human(&self) -> Self::Human {
        HumanSettingsGroupsResponse {
            groups: self.groups.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetSettingsGroupResponse {
    type Human = HumanSettingsGroupResponse<HumanSettingsGroup>;

    fn to_human(&self) -> Self::Human {
        HumanSettingsGroupResponse {
            group: self.group.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::AddAdminResponse {
    type Human = HumanUserResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserResponse {
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListAdminsResponse {
    type Human = HumanAdminsResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanAdminsResponse {
            admins: self.admins.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::CreateRoomResponse {
    type Human = HumanRoomResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::GetRoomResponse {
    type Human = HumanGetRoomWithPlaybackResponse<HumanRoom, HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanGetRoomWithPlaybackResponse {
            room: self.room.to_human(),
            playback_state: self.playback_state.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::JoinRoomResponse {
    type Human = HumanJoinRoomResponse<HumanRoom, HumanPlaybackState, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanJoinRoomResponse {
            room: self.room.to_human(),
            playback_state: self.playback_state.to_human(),
            members: self.members.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomsResponse {
    type Human = HumanRoomsResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::UpdateRoomSettingsResponse {
    type Human = HumanRoomResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::TransferRoomOwnershipResponse {
    type Human = HumanRoomResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::GetRoomMembersResponse {
    type Human = HumanRoomMembersResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanRoomMembersResponse {
            members: self.members.to_human(),
            total: self.total,
            version: Some(self.version.clone()),
        }
    }
}

impl ToHuman for synctv_proto::client::UpdateMemberPermissionsResponse {
    type Human = HumanMemberResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanMemberResponse {
            member: self.member.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::AddMemberResponse {
    type Human = HumanMemberResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanMemberResponse {
            member: self.member.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::CreatePlaylistResponse {
    type Human = HumanPlaylistResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistResponse {
            playlist: self.playlist.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::UpdatePlaylistResponse {
    type Human = HumanPlaylistResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistResponse {
            playlist: self.playlist.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::MovePlaylistResponse {
    type Human = HumanPlaylistResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistResponse {
            playlist: self.playlist.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::GetPlaylistResponse {
    type Human = HumanGetPlaylistResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanGetPlaylistResponse {
            playlist: self.playlist.to_human(),
            child_folder_count: self.child_folder_count,
            media_count: self.media_count,
        }
    }
}

impl ToHuman for synctv_proto::client::ListPlaylistsResponse {
    type Human = HumanPlaylistsResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistsResponse {
            playlists: self.playlists.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::AddMediaResponse {
    type Human = HumanMediaResponse<HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanMediaResponse {
            media: self.media.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::EditMediaResponse {
    type Human = HumanMediaResponse<HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanMediaResponse {
            media: self.media.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::MoveMediaResponse {
    type Human = HumanMediaBatchResponse<HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanMediaBatchResponse {
            moved_count: self.moved_count,
            media: self.media.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListPlaylistItemsResponse {
    type Human = HumanPlaylistItemsResponse<HumanPlaylist, HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistItemsResponse {
            playlists: self.playlists.to_human(),
            media: self.media.to_human(),
            total: self.total,
            folder_count: self.folder_count,
            file_count: self.file_count,
            dynamic_items: self.dynamic_items.clone(),
            current_path: self.current_path.clone(),
            version: self.version.clone(),
        }
    }
}

impl ToHuman for GetPlaybackCliOutput {
    type Human = HumanGetPlaybackResponse<HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanGetPlaybackResponse {
            playback_state: self.playback_state.to_human(),
            playback: self.playback.clone(),
            default_mode: self.default_mode.clone(),
            pull_urls: self.pull_urls.clone(),
            default_pull_url: self.default_pull_url.clone(),
            default_absolute_pull_url: self.default_absolute_pull_url.clone(),
            hls_pull_url: self.hls_pull_url.clone(),
            hls_absolute_pull_url: self.hls_absolute_pull_url.clone(),
            flv_pull_url: self.flv_pull_url.clone(),
            flv_absolute_pull_url: self.flv_absolute_pull_url.clone(),
        }
    }
}

impl ToHuman for synctv_proto::client::UpdatePlaybackStateResponse {
    type Human = HumanUpdatePlaybackStateResponse<HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanUpdatePlaybackStateResponse {
            playback_state: self.playback_state.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::CreatePublishKeyResponse {
    type Human = HumanCreatePublishKeyResponse;

    fn to_human(&self) -> Self::Human {
        HumanCreatePublishKeyResponse {
            publish_key: self.publish_key.clone(),
            rtmp_url: self.rtmp_url.clone(),
            stream_key: self.stream_key.clone(),
            expires_at: humanize_timestamp(self.expires_at),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::GetStreamInfoResponse {
    type Human = HumanGetStreamInfoResponse;

    fn to_human(&self) -> Self::Human {
        HumanGetStreamInfoResponse {
            active: self.active,
            publisher: self.publisher.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomStreamsResponse {
    type Human = HumanListRoomStreamsResponse;

    fn to_human(&self) -> Self::Human {
        HumanListRoomStreamsResponse {
            streams: self.streams.to_human(),
            total: self.total,
        }
    }
}

impl_identity_to_human!(
    synctv_proto::admin::DeleteUserResponse,
    synctv_proto::admin::SetUserPasswordResponse,
    synctv_proto::admin::GetRoomSettingsResponse,
    synctv_proto::admin::UpdateRoomPasswordResponse,
    synctv_proto::admin::DeleteRoomResponse,
    synctv_proto::admin::RemoveAdminResponse,
    synctv_proto::admin::GetSystemStatsResponse,
    synctv_proto::admin::ListActiveStreamsResponse,
    synctv_proto::admin::KickStreamResponse,
    synctv_proto::client::KickRoomStreamResponse,
    synctv_proto::admin::BatchBanUsersResponse,
    synctv_proto::admin::BatchDeleteUsersResponse,
    synctv_proto::admin::BatchBanRoomsResponse,
    synctv_proto::admin::BatchDeleteRoomsResponse,
    synctv_proto::providers::common::DeleteProviderInstanceResponse,
    synctv_proto::admin::UpdateSettingsResponse,
    synctv_proto::admin::SendTestEmailResponse,
    synctv_management::proto::SliceCacheConfigInfo,
    synctv_management::proto::SliceCacheStatsResponse,
    synctv_management::proto::SliceCacheNodeFailure,
    synctv_management::proto::GetSliceCacheStatsResponse,
    synctv_management::proto::PurgeSliceCacheNodeResult,
    synctv_management::proto::PurgeSliceCacheResponse,
    synctv_management::proto::EvictExpiredSliceCacheNodeResult,
    synctv_management::proto::EvictExpiredSliceCacheResponse,
    synctv_proto::client::LeaveRoomResponse,
    synctv_proto::client::DeleteRoomResponse,
    synctv_proto::client::GetRoomSettingsResponse,
    synctv_proto::client::ResetRoomSettingsResponse,
    synctv_proto::client::SetRoomPasswordResponse,
    synctv_proto::client::KickMemberResponse,
    synctv_proto::client::DeletePlaylistResponse,
    synctv_proto::client::DeleteMediaResponse,
    synctv_proto::client::DeleteEntriesResponse,
    synctv_proto::client::ClearPlaylistResponse,
    synctv_proto::providers::alist::LoginResponse,
    synctv_proto::providers::alist::ListResponse,
    synctv_proto::providers::alist::SearchResponse,
    synctv_proto::providers::alist::GetMeResponse,
    synctv_proto::providers::alist::LogoutResponse,
    synctv_proto::providers::alist::GetBindsResponse,
    synctv_proto::providers::emby::LoginResponse,
    synctv_proto::providers::emby::ListResponse,
    synctv_proto::providers::emby::GetMeResponse,
    synctv_proto::providers::emby::LogoutResponse,
    synctv_proto::providers::emby::GetBindsResponse,
    synctv_proto::providers::bilibili::ParseResponse,
    synctv_proto::providers::bilibili::QrCodeResponse,
    synctv_proto::providers::bilibili::QrStatusResponse,
    synctv_proto::providers::bilibili::StartSmsLoginResponse,
    synctv_proto::providers::bilibili::SendSmsResponse,
    synctv_proto::providers::bilibili::LoginSmsResponse,
    synctv_proto::providers::bilibili::UserInfoResponse,
    synctv_proto::providers::bilibili::LogoutResponse,
    synctv_proto::providers::bilibili::GetBindsResponse
);

impl_identity_to_human!(
    PlaybackStartCliOutput,
    PlaybackStopCliOutput,
    KickStreamCliOutput
);

#[cfg(test)]
fn render_human_output<T>(value: &T) -> Result<Value>
where
    T: ?Sized + ToHuman,
{
    Ok(serde_json::to_value(value.to_human())?)
}

fn i64_to_i32(raw: i64) -> Option<i32> {
    i32::try_from(raw).ok()
}

fn humanize_user_role(raw: i64) -> Option<String> {
    use synctv_proto::common::UserRole;

    Some(
        match UserRole::try_from(i64_to_i32(raw)?).ok()? {
            UserRole::Unspecified => "unspecified",
            UserRole::User => "user",
            UserRole::Admin => "admin",
            UserRole::Root => "root",
        }
        .to_string(),
    )
}

fn humanize_user_status(raw: i64) -> Option<String> {
    use synctv_proto::common::UserStatus;

    Some(
        match UserStatus::try_from(i64_to_i32(raw)?).ok()? {
            UserStatus::Unspecified => "unspecified",
            UserStatus::Active => "active",
            UserStatus::Banned => "banned",
        }
        .to_string(),
    )
}

fn humanize_room_status(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomStatus;

    Some(
        match RoomStatus::try_from(i64_to_i32(raw)?).ok()? {
            RoomStatus::Unspecified => "unspecified",
            RoomStatus::Active => "active",
            RoomStatus::Closed => "closed",
        }
        .to_string(),
    )
}

fn humanize_resource_availability(raw: i64) -> Option<String> {
    use synctv_proto::client::ResourceAvailability;

    Some(
        match ResourceAvailability::try_from(i64_to_i32(raw)?).ok()? {
            ResourceAvailability::Unspecified => "unspecified",
            ResourceAvailability::Available => "available",
            ResourceAvailability::CreatorInactive => "creator_inactive",
        }
        .to_string(),
    )
}

fn humanize_room_member_role(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomMemberRole;

    Some(
        match RoomMemberRole::try_from(i64_to_i32(raw)?).ok()? {
            RoomMemberRole::Unspecified => "unspecified",
            RoomMemberRole::Guest => "guest",
            RoomMemberRole::Member => "member",
            RoomMemberRole::Admin => "admin",
            RoomMemberRole::Creator => "creator",
        }
        .to_string(),
    )
}

fn humanize_permission_bits(bits: u64) -> Vec<String> {
    humanize_named_permission_bits(bits, CLI_NAMED_PERMISSIONS)
}

fn humanize_named_permission_bits(bits: u64, named_permissions: &[(&str, u64)]) -> Vec<String> {
    named_permissions
        .iter()
        .copied()
        .map(|(name, permission)| (permission, name))
        .filter(|&(permission, _)| bits & permission != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn humanize_provider_instance_status(raw: i64) -> Option<String> {
    use synctv_proto::providers::common::ProviderInstanceStatus;

    Some(
        match ProviderInstanceStatus::try_from(i64_to_i32(raw)?).ok()? {
            ProviderInstanceStatus::Unspecified => "unspecified",
            ProviderInstanceStatus::Connected => "connected",
            ProviderInstanceStatus::Disconnected => "disconnected",
            ProviderInstanceStatus::Error => "error",
        }
        .to_string(),
    )
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

fn mask_connection_url(url: &str) -> String {
    synctv_common::redaction::mask_url_credentials(url, "<redacted>", "<redacted>")
}

#[cfg(test)]
mod tests;
