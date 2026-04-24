use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackDeliveryPreference {
    #[default]
    Auto,
    DirectPlay,
    Transcode,
}

impl PlaybackDeliveryPreference {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::DirectPlay => "direct",
            Self::Transcode => "transcode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackSubtitlePreference {
    #[default]
    External,
    EmbeddedOrExternal,
    None,
}

impl PlaybackSubtitlePreference {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::EmbeddedOrExternal => "embedded_or_external",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackVideoCodec {
    H264,
    Hevc,
    Vp9,
    Av1,
}

impl PlaybackVideoCodec {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackContainer {
    Mp4,
    Mkv,
    Webm,
}

impl PlaybackContainer {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackAudioCapability {
    Stereo,
    Surround,
    #[default]
    LosslessSurround,
}

impl PlaybackAudioCapability {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Stereo => "stereo",
            Self::Surround => "surround",
            Self::LosslessSurround => "lossless_surround",
        }
    }
}

/// SyncTV-owned, request-scoped playback capability model.
///
/// This deliberately captures only the client characteristics that materially
/// affect provider playback negotiation. Provider-specific device profiles are
/// derived from this structure at the edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackClientProfile {
    pub delivery_preference: PlaybackDeliveryPreference,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub supported_video_codecs: Vec<PlaybackVideoCodec>,
    pub supported_containers: Vec<PlaybackContainer>,
    pub audio_capability: PlaybackAudioCapability,
    pub subtitle_preference: PlaybackSubtitlePreference,
}

impl Default for PlaybackClientProfile {
    fn default() -> Self {
        Self {
            delivery_preference: PlaybackDeliveryPreference::Auto,
            max_streaming_bitrate: None,
            max_audio_channels: Some(2),
            supported_video_codecs: vec![
                PlaybackVideoCodec::H264,
                PlaybackVideoCodec::Hevc,
                PlaybackVideoCodec::Vp9,
                PlaybackVideoCodec::Av1,
            ],
            supported_containers: vec![
                PlaybackContainer::Mp4,
                PlaybackContainer::Mkv,
                PlaybackContainer::Webm,
            ],
            audio_capability: PlaybackAudioCapability::LosslessSurround,
            subtitle_preference: PlaybackSubtitlePreference::External,
        }
    }
}

impl PlaybackClientProfile {
    #[must_use]
    pub fn cache_fingerprint(&self) -> String {
        format!(
            "delivery={}:bitrate={}:channels={}:video_codecs={}:containers={}:audio={}:subtitle={}",
            self.delivery_preference.cache_token(),
            self.max_streaming_bitrate
                .map_or_else(|| "none".to_string(), |value| value.to_string()),
            self.max_audio_channels
                .map_or_else(|| "none".to_string(), |value| value.to_string()),
            self.supported_video_codecs
                .iter()
                .map(|codec| codec.cache_token())
                .collect::<Vec<_>>()
                .join(","),
            self.supported_containers
                .iter()
                .map(|container| container.cache_token())
                .collect::<Vec<_>>()
                .join(","),
            self.audio_capability.cache_token(),
            self.subtitle_preference.cache_token(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_playback_client_profile_keeps_sync_tv_profile_small_and_useful() {
        let profile = PlaybackClientProfile::default();

        assert_eq!(
            profile.delivery_preference,
            PlaybackDeliveryPreference::Auto
        );
        assert_eq!(profile.max_streaming_bitrate, None);
        assert_eq!(profile.max_audio_channels, Some(2));
        assert_eq!(
            profile.supported_video_codecs,
            vec![
                PlaybackVideoCodec::H264,
                PlaybackVideoCodec::Hevc,
                PlaybackVideoCodec::Vp9,
                PlaybackVideoCodec::Av1,
            ]
        );
        assert_eq!(
            profile.supported_containers,
            vec![
                PlaybackContainer::Mp4,
                PlaybackContainer::Mkv,
                PlaybackContainer::Webm,
            ]
        );
        assert_eq!(
            profile.audio_capability,
            PlaybackAudioCapability::LosslessSurround
        );
        assert_eq!(
            profile.subtitle_preference,
            PlaybackSubtitlePreference::External
        );
    }

    #[test]
    fn cache_fingerprint_includes_every_playback_negotiation_field() {
        let profile = PlaybackClientProfile {
            delivery_preference: PlaybackDeliveryPreference::Transcode,
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            supported_video_codecs: vec![PlaybackVideoCodec::H264, PlaybackVideoCodec::Av1],
            supported_containers: vec![PlaybackContainer::Mp4, PlaybackContainer::Webm],
            audio_capability: PlaybackAudioCapability::Surround,
            subtitle_preference: PlaybackSubtitlePreference::EmbeddedOrExternal,
        };

        assert_eq!(
            profile.cache_fingerprint(),
            "delivery=transcode:bitrate=8000000:channels=2:video_codecs=h264,av1:containers=mp4,webm:audio=surround:subtitle=embedded_or_external"
        );
    }
}
