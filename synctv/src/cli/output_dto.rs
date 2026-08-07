use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserMutationCliOutput {
    pub(super) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) user: Option<synctv_proto::admin::AdminUser>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlaybackPullUrlCliOutput {
    pub(super) mode: String,
    pub(super) format: String,
    pub(super) name: String,
    pub(super) url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) absolute_url: Option<String>,
    pub(super) default: bool,
    pub(super) headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expire_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GetPlaybackCliOutput {
    pub(super) playback_state: Option<synctv_proto::client::PlaybackState>,
    pub(super) playback: Option<synctv_proto::client::Playback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) default_mode: Option<String>,
    pub(super) pull_urls: Vec<PlaybackPullUrlCliOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) default_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) default_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hls_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hls_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) flv_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) flv_absolute_pull_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlaybackStartCliOutput {
    pub(super) success: bool,
    pub(super) room_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) media_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) playlist_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlaybackStopCliOutput {
    pub(super) success: bool,
    pub(super) room_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KickStreamCliOutput {
    pub(super) success: bool,
    pub(super) room_id: String,
    pub(super) media_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
}
