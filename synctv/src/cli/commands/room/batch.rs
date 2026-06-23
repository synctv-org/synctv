use super::super::prelude::*;

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
    pub(in crate::cli) fn resolved_room_ids(&self) -> Vec<String> {
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
    pub(in crate::cli) fn resolved_room_ids(&self) -> Vec<String> {
        self.room_ids
            .iter()
            .chain(self.room_id_flags.iter())
            .cloned()
            .collect()
    }
}
