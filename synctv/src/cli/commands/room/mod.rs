use super::prelude::*;

mod batch;
mod category;
mod chat;
mod label;
mod member;
mod playback;
mod settings;
mod stream;
mod taxonomy;

pub use batch::*;
pub use category::*;
pub use chat::*;
pub use label::*;
pub use member::*;
pub use playback::*;
pub use settings::*;
pub use stream::*;
pub use taxonomy::*;

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
    /// Change whether a room is public or private
    Visibility(RoomVisibilityArgs),
    /// Manage room categories
    Category(RoomCategoryCommand),
    /// Manage room labels
    Label(RoomLabelCommand),
    /// Assign room category and labels
    Taxonomy(RoomTaxonomyCommand),
    /// Search room chat history
    Chat(RoomChatCommand),
    /// Transfer room ownership to another existing member
    TransferOwner(RoomTransferOwnerArgs),
    /// Manage a user's favorite rooms
    Favorite(RoomFavoriteCommand),
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
    /// Inspect room ban records
    Bans(RoomBansCommand),
    /// Delete a room
    Delete(RoomDeleteArgs),
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

    /// Room category public ID
    #[arg(long, allow_hyphen_values = true)]
    pub category_id: Option<String>,

    /// Room label public ID. Repeat or pass comma-separated values.
    #[arg(long = "label-id", value_delimiter = ',', allow_hyphen_values = true)]
    pub label_ids: Vec<String>,

    /// Hide the room from discovery and deny anonymous guest access
    #[arg(long = "private")]
    pub private_room: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_visibility")
        .args(["public_room", "private_room"])
        .required(true)
        .multiple(false)
))]
pub struct RoomVisibilityArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    /// Room public ID
    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: String,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// List the room in discovery and allow anonymous guest access
    #[arg(long = "public", group = "room_visibility")]
    pub public_room: bool,

    /// Hide the room from discovery and deny anonymous guest access
    #[arg(long = "private", group = "room_visibility")]
    pub private_room: bool,
}

impl RoomVisibilityArgs {
    pub(in crate::cli) const fn is_public(&self) -> bool {
        self.public_room
    }
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
    pub(in crate::cli) fn to_management_proto(&self) -> Result<management_proto::UserRef> {
        UserRefArgs {
            username: self.new_owner_username.clone(),
            user_id: self.new_owner_user_id.clone(),
        }
        .to_management_proto()
    }
}

#[derive(Debug, Args)]
pub struct RoomFavoriteCommand {
    #[command(subcommand)]
    pub command: RoomFavoriteSubcommand,
}

#[derive(Debug, Args)]
pub struct RoomBansCommand {
    #[command(subcommand)]
    pub command: RoomBansSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomBansSubcommand {
    /// List room ban records
    List(RoomBansListArgs),
}

#[derive(Debug, Args)]
pub struct RoomBansListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    /// Filter by active or inactive records
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub active: Option<bool>,

    /// Filter by public room ID
    #[arg(long, allow_hyphen_values = true)]
    pub room_id: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Subcommand)]
pub enum RoomFavoriteSubcommand {
    /// Add a room to a user's favorites
    Add(RoomFavoriteUpdateArgs),
    /// Remove a room from a user's favorites
    Remove(RoomFavoriteUpdateArgs),
    /// List a user's favorite rooms
    List(RoomFavoriteListArgs),
}

#[derive(Debug, Args)]
pub struct RoomFavoriteUpdateArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[command(flatten)]
    pub actor: ActorUserArgs,
}

#[derive(Debug, Args)]
pub struct RoomFavoriteListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,
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

    #[arg(long, allow_hyphen_values = true)]
    pub category_id: Option<String>,

    /// Room label public ID. Repeat or pass comma-separated values.
    #[arg(long = "label-id", value_delimiter = ',', allow_hyphen_values = true)]
    pub label_ids: Vec<String>,

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
