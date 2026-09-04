use serde::{Deserialize, Serialize};

pub const CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackStreamPreference {
    #[default]
    Auto,
    DirectPlay,
    Transcode,
}

impl PlaybackStreamPreference {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackLiveTransport {
    Hls,
    Flv,
    Whep,
}

impl PlaybackLiveTransport {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Hls => "hls",
            Self::Flv => "flv",
            Self::Whep => "whep",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackClientEnvironment {
    #[default]
    Native,
    Web,
}

impl PlaybackClientEnvironment {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Web => "web",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackMediaTransport {
    Progressive,
    Hls,
    Dash,
    Flv,
    MpegTs,
    WebRtc,
}

impl PlaybackMediaTransport {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Progressive => "progressive",
            Self::Hls => "hls",
            Self::Dash => "dash",
            Self::Flv => "flv",
            Self::MpegTs => "mpeg_ts",
            Self::WebRtc => "web_rtc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackMediaPipeline {
    Native,
    MediaSource,
    ManagedMediaSource,
}

impl PlaybackMediaPipeline {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::MediaSource => "media_source",
            Self::ManagedMediaSource => "managed_media_source",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackAudioCodec {
    Aac,
    Mp3,
    Opus,
    Vorbis,
    Ac3,
    Eac3,
    Flac,
}

impl PlaybackAudioCodec {
    #[must_use]
    pub const fn cache_token(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Vorbis => "vorbis",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Flac => "flac",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackMediaCapability {
    pub transport: PlaybackMediaTransport,
    pub container: Option<PlaybackContainer>,
    pub video_codec: Option<PlaybackVideoCodec>,
    pub audio_codec: Option<PlaybackAudioCodec>,
    pub pipeline: PlaybackMediaPipeline,
    pub codec_string: Option<String>,
}

impl PlaybackMediaCapability {
    fn cache_token(&self) -> String {
        format!(
            "{}+{}+{}+{}+{}+{}",
            self.transport.cache_token(),
            self.container.map_or("any", PlaybackContainer::cache_token),
            self.video_codec
                .map_or("any", PlaybackVideoCodec::cache_token),
            self.audio_codec
                .map_or("any", PlaybackAudioCodec::cache_token),
            self.pipeline.cache_token(),
            self.codec_string.as_deref().unwrap_or("none"),
        )
    }
}

/// SyncTV-owned, request-scoped playback capability model.
///
/// This deliberately captures only the client characteristics that materially
/// affect provider playback negotiation. Provider-specific device profiles are
/// derived from this structure at the edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackClientProfile {
    pub profile_version: u32,
    pub environment: PlaybackClientEnvironment,
    pub stream_preference: PlaybackStreamPreference,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub supported_video_codecs: Vec<PlaybackVideoCodec>,
    pub supported_containers: Vec<PlaybackContainer>,
    pub audio_capability: PlaybackAudioCapability,
    pub subtitle_preference: PlaybackSubtitlePreference,
    pub supported_live_transports: Vec<PlaybackLiveTransport>,
    pub media_capabilities: Vec<PlaybackMediaCapability>,
    pub supports_custom_http_headers: bool,
    pub supports_provider_proxy: bool,
    pub supports_insecure_http_media: bool,
}

impl Default for PlaybackClientProfile {
    fn default() -> Self {
        Self {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Native,
            stream_preference: PlaybackStreamPreference::Auto,
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
            supported_live_transports: vec![PlaybackLiveTransport::Hls],
            media_capabilities: Vec::new(),
            supports_custom_http_headers: true,
            supports_provider_proxy: true,
            supports_insecure_http_media: true,
        }
    }
}

impl PlaybackClientProfile {
    #[must_use]
    pub const fn is_web(&self) -> bool {
        matches!(self.environment, PlaybackClientEnvironment::Web)
    }

    #[must_use]
    pub fn supports_transport(&self, transport: PlaybackMediaTransport) -> bool {
        self.media_capabilities
            .iter()
            .any(|capability| capability.transport == transport)
    }

    #[must_use]
    pub fn supports_video_codec(&self, codec: PlaybackVideoCodec) -> bool {
        self.media_capabilities
            .iter()
            .any(|capability| capability.video_codec.is_none_or(|value| value == codec))
    }

    #[must_use]
    pub fn supports_container(&self, container: PlaybackContainer) -> bool {
        self.media_capabilities
            .iter()
            .any(|capability| capability.container.is_none_or(|value| value == container))
    }

    #[must_use]
    pub fn supports_media(
        &self,
        transport: PlaybackMediaTransport,
        container: Option<PlaybackContainer>,
        video_codec: Option<PlaybackVideoCodec>,
        audio_codec: Option<PlaybackAudioCodec>,
    ) -> bool {
        self.media_capabilities.iter().any(|capability| {
            capability.transport == transport
                && container
                    .is_none_or(|value| capability.container.is_none_or(|item| item == value))
                && video_codec
                    .is_none_or(|value| capability.video_codec.is_none_or(|item| item == value))
                && audio_codec
                    .is_none_or(|value| capability.audio_codec.is_none_or(|item| item == value))
        })
    }

    /// Checks one codec token against the exact strings advertised for a media route.
    /// Capabilities without a codec string remain family-level wildcards for native clients.
    #[must_use]
    pub fn supports_codec_string(
        &self,
        transport: PlaybackMediaTransport,
        container: Option<PlaybackContainer>,
        video_codec: Option<PlaybackVideoCodec>,
        audio_codec: Option<PlaybackAudioCodec>,
        codec_string: &str,
    ) -> bool {
        let codec_string = codec_string.trim();
        if codec_string.is_empty() {
            return false;
        }

        self.media_capabilities.iter().any(|capability| {
            capability.transport == transport
                && container
                    .is_none_or(|value| capability.container.is_none_or(|item| item == value))
                && video_codec
                    .is_none_or(|value| capability.video_codec.is_none_or(|item| item == value))
                && audio_codec
                    .is_none_or(|value| capability.audio_codec.is_none_or(|item| item == value))
                && capability.codec_string.as_deref().is_none_or(|advertised| {
                    advertised
                        .split(',')
                        .map(str::trim)
                        .any(|codec| codec.eq_ignore_ascii_case(codec_string))
                })
        })
    }

    #[must_use]
    pub fn supports_media_with_pipeline(
        &self,
        transport: PlaybackMediaTransport,
        container: Option<PlaybackContainer>,
        video_codec: Option<PlaybackVideoCodec>,
        audio_codec: Option<PlaybackAudioCodec>,
        pipeline: PlaybackMediaPipeline,
    ) -> bool {
        self.media_capabilities.iter().any(|capability| {
            capability.transport == transport
                && capability.pipeline == pipeline
                && container
                    .is_none_or(|value| capability.container.is_none_or(|item| item == value))
                && video_codec
                    .is_none_or(|value| capability.video_codec.is_none_or(|item| item == value))
                && audio_codec
                    .is_none_or(|value| capability.audio_codec.is_none_or(|item| item == value))
        })
    }

    #[must_use]
    pub fn cache_fingerprint(&self) -> String {
        let mut capabilities = self
            .media_capabilities
            .iter()
            .map(PlaybackMediaCapability::cache_token)
            .collect::<Vec<_>>();
        capabilities.sort_unstable();
        format!(
            "v={}:environment={}:stream={}:bitrate={}:channels={}:video_codecs={}:containers={}:audio={}:subtitle={}:live_transports={}:media={}:headers={}:proxy={}:insecure_http_media={}",
            self.profile_version,
            self.environment.cache_token(),
            self.stream_preference.cache_token(),
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
            self.supported_live_transports
                .iter()
                .map(|transport| transport.cache_token())
                .collect::<Vec<_>>()
                .join(","),
            capabilities.join(","),
            self.supports_custom_http_headers,
            self.supports_provider_proxy,
            self.supports_insecure_http_media,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_playback_client_profile_keeps_sync_tv_profile_small_and_useful() {
        let profile = PlaybackClientProfile::default();

        assert_eq!(profile.stream_preference, PlaybackStreamPreference::Auto);
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
        assert_eq!(
            profile.supported_live_transports,
            vec![PlaybackLiveTransport::Hls]
        );
    }

    #[test]
    fn cache_fingerprint_includes_every_playback_negotiation_field() {
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            stream_preference: PlaybackStreamPreference::Transcode,
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            supported_video_codecs: vec![PlaybackVideoCodec::H264, PlaybackVideoCodec::Av1],
            supported_containers: vec![PlaybackContainer::Mp4, PlaybackContainer::Webm],
            audio_capability: PlaybackAudioCapability::Surround,
            subtitle_preference: PlaybackSubtitlePreference::EmbeddedOrExternal,
            supported_live_transports: vec![PlaybackLiveTransport::Hls, PlaybackLiveTransport::Flv],
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Progressive,
                container: Some(PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.42E01E,mp4a.40.2".to_string()),
            }],
            supports_custom_http_headers: false,
            supports_provider_proxy: true,
            supports_insecure_http_media: false,
        };

        assert_eq!(
            profile.cache_fingerprint(),
            "v=2:environment=web:stream=transcode:bitrate=8000000:channels=2:video_codecs=h264,av1:containers=mp4,webm:audio=surround:subtitle=embedded_or_external:live_transports=hls,flv:media=progressive+mp4+h264+aac+media_source+avc1.42E01E,mp4a.40.2:headers=false:proxy=true:insecure_http_media=false"
        );
    }

    #[test]
    fn version_two_empty_capability_set_means_no_media_support() {
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            supported_video_codecs: Vec::new(),
            supported_containers: Vec::new(),
            media_capabilities: Vec::new(),
            ..PlaybackClientProfile::default()
        };

        assert!(!profile.supports_transport(PlaybackMediaTransport::Progressive));
        assert!(!profile.supports_container(PlaybackContainer::Mp4));
        assert!(!profile.supports_video_codec(PlaybackVideoCodec::H264));
    }

    #[test]
    fn version_two_checks_codec_and_container_on_the_same_capability() {
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![
                PlaybackMediaCapability {
                    transport: PlaybackMediaTransport::Progressive,
                    container: Some(PlaybackContainer::Mp4),
                    video_codec: Some(PlaybackVideoCodec::H264),
                    audio_codec: Some(PlaybackAudioCodec::Aac),
                    pipeline: PlaybackMediaPipeline::Native,
                    codec_string: None,
                },
                PlaybackMediaCapability {
                    transport: PlaybackMediaTransport::Progressive,
                    container: Some(PlaybackContainer::Webm),
                    video_codec: Some(PlaybackVideoCodec::Vp9),
                    audio_codec: Some(PlaybackAudioCodec::Opus),
                    pipeline: PlaybackMediaPipeline::Native,
                    codec_string: None,
                },
            ],
            ..PlaybackClientProfile::default()
        };

        assert!(profile.supports_media(
            PlaybackMediaTransport::Progressive,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::H264),
            Some(PlaybackAudioCodec::Aac),
        ));
        assert!(!profile.supports_media(
            PlaybackMediaTransport::Progressive,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::Vp9),
            Some(PlaybackAudioCodec::Aac),
        ));
    }

    #[test]
    fn exact_codec_support_matches_individual_tokens_case_insensitively() {
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.64001F,mp4a.40.2".to_string()),
            }],
            ..PlaybackClientProfile::default()
        };

        assert!(profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::H264),
            None,
            "AVC1.64001f",
        ));
        assert!(profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            None,
            Some(PlaybackAudioCodec::Aac),
            "mp4a.40.2",
        ));
        assert!(!profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::H264),
            None,
            "avc1.640033",
        ));
    }

    #[test]
    fn missing_codec_string_keeps_family_level_support() {
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::Native,
                codec_string: None,
            }],
            ..PlaybackClientProfile::default()
        };

        assert!(profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::H264),
            None,
            "avc1.640033",
        ));
        assert!(profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            None,
            Some(PlaybackAudioCodec::Aac),
            "mp4a.40.2",
        ));
        assert!(!profile.supports_codec_string(
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            Some(PlaybackVideoCodec::Hevc),
            None,
            "hvc1.1.6.L120.90",
        ));
    }
}
