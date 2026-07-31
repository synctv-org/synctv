use super::prelude::*;

mod acfun;
mod alist;
mod bilibili;
mod cctv;
mod cloudreve;
mod douyin;
mod douyu;
mod emby;
mod fnos;
mod huya;
mod nextcloud;
mod qnap;
mod rtmp;
mod seafile;
mod shared;
mod synology;
mod tiktok;
mod truenas;
mod twitch;
mod youtube;

pub use acfun::*;
pub use alist::*;
pub use bilibili::*;
pub use cctv::*;
pub use cloudreve::*;
pub use douyin::*;
pub use douyu::*;
pub use emby::*;
pub use fnos::*;
pub use huya::*;
pub use nextcloud::*;
pub use qnap::*;
pub use rtmp::*;
pub use seafile::*;
pub use shared::*;
pub use synology::*;
pub use tiktok::*;
pub use truenas::*;
pub use twitch::*;
pub use youtube::*;

#[derive(Debug, Args)]
pub struct ProviderCommand {
    #[command(subcommand)]
    pub command: ProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderSubcommand {
    /// List backends for one provider type, including the default backend when present
    Backends(ProviderBackendsArgs),
    /// Alist provider service operations
    Alist(ProviderAlistCommand),
    /// Emby provider service operations
    Emby(ProviderEmbyCommand),
    /// Bilibili provider service operations
    Bilibili(ProviderBilibiliCommand),
    /// Douyin provider service operations
    Douyin(ProviderDouyinCommand),
    /// TikTok provider service operations
    Tiktok(ProviderTikTokCommand),
    /// Twitch provider service operations
    Twitch(ProviderTwitchCommand),
    /// RTMP provider service operations
    Rtmp(ProviderRtmpCommand),
    /// AcFun provider service operations
    Acfun(ProviderAcfunCommand),
    /// CCTV provider service operations
    Cctv(ProviderCctvCommand),
    /// Cloudreve provider service operations
    Cloudreve(ProviderCloudreveCommand),
    /// Douyu provider service operations
    Douyu(ProviderDouyuCommand),
    /// FNOS provider service operations
    Fnos(ProviderFnosCommand),
    /// Huya provider service operations
    Huya(ProviderHuyaCommand),
    /// Nextcloud provider service operations
    Nextcloud(ProviderNextcloudCommand),
    /// QNAP provider service operations
    Qnap(ProviderQnapCommand),
    /// Seafile provider service operations
    Seafile(ProviderSeafileCommand),
    /// Synology provider service operations
    Synology(ProviderSynologyCommand),
    /// TrueNAS provider service operations
    Truenas(ProviderTruenasCommand),
    /// YouTube provider service operations
    Youtube(ProviderYoutubeCommand),
}

#[derive(Debug, Args)]
pub struct ProviderBackendsArgs {
    pub provider_type: CliSourceProvider,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
