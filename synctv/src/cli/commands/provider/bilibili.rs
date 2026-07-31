use super::super::prelude::*;
use super::{ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs};

#[derive(Debug, Args)]
pub struct ProviderBilibiliCommand {
    #[command(subcommand)]
    pub command: ProviderBilibiliSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderBilibiliSubcommand {
    /// Parse a Bilibili URL, using the user's global bind when available
    Parse(ProviderBilibiliParseArgs),
    /// Generate a QR code for Bilibili login
    LoginQr(ProviderBilibiliLoginQrArgs),
    /// Poll the QR code login status and persist the bind on success
    CheckQr(ProviderBilibiliCheckQrArgs),
    /// Start a Bilibili SMS login session and return Geetest parameters
    StartSmsLogin(ProviderBilibiliStartSmsLoginArgs),
    /// Send a Bilibili SMS verification code
    SendSms(ProviderBilibiliSendSmsArgs),
    /// Log in with Bilibili SMS and persist the bind
    LoginSms(ProviderBilibiliLoginSmsArgs),
    /// Show the current Bilibili account info for the user's global bind
    Me(ProviderBilibiliGetUserInfoArgs),
    /// Remove a saved Bilibili bind
    Logout(ProviderServiceRemoteActorArgs),
    /// List saved Bilibili binds for a user
    Binds(ProviderBilibiliBindsArgs),
    /// List live-area categories
    LiveAreas(ProviderBilibiliListLiveAreasArgs),
    /// List the user's favorite folders
    FavoriteFolders(ProviderBilibiliFavoriteFoldersArgs),
    /// List followed PGC seasons
    FollowedPgc(ProviderBilibiliFollowedPgcArgs),
    /// List watch history
    History(ProviderBilibiliHistoryArgs),
    /// List the PGC release timeline
    PgcTimeline(ProviderBilibiliPgcTimelineArgs),
    /// Browse PGC seasons
    PgcSeasons(ProviderBilibiliPgcSeasonsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliParseArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// Bilibili page URL to parse
    pub url: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliCheckQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// QR login polling key returned by `login-qr`
    #[arg(long)]
    pub key: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliStartSmsLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliSendSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Mobile phone number in the format expected by the backend provider
    #[arg(long)]
    pub phone: String,

    /// Signed SMS login session token returned by `start-sms-login`
    #[arg(long)]
    pub session_token: String,

    /// Geetest validate result produced by a frontend captcha widget
    #[arg(long)]
    pub validate: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// SMS verification code
    #[arg(long)]
    pub code: String,

    /// Signed SMS login session token returned by `send-sms`
    #[arg(long)]
    pub session_token: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliGetUserInfoArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliListLiveAreasArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderBilibiliFavoriteFoldersArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderBilibiliFollowType {
    Anime,
    Cinema,
}
impl ProviderBilibiliFollowType {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::Anime => 1,
            Self::Cinema => 2,
        }
    }
}
#[derive(Debug, Args)]
pub struct ProviderBilibiliFollowedPgcArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long, value_enum, default_value_t = ProviderBilibiliFollowType::Anime)]
    pub r#type: ProviderBilibiliFollowType,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=30))]
    pub page_size: u32,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderBilibiliHistoryType {
    All,
    Archive,
    Live,
}
impl ProviderBilibiliHistoryType {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::All => 0,
            Self::Archive => 1,
            Self::Live => 2,
        }
    }
}
#[derive(Debug, Args)]
pub struct ProviderBilibiliHistoryArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long, value_enum, default_value_t = ProviderBilibiliHistoryType::All)]
    pub r#type: ProviderBilibiliHistoryType,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=30))]
    pub page_size: u32,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderBilibiliTimelineType {
    Anime,
    Cinema,
    Guochuang,
}
impl ProviderBilibiliTimelineType {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::Anime => 1,
            Self::Cinema => 3,
            Self::Guochuang => 4,
        }
    }
}
#[derive(Debug, Args)]
pub struct ProviderBilibiliPgcTimelineArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long, value_enum, default_value_t = ProviderBilibiliTimelineType::Anime)]
    pub r#type: ProviderBilibiliTimelineType,
    #[arg(long, default_value_t = 0)]
    pub before_days: u32,
    #[arg(long, default_value_t = 0)]
    pub after_days: u32,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderBilibiliSeasonType {
    Anime,
    Movie,
    Documentary,
    Guochuang,
    Tv,
    Variety,
}
impl ProviderBilibiliSeasonType {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::Anime => 1,
            Self::Movie => 2,
            Self::Documentary => 3,
            Self::Guochuang => 4,
            Self::Tv => 5,
            Self::Variety => 7,
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderBilibiliSeasonOrder {
    Updated,
    Danmaku,
    Play,
    Follow,
    Score,
    Started,
    Released,
}
impl ProviderBilibiliSeasonOrder {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::Updated => 0,
            Self::Danmaku => 1,
            Self::Play => 2,
            Self::Follow => 3,
            Self::Score => 4,
            Self::Started => 5,
            Self::Released => 6,
        }
    }
}
#[derive(Debug, Args)]
pub struct ProviderBilibiliPgcSeasonsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long, value_enum, default_value_t = ProviderBilibiliSeasonType::Anime)]
    pub r#type: ProviderBilibiliSeasonType,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
    pub page_size: u32,
    #[arg(long, value_enum, default_value_t = ProviderBilibiliSeasonOrder::Updated)]
    pub order: ProviderBilibiliSeasonOrder,
    #[arg(long, default_value_t = false)]
    pub ascending: bool,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub finished: Option<bool>,
    #[arg(long)]
    pub area: Option<String>,
    #[arg(long)]
    pub year: Option<String>,
    #[arg(long)]
    pub style_id: Option<u64>,
}
