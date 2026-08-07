use super::prelude::*;

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
    pub(in crate::cli) const fn to_proto(self) -> i32 {
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
