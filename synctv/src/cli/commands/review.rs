use super::prelude::*;

#[derive(Debug, Args)]
pub struct ReviewCommand {
    #[command(subcommand)]
    pub command: ReviewSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewSubcommand {
    /// User registration review workflow
    UserRegistration(ReviewUserRegistrationCommand),
    /// Room creation review workflow
    RoomCreation(ReviewRoomCreationCommand),
    /// Room join review workflow
    RoomJoin(ReviewRoomJoinCommand),
}

#[derive(Debug, Args)]
pub struct ReviewUserRegistrationCommand {
    #[command(subcommand)]
    pub command: ReviewUserRegistrationSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewUserRegistrationSubcommand {
    List(ReviewUserRegistrationListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Debug, Args)]
pub struct ReviewRoomCreationCommand {
    #[command(subcommand)]
    pub command: ReviewRoomCreationSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewRoomCreationSubcommand {
    List(ReviewRoomCreationListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Debug, Args)]
pub struct ReviewRoomJoinCommand {
    #[command(subcommand)]
    pub command: ReviewRoomJoinSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewRoomJoinSubcommand {
    List(ReviewRoomJoinListArgs),
    Approve(ReviewApproveArgs),
    Reject(ReviewRejectArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum CliReviewStatus {
    Pending,
    Approved,
    Rejected,
}

impl CliReviewStatus {
    pub(in crate::cli) const fn to_proto(self) -> i32 {
        match self {
            Self::Pending => synctv_proto::common::ReviewStatus::Pending as i32,
            Self::Approved => synctv_proto::common::ReviewStatus::Approved as i32,
            Self::Rejected => synctv_proto::common::ReviewStatus::Rejected as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct ReviewUserRegistrationListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long)]
    pub search: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewRoomCreationListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long, allow_hyphen_values = true)]
    pub requested_by: Option<String>,
    #[arg(long)]
    pub search: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewRoomJoinListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(long, value_enum, default_value_t = CliReviewStatus::Pending)]
    pub status: CliReviewStatus,
    #[arg(long, allow_hyphen_values = true)]
    pub room_id: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    pub user_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i32,
    #[arg(long, default_value_t = 50)]
    pub page_size: i32,
}

#[derive(Debug, Args)]
pub struct ReviewApproveArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(allow_hyphen_values = true)]
    pub request_id: String,
}

#[derive(Debug, Args)]
pub struct ReviewRejectArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
    #[arg(allow_hyphen_values = true)]
    pub request_id: String,
    #[arg(long)]
    pub reason: Option<String>,
}
