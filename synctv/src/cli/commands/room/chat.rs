use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomChatCommand {
    #[command(subcommand)]
    pub command: RoomChatSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomChatSubcommand {
    /// Search room chat history as a real room actor
    Search(RoomChatSearchArgs),
}

#[derive(Debug, Args)]
pub struct RoomChatSearchArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub query: String,

    #[arg(long)]
    pub cursor: Option<String>,

    #[arg(long, default_value_t = 50)]
    pub limit: i32,

    #[arg(long)]
    pub include_deleted: bool,

    #[command(flatten)]
    pub sender: ChatSenderFilterArgs,
}

#[derive(Debug, Clone, Args, Default)]
#[command(group(
    ArgGroup::new("chat_sender_ref")
        .args(["sender_username", "sender_user_id"])
        .multiple(false)
))]
pub struct ChatSenderFilterArgs {
    /// Restrict results to messages sent by this username
    #[arg(
        long = "sender-username",
        value_name = "USERNAME",
        group = "chat_sender_ref"
    )]
    pub sender_username: Option<String>,

    /// Restrict results to messages sent by this public user ID
    #[arg(
        long = "sender-user-id",
        value_name = "USER_ID",
        group = "chat_sender_ref"
    )]
    pub sender_user_id: Option<String>,
}

impl ChatSenderFilterArgs {
    pub(in crate::cli) fn to_management_proto(&self) -> Option<management_proto::UserRef> {
        let value = self
            .sender_user_id
            .as_deref()
            .map(|user_id| management_proto::user_ref::Value::UserId(user_id.to_string()))
            .or_else(|| {
                self.sender_username.as_deref().map(|username| {
                    management_proto::user_ref::Value::Username(username.to_string())
                })
            })?;
        Some(management_proto::UserRef { value: Some(value) })
    }
}
