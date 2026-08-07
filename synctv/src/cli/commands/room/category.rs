use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomCategoryCommand {
    #[command(subcommand)]
    pub command: RoomCategorySubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomCategorySubcommand {
    /// List room categories
    List(RoomCategoryListArgs),
    /// Create or update a room category by stable key
    Upsert(RoomCategoryUpsertArgs),
    /// Delete a room category by public ID
    Delete(RoomCategoryDeleteArgs),
}

#[derive(Debug, Args)]
pub struct RoomCategoryListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = false)]
    pub include_disabled: bool,
}

#[derive(Debug, Args)]
pub struct RoomCategoryUpsertArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    pub key: String,

    #[arg(long)]
    pub name: String,

    #[arg(long)]
    pub description: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub sort_order: i32,

    #[arg(long)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Args)]
pub struct RoomCategoryDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub category_id: String,
}
