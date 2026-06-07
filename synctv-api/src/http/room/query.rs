use axum::http::HeaderMap;

use super::AppResult;
use synctv_proto::client::GetPlaybackRequest;

#[cfg(test)]
pub(in crate::http::room) fn parse_optional_query_i32(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<i32>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<i32>().map_err(|_| {
                super::super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected an integer"
                ))
            })
        })
        .transpose()
}

#[cfg(test)]
pub(in crate::http::room) fn parse_optional_query_bool(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<bool>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<bool>().map_err(|_| {
                super::super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected true or false"
                ))
            })
        })
        .transpose()
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetPlaybackQuery {
    pub delivery_preference: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<String>,
    pub subtitle_preference: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchQuery {
    pub delivery_mode: Option<String>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchPlaybackQuery {
    pub delivery_mode: Option<String>,
    pub format: Option<String>,
    pub delivery_preference: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<String>,
    pub subtitle_preference: Option<String>,
}

pub(crate) fn parse_watch_delivery_mode(value: Option<&str>) -> AppResult<i32> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("push_snapshot") => {
            Ok(synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32)
        }
        Some("notify_only") => Ok(synctv_proto::client::ResourceDeliveryMode::NotifyOnly as i32),
        Some(other) => Err(super::super::AppError::bad_request(format!(
            "Invalid delivery_mode '{other}'. Expected push_snapshot or notify_only"
        ))),
    }
}

pub(crate) fn watch_after_event_sequence(
    headers: &HeaderMap,
    query_sequence: Option<i64>,
) -> AppResult<Option<i64>> {
    fn validate_sequence(sequence: i64) -> AppResult<i64> {
        if sequence < 0 {
            return Err(super::super::AppError::bad_request(
                "Invalid event sequence; expected a non-negative integer",
            ));
        }
        Ok(sequence)
    }

    let Some(header_value) = headers.get("last-event-id") else {
        return query_sequence.map(validate_sequence).transpose();
    };
    let header_value = header_value
        .to_str()
        .map_err(|_| super::super::AppError::bad_request("Invalid Last-Event-ID event sequence"))?
        .trim();

    if header_value.is_empty() {
        query_sequence.map(validate_sequence).transpose()
    } else {
        let sequence = header_value.parse::<i64>().map_err(|_| {
            super::super::AppError::bad_request("Invalid Last-Event-ID event sequence")
        })?;
        validate_sequence(sequence).map(Some)
    }
}

fn parse_delivery_preference(
    value: Option<&str>,
) -> Result<synctv_proto::client::PlaybackDeliveryPreference, super::super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(synctv_proto::client::PlaybackDeliveryPreference::Unspecified),
        Some("auto") => Ok(synctv_proto::client::PlaybackDeliveryPreference::Auto),
        Some("direct_play") => Ok(synctv_proto::client::PlaybackDeliveryPreference::DirectPlay),
        Some("transcode") => Ok(synctv_proto::client::PlaybackDeliveryPreference::Transcode),
        Some(other) => Err(super::super::AppError::bad_request(format!(
            "Invalid delivery_preference '{other}'. Expected auto, direct_play, or transcode"
        ))),
    }
}

fn parse_subtitle_preference(
    value: Option<&str>,
) -> Result<synctv_proto::client::PlaybackSubtitlePreference, super::super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(synctv_proto::client::PlaybackSubtitlePreference::Unspecified),
        Some("external") => Ok(synctv_proto::client::PlaybackSubtitlePreference::External),
        Some("embedded_or_external") => {
            Ok(synctv_proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal)
        }
        Some("none") => Ok(synctv_proto::client::PlaybackSubtitlePreference::None),
        Some(other) => Err(super::super::AppError::bad_request(format!(
            "Invalid subtitle_preference '{other}'. Expected external, embedded_or_external, or none"
        ))),
    }
}

fn parse_video_codecs(value: Option<&str>) -> Result<Vec<i32>, super::super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|codec| match codec {
            "h264" => Ok(synctv_proto::client::PlaybackVideoCodec::H264 as i32),
            "hevc" => Ok(synctv_proto::client::PlaybackVideoCodec::Hevc as i32),
            "vp9" => Ok(synctv_proto::client::PlaybackVideoCodec::Vp9 as i32),
            "av1" => Ok(synctv_proto::client::PlaybackVideoCodec::Av1 as i32),
            other => Err(super::super::AppError::bad_request(format!(
                "Invalid video codec '{other}'. Expected h264, hevc, vp9, or av1"
            ))),
        })
        .collect()
}

fn parse_containers(value: Option<&str>) -> Result<Vec<i32>, super::super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|container| match container {
            "mp4" => Ok(synctv_proto::client::PlaybackContainer::Mp4 as i32),
            "mkv" => Ok(synctv_proto::client::PlaybackContainer::Mkv as i32),
            "webm" => Ok(synctv_proto::client::PlaybackContainer::Webm as i32),
            other => Err(super::super::AppError::bad_request(format!(
                "Invalid container '{other}'. Expected mp4, mkv, or webm"
            ))),
        })
        .collect()
}

fn parse_audio_capability(
    value: Option<&str>,
) -> Result<synctv_proto::client::PlaybackAudioCapability, super::super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(synctv_proto::client::PlaybackAudioCapability::Unspecified),
        Some("stereo") => Ok(synctv_proto::client::PlaybackAudioCapability::Stereo),
        Some("surround") => Ok(synctv_proto::client::PlaybackAudioCapability::Surround),
        Some("lossless_surround") => {
            Ok(synctv_proto::client::PlaybackAudioCapability::LosslessSurround)
        }
        Some(other) => Err(super::super::AppError::bad_request(format!(
            "Invalid audio_capability '{other}'. Expected stereo, surround, or lossless_surround"
        ))),
    }
}

pub(crate) fn build_get_playback_request(
    query: &GetPlaybackQuery,
) -> AppResult<GetPlaybackRequest> {
    let has_profile = query.delivery_preference.is_some()
        || query.max_streaming_bitrate.is_some()
        || query.max_audio_channels.is_some()
        || query.video_codecs.is_some()
        || query.containers.is_some()
        || query.audio_capability.is_some()
        || query.subtitle_preference.is_some();

    let playback_client_profile = if has_profile {
        Some(synctv_proto::client::PlaybackClientProfile {
            delivery_preference: parse_delivery_preference(query.delivery_preference.as_deref())?
                as i32,
            max_streaming_bitrate: query.max_streaming_bitrate,
            max_audio_channels: query.max_audio_channels,
            supported_video_codecs: parse_video_codecs(query.video_codecs.as_deref())?,
            supported_containers: parse_containers(query.containers.as_deref())?,
            audio_capability: parse_audio_capability(query.audio_capability.as_deref())? as i32,
            subtitle_preference: parse_subtitle_preference(query.subtitle_preference.as_deref())?
                as i32,
        })
    } else {
        None
    };

    let request = GetPlaybackRequest {
        playback_client_profile,
    };
    Ok(request)
}

pub(crate) fn build_playback_client_profile_from_watch_query(
    query: &WatchPlaybackQuery,
) -> AppResult<Option<synctv_proto::client::PlaybackClientProfile>> {
    build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: query.delivery_preference.clone(),
        max_streaming_bitrate: query.max_streaming_bitrate,
        max_audio_channels: query.max_audio_channels,
        video_codecs: query.video_codecs.clone(),
        containers: query.containers.clone(),
        audio_capability: query.audio_capability.clone(),
        subtitle_preference: query.subtitle_preference.clone(),
    })
    .map(|request| request.playback_client_profile)
}
