use super::prelude::*;

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
    /// Restore a user during the deletion retention window
    Restore(UserRestoreArgs),
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

    /// Include users currently in the deletion recovery window
    #[arg(long)]
    pub include_deleted: bool,
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
pub struct UserRestoreArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    /// Restore the account while leaving occupied username/email/OAuth identities released
    #[arg(long)]
    pub ignore_identity_conflicts: bool,
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
pub struct UserSetRoleArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum, required = true)]
    pub role: CliUserRole,
}

impl UserSetRoleArgs {
    pub(in crate::cli) const fn resolved_role(&self) -> CliUserRole {
        self.role
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
