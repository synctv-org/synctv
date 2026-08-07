use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomLabelCommand {
    #[command(subcommand)]
    pub command: RoomLabelSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomLabelSubcommand {
    /// List room labels
    List(RoomLabelListArgs),
    /// Create or update a room label by stable key
    Upsert(RoomLabelUpsertArgs),
    /// Delete a room label by public ID
    Delete(RoomLabelDeleteArgs),
}

#[derive(Debug, Args)]
pub struct RoomLabelListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = false)]
    pub include_disabled: bool,

    #[arg(long, allow_hyphen_values = true)]
    pub category_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomLabelUpsertArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    pub key: String,

    #[arg(long)]
    pub name: String,

    #[arg(long)]
    pub description: Option<String>,

    #[arg(long)]
    pub color: Option<String>,

    #[arg(long, allow_hyphen_values = true)]
    pub category_id: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub sort_order: i32,

    #[arg(long)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Args)]
pub struct RoomLabelDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub label_id: String,
}
