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
pub struct RoomBatchBanArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", num_args = 1..)]
    pub room_ids: Vec<String>,

    #[arg(long)]
    pub reason: Option<String>,
}

impl RoomBatchBanArgs {
    pub(in crate::cli) fn resolved_room_ids(&self) -> Vec<String> {
        self.room_ids.clone()
    }
}

#[derive(Debug, Args)]
pub struct RoomBatchDeleteArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", num_args = 1..)]
    pub room_ids: Vec<String>,
}

impl RoomBatchDeleteArgs {
    pub(in crate::cli) fn resolved_room_ids(&self) -> Vec<String> {
        self.room_ids.clone()
    }
}
