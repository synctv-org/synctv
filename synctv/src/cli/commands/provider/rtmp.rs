use super::super::prelude::*;
use super::ProviderServiceRemoteActorArgs;

#[derive(Debug, Args)]
pub struct ProviderRtmpCommand {
    #[command(subcommand)]
    pub command: ProviderRtmpSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderRtmpSubcommand {
    /// Create a single-use RTMP publish key for a media item as a specific real user
    CreatePublishKey(ProviderRtmpPublishKeyArgs),
    /// Get the active RTMP stream state for one room media item
    GetStreamInfo(ProviderRtmpGetStreamInfoArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("provider_rtmp_media_ref")
        .args(["media_id", "media_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderRtmpPublishKeyArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long = "room-id")]
    pub room_id: String,

    #[arg(
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_media_ref"
    )]
    pub media_id: Option<String>,

    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_media_ref"
    )]
    pub media_id_flag: Option<String>,
}

impl ProviderRtmpPublishKeyArgs {
    pub(in crate::cli) fn resolved_media_id(&self) -> Result<&str> {
        self.media_id
            .as_deref()
            .or(self.media_id_flag.as_deref())
            .ok_or_else(|| {
                anyhow!("provider rtmp create-publish-key requires MEDIA_ID or --media-id")
            })
    }
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("provider_rtmp_stream_media_ref")
        .args(["media_id", "media_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderRtmpGetStreamInfoArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long = "room-id")]
    pub room_id: String,

    #[arg(
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_stream_media_ref"
    )]
    pub media_id: Option<String>,

    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        group = "provider_rtmp_stream_media_ref"
    )]
    pub media_id_flag: Option<String>,
}

impl ProviderRtmpGetStreamInfoArgs {
    pub(in crate::cli) fn resolved_media_id(&self) -> Result<&str> {
        self.media_id
            .as_deref()
            .or(self.media_id_flag.as_deref())
            .ok_or_else(|| anyhow!("provider rtmp get-stream-info requires MEDIA_ID or --media-id"))
    }
}
