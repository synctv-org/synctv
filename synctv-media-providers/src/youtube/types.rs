use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YoutubeChannelTab {
    Videos,
    Shorts,
    Live,
}

impl YoutubeChannelTab {
    pub(crate) const fn params(self) -> &'static str {
        match self {
            Self::Videos => "EgZ2aWRlb3PyBgQKAjoA",
            Self::Shorts => "EgZzaG9ydHPyBgUKA5oBAA==",
            Self::Live => "EgdzdHJlYW1z8gYECgJ6AA==",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlayerResponse {
    #[serde(default)]
    pub playability_status: YoutubePlayabilityStatus,
    pub streaming_data: Option<YoutubeStreamingData>,
    pub video_details: Option<YoutubeVideoDetails>,
    pub captions: Option<YoutubeCaptions>,
    pub storyboards: Option<YoutubeStoryboards>,
    pub microformat: Option<YoutubeMicroformat>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlayabilityStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reason: String,
    pub live_streamability: Option<serde_json::Value>,
}

impl YoutubePlayabilityStatus {
    #[must_use]
    pub fn is_playable(&self) -> bool {
        self.status == "OK" || self.status == "LIVE_STREAM_OFFLINE"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeStreamingData {
    #[serde(default)]
    pub expires_in_seconds: String,
    #[serde(default)]
    pub formats: Vec<YoutubeFormat>,
    #[serde(default)]
    pub adaptive_formats: Vec<YoutubeFormat>,
    pub dash_manifest_url: Option<String>,
    pub hls_manifest_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeFormat {
    #[serde(default)]
    pub itag: u32,
    pub url: Option<String>,
    pub signature_cipher: Option<String>,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub bitrate: u64,
    pub average_bitrate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub content_length: Option<String>,
    pub quality: Option<String>,
    pub quality_label: Option<String>,
    pub audio_quality: Option<String>,
    pub audio_sample_rate: Option<String>,
    pub audio_channels: Option<u32>,
    pub approx_duration_ms: Option<String>,
    pub projection_type: Option<String>,
    pub color_info: Option<YoutubeColorInfo>,
    pub audio_track: Option<YoutubeAudioTrack>,
}

impl YoutubeFormat {
    #[must_use]
    pub fn name(&self) -> String {
        self.quality_label
            .clone()
            .or_else(|| self.quality.clone())
            .or_else(|| self.audio_quality.clone())
            .unwrap_or_else(|| format!("itag {}", self.itag))
    }

    #[must_use]
    pub fn container(&self) -> String {
        self.mime_type
            .split([';', '/'])
            .nth(1)
            .unwrap_or("video")
            .trim()
            .to_string()
    }

    #[must_use]
    pub fn codecs(&self) -> Vec<String> {
        self.mime_type
            .split("codecs=\"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .map(|value| {
                value
                    .split(',')
                    .map(|codec| codec.trim().to_string())
                    .filter(|codec| !codec.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeColorInfo {
    pub primaries: Option<String>,
    pub transfer_characteristics: Option<String>,
    pub matrix_coefficients: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeAudioTrack {
    pub display_name: Option<String>,
    pub id: Option<String>,
    #[serde(default)]
    pub audio_is_default: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeVideoDetails {
    #[serde(default)]
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub length_seconds: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub short_description: String,
    #[serde(default)]
    pub view_count: String,
    #[serde(default)]
    pub is_live_content: bool,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub thumbnail: Option<YoutubeThumbnailCollection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct YoutubeThumbnailCollection {
    #[serde(default)]
    pub thumbnails: Vec<YoutubeThumbnail>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct YoutubeThumbnail {
    #[serde(default)]
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeCaptions {
    pub player_captions_tracklist_renderer: Option<YoutubeCaptionTracklist>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeCaptionTracklist {
    #[serde(default)]
    pub caption_tracks: Vec<YoutubeCaptionTrack>,
    #[serde(default)]
    pub translation_languages: Vec<YoutubeTranslationLanguage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeCaptionTrack {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub name: YoutubeText,
    #[serde(default)]
    pub vss_id: String,
    #[serde(default)]
    pub language_code: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub is_translatable: bool,
}

impl YoutubeCaptionTrack {
    #[must_use]
    pub fn is_automatic(&self) -> bool {
        self.kind.as_deref() == Some("asr") || self.vss_id.starts_with("a.")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeTranslationLanguage {
    #[serde(default)]
    pub language_code: String,
    #[serde(default)]
    pub language_name: YoutubeText,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeStoryboards {
    pub player_storyboard_spec_renderer: Option<YoutubeStoryboardSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeStoryboardSpec {
    #[serde(default)]
    pub spec: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeMicroformat {
    pub player_microformat_renderer: Option<YoutubePlayerMicroformat>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlayerMicroformat {
    pub publish_date: Option<String>,
    pub upload_date: Option<String>,
    pub category: Option<String>,
    pub owner_channel_name: Option<String>,
    pub live_broadcast_details: Option<YoutubeLiveBroadcastDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeLiveBroadcastDetails {
    pub start_timestamp: Option<String>,
    pub end_timestamp: Option<String>,
    pub is_live_now: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeText {
    #[serde(default)]
    pub simple_text: String,
    #[serde(default)]
    pub runs: Vec<YoutubeTextRun>,
}

impl YoutubeText {
    #[must_use]
    pub fn value(&self) -> String {
        if !self.simple_text.is_empty() {
            return self.simple_text.clone();
        }
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct YoutubeTextRun {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YoutubeListItem {
    pub video_id: String,
    pub title: String,
    pub channel_name: String,
    pub channel_id: String,
    pub duration_seconds: Option<u64>,
    pub view_count_text: String,
    pub published_time_text: String,
    pub thumbnail: Option<YoutubeThumbnail>,
    pub is_live: bool,
    pub is_short: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YoutubeListPage {
    pub items: Vec<YoutubeListItem>,
    pub next_cursor: Option<String>,
}
