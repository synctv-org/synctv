use crate::impls::ApiError;
use synctv_core::provider;
use synctv_proto::client as proto;

pub fn playback_client_profile_from_proto(
    profile: Option<&proto::PlaybackClientProfile>,
) -> Result<Option<provider::PlaybackClientProfile>, ApiError> {
    let Some(profile) = profile else {
        return Ok(None);
    };

    if profile.profile_version != provider::CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION {
        return Err(ApiError::InvalidInput(format!(
            "Unsupported playback client profile version {}",
            profile.profile_version
        )));
    }

    let environment = match proto::PlaybackClientEnvironment::try_from(profile.environment)
        .map_err(|_| {
            ApiError::InvalidInput("Unsupported playback client environment".to_string())
        })? {
        proto::PlaybackClientEnvironment::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback client environment is required".to_string(),
            ));
        }
        proto::PlaybackClientEnvironment::Native => provider::PlaybackClientEnvironment::Native,
        proto::PlaybackClientEnvironment::Web => provider::PlaybackClientEnvironment::Web,
    };
    let stream_preference = match proto::PlaybackStreamPreference::try_from(
        profile.stream_preference,
    )
    .map_err(|_| ApiError::InvalidInput("Unsupported playback stream preference".to_string()))?
    {
        proto::PlaybackStreamPreference::Unspecified | proto::PlaybackStreamPreference::Auto => {
            provider::PlaybackStreamPreference::Auto
        }
        proto::PlaybackStreamPreference::DirectPlay => {
            provider::PlaybackStreamPreference::DirectPlay
        }
        proto::PlaybackStreamPreference::Transcode => provider::PlaybackStreamPreference::Transcode,
    };

    let supported_video_codecs = profile
        .supported_video_codecs
        .iter()
        .filter_map(|codec| {
            Some(match proto::PlaybackVideoCodec::try_from(*codec) {
                Ok(proto::PlaybackVideoCodec::Unspecified) => return None,
                Ok(proto::PlaybackVideoCodec::H264) => Ok(provider::PlaybackVideoCodec::H264),
                Ok(proto::PlaybackVideoCodec::Hevc) => Ok(provider::PlaybackVideoCodec::Hevc),
                Ok(proto::PlaybackVideoCodec::Vp9) => Ok(provider::PlaybackVideoCodec::Vp9),
                Ok(proto::PlaybackVideoCodec::Av1) => Ok(provider::PlaybackVideoCodec::Av1),
                Err(_) => Err(ApiError::InvalidInput(
                    "Unsupported playback video codec".to_string(),
                )),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let supported_containers = profile
        .supported_containers
        .iter()
        .filter_map(|container| {
            Some(match proto::PlaybackContainer::try_from(*container) {
                Ok(proto::PlaybackContainer::Unspecified) => return None,
                Ok(proto::PlaybackContainer::Mp4) => Ok(provider::PlaybackContainer::Mp4),
                Ok(proto::PlaybackContainer::Mkv) => Ok(provider::PlaybackContainer::Mkv),
                Ok(proto::PlaybackContainer::Webm) => Ok(provider::PlaybackContainer::Webm),
                Err(_) => Err(ApiError::InvalidInput(
                    "Unsupported playback container".to_string(),
                )),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let audio_capability = match proto::PlaybackAudioCapability::try_from(profile.audio_capability)
        .map_err(|_| ApiError::InvalidInput("Unsupported playback audio capability".to_string()))?
    {
        proto::PlaybackAudioCapability::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback audio capability is required".to_string(),
            ));
        }
        proto::PlaybackAudioCapability::Stereo => provider::PlaybackAudioCapability::Stereo,
        proto::PlaybackAudioCapability::Surround => provider::PlaybackAudioCapability::Surround,
        proto::PlaybackAudioCapability::LosslessSurround => {
            provider::PlaybackAudioCapability::LosslessSurround
        }
    };

    let subtitle_preference =
        match proto::PlaybackSubtitlePreference::try_from(profile.subtitle_preference).map_err(
            |_| ApiError::InvalidInput("Unsupported playback subtitle preference".to_string()),
        )? {
            proto::PlaybackSubtitlePreference::Unspecified
            | proto::PlaybackSubtitlePreference::External => {
                provider::PlaybackSubtitlePreference::External
            }
            proto::PlaybackSubtitlePreference::EmbeddedOrExternal => {
                provider::PlaybackSubtitlePreference::EmbeddedOrExternal
            }
            proto::PlaybackSubtitlePreference::None => provider::PlaybackSubtitlePreference::None,
        };

    let supported_live_transports = profile
        .supported_live_transports
        .iter()
        .filter_map(|transport| {
            Some(match proto::PlaybackLiveTransport::try_from(*transport) {
                Ok(proto::PlaybackLiveTransport::Unspecified) => return None,
                Ok(proto::PlaybackLiveTransport::Hls) => Ok(provider::PlaybackLiveTransport::Hls),
                Ok(proto::PlaybackLiveTransport::Flv) => Ok(provider::PlaybackLiveTransport::Flv),
                Ok(proto::PlaybackLiveTransport::Whep) => Ok(provider::PlaybackLiveTransport::Whep),
                Err(_) => Err(ApiError::InvalidInput(
                    "Unsupported playback live transport".to_string(),
                )),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let media_capabilities = profile
        .media_capabilities
        .iter()
        .map(media_capability_from_proto)
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Some(provider::PlaybackClientProfile {
        profile_version: profile.profile_version,
        environment,
        stream_preference,
        max_streaming_bitrate: profile.max_streaming_bitrate,
        max_audio_channels: profile.max_audio_channels,
        supported_video_codecs,
        supported_containers,
        audio_capability,
        subtitle_preference,
        supported_live_transports,
        media_capabilities,
        supports_custom_http_headers: profile.supports_custom_http_headers,
        supports_provider_proxy: profile.supports_provider_proxy,
        supports_insecure_http_media: profile.supports_insecure_http_media,
    }))
}

fn media_capability_from_proto(
    capability: &proto::PlaybackMediaCapability,
) -> Result<provider::PlaybackMediaCapability, ApiError> {
    let transport = match proto::PlaybackMediaTransport::try_from(capability.transport)
        .map_err(|_| ApiError::InvalidInput("Unsupported playback media transport".to_string()))?
    {
        proto::PlaybackMediaTransport::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback media transport is required".to_string(),
            ));
        }
        proto::PlaybackMediaTransport::Progressive => provider::PlaybackMediaTransport::Progressive,
        proto::PlaybackMediaTransport::Hls => provider::PlaybackMediaTransport::Hls,
        proto::PlaybackMediaTransport::Dash => provider::PlaybackMediaTransport::Dash,
        proto::PlaybackMediaTransport::Flv => provider::PlaybackMediaTransport::Flv,
        proto::PlaybackMediaTransport::MpegTs => provider::PlaybackMediaTransport::MpegTs,
        proto::PlaybackMediaTransport::WebRtc => provider::PlaybackMediaTransport::WebRtc,
    };
    let pipeline = match proto::PlaybackMediaPipeline::try_from(capability.pipeline)
        .map_err(|_| ApiError::InvalidInput("Unsupported playback media pipeline".to_string()))?
    {
        proto::PlaybackMediaPipeline::Unspecified => {
            return Err(ApiError::InvalidInput(
                "Playback media pipeline is required".to_string(),
            ));
        }
        proto::PlaybackMediaPipeline::Native => provider::PlaybackMediaPipeline::Native,
        proto::PlaybackMediaPipeline::MediaSource => provider::PlaybackMediaPipeline::MediaSource,
        proto::PlaybackMediaPipeline::ManagedMediaSource => {
            provider::PlaybackMediaPipeline::ManagedMediaSource
        }
    };
    let container = capability
        .container
        .map(proto::PlaybackContainer::try_from)
        .transpose()
        .map_err(|_| {
            ApiError::InvalidInput("Unsupported playback capability container".to_string())
        })?
        .map(|container| match container {
            proto::PlaybackContainer::Unspecified => Err(ApiError::InvalidInput(
                "Playback capability container cannot be unspecified".to_string(),
            )),
            proto::PlaybackContainer::Mp4 => Ok(provider::PlaybackContainer::Mp4),
            proto::PlaybackContainer::Mkv => Ok(provider::PlaybackContainer::Mkv),
            proto::PlaybackContainer::Webm => Ok(provider::PlaybackContainer::Webm),
        })
        .transpose()?;
    let video_codec = capability
        .video_codec
        .map(proto::PlaybackVideoCodec::try_from)
        .transpose()
        .map_err(|_| {
            ApiError::InvalidInput("Unsupported playback capability video codec".to_string())
        })?
        .map(|codec| match codec {
            proto::PlaybackVideoCodec::Unspecified => Err(ApiError::InvalidInput(
                "Playback capability video codec cannot be unspecified".to_string(),
            )),
            proto::PlaybackVideoCodec::H264 => Ok(provider::PlaybackVideoCodec::H264),
            proto::PlaybackVideoCodec::Hevc => Ok(provider::PlaybackVideoCodec::Hevc),
            proto::PlaybackVideoCodec::Vp9 => Ok(provider::PlaybackVideoCodec::Vp9),
            proto::PlaybackVideoCodec::Av1 => Ok(provider::PlaybackVideoCodec::Av1),
        })
        .transpose()?;
    let audio_codec = capability
        .audio_codec
        .map(proto::PlaybackAudioCodec::try_from)
        .transpose()
        .map_err(|_| {
            ApiError::InvalidInput("Unsupported playback capability audio codec".to_string())
        })?
        .map(|codec| match codec {
            proto::PlaybackAudioCodec::Unspecified => Err(ApiError::InvalidInput(
                "Playback capability audio codec cannot be unspecified".to_string(),
            )),
            proto::PlaybackAudioCodec::Aac => Ok(provider::PlaybackAudioCodec::Aac),
            proto::PlaybackAudioCodec::Mp3 => Ok(provider::PlaybackAudioCodec::Mp3),
            proto::PlaybackAudioCodec::Opus => Ok(provider::PlaybackAudioCodec::Opus),
            proto::PlaybackAudioCodec::Vorbis => Ok(provider::PlaybackAudioCodec::Vorbis),
            proto::PlaybackAudioCodec::Ac3 => Ok(provider::PlaybackAudioCodec::Ac3),
            proto::PlaybackAudioCodec::Eac3 => Ok(provider::PlaybackAudioCodec::Eac3),
            proto::PlaybackAudioCodec::Flac => Ok(provider::PlaybackAudioCodec::Flac),
        })
        .transpose()?;

    Ok(provider::PlaybackMediaCapability {
        transport,
        container,
        video_codec,
        audio_codec,
        pipeline,
        codec_string: capability.codec_string.clone(),
    })
}

#[cfg(test)]
mod playback_client_profile_conversion_tests {
    use super::playback_client_profile_from_proto;
    use synctv_core::provider::{
        PlaybackAudioCodec, PlaybackClientEnvironment, PlaybackContainer, PlaybackMediaPipeline,
        PlaybackMediaTransport, PlaybackVideoCodec,
    };
    use synctv_proto::client as proto;

    fn v2_profile() -> proto::PlaybackClientProfile {
        proto::PlaybackClientProfile {
            profile_version: 2,
            environment: proto::PlaybackClientEnvironment::Web as i32,
            audio_capability: proto::PlaybackAudioCapability::Stereo as i32,
            ..Default::default()
        }
    }

    #[test]
    fn obsolete_profile_version_is_rejected() {
        let error =
            playback_client_profile_from_proto(Some(&proto::PlaybackClientProfile::default()))
                .expect_err("obsolete profiles must fail closed");
        assert!(matches!(
            error,
            crate::impls::ApiError::InvalidInput(message)
                if message.contains("Unsupported playback client profile version 0")
        ));
    }

    #[test]
    fn v2_empty_capability_lists_remain_explicitly_empty() {
        let converted = playback_client_profile_from_proto(Some(&v2_profile()))
            .expect("v2 profile should convert")
            .expect("profile should be present");

        assert_eq!(converted.environment, PlaybackClientEnvironment::Web);
        assert!(converted.supported_video_codecs.is_empty());
        assert!(converted.supported_containers.is_empty());
        assert!(converted.supported_live_transports.is_empty());
        assert!(converted.media_capabilities.is_empty());
        assert!(!converted.supports_custom_http_headers);
        assert!(!converted.supports_provider_proxy);
    }

    #[test]
    fn unsupported_profile_version_is_rejected() {
        let mut profile = v2_profile();
        profile.profile_version = 3;

        let error = playback_client_profile_from_proto(Some(&profile))
            .expect_err("unknown versions must fail closed");
        assert!(matches!(
            error,
            crate::impls::ApiError::InvalidInput(message)
                if message.contains("Unsupported playback client profile version 3")
        ));
    }

    #[test]
    fn v2_exact_media_capability_combination_survives_conversion() {
        let mut profile = v2_profile();
        profile.media_capabilities = vec![proto::PlaybackMediaCapability {
            transport: proto::PlaybackMediaTransport::Dash as i32,
            container: Some(proto::PlaybackContainer::Mp4 as i32),
            video_codec: Some(proto::PlaybackVideoCodec::Hevc as i32),
            audio_codec: Some(proto::PlaybackAudioCodec::Eac3 as i32),
            pipeline: proto::PlaybackMediaPipeline::ManagedMediaSource as i32,
            codec_string: Some("hvc1.1.6.L93.B0,ec-3".to_string()),
        }];

        let converted = playback_client_profile_from_proto(Some(&profile))
            .expect("exact profile should convert")
            .expect("profile should be present");
        let capability = converted
            .media_capabilities
            .first()
            .expect("capability should be retained");

        assert_eq!(capability.transport, PlaybackMediaTransport::Dash);
        assert_eq!(capability.container, Some(PlaybackContainer::Mp4));
        assert_eq!(capability.video_codec, Some(PlaybackVideoCodec::Hevc));
        assert_eq!(capability.audio_codec, Some(PlaybackAudioCodec::Eac3));
        assert_eq!(
            capability.pipeline,
            PlaybackMediaPipeline::ManagedMediaSource
        );
        assert_eq!(
            capability.codec_string.as_deref(),
            Some("hvc1.1.6.L93.B0,ec-3")
        );
    }

    #[test]
    fn v2_webrtc_h264_opus_capability_survives_conversion() {
        let mut profile = v2_profile();
        profile.media_capabilities = vec![proto::PlaybackMediaCapability {
            transport: proto::PlaybackMediaTransport::WebRtc as i32,
            container: None,
            video_codec: Some(proto::PlaybackVideoCodec::H264 as i32),
            audio_codec: Some(proto::PlaybackAudioCodec::Opus as i32),
            pipeline: proto::PlaybackMediaPipeline::Native as i32,
            codec_string: None,
        }];

        let converted = playback_client_profile_from_proto(Some(&profile))
            .expect("WebRTC profile should convert")
            .expect("profile should be present");
        let capability = converted
            .media_capabilities
            .first()
            .expect("WebRTC capability should be retained");

        assert_eq!(capability.transport, PlaybackMediaTransport::WebRtc);
        assert_eq!(capability.video_codec, Some(PlaybackVideoCodec::H264));
        assert_eq!(capability.audio_codec, Some(PlaybackAudioCodec::Opus));
        assert_eq!(capability.pipeline, PlaybackMediaPipeline::Native);
    }
}
