use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomTaxonomyCommand {
    #[command(subcommand)]
    pub command: RoomTaxonomySubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomTaxonomySubcommand {
    /// Set or clear a room category and replace labels
    Set(RoomTaxonomySetArgs),
}

#[derive(Debug, Args)]
pub struct RoomTaxonomySetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(long, allow_hyphen_values = true, conflicts_with = "clear_category")]
    pub category_id: Option<String>,

    #[arg(long, default_value_t = false, conflicts_with = "category_id")]
    pub clear_category: bool,

    /// Room label public ID. Repeat or pass comma-separated values.
    #[arg(long = "label-id", value_delimiter = ',', allow_hyphen_values = true)]
    pub label_ids: Vec<String>,
}
