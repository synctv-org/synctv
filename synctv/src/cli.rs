use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgAction, ArgGroup, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Map, Value};

use synctv_core::bootstrap::{load_config_with_options, LoadConfigOptions};
#[cfg(test)]
use synctv_core::config::default_management_unix_socket_path;
use synctv_core::config::{absolute_display_path, default_management_runtime_dir};
use synctv_core::time as app_time;
use synctv_management::proto as management_proto;

use crate::admin_client::{AdminConnectionOptions, RemoteAdminSession};
use crate::app::Application;

const INTERNAL_DAEMON_CHILD_ENV: &str = "SYNCTV_INTERNAL_DAEMON_CHILD";
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_READY_POLL_INTERVAL: Duration = Duration::from_millis(200);
const MANAGEMENT_UNARY_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

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
    /// Playlist lifecycle operations within a room
    Playlist(PlaylistCommand),
    /// Media lifecycle operations within a room
    Media(MediaCommand),
    /// Remote media provider instance lifecycle management
    Provider(ProviderCommand),
    /// Runtime settings management through the management daemon endpoint
    Settings(SettingsCommand),
    /// System inspection commands through the management daemon endpoint
    System(SystemCommand),
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

    /// Do not load .env before resolving configuration.
    #[arg(long, global = true, default_value_t = false)]
    pub no_dotenv: bool,

    /// Emit verbose configuration-loading diagnostics to stderr.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
}

impl GlobalConfigArgs {
    pub fn load_options(&self, validate: bool) -> LoadConfigOptions {
        LoadConfigOptions {
            config_path: self.config.as_ref().map(|path| path.display().to_string()),
            load_dotenv: !self.no_dotenv,
            validate,
            verbose: self.verbose > 0,
        }
    }

    fn merged_with_parent(&self, parent: &Self) -> Self {
        Self {
            config: self.config.clone().or_else(|| parent.config.clone()),
            no_dotenv: self.no_dotenv || parent.no_dotenv,
            verbose: self.verbose.max(parent.verbose),
        }
    }
}

#[derive(Clone)]
struct CliConfigContext {
    global: GlobalConfigArgs,
    unvalidated: Arc<OnceLock<std::result::Result<synctv_core::Config, String>>>,
    validated: Arc<OnceLock<std::result::Result<synctv_core::Config, String>>>,
}

impl CliConfigContext {
    fn new(global: GlobalConfigArgs) -> Self {
        Self {
            global,
            unvalidated: Arc::new(OnceLock::new()),
            validated: Arc::new(OnceLock::new()),
        }
    }

    fn config(&self) -> Result<synctv_core::Config> {
        self.load(false)
    }

    fn validated_config(&self) -> Result<synctv_core::Config> {
        self.load(true)
    }

    fn load(&self, validate: bool) -> Result<synctv_core::Config> {
        let cache = if validate {
            &self.validated
        } else {
            &self.unvalidated
        };

        match cache.get_or_init(|| {
            load_config_with_options(self.global.load_options(validate))
                .map_err(|error| error.to_string())
        }) {
            Ok(config) => Ok(config.clone()),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }
}

#[derive(Clone)]
struct RemoteCliContext {
    config: CliConfigContext,
    explicit_endpoint: Option<String>,
    resolved_config_endpoint: Arc<OnceLock<std::result::Result<Option<String>, String>>>,
}

impl RemoteCliContext {
    fn new(args: &RemoteAccessArgs) -> Self {
        Self {
            config: CliConfigContext::new(args.global.clone()),
            explicit_endpoint: resolve_remote_endpoint(args.endpoint.as_deref()),
            resolved_config_endpoint: Arc::new(OnceLock::new()),
        }
    }

    fn initialize_output_state(&self) -> Result<()> {
        let _ = self.config.config()?;
        Ok(())
    }

    fn connection_options(&self, args: &RemoteAccessArgs) -> Result<AdminConnectionOptions> {
        let mut options = args.connection_options(self.explicit_endpoint.clone());
        options.resolved_config_endpoint = self.resolved_config_endpoint()?;
        Ok(options)
    }

    fn resolved_config_endpoint(&self) -> Result<Option<String>> {
        match self.resolved_config_endpoint.get_or_init(|| {
            if self.explicit_endpoint.is_some() {
                let _ = self.config.config().map_err(|error| error.to_string())?;
                Ok(None)
            } else {
                let config = self.config.config().map_err(|error| error.to_string())?;
                Ok(Some(config.management_endpoint()))
            }
        }) {
            Ok(endpoint) => Ok(endpoint.clone()),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    /// Run the server in the background and return after the management endpoint is ready
    #[arg(long, default_value_t = false)]
    pub daemon: bool,

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
pub struct ConfigValidateArgs {}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// Output format for the rendered configuration
    #[arg(long, short = 'o', value_enum, default_value_t = ConfigOutputFormat::Yaml)]
    pub output: ConfigOutputFormat,
}

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
pub struct DbMigrateArgs {}

#[derive(Debug, Args)]
pub struct DbStatusArgs {}

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
    /// Create a user
    Create(UserAddArgs),
    /// Delete a user
    Delete(UserDeleteArgs),
    /// Ban a user
    Ban(UserBanArgs),
    /// Unban a user
    Unban(UserUnbanArgs),
    /// Approve a pending user
    Approve(UserApproveArgs),
    /// Update a user's global role
    SetRole(UserSetRoleArgs),
    /// Reset a user's password
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
    /// Approve a pending room
    Approve(RoomApproveArgs),
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

    #[arg(long)]
    pub password: Option<String>,

    #[arg(long)]
    pub settings_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum RoomSettingsSubcommand {
    /// Get room settings
    Get(RoomSettingsGetArgs),
    /// Replace room settings with a full JSON payload
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
    /// Update a room member's role or permission bitmasks
    SetPermissions(RoomMemberSetPermissionsArgs),
    /// Kick a room member
    Kick(RoomMemberKickArgs),
    /// Ban a room member
    Ban(RoomMemberBanArgs),
    /// Unban a room member
    Unban(RoomMemberUnbanArgs),
}

#[derive(Debug, Args)]
pub struct RoomPlaybackCommand {
    #[command(subcommand)]
    pub command: RoomPlaybackSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomPlaybackSubcommand {
    /// Get current playback state
    Get(RoomPlaybackGetArgs),
    /// Start playback as a specific real user for a static media item or dynamic playlist target
    Start(RoomPlaybackStartArgs),
    /// Stop current playback
    Stop(RoomPlaybackStopArgs),
}

#[derive(Debug, Args)]
pub struct RoomStreamCommand {
    #[command(subcommand)]
    pub command: RoomStreamSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomStreamSubcommand {
    /// List active streams in a room
    List(RoomStreamListArgs),
    /// Get one room stream by media ID
    Get(RoomStreamInfoArgs),
    /// Create an RTMP publish key for a media item as a specific real user
    PublishKey(RoomStreamPublishKeyArgs),
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
pub struct ProviderCommand {
    #[command(subcommand)]
    pub command: ProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderSubcommand {
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
}

#[derive(Debug, Clone, Args)]
pub struct RemoteAccessArgs {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    /// SyncTV management daemon endpoint (`unix:///path` or `http://host:port`)
    #[arg(long, env = "SYNCTV_MANAGEMENT_ENDPOINT")]
    pub endpoint: Option<String>,

    /// Output format for management command results
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}

impl RemoteAccessArgs {
    fn connection_options(&self, endpoint: Option<String>) -> AdminConnectionOptions {
        AdminConnectionOptions {
            endpoint,
            config_path: self
                .global
                .config
                .as_ref()
                .map(|path| path.display().to_string()),
            load_dotenv: !self.global.no_dotenv,
            verbose: self.global.verbose > 0,
            resolved_config_endpoint: None,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliUserRole {
    User,
    Admin,
    Root,
}

impl CliUserRole {
    const fn to_proto(self) -> i32 {
        match self {
            Self::User => management_proto::UserRole::User as i32,
            Self::Admin => management_proto::UserRole::Admin as i32,
            Self::Root => management_proto::UserRole::Root as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliUserStatus {
    Active,
    Pending,
    Banned,
}

impl CliUserStatus {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Active => management_proto::UserStatus::Active as i32,
            Self::Pending => management_proto::UserStatus::Pending as i32,
            Self::Banned => management_proto::UserStatus::Banned as i32,
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
    Pending,
    Closed,
}

impl CliRoomStatus {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Active => management_proto::RoomStatus::Active as i32,
            Self::Pending => management_proto::RoomStatus::Pending as i32,
            Self::Closed => management_proto::RoomStatus::Closed as i32,
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
    Status,
}

impl CliRoomMemberSortField {
    const fn to_proto(self) -> i32 {
        match self {
            Self::JoinedAt => management_proto::RoomMemberListSortBy::JoinedAt as i32,
            Self::Username => management_proto::RoomMemberListSortBy::Username as i32,
            Self::Role => management_proto::RoomMemberListSortBy::Role as i32,
            Self::Status => management_proto::RoomMemberListSortBy::Status as i32,
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
pub enum CliProviderSortField {
    Name,
    Endpoint,
    CreatedAt,
    UpdatedAt,
}

impl CliProviderSortField {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Name => management_proto::ProviderInstanceListSortBy::Name as i32,
            Self::Endpoint => management_proto::ProviderInstanceListSortBy::Endpoint as i32,
            Self::CreatedAt => management_proto::ProviderInstanceListSortBy::CreatedAt as i32,
            Self::UpdatedAt => management_proto::ProviderInstanceListSortBy::UpdatedAt as i32,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliMemberStatus {
    Active,
    Pending,
    Banned,
    Left,
}

impl CliMemberStatus {
    const fn to_proto(self) -> i32 {
        match self {
            Self::Active => synctv_proto::common::MemberStatus::Active as i32,
            Self::Pending => synctv_proto::common::MemberStatus::Pending as i32,
            Self::Banned => synctv_proto::common::MemberStatus::Banned as i32,
            Self::Left => synctv_proto::common::MemberStatus::Left as i32,
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
    fn to_user_ref(&self) -> UserRefArgs {
        UserRefArgs {
            username: self.username.clone(),
            user_id: self.user_id.clone(),
        }
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
    #[arg(long, value_name = "USERNAME", group = "room_creator_ref")]
    pub creator: Option<String>,

    /// Explicit creator internal user ID used to filter rooms
    #[arg(long, value_name = "USER_ID", group = "room_creator_ref")]
    pub creator_id: Option<String>,
}

impl RoomCreatorRefArgs {
    fn to_user_ref(&self) -> Option<UserRefArgs> {
        if self.creator.is_none() && self.creator_id.is_none() {
            return None;
        }

        Some(UserRefArgs {
            username: self.creator.clone(),
            user_id: self.creator_id.clone(),
        })
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
    fn to_user_ref(&self) -> Option<UserRefArgs> {
        if self.username.is_none() && self.user_id.is_none() {
            return None;
        }

        Some(UserRefArgs {
            username: self.username.clone(),
            user_id: self.user_id.clone(),
        })
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

    #[arg(long)]
    pub password: String,

    #[arg(long, value_enum, default_value_t = CliUserRole::User)]
    pub role: CliUserRole,

    #[arg(long, value_enum)]
    pub status: Option<CliUserStatus>,
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
pub struct UserApproveArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct UserSetRoleArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(value_enum)]
    pub role: CliUserRole,
}

#[derive(Debug, Args)]
pub struct UserSetPasswordArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long = "password", value_name = "PASSWORD")]
    pub new_password: String,

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
pub struct RoomMembersArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(long, value_enum)]
    pub status: Option<CliMemberStatus>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomMemberSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct RoomMemberSetPermissionsArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(long)]
    pub added_permissions: Option<u64>,

    #[arg(long)]
    pub removed_permissions: Option<u64>,

    #[arg(long)]
    pub admin_added_permissions: Option<u64>,

    #[arg(long)]
    pub admin_removed_permissions: Option<u64>,
}

#[derive(Debug, Args)]
pub struct RoomMemberKickArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct RoomMemberBanArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomMemberUnbanArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,
}

#[derive(Debug, Args)]
pub struct RoomSettingsGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Debug, Args)]
pub struct RoomSettingsUpdateArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(long)]
    pub settings_json: String,
}

#[derive(Debug, Args)]
pub struct RoomSettingsResetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
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
pub struct RoomApproveArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,
}

#[derive(Debug, Args)]
pub struct RoomBatchBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long = "room-id", value_name = "ROOM_ID", required = true, num_args = 1..)]
    pub room_ids: Vec<String>,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomBatchDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long = "room-id", value_name = "ROOM_ID", required = true, num_args = 1..)]
    pub room_ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,
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

    #[arg(long, requires = "playlist_id")]
    pub target_json: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackStopArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,
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
pub struct RoomStreamInfoArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,
}

#[derive(Debug, Args)]
pub struct RoomStreamPublishKeyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,
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

    #[arg(long, requires = "source_provider")]
    pub source_config_json: Option<String>,

    #[arg(long, requires = "source_provider")]
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
    /// Update media title
    Update(MediaEditArgs),
    /// Delete a media item
    Delete(MediaDeleteArgs),
    /// Move a media item before or after a sibling
    Move(MediaMoveArgs),
}

#[derive(Debug, Args)]
pub struct MediaListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long, requires = "playlist_id")]
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
    pub title: Option<String>,
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
    pub provider: Option<String>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub source_config_json: String,

    #[arg(long)]
    pub title: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaEditArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long)]
    pub title: String,
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
#[command(group(
    ArgGroup::new("media_move_anchor")
        .args(["before_media_id", "after_media_id"])
        .required(true)
        .multiple(false)
))]
pub struct MediaMoveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long, conflicts_with = "after_media_id")]
    pub before_media_id: Option<String>,

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

    #[arg(long)]
    pub config_json: Option<String>,

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

    #[arg(long)]
    pub config_json: Option<String>,

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
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

pub async fn execute(cli: Cli) -> Result<()> {
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Serve(serve) => execute_serve(serve).await,
        Commands::Stop(stop) => execute_stop(stop).await,
        Commands::Config(config) => execute_config(config).await,
        Commands::Db(db) => execute_db(db).await,
        Commands::User(user) => execute_user(user).await,
        Commands::Room(room) => execute_room(room).await,
        Commands::Playlist(playlist) => execute_playlist(playlist).await,
        Commands::Media(media) => execute_media(media).await,
        Commands::Provider(provider) => execute_provider(provider).await,
        Commands::Settings(settings) => execute_settings(settings).await,
        Commands::System(system) => execute_system(system).await,
        Commands::Completion(args) => execute_completion(args),
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
        Commands::Playlist(command) => merge_playlist_command_globals(command, &root),
        Commands::Media(command) => merge_media_command_globals(command, &root),
        Commands::Provider(command) => merge_provider_command_globals(command, &root),
        Commands::Settings(command) => merge_settings_command_globals(command, &root),
        Commands::System(command) => merge_system_command_globals(command, &root),
        Commands::Completion(_) | Commands::Version => {}
    }
    cli
}

fn merge_remote_access_args(remote: &mut RemoteAccessArgs, root: &GlobalConfigArgs) {
    remote.global = remote.global.merged_with_parent(root);
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
        UserSubcommand::Approve(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetRole(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetUsername(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Rooms(args) => merge_remote_access_args(&mut args.remote, root),
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
        RoomSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Unban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Approve(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Settings(command) => match &mut command.command {
            RoomSettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
            RoomSettingsSubcommand::Update(args) => {
                merge_remote_access_args(&mut args.remote, root)
            }
            RoomSettingsSubcommand::Reset(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::Member(command) => match &mut command.command {
            RoomMemberSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomMemberSubcommand::SetPermissions(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
            RoomMemberSubcommand::Kick(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomMemberSubcommand::Ban(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomMemberSubcommand::Unban(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
        },
        RoomSubcommand::Playback(command) => match &mut command.command {
            RoomPlaybackSubcommand::Get(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
            RoomPlaybackSubcommand::Start(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
            RoomPlaybackSubcommand::Stop(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
        },
        RoomSubcommand::Stream(command) => match &mut command.command {
            RoomStreamSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomStreamSubcommand::Get(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomStreamSubcommand::PublishKey(args) => {
                merge_room_scoped_remote_args(&mut args.room, root)
            }
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
    }
}

fn merge_provider_command_globals(command: &mut ProviderCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ProviderSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Reconnect(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Enable(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Disable(args) => merge_remote_access_args(&mut args.remote, root),
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

async fn execute_serve(args: ServeArgs) -> Result<()> {
    let context = CliConfigContext::new(args.global.clone());
    let config = context.validated_config()?;

    if args.daemon && args.dry_run {
        bail!("--daemon cannot be combined with --dry-run");
    }

    if args.daemon && std::env::var_os(INTERNAL_DAEMON_CHILD_ENV).is_none() {
        return spawn_daemonized_serve(&config).await;
    }

    crate::install_panic_hook(config.logging.backtrace);
    let _log_guard = synctv_core::logging::init_logging(&config.logging)?;

    if args.dry_run {
        tracing::info!("Configuration and logging initialized successfully");
        tracing::info!("Dry run requested, not starting server");
        return Ok(());
    }

    tracing::info!("SyncTV server starting...");
    tracing::info!("API address: {}", config.api_address());

    let app = Application::build(config).await?;
    app.run().await
}

async fn spawn_daemonized_serve(config: &synctv_core::Config) -> Result<()> {
    let readiness_probe = daemon_readiness_probe(config);
    let readiness_target = daemon_readiness_probe_target(&readiness_probe);

    if daemon_probe_is_ready(&readiness_probe).await.is_ok() {
        bail!("daemon readiness target {readiness_target} is already serving");
    }

    let log_path = daemon_log_path(config)?;
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| {
            format!(
                "failed to open daemon log file {}",
                absolute_display_path(&log_path)
            )
        })?;
    let stderr = stdout.try_clone().with_context(|| {
        format!(
            "failed to clone daemon log file {}",
            absolute_display_path(&log_path)
        )
    })?;

    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut child = tokio::process::Command::new(current_exe)
        .args(std::env::args_os().skip(1))
        .env(INTERNAL_DAEMON_CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("failed to spawn daemon child process")?;

    let deadline = tokio::time::Instant::now() + DAEMON_READY_TIMEOUT;
    let mut last_error = None;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll daemon child process state")?
        {
            let detail = last_error
                .map(|error| format!("; last readiness error: {error}"))
                .unwrap_or_default();
            bail!("daemon child exited before becoming ready: {status}{detail}");
        }

        match daemon_probe_is_ready(&readiness_probe).await {
            Ok(_) => {
                println!("daemon started");
                if config.management.enabled {
                    println!("management endpoint: {readiness_target}");
                } else {
                    println!("api health: {readiness_target}");
                }
                println!("log file: {}", absolute_display_path(&log_path));
                return Ok(());
            }
            Err(error) => last_error = Some(error.to_string()),
        }

        if tokio::time::Instant::now() >= deadline {
            let detail = last_error.unwrap_or_else(|| "unknown readiness failure".to_string());
            bail!(
                "daemon child did not become ready within {}s: {detail}",
                DAEMON_READY_TIMEOUT.as_secs()
            );
        }

        tokio::time::sleep(DAEMON_READY_POLL_INTERVAL).await;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DaemonReadinessProbe {
    ManagementEndpoint(String),
    ApiTcpAddress(String),
}

fn daemon_readiness_probe(config: &synctv_core::Config) -> DaemonReadinessProbe {
    if config.management.enabled {
        DaemonReadinessProbe::ManagementEndpoint(config.management_endpoint())
    } else {
        DaemonReadinessProbe::ApiTcpAddress(daemon_local_api_probe_address(config))
    }
}

fn daemon_readiness_probe_target(probe: &DaemonReadinessProbe) -> &str {
    match probe {
        DaemonReadinessProbe::ManagementEndpoint(endpoint)
        | DaemonReadinessProbe::ApiTcpAddress(endpoint) => endpoint,
    }
}

fn daemon_local_api_probe_address(config: &synctv_core::Config) -> String {
    let host = match config.server.host.trim() {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "::1".to_string(),
        host if host.contains(':') && !host.starts_with('[') => format!("[{host}]"),
        host => host.to_string(),
    };
    format!("{host}:{}", config.server.port)
}

async fn daemon_probe_is_ready(probe: &DaemonReadinessProbe) -> Result<()> {
    match probe {
        DaemonReadinessProbe::ManagementEndpoint(endpoint) => {
            RemoteAdminSession::connect(AdminConnectionOptions {
                endpoint: Some(endpoint.clone()),
                config_path: None,
                load_dotenv: false,
                verbose: false,
                resolved_config_endpoint: None,
            })
            .await
            .map(|_| ())
        }
        DaemonReadinessProbe::ApiTcpAddress(address) => {
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(address),
            )
            .await
            .with_context(|| format!("timed out probing daemon API listener {address}"))?
            .with_context(|| format!("failed to connect to daemon API listener {address}"))?;
            Ok(())
        }
    }
}

fn daemon_log_path(config: &synctv_core::Config) -> Result<PathBuf> {
    let runtime_dir = daemon_runtime_dir(config);
    std::fs::create_dir_all(&runtime_dir).with_context(|| {
        format!(
            "failed to create daemon runtime directory {}",
            absolute_display_path(&runtime_dir)
        )
    })?;
    Ok(runtime_dir.join("synctv-daemon.log"))
}

fn daemon_runtime_dir(config: &synctv_core::Config) -> PathBuf {
    Path::new(&config.management.unix_socket_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(default_management_runtime_dir)
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
                let message = event.message.trim();
                if !message.is_empty() {
                    println!("{message}");
                }
                last_stage = management_proto::StopServerStage::try_from(event.stage).ok();
                if event.terminal {
                    saw_terminal = true;
                    break;
                }
            }
            Ok(None) => {
                if stop_stream_end_can_be_treated_as_success(last_stage) {
                    return Ok(());
                }
                break;
            }
            Err(error) => {
                if stop_stream_disconnect_can_be_treated_as_success(last_stage, &error) {
                    return Ok(());
                }
                return Err(error);
            }
        }
    }

    if !saw_terminal {
        bail!("management stop stream ended before terminal shutdown status")
    }

    Ok(())
}

fn stop_stream_end_can_be_treated_as_success(
    last_stage: Option<management_proto::StopServerStage>,
) -> bool {
    matches!(
        last_stage,
        Some(management_proto::StopServerStage::Finalizing)
    )
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

async fn execute_config(config_command: ConfigCommand) -> Result<()> {
    let context = CliConfigContext::new(config_command.global.clone());
    match config_command.command {
        ConfigSubcommand::Validate(_) => {
            let config = context.validated_config()?;
            println!("Configuration is valid");
            println!("API address: {}", config.api_address());
            Ok(())
        }
        ConfigSubcommand::Show(args) => {
            let config = context.config()?;
            let rendered = render_config_for_display(&config);
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
        DbSubcommand::Migrate(_) => {
            let redis_init = synctv_core::bootstrap::init_redis(&config, None).await?;
            let pool = synctv_core::bootstrap::init_database(&config).await?.pool;

            let lock: std::sync::Arc<dyn synctv_core::service::MigrationLock> =
                if let Some(ref rh) = redis_init.handles {
                    let is_sentinel = matches!(
                        config.redis.deployment_mode,
                        synctv_core::config::RedisDeploymentMode::Sentinel
                    );
                    if is_sentinel {
                        std::sync::Arc::new(synctv_core::service::PgAdvisoryMigrationLock::new(
                            pool.clone(),
                        ))
                    } else {
                        std::sync::Arc::new(
                            synctv_core::service::DistributedLock::new_shared_with_mode(
                                rh.conn.clone(),
                                false,
                            ),
                        )
                    }
                } else {
                    std::sync::Arc::new(synctv_core::service::PgAdvisoryMigrationLock::new(
                        pool.clone(),
                    ))
                };

            crate::migrations::run_migrations(
                &pool,
                lock,
                &config.redis.key_prefix,
                config.cluster_runtime_enabled(),
            )
            .await?;

            pool.close().await;
            Ok(())
        }
        DbSubcommand::Status(_) => {
            let pool = synctv_core::bootstrap::init_database(&config).await?.pool;
            sqlx::query("SELECT 1").execute(&pool).await?;
            println!("{}", database_status_summary(&config));
            pool.close().await;
            Ok(())
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
                        management_proto::UserStatus::Unspecified as i32,
                        CliUserStatus::to_proto,
                    ),
                    role: args.role.map_or(
                        management_proto::UserRole::Unspecified as i32,
                        CliUserRole::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    sort_by: args.sort_by.map_or(
                        management_proto::UserListSortBy::CreatedAt as i32,
                        CliUserSortField::to_proto
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "get user",
                get_user,
                management_proto::GetUserRequest { user_id }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create user",
                create_user,
                management_proto::CreateUserRequest {
                    username: args.username,
                    password: args.password,
                    email: args.email.unwrap_or_default(),
                    role: args.role.to_proto(),
                    status: args.status.map_or(
                        management_proto::UserStatus::Unspecified as i32,
                        CliUserStatus::to_proto,
                    ),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "delete user",
                delete_user,
                management_proto::DeleteUserRequest { user_id }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Ban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "ban user",
                ban_user,
                management_proto::BanUserRequest {
                    user_id,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Unban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "unban user",
                unban_user,
                management_proto::UnbanUserRequest { user_id }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Approve(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "approve user",
                approve_user,
                management_proto::ApproveUserRequest { user_id }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::SetRole(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "update user role",
                update_user_role,
                management_proto::UpdateUserRoleRequest {
                    user_id,
                    role: args.role.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::SetPassword(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "update user password",
                update_user_password,
                management_proto::UpdateUserPasswordRequest {
                    user_id,
                    new_password: args.new_password,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::SetUsername(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "update user username",
                update_user_username,
                management_proto::UpdateUserUsernameRequest {
                    user_id,
                    new_username: args.new_username,
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Rooms(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let user_id = resolve_user_ref(&session, &args.user).await?;
            let response = management_unary_call!(
                session,
                "get user rooms",
                get_user_rooms,
                management_proto::GetUserRoomsRequest {
                    user_id,
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        management_proto::RoomStatus::Unspecified as i32,
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
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "add admin",
                    add_admin,
                    management_proto::AddAdminRequest { user_id }
                )?;
                args.remote.print_output(&response)
            }
            UserAdminSubcommand::Revoke(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "remove admin",
                    remove_admin,
                    management_proto::RemoveAdminRequest { user_id }
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
                let user_ids =
                    resolve_user_refs_batch(&session, &args.usernames, &args.user_ids).await?;
                let response = management_unary_call!(
                    session,
                    "batch ban users",
                    batch_ban_users,
                    management_proto::BatchBanUsersRequest {
                        user_ids,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            UserBatchSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let user_ids =
                    resolve_user_refs_batch(&session, &args.usernames, &args.user_ids).await?;
                let response = management_unary_call!(
                    session,
                    "batch delete users",
                    batch_delete_users,
                    management_proto::BatchDeleteUsersRequest { user_ids }
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
            let actor_user_id = resolve_user_ref(&session, &args.actor.to_user_ref()).await?;
            let response = management_unary_call!(
                session,
                "create room",
                create_room,
                management_proto::CreateRoomRequest {
                    actor_user_id,
                    name: args.name,
                    password: args.password.unwrap_or_default(),
                    settings_json: optional_json_bytes(
                        "settings_json",
                        args.settings_json.as_deref(),
                    )?,
                    description: args.description.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let creator_id = match args.creator.to_user_ref() {
                Some(user) => resolve_user_ref(&session, &user).await?,
                None => String::new(),
            };
            let response = management_unary_call!(
                session,
                "list rooms",
                list_rooms,
                management_proto::ListRoomsRequest {
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        management_proto::RoomStatus::Unspecified as i32,
                        CliRoomStatus::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    creator_id,
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
        RoomSubcommand::Settings(settings_command) => match settings_command.command {
            RoomSettingsSubcommand::Get(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room settings",
                    get_room_settings,
                    management_proto::GetRoomSettingsRequest {
                        room_id: args.room_id,
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
                        room_id: args.room_id,
                        settings_json: optional_json_bytes(
                            "settings_json",
                            Some(args.settings_json.as_str()),
                        )?,
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
                        room_id: args.room_id,
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
                        room_id: args.room_id,
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        status: args.status.map_or(
                            synctv_proto::common::MemberStatus::Unspecified as i32,
                            CliMemberStatus::to_proto,
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
            RoomMemberSubcommand::SetPermissions(args) => {
                ensure_room_member_permission_update_requested(&args)?;
                let session = connect_remote_access(&args.room.remote).await?;
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "update room member permissions",
                    update_member_permissions,
                    management_proto::UpdateMemberPermissionsRequest {
                        room_id: args.room.room_id,
                        user_id,
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        added_permissions: args.added_permissions.unwrap_or_default(),
                        removed_permissions: args.removed_permissions.unwrap_or_default(),
                        admin_added_permissions: args.admin_added_permissions.unwrap_or_default(),
                        admin_removed_permissions: args
                            .admin_removed_permissions
                            .unwrap_or_default(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "kick room member",
                    kick_member,
                    management_proto::KickMemberRequest {
                        room_id: args.room.room_id,
                        user_id,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::Ban(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "ban room member",
                    ban_member,
                    management_proto::BanMemberRequest {
                        room_id: args.room.room_id,
                        user_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::Unban(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let user_id = resolve_user_ref(&session, &args.user).await?;
                let response = management_unary_call!(
                    session,
                    "unban room member",
                    unban_member,
                    management_proto::UnbanMemberRequest {
                        room_id: args.room.room_id,
                        user_id,
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
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomPlaybackSubcommand::Start(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "start room playback",
                    start_playback,
                    management_proto::StartPlaybackRequest {
                        room_id: args.room.room_id,
                        media_id: args.media_id.unwrap_or_default(),
                        playlist_id: args.playlist_id.unwrap_or_default(),
                        target_json: optional_json_bytes(
                            "target_json",
                            args.target_json.as_deref()
                        )?,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomPlaybackSubcommand::Stop(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "stop room playback",
                    stop_playback,
                    management_proto::StopPlaybackRequest {
                        room_id: args.room.room_id,
                    }
                )?;
                args.room.remote.print_output(&response)
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
            RoomStreamSubcommand::Get(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room stream info",
                    get_stream_info,
                    management_proto::GetStreamInfoRequest {
                        room_id: args.room.room_id,
                        media_id: args.media_id,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomStreamSubcommand::PublishKey(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let actor_user_id = resolve_user_ref(&session, &args.actor.to_user_ref()).await?;
                let response = management_unary_call!(
                    session,
                    "create room publish key",
                    create_publish_key,
                    management_proto::CreatePublishKeyRequest {
                        actor_user_id,
                        room_id: args.room.room_id,
                        media_id: args.media_id,
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
                        room_ids: args.room_ids,
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
                        room_ids: args.room_ids,
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
        RoomSubcommand::Approve(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "approve room",
                approve_room,
                management_proto::ApproveRoomRequest {
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
                    parent_id: args.parent_id.unwrap_or_default(),
                    page: args.page,
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider: args.source_provider.unwrap_or_default(),
                    provider_instance_name: args.provider_instance_name.unwrap_or_default(),
                    dynamic_only: args.dynamic_only,
                    sort_by: args.sort_by.map_or(
                        management_proto::PlaylistListSortBy::Position as i32,
                        CliPlaylistSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
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
            validate_playlist_create(&args)?;
            let session = connect_remote_access(&args.room.remote).await?;
            let actor_user_id = resolve_user_ref(&session, &args.actor.to_user_ref()).await?;
            let response = management_unary_call!(
                session,
                "create playlist",
                create_playlist,
                management_proto::CreatePlaylistRequest {
                    actor_user_id,
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: args.parent_id.unwrap_or_default(),
                    source_provider: args.source_provider.unwrap_or_default(),
                    source_config_json: optional_json_bytes(
                        "source_config_json",
                        args.source_config_json.as_deref(),
                    )?,
                    provider_instance_name: args.provider_instance_name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Update(args) => {
            if args.name.is_none() {
                bail!("playlist update requires --name");
            }

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
                (Some(id), None) => Some(
                    management_proto::move_playlist_request::Anchor::BeforePlaylistId(id),
                ),
                (None, Some(id)) => Some(
                    management_proto::move_playlist_request::Anchor::AfterPlaylistId(id),
                ),
                _ => bail!("playlist move requires exactly one of --before-playlist-id or --after-playlist-id"),
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
                    playlist_id: args.playlist_id.unwrap_or_default(),
                    target_json: optional_json_bytes("target_json", args.target_json.as_deref())?,
                    page: args.page,
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider: args.source_provider.unwrap_or_default(),
                    provider_instance_name: args.provider_instance_name.unwrap_or_default(),
                    sort_by: args.sort_by.map_or(
                        management_proto::MediaListSortBy::Position as i32,
                        CliMediaSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Add(args) => {
            validate_generic_media_add(&args)?;
            let session = connect_remote_access(&args.room.remote).await?;
            let actor_user_id = resolve_user_ref(&session, &args.actor.to_user_ref()).await?;
            let response = management_unary_call!(
                session,
                "add media",
                add_media,
                management_proto::AddMediaRequest {
                    actor_user_id,
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id.unwrap_or_default(),
                    provider: args.provider.unwrap_or_default(),
                    provider_instance_name: args.provider_instance_name.unwrap_or_default(),
                    source_config_json: optional_json_bytes(
                        "source_config_json",
                        Some(args.source_config_json.as_str()),
                    )?,
                    title: args.title.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::AddUrl(args) => {
            validate_direct_media_url(&args.url)?;
            let session = connect_remote_access(&args.room.remote).await?;
            let actor_user_id = resolve_user_ref(&session, &args.actor.to_user_ref()).await?;
            let response = management_unary_call!(
                session,
                "add direct url media",
                add_direct_url_media,
                management_proto::AddDirectUrlMediaRequest {
                    actor_user_id,
                    room_id: args.room.room_id,
                    url: args.url,
                    playlist_id: args.playlist_id.unwrap_or_default(),
                    title: args.title.unwrap_or_default(),
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
                    title: args.title,
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
                (Some(id), None) => {
                    Some(management_proto::move_media_request::Anchor::BeforeMediaId(id))
                }
                (None, Some(id)) => {
                    Some(management_proto::move_media_request::Anchor::AfterMediaId(id))
                }
                _ => bail!("media move requires exactly one of --before-media-id or --after-media-id"),
            };
            let response = management_unary_call!(
                session,
                "move media",
                move_media,
                management_proto::MoveMediaRequest {
                    room_id: args.room.room_id,
                    media_id: args.media_id,
                    anchor,
                }
            )?;
            args.room.remote.print_output(&response)
        }
    }
}

async fn execute_provider(provider_command: ProviderCommand) -> Result<()> {
    let ProviderCommand { command } = provider_command;
    match command {
        ProviderSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list provider instances",
                list_provider_instances,
                management_proto::ListProviderInstancesRequest {
                    page: args.page,
                    page_size: args.page_size,
                    provider_type: args.provider_type.unwrap_or_default(),
                    search: args.search.unwrap_or_default(),
                    enabled: args.enabled,
                    tls: args.tls,
                    sort_by: args.sort_by.map_or(
                        management_proto::ProviderInstanceListSortBy::CreatedAt as i32,
                        CliProviderSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Create(args) => {
            validate_provider_add(&args)?;
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "add provider instance",
                add_provider_instance,
                management_proto::AddProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment: args.comment.unwrap_or_default(),
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: args.providers,
                    config_json: optional_json_bytes("config_json", args.config_json.as_deref())?,
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Update(args) => {
            ensure_provider_update_requested(&args)?;
            validate_provider_update_transport_flags(&args)?;

            let comment = provider_update_comment(&args);
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update provider instance",
                update_provider_instance,
                management_proto::UpdateProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment,
                    clear_comment: args.clear_comment,
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: args.providers,
                    config_json: optional_json_bytes("config_json", args.config_json.as_deref())?,
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
                management_proto::DeleteProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Reconnect(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "reconnect provider instance",
                reconnect_provider_instance,
                management_proto::ReconnectProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Enable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "enable provider instance",
                enable_provider_instance,
                management_proto::EnableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Disable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "disable provider instance",
                disable_provider_instance,
                management_proto::DisableProviderInstanceRequest { name: args.name }
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
            management_unary_call!(
                session,
                "update settings",
                update_settings,
                management_proto::UpdateSettingsRequest {
                    group: args.group.clone(),
                    settings,
                }
            )?;

            let response = management_unary_call!(
                session,
                "get updated settings group",
                get_settings_group,
                management_proto::GetSettingsGroupRequest { group: args.group }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management settings group response did not include group")
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
                let user_id = match args.user.to_user_ref() {
                    Some(user) => resolve_user_ref(&session, &user).await?,
                    None => String::new(),
                };
                let response = management_unary_call!(
                    session,
                    "list active streams",
                    list_active_streams,
                    management_proto::ListActiveStreamsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        room_id: args.room_id.unwrap_or_default(),
                        user_id,
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
                let response = management_unary_call!(
                    session,
                    "kick active stream",
                    kick_stream,
                    management_proto::KickStreamRequest {
                        room_id: args.room_id,
                        media_id: args.media_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
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
        tonic::Code::NotFound => format!("management {operation} failed: {detail}"),
        tonic::Code::AlreadyExists => format!("management {operation} failed: {detail}"),
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
        tonic::Code::Unknown => format!("management {operation} failed: {detail}"),
        _ => format!("management {operation} failed: {}: {detail}", status.code()),
    };

    anyhow!(rendered)
}

fn resolve_remote_endpoint(cli_endpoint: Option<&str>) -> Option<String> {
    cli_endpoint
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

async fn resolve_user_ref(session: &RemoteAdminSession, user: &UserRefArgs) -> Result<String> {
    if let Some(user_id) = user.user_id.as_deref() {
        return Ok(user_id.to_string());
    }

    let username = user
        .username
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("username is required when --user-id is not provided"))?;

    let response = management_unary_call!(
        session,
        "resolve user by username",
        get_user_by_username,
        management_proto::GetUserByUsernameRequest {
            username: username.to_string(),
        }
    )?;
    let user = response
        .user
        .ok_or_else(|| anyhow!("user '{username}' was not found"))?;
    Ok(user.id)
}

async fn resolve_user_refs_batch(
    session: &RemoteAdminSession,
    usernames: &[String],
    user_ids: &[String],
) -> Result<Vec<String>> {
    if usernames.is_empty() && user_ids.is_empty() {
        bail!("at least one username or --user-id must be provided");
    }

    let mut resolved = Vec::with_capacity(usernames.len() + user_ids.len());
    let mut seen = std::collections::HashSet::with_capacity(usernames.len() + user_ids.len());

    for username in usernames {
        let user_id = resolve_user_ref(
            session,
            &UserRefArgs {
                username: Some(username.clone()),
                user_id: None,
            },
        )
        .await?;
        if seen.insert(user_id.clone()) {
            resolved.push(user_id);
        }
    }

    for user_id in user_ids {
        let trimmed = user_id.trim();
        if trimmed.is_empty() {
            bail!("--user-id values must not be empty");
        }
        if seen.insert(trimmed.to_string()) {
            resolved.push(trimmed.to_string());
        }
    }

    Ok(resolved)
}

fn optional_json_bytes(field_name: &str, raw: Option<&str>) -> Result<Vec<u8>> {
    match raw.map(str::trim) {
        None | Some("") => Ok(Vec::new()),
        Some(value) => {
            let json: Value = serde_json::from_str(value)
                .with_context(|| format!("{field_name} must be valid JSON"))?;
            serde_json::to_vec(&json)
                .with_context(|| format!("failed to encode {field_name} as JSON bytes"))
        }
    }
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

fn validate_generic_media_add(args: &MediaAddArgs) -> Result<()> {
    let provider = args.provider.as_deref().map(str::trim).unwrap_or_default();
    let provider_instance_name = args
        .provider_instance_name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();

    if provider.is_empty() && !provider_instance_name.is_empty() {
        bail!("--provider-instance-name requires --provider");
    }

    if !provider.is_empty() && provider_instance_name.is_empty() {
        bail!("--provider-instance-name is required when --provider is set");
    }

    optional_json_bytes("source_config_json", Some(args.source_config_json.as_str()))?;
    Ok(())
}

fn validate_playlist_create(args: &PlaylistCreateArgs) -> Result<()> {
    let source_provider = args
        .source_provider
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let source_config_json = args
        .source_config_json
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let provider_instance_name = args
        .provider_instance_name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();

    if source_provider.is_empty() {
        if !source_config_json.is_empty() || !provider_instance_name.is_empty() {
            bail!(
                "--source-provider is required when setting --source-config-json or --provider-instance-name"
            );
        }
        return Ok(());
    }

    if source_config_json.is_empty() {
        bail!("--source-config-json is required when --source-provider is set");
    }

    if provider_instance_name.is_empty() {
        bail!("--provider-instance-name is required when --source-provider is set");
    }

    optional_json_bytes("source_config_json", args.source_config_json.as_deref())?;
    Ok(())
}

fn validate_provider_add(args: &ProviderAddArgs) -> Result<()> {
    if args.insecure_tls && !args.tls {
        bail!("--insecure-tls requires --tls");
    }

    optional_json_bytes("config_json", args.config_json.as_deref())?;
    Ok(())
}

fn provider_update_comment(args: &ProviderUpdateArgs) -> Option<String> {
    if args.clear_comment {
        Some(String::new())
    } else {
        args.comment.clone()
    }
}

fn ensure_provider_update_requested(args: &ProviderUpdateArgs) -> Result<()> {
    if args.provider_endpoint.is_none()
        && args.comment.is_none()
        && !args.clear_comment
        && args.timeout_seconds.is_none()
        && args.tls.is_none()
        && args.insecure_tls.is_none()
        && args.providers.is_empty()
        && args.config_json.is_none()
    {
        bail!(
            "provider update requires at least one of --provider-endpoint, --comment, --clear-comment, --timeout-seconds, --tls, --insecure-tls, --provider, or --config-json"
        );
    }
    if args.comment.is_some() && args.clear_comment {
        bail!("provider update cannot use --comment and --clear-comment together");
    }
    Ok(())
}

fn validate_provider_update_transport_flags(args: &ProviderUpdateArgs) -> Result<()> {
    if matches!(args.tls, Some(false)) && matches!(args.insecure_tls, Some(true)) {
        bail!("--insecure-tls true cannot be combined with --tls false");
    }

    optional_json_bytes("config_json", args.config_json.as_deref())?;
    Ok(())
}

fn validate_direct_media_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).with_context(|| format!("invalid media URL: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("media URL must use http:// or https://");
    }
    if parsed.host_str().is_none() {
        bail!("media URL must include a host");
    }
    Ok(())
}

fn ensure_room_member_permission_update_requested(
    args: &RoomMemberSetPermissionsArgs,
) -> Result<()> {
    if args.role.is_none()
        && args.added_permissions.is_none()
        && args.removed_permissions.is_none()
        && args.admin_added_permissions.is_none()
        && args.admin_removed_permissions.is_none()
    {
        bail!(
            "room member permission update requires at least one of --role, --added-permissions, --removed-permissions, --admin-added-permissions, or --admin-removed-permissions"
        );
    }

    Ok(())
}

fn print_json<T>(value: &T) -> Result<()>
where
    T: ?Sized + serde::Serialize,
{
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_structured_output<T>(format: RemoteOutputFormat, value: &T) -> Result<()>
where
    T: ?Sized + serde::Serialize + ToHuman,
{
    match format {
        RemoteOutputFormat::Human => print_human(value),
        RemoteOutputFormat::Json => print_json(value),
        RemoteOutputFormat::Yaml => print_yaml(value),
    }
}

fn print_human<T>(value: &T) -> Result<()>
where
    T: ?Sized + ToHuman,
{
    print_yaml(&value.to_human())
}

fn print_yaml<T>(value: &T) -> Result<()>
where
    T: ?Sized + serde::Serialize,
{
    print!("{}", serde_yaml::to_string(value)?);
    Ok(())
}

fn print_toml(value: &Value) -> Result<()> {
    let mut value = value.clone();
    prune_null_config_values(&mut value);
    print!("{}", toml::to_string_pretty(&value)?);
    Ok(())
}

fn execute_completion(args: CompletionArgs) -> Result<()> {
    let mut command = Cli::command();
    match args.shell {
        CompletionShell::Bash => print_completion(&mut command, clap_complete::Shell::Bash),
        CompletionShell::Zsh => print_completion(&mut command, clap_complete::Shell::Zsh),
        CompletionShell::Fish => print_completion(&mut command, clap_complete::Shell::Fish),
        CompletionShell::PowerShell => {
            print_completion(&mut command, clap_complete::Shell::PowerShell)
        }
        CompletionShell::Elvish => print_completion(&mut command, clap_complete::Shell::Elvish),
    }
    Ok(())
}

fn print_completion(command: &mut clap::Command, shell: clap_complete::Shell) {
    clap_complete::generate(shell, command, "synctv", &mut std::io::stdout());
}

pub fn version_string() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

fn database_status_summary(config: &synctv_core::Config) -> String {
    let masked_url = mask_connection_url(&config.database.url);

    format!("Database connection: OK\nDatabase URL: {masked_url}")
}

fn render_config_for_display(config: &synctv_core::Config) -> Value {
    let mut value = serde_json::to_value(config).expect("config should serialize");
    redact_config_value(&mut value);
    value
}

trait ToHuman {
    type Human: Serialize;

    fn to_human(&self) -> Self::Human;
}

impl ToHuman for String {
    type Human = String;

    fn to_human(&self) -> Self::Human {
        self.clone()
    }
}

impl ToHuman for bool {
    type Human = bool;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i32 {
    type Human = i32;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i64 {
    type Human = i64;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u32 {
    type Human = u32;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u64 {
    type Human = u64;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for f64 {
    type Human = f64;

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
}

#[derive(Debug, Clone, Serialize)]
struct HumanAdminRoom {
    id: String,
    name: String,
    creator_id: String,
    creator_username: String,
    status: String,
    settings: Value,
    member_count: i32,
    created_at: String,
    updated_at: String,
    description: String,
    is_banned: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HumanRoomMember {
    room_id: String,
    user_id: String,
    username: String,
    role: String,
    permissions: u64,
    added_permissions: u64,
    removed_permissions: u64,
    admin_added_permissions: u64,
    admin_removed_permissions: u64,
    joined_at: String,
    is_online: bool,
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
    item_count: i32,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct HumanMedia {
    id: String,
    room_id: String,
    provider: String,
    title: String,
    metadata: Value,
    position: f64,
    added_at: String,
    added_by: String,
    provider_instance_name: String,
    source_config: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanPlaybackState {
    room_id: String,
    playing_media_id: String,
    current_time: f64,
    speed: f64,
    is_playing: bool,
    updated_at: String,
    version: i64,
    playing_playlist_id: String,
    target: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HumanSettingsGroup {
    name: String,
    settings: Value,
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
struct HumanPlaylistItemsResponse<P, M> {
    playlists: Vec<P>,
    media: Vec<M>,
    total: i32,
    folder_count: i32,
    file_count: i32,
    dynamic_items: Vec<synctv_proto::client::PlaylistItem>,
    current_path: Vec<synctv_proto::client::PlaylistBrowsePathNode>,
}

#[derive(Debug, Clone, Serialize)]
struct HumanGetPlaybackResponse<T> {
    playback_state: Option<T>,
    playback_result: Option<synctv_proto::client::PlaybackResult>,
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
            role: humanize_user_role(self.role as i64).unwrap_or_else(|| self.role.to_string()),
            status: humanize_user_status(self.status as i64)
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
            status: humanize_room_status(self.status as i64)
                .unwrap_or_else(|| self.status.to_string()),
            settings: parse_json_bytes(&self.settings),
            created_at: humanize_timestamp(self.created_at),
            member_count: self.member_count,
            description: self.description.clone(),
            updated_at: humanize_timestamp(self.updated_at),
            is_banned: self.is_banned,
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
            status: humanize_room_status(self.status as i64)
                .unwrap_or_else(|| self.status.to_string()),
            settings: parse_json_bytes(&self.settings),
            member_count: self.member_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
            description: self.description.clone(),
            is_banned: self.is_banned,
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
            role: humanize_room_member_role(self.role as i64)
                .unwrap_or_else(|| self.role.to_string()),
            permissions: self.permissions,
            added_permissions: self.added_permissions,
            removed_permissions: self.removed_permissions,
            admin_added_permissions: self.admin_added_permissions,
            admin_removed_permissions: self.admin_removed_permissions,
            joined_at: humanize_timestamp(self.joined_at),
            is_online: self.is_online,
        }
    }
}

impl ToHuman for synctv_proto::admin::ProviderInstance {
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
            status: humanize_provider_instance_status(self.status as i64)
                .unwrap_or_else(|| self.status.to_string()),
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::client::StreamPublisherInfo {
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
            item_count: self.item_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::client::Media {
    type Human = HumanMedia;

    fn to_human(&self) -> Self::Human {
        HumanMedia {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            provider: self.provider.clone(),
            title: self.title.clone(),
            metadata: parse_json_bytes(&self.metadata),
            position: self.position,
            added_at: humanize_timestamp(self.added_at),
            added_by: self.added_by.clone(),
            provider_instance_name: self.provider_instance_name.clone(),
            source_config: parse_json_bytes(&self.source_config),
        }
    }
}

impl ToHuman for synctv_proto::client::PlaybackState {
    type Human = HumanPlaybackState;

    fn to_human(&self) -> Self::Human {
        HumanPlaybackState {
            room_id: self.room_id.clone(),
            playing_media_id: self.playing_media_id.clone(),
            current_time: self.current_time,
            speed: self.speed,
            is_playing: self.is_playing,
            updated_at: humanize_timestamp(self.updated_at),
            version: self.version,
            playing_playlist_id: self.playing_playlist_id.clone(),
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

impl ToHuman for synctv_proto::admin::ApproveUserResponse {
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

impl ToHuman for synctv_proto::admin::ApproveRoomResponse {
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
        }
    }
}

impl ToHuman for synctv_proto::admin::ListProviderInstancesResponse {
    type Human = HumanProviderInstancesResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstancesResponse {
            instances: self.instances.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::AddProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ReconnectProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::EnableProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::DisableProviderInstanceResponse {
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

impl ToHuman for synctv_proto::client::GetRoomMembersResponse {
    type Human = HumanRoomMembersResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanRoomMembersResponse {
            members: self.members.to_human(),
            total: self.total,
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
    type Human = HumanMediaResponse<HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanMediaResponse {
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
        }
    }
}

impl ToHuman for synctv_proto::client::GetPlaybackResponse {
    type Human = HumanGetPlaybackResponse<HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanGetPlaybackResponse {
            playback_state: self.playback_state.to_human(),
            playback_result: self.playback_result.clone(),
        }
    }
}

impl ToHuman for synctv_proto::client::CreatePublishKeyResponse {
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

impl ToHuman for synctv_proto::client::GetStreamInfoResponse {
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
    synctv_proto::admin::UpdateUserPasswordResponse,
    synctv_proto::admin::GetRoomSettingsResponse,
    synctv_proto::admin::UpdateRoomPasswordResponse,
    synctv_proto::admin::DeleteRoomResponse,
    synctv_proto::admin::RemoveAdminResponse,
    synctv_proto::admin::GetSystemStatsResponse,
    synctv_proto::admin::ListActiveStreamsResponse,
    synctv_proto::admin::KickStreamResponse,
    synctv_proto::admin::BatchBanUsersResponse,
    synctv_proto::admin::BatchDeleteUsersResponse,
    synctv_proto::admin::BatchBanRoomsResponse,
    synctv_proto::admin::BatchDeleteRoomsResponse,
    synctv_proto::admin::DeleteProviderInstanceResponse,
    synctv_proto::admin::UpdateSettingsResponse,
    synctv_proto::admin::SendTestEmailResponse,
    synctv_proto::client::LeaveRoomResponse,
    synctv_proto::client::DeleteRoomResponse,
    synctv_proto::client::GetRoomSettingsResponse,
    synctv_proto::client::ResetRoomSettingsResponse,
    synctv_proto::client::SetRoomPasswordResponse,
    synctv_proto::client::CheckRoomPasswordResponse,
    synctv_proto::client::KickMemberResponse,
    synctv_proto::client::BanMemberResponse,
    synctv_proto::client::UnbanMemberResponse,
    synctv_proto::client::DeletePlaylistResponse,
    synctv_proto::client::StartPlaybackResponse,
    synctv_proto::client::StopPlaybackResponse,
    synctv_proto::client::DeleteMediaResponse,
    synctv_proto::client::DeleteEntriesResponse,
    synctv_proto::client::ClearPlaylistResponse
);

#[cfg(test)]
fn render_human_output<T>(value: &T) -> Result<Value>
where
    T: ?Sized + ToHuman,
{
    Ok(serde_json::to_value(value.to_human())?)
}

fn humanize_user_role(raw: i64) -> Option<String> {
    use synctv_proto::common::UserRole;

    Some(
        match UserRole::try_from(raw as i32).ok()? {
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
        match UserStatus::try_from(raw as i32).ok()? {
            UserStatus::Unspecified => "unspecified",
            UserStatus::Active => "active",
            UserStatus::Pending => "pending",
            UserStatus::Banned => "banned",
        }
        .to_string(),
    )
}

fn humanize_room_status(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomStatus;

    Some(
        match RoomStatus::try_from(raw as i32).ok()? {
            RoomStatus::Unspecified => "unspecified",
            RoomStatus::Active => "active",
            RoomStatus::Pending => "pending",
            RoomStatus::Closed => "closed",
        }
        .to_string(),
    )
}

fn humanize_room_member_role(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomMemberRole;

    Some(
        match RoomMemberRole::try_from(raw as i32).ok()? {
            RoomMemberRole::Unspecified => "unspecified",
            RoomMemberRole::Guest => "guest",
            RoomMemberRole::Member => "member",
            RoomMemberRole::Admin => "admin",
            RoomMemberRole::Creator => "creator",
        }
        .to_string(),
    )
}

fn humanize_provider_instance_status(raw: i64) -> Option<String> {
    use synctv_proto::admin::ProviderInstanceStatus;

    Some(
        match ProviderInstanceStatus::try_from(raw as i32).ok()? {
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
        "bearer_token",
        "basic_password",
        "turn_shared_secret",
        "smtp_password",
        "root_password",
        "client_secret",
        "credential_encryption_key",
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
    if !url.contains("://") {
        return "<redacted>".to_string();
    }

    if let Ok(mut parsed) = url::Url::parse(url) {
        if !parsed.username().is_empty() {
            let _ = parsed.set_username("***");
        }
        if parsed.password().is_some() {
            let _ = parsed.set_password(Some("***"));
        }
        return parsed.to_string();
    }

    if let Some(at_pos) = url.rfind('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let host_part = &url[at_pos..];
            return format!("{scheme}***:***{host_part}");
        }
    }

    "<redacted>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn acquire_time_test_lock() -> MutexGuard<'static, ()> {
        static TIME_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TIME_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct TimeZoneGuard {
        previous: String,
    }

    impl TimeZoneGuard {
        fn set(name: &str) -> Self {
            let previous = synctv_core::time::default_timezone_name();
            synctv_core::time::set_default_timezone_name(name).expect("timezone should be valid");
            Self { previous }
        }
    }

    impl Drop for TimeZoneGuard {
        fn drop(&mut self) {
            let _ = synctv_core::time::set_default_timezone_name(&self.previous);
        }
    }

    fn sample_config() -> synctv_core::Config {
        let mut config = synctv_core::Config::default();
        config.database.url = "postgresql://synctv:super-secret-db@db.internal:5432/synctv".into();
        config.redis.url = "redis://:redis-secret@redis.internal:6379/0".into();
        config.jwt.secret = "jwt-secret-123456789012345678901234".into();
        config.server.cluster_secret = "cluster-secret-value".into();
        config.metrics.auth.bearer_token = "metrics-bearer-token".into();
        config.metrics.auth.basic_password = "metrics-basic-password".into();
        config.email.smtp_password = "smtp-secret".into();
        config.webrtc.turn_shared_secret = "turn-secret".into();
        config.bootstrap.root_password = "RootPass12345".into();
        config.oauth2.providers = json!({
            "github": {
                "client_id": "client-id",
                "client_secret": "oauth-client-secret"
            }
        });
        config
    }

    #[test]
    fn cli_requires_subcommand() {
        assert!(Cli::try_parse_from(["synctv"]).is_err());
    }

    #[test]
    fn cli_parses_serve_subcommand() {
        let cli = Cli::parse_from(["synctv", "serve", "--dry-run"]);
        match cli.command {
            Commands::Serve(args) => assert!(args.dry_run),
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_serve_daemon_mode() {
        let cli = Cli::parse_from(["synctv", "serve", "--daemon"]);
        match cli.command {
            Commands::Serve(args) => assert!(args.daemon),
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
                    args.remote.endpoint.as_deref(),
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
    fn cli_parses_db_status() {
        let cli = Cli::parse_from(["synctv", "db", "status"]);
        match cli.command {
            Commands::Db(DbCommand {
                command: DbSubcommand::Status(_),
                ..
            }) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_config_show_json_output() {
        let cli = Cli::parse_from(["synctv", "config", "show", "--output", "json"]);
        match cli.command {
            Commands::Config(ConfigCommand {
                command: ConfigSubcommand::Show(args),
                ..
            }) => assert_eq!(args.output, ConfigOutputFormat::Json),
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_config_show_toml_output_short_flag() {
        let cli = Cli::parse_from(["synctv", "config", "show", "-o", "toml"]);
        match cli.command {
            Commands::Config(ConfigCommand {
                command: ConfigSubcommand::Show(args),
                ..
            }) => assert_eq!(args.output, ConfigOutputFormat::Toml),
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_config_validate() {
        let cli = Cli::parse_from(["synctv", "config", "validate"]);
        match cli.command {
            Commands::Config(ConfigCommand {
                command: ConfigSubcommand::Validate(_),
                ..
            }) => {}
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
                assert!(args.remote.global.no_dotenv);
                assert_eq!(args.remote.global.verbose, 3);
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
                    args.remote.endpoint.as_deref(),
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
    fn cli_rejects_removed_management_access_token_flag() {
        let result = Cli::try_parse_from(["synctv", "user", "list", "--access-token", "token-123"]);
        assert!(
            result.is_err(),
            "management CLI must not accept user access tokens anymore"
        );
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
                assert_eq!(args.room_id, "room-123");
                assert_eq!(args.page, 3);
                assert_eq!(args.page_size, 10);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
        let cli = Cli::parse_from(["synctv", "room", "list", "--creator", "alice"]);
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
            "--password",
            "RoomPass12345",
            "--settings-json",
            "{\"chat_enabled\":false}",
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
                assert_eq!(args.password.as_deref(), Some("RoomPass12345"));
                assert_eq!(
                    args.settings_json.as_deref(),
                    Some("{\"chat_enabled\":false}")
                );
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
                assert_eq!(args.room_id, "room-123");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_room_settings_update_with_json_payload() {
        let cli = Cli::parse_from([
            "synctv",
            "room",
            "settings",
            "update",
            "room-123",
            "--settings-json",
            "{\"chat_enabled\":false}",
        ]);
        match cli.command {
            Commands::Room(RoomCommand {
                command:
                    RoomSubcommand::Settings(RoomSettingsCommand {
                        command: RoomSettingsSubcommand::Update(args),
                    }),
                ..
            }) => {
                assert_eq!(args.room_id, "room-123");
                assert_eq!(args.settings_json, "{\"chat_enabled\":false}");
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
    fn cli_parses_user_set_password_with_password_flag() {
        let cli = Cli::parse_from([
            "synctv",
            "user",
            "set-password",
            "alice",
            "--password",
            "StrongPass123!",
        ]);
        match cli.command {
            Commands::User(UserCommand {
                command: UserSubcommand::SetPassword(args),
                ..
            }) => {
                assert_eq!(args.user.username.as_deref(), Some("alice"));
                assert_eq!(args.new_password, "StrongPass123!");
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
            set_password_help.contains("--password <PASSWORD>"),
            "user set-password help should use --password: {set_password_help}"
        );
        assert!(
            !set_password_help.contains("--new-password"),
            "user set-password help should not expose --new-password: {set_password_help}"
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
            "--password",
            "StrongPass123",
        ]);
        match cli.command {
            Commands::User(UserCommand {
                command: UserSubcommand::Create(args),
                ..
            }) => {
                assert_eq!(args.username, "alice");
                assert_eq!(args.email.as_deref(), Some("alice@example.com"));
                assert_eq!(args.password, "StrongPass123");
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
    fn cli_parses_room_approve() {
        let cli = Cli::parse_from(["synctv", "room", "approve", "room-approve"]);
        match cli.command {
            Commands::Room(RoomCommand {
                command: RoomSubcommand::Approve(args),
                ..
            }) => {
                assert_eq!(args.room_id, "room-approve");
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
                assert_eq!(args.source_provider.as_deref(), Some("alist"));
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
        ]);
        match cli.command {
            Commands::Playlist(PlaylistCommand {
                command: PlaylistSubcommand::List(args),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.dynamic_only, Some(true));
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
    fn validate_playlist_create_requires_full_dynamic_field_set() {
        let args = PlaylistCreateArgs {
            room: RoomScopedRemoteArgs {
                remote: RemoteAccessArgs {
                    global: GlobalConfigArgs::default(),
                    endpoint: None,
                    output: RemoteOutputFormat::Human,
                },
                room_id: "room-123".to_string(),
            },
            actor: ActorUserArgs {
                username: Some("alice".to_string()),
                user_id: None,
            },
            name: "Favorites".to_string(),
            parent_id: None,
            source_provider: Some("alist".to_string()),
            source_config_json: None,
            provider_instance_name: None,
        };

        let error = validate_playlist_create(&args)
            .expect_err("dynamic playlist create should require config and provider instance");
        assert!(
            error
                .to_string()
                .contains("--source-config-json is required when --source-provider is set"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn validate_playlist_create_accepts_static_folder_without_dynamic_fields() {
        let args = PlaylistCreateArgs {
            room: RoomScopedRemoteArgs {
                remote: RemoteAccessArgs {
                    global: GlobalConfigArgs::default(),
                    endpoint: None,
                    output: RemoteOutputFormat::Human,
                },
                room_id: "room-123".to_string(),
            },
            actor: ActorUserArgs {
                username: Some("alice".to_string()),
                user_id: None,
            },
            name: "Favorites".to_string(),
            parent_id: None,
            source_provider: None,
            source_config_json: None,
            provider_instance_name: None,
        };

        validate_playlist_create(&args)
            .expect("static playlist create should not require dynamic fields");
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
            "--title",
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
                assert_eq!(args.title.as_deref(), Some("Demo Video"));
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
            "--provider",
            "alist",
            "--provider-instance-name",
            "alist-main",
            "--source-config-json",
            "{\"path\":\"/movies/demo.mp4\"}",
            "--title",
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
                assert_eq!(args.provider.as_deref(), Some("alist"));
                assert_eq!(args.provider_instance_name.as_deref(), Some("alist-main"));
                assert_eq!(args.source_config_json, "{\"path\":\"/movies/demo.mp4\"}");
                assert_eq!(args.title.as_deref(), Some("Demo Video"));
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
            "--title",
            "Renamed",
        ]);
        match cli.command {
            Commands::Media(MediaCommand {
                command: MediaSubcommand::Update(args),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.media_id, "media-1");
                assert_eq!(args.title, "Renamed");
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
            "--source-config-json",
            "{\"url\":\"https://cdn.example.com/video.mp4\"}",
        ]);
        assert!(
            result.is_err(),
            "media add must require --username or --user-id"
        );
    }

    #[test]
    fn cli_rejects_media_list_target_json_without_playlist_id() {
        let result = Cli::try_parse_from([
            "synctv",
            "media",
            "list",
            "--room-id",
            "room-123",
            "--target-json",
            "{\"path\":\"/movies\"}",
        ]);

        assert!(
            result.is_err(),
            "media list must require --playlist-id when --target-json is provided"
        );
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
            "media-1",
        ]);
        match cli.command {
            Commands::Media(MediaCommand {
                command: MediaSubcommand::Move(args),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.media_id, "media-1");
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
            "--provider",
            "alist",
            "--provider-instance-name",
            "alist_main",
            "--source-config-json",
            "{\"path\":\"/movies/demo.mp4\"}",
            "--title",
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
                assert_eq!(args.provider.as_deref(), Some("alist"));
                assert_eq!(args.provider_instance_name.as_deref(), Some("alist_main"));
                assert_eq!(args.source_config_json, "{\"path\":\"/movies/demo.mp4\"}");
                assert_eq!(args.title.as_deref(), Some("Demo Media"));
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
            "media-a",
        ]);
        match cli.command {
            Commands::Media(MediaCommand {
                command: MediaSubcommand::Move(args),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.media_id, "media-a");
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
            "-99tNxdXRosK",
        ])
        .expect("hyphen-prefixed media ids should be accepted as positional media ids");
        match cli.command {
            Commands::Media(MediaCommand {
                command: MediaSubcommand::Move(args),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.media_id, "-99tNxdXRosK");
                assert_eq!(args.before_media_id.as_deref(), Some("media-1"));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_rejects_media_move_without_anchor() {
        let error = Cli::try_parse_from([
            "synctv",
            "media",
            "move",
            "--room-id",
            "room-123",
            "media-1",
        ])
        .expect_err("move should require one anchor flag");
        let rendered = error.to_string();
        assert!(
            rendered.contains("--before-media-id")
                || rendered.contains("--after-media-id")
                || rendered.contains("required"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn validate_generic_media_add_requires_provider_instance_for_remote_provider() {
        let args = MediaAddArgs {
            room: RoomScopedRemoteArgs {
                remote: RemoteAccessArgs {
                    global: GlobalConfigArgs::default(),
                    endpoint: None,
                    output: RemoteOutputFormat::Human,
                },
                room_id: "room-123".to_string(),
            },
            actor: ActorUserArgs {
                username: Some("alice".to_string()),
                user_id: None,
            },
            playlist_id: None,
            provider: Some("alist".to_string()),
            provider_instance_name: None,
            source_config_json: "{\"path\":\"/movies/demo.mp4\"}".to_string(),
            title: None,
        };

        let error = validate_generic_media_add(&args)
            .expect_err("provider-backed media add should require provider_instance_name");
        assert!(
            error
                .to_string()
                .contains("--provider-instance-name is required"),
            "unexpected error: {error:#}"
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
            "--config-json",
            "{\"jwt_secret\":\"provider-secret-12345678901234567890\"}",
        ]);
        match cli.command {
            Commands::Provider(ProviderCommand {
                command: ProviderSubcommand::Create(args),
                ..
            }) => {
                assert_eq!(args.name, "alist-edge");
                assert_eq!(args.provider_endpoint, "https://provider.example.com:50051");
                assert_eq!(args.providers, vec!["alist", "emby"]);
                assert_eq!(args.comment.as_deref(), Some("edge provider"));
                assert_eq!(args.timeout_seconds, 15);
                assert!(args.tls);
                assert_eq!(
                    args.config_json.as_deref(),
                    Some("{\"jwt_secret\":\"provider-secret-12345678901234567890\"}")
                );
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn validate_provider_add_requires_tls_when_insecure_tls_is_enabled() {
        let args = ProviderAddArgs {
            name: "alist-edge".to_string(),
            provider_endpoint: "https://provider.example.com:50051".to_string(),
            comment: None,
            timeout_seconds: 10,
            tls: false,
            insecure_tls: true,
            providers: vec!["alist".to_string()],
            config_json: None,
            remote: RemoteAccessArgs {
                global: GlobalConfigArgs::default(),
                endpoint: None,
                output: RemoteOutputFormat::Human,
            },
        };

        let error = validate_provider_add(&args)
            .expect_err("provider add should reject insecure TLS without TLS enabled");
        assert!(
            error.to_string().contains("--insecure-tls requires --tls"),
            "unexpected error: {error:#}"
        );
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
                assert_eq!(args.providers, vec!["alist"]);
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
    fn validate_provider_update_rejects_insecure_tls_when_tls_is_explicitly_disabled() {
        let args = ProviderUpdateArgs {
            name: "alist-edge".to_string(),
            provider_endpoint: None,
            comment: None,
            clear_comment: false,
            timeout_seconds: None,
            tls: Some(false),
            insecure_tls: Some(true),
            providers: Vec::new(),
            config_json: None,
            remote: RemoteAccessArgs {
                global: GlobalConfigArgs::default(),
                endpoint: None,
                output: RemoteOutputFormat::Human,
            },
        };

        let error = validate_provider_update_transport_flags(&args).expect_err(
            "provider update should reject insecure TLS when TLS is explicitly disabled",
        );
        assert!(
            error
                .to_string()
                .contains("--insecure-tls true cannot be combined with --tls false"),
            "unexpected error: {error:#}"
        );
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
                assert_eq!(args.provider_type.as_deref(), Some("alist"));
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
                assert_eq!(args.provider_type.as_deref(), Some("alist"));
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
                assert_eq!(args.admin_added_permissions, Some(7));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_room_stream_publish_key_subcommand() {
        let cli = Cli::parse_from([
            "synctv",
            "room",
            "stream",
            "publish-key",
            "--room-id",
            "room-1",
            "--username",
            "alice",
            "media-1",
        ]);
        match cli.command {
            Commands::Room(RoomCommand {
                command:
                    RoomSubcommand::Stream(RoomStreamCommand {
                        command: RoomStreamSubcommand::PublishKey(args),
                    }),
            }) => {
                assert_eq!(args.room.room_id, "room-1");
                assert_eq!(args.actor.username.as_deref(), Some("alice"));
                assert_eq!(args.actor.user_id, None);
                assert_eq!(args.media_id, "media-1");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_room_stream_get() {
        let cli = Cli::parse_from([
            "synctv",
            "room",
            "stream",
            "get",
            "--room-id",
            "room-1",
            "media-1",
        ]);
        match cli.command {
            Commands::Room(RoomCommand {
                command:
                    RoomSubcommand::Stream(RoomStreamCommand {
                        command: RoomStreamSubcommand::Get(args),
                    }),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-1");
                assert_eq!(args.media_id, "media-1");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
                    management_proto::UserRole::Admin as i32
                );
                assert_eq!(
                    args.status.map(CliUserStatus::to_proto),
                    Some(management_proto::UserStatus::Active as i32)
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
    fn cli_parses_room_stream_get_with_hyphen_prefixed_media_id() {
        let cli = Cli::try_parse_from([
            "synctv",
            "room",
            "stream",
            "get",
            "--room-id",
            "room-123",
            "-99tNxdXRosK",
        ])
        .expect("hyphen-prefixed media ids should be accepted as positional values");
        match cli.command {
            Commands::Room(RoomCommand {
                command:
                    RoomSubcommand::Stream(RoomStreamCommand {
                        command: RoomStreamSubcommand::Get(args),
                    }),
                ..
            }) => {
                assert_eq!(args.room.room_id, "room-123");
                assert_eq!(args.media_id, "-99tNxdXRosK");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_settings_get() {
        let cli = Cli::parse_from(["synctv", "settings", "get", "server"]);
        match cli.command {
            Commands::Settings(SettingsCommand {
                command: SettingsSubcommand::Get(args),
                ..
            }) => {
                assert_eq!(args.group, "server");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_settings_update_with_repeated_set_entries() {
        let cli = Cli::parse_from([
            "synctv",
            "settings",
            "update",
            "server",
            "--set",
            "signup_enabled=false",
            "--set",
            "max_rooms_per_user=42",
        ]);
        match cli.command {
            Commands::Settings(SettingsCommand {
                command: SettingsSubcommand::Update(args),
                ..
            }) => {
                assert_eq!(args.group, "server");
                assert_eq!(
                    args.entries,
                    vec![
                        "signup_enabled=false".to_string(),
                        "max_rooms_per_user=42".to_string()
                    ]
                );
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
        let rendered = render_config_for_display(&sample_config());
        let rendered_text =
            serde_json::to_string(&rendered).expect("rendered config should serialize");

        for secret in [
            "super-secret-db",
            "redis-secret",
            "jwt-secret-123456789012345678901234",
            "cluster-secret-value",
            "metrics-bearer-token",
            "metrics-basic-password",
            "smtp-secret",
            "turn-secret",
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
    fn database_status_summary_masks_credentials() {
        let summary = database_status_summary(&sample_config());

        assert!(
            !summary.contains("super-secret-db"),
            "database status summary leaked password: {summary}"
        );
        assert!(
            summary.contains("postgresql://***:***@db.internal:5432/synctv"),
            "database status summary should print masked url: {summary}"
        );
    }

    #[test]
    fn render_human_output_converts_user_timestamps_role_and_status() {
        let _lock = acquire_time_test_lock();
        let _timezone = TimeZoneGuard::set("UTC");
        let rendered = render_human_output(&synctv_proto::admin::GetUserResponse {
            user: Some(synctv_proto::admin::AdminUser {
                id: "I9jXL5s61FPV".into(),
                username: "root".into(),
                email: String::new(),
                role: synctv_proto::common::UserRole::Root as i32,
                status: synctv_proto::common::UserStatus::Banned as i32,
                created_at: 1_775_144_583_i64,
                updated_at: 1_775_291_071_i64,
            }),
        })
        .expect("human output should render");

        assert_eq!(rendered["user"]["role"], "root");
        assert_eq!(rendered["user"]["status"], "banned");
        assert_eq!(
            rendered["user"]["created_at"],
            "2026-04-02 15:43:03 +00:00 (UTC) (1775144583)"
        );
        assert_eq!(
            rendered["user"]["updated_at"],
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
                settings: br#"{"sync":true}"#.to_vec(),
                created_at: 1_775_144_583_i64,
                member_count: 1,
                description: String::new(),
                updated_at: 1_775_291_071_i64,
                is_banned: false,
            }),
            playback_state: None,
            members: vec![synctv_proto::common::RoomMember {
                room_id: "room-1".into(),
                user_id: "user-1".into(),
                username: "root".into(),
                role: synctv_proto::common::RoomMemberRole::Creator as i32,
                permissions: 0,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
                joined_at: 1_775_291_657_i64,
                is_online: true,
            }],
        })
        .expect("human output should render");
        let instances = render_human_output(&synctv_proto::admin::ListProviderInstancesResponse {
            instances: vec![synctv_proto::admin::ProviderInstance {
                name: "provider-1".into(),
                endpoint: "http://127.0.0.1:50052".into(),
                comment: String::new(),
                timeout_seconds: 30,
                tls: false,
                insecure_tls: false,
                providers: vec!["direct_url".into()],
                enabled: true,
                status: synctv_proto::admin::ProviderInstanceStatus::Disconnected as i32,
                created_at: 1_775_144_583_i64,
                updated_at: 1_775_291_071_i64,
            }],
        })
        .expect("human output should render");

        assert_eq!(rendered["room"]["status"], "closed");
        assert_eq!(rendered["members"][0]["role"], "creator");
        assert_eq!(
            rendered["members"][0]["joined_at"],
            "2026-04-04 08:34:17 +00:00 (UTC) (1775291657)"
        );
        assert_eq!(instances["instances"][0]["status"], "disconnected");
    }

    #[test]
    fn render_human_output_converts_room_listing_without_context_inference() {
        let _lock = acquire_time_test_lock();
        let _timezone = TimeZoneGuard::set("UTC");
        let rendered = render_human_output(&synctv_proto::admin::ListRoomsResponse {
            rooms: vec![synctv_proto::admin::AdminRoom {
                id: "room-1".into(),
                name: "General".into(),
                creator_id: "user-1".into(),
                creator_username: "root".into(),
                status: synctv_proto::common::RoomStatus::Active as i32,
                settings: br#"{"public":true}"#.to_vec(),
                member_count: 3,
                created_at: 1_775_144_583_i64,
                updated_at: 1_775_291_071_i64,
                description: "main room".into(),
                is_banned: false,
            }],
            total: 1,
        })
        .expect("human output should render");

        assert_eq!(rendered["rooms"][0]["status"], "active");
        assert_eq!(
            rendered["rooms"][0]["created_at"],
            "2026-04-02 15:43:03 +00:00 (UTC) (1775144583)"
        );
    }

    #[test]
    fn parse_setting_entries_rejects_invalid_and_duplicate_entries() {
        let invalid = parse_setting_entries(&["signup_enabled".to_string()]);
        assert!(invalid.is_err(), "missing '=' must be rejected");

        let duplicate = parse_setting_entries(&[
            "signup_enabled=false".to_string(),
            "signup_enabled=true".to_string(),
        ]);
        assert!(duplicate.is_err(), "duplicate keys must be rejected");
    }

    #[test]
    fn ensure_provider_update_requested_rejects_empty_update() {
        let args = ProviderUpdateArgs {
            name: "alist-edge".to_string(),
            provider_endpoint: None,
            comment: None,
            clear_comment: false,
            timeout_seconds: None,
            tls: None,
            insecure_tls: None,
            providers: Vec::new(),
            config_json: None,
            remote: RemoteAccessArgs {
                global: GlobalConfigArgs::default(),
                endpoint: None,
                output: RemoteOutputFormat::Human,
            },
        };
        assert!(
            ensure_provider_update_requested(&args).is_err(),
            "provider update without any requested changes must be rejected"
        );
    }

    #[test]
    fn resolve_remote_endpoint_returns_none_when_cli_endpoint_is_absent() {
        let endpoint = resolve_remote_endpoint(None);

        assert_eq!(endpoint, None);
    }

    #[test]
    fn resolve_remote_endpoint_preserves_explicit_unix_socket_endpoint() {
        let raw = format!("unix://{}", default_management_unix_socket_path().display());
        let endpoint = resolve_remote_endpoint(Some(&raw));

        assert_eq!(endpoint.as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn resolve_remote_endpoint_preserves_explicit_tcp_endpoint() {
        let endpoint = resolve_remote_endpoint(Some("http://192.0.2.10:50099"));

        assert_eq!(endpoint.as_deref(), Some("http://192.0.2.10:50099"));
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
                no_dotenv: true,
                verbose: 0,
            },
            endpoint: None,
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
    fn daemon_readiness_probe_uses_management_endpoint_when_enabled() {
        let config = synctv_core::Config::default();

        assert_eq!(
            daemon_readiness_probe(&config),
            DaemonReadinessProbe::ManagementEndpoint(config.management_endpoint())
        );
    }

    #[test]
    fn daemon_readiness_probe_uses_api_health_when_management_is_disabled() {
        let mut config = synctv_core::Config::default();
        config.management.enabled = false;
        config.server.host = "0.0.0.0".to_string();
        config.server.port = 58080;

        assert_eq!(
            daemon_readiness_probe(&config),
            DaemonReadinessProbe::ApiTcpAddress("127.0.0.1:58080".to_string())
        );
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
}
