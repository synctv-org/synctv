use axum::http::HeaderMap;

use super::AppResult;
use synctv_proto::client::{GetPlaybackRequest, ResourceDeliveryMode};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(
    feature = "openapi",
    into_params(parameter_in = Query, rename_all = "camelCase")
)]
pub struct GetPlaybackQuery {
    pub stream_preference: Option<i32>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<i32>,
    pub subtitle_preference: Option<i32>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchPlaylistItemsQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
    pub playlist_id: Option<String>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub search: Option<String>,
    pub source_provider: Option<i32>,
    pub provider_instance_name: Option<String>,
    pub sort_by: Option<i32>,
    pub sort_direction: Option<i32>,
    pub availability: Option<i32>,
    pub refresh: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchPlaybackQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
    pub stream_preference: Option<i32>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<i32>,
    pub subtitle_preference: Option<i32>,
}

pub(crate) fn parse_watch_delivery_mode(value: Option<i32>) -> AppResult<i32> {
    match value.map(ResourceDeliveryMode::try_from).transpose() {
        Ok(None | Some(ResourceDeliveryMode::Unspecified | ResourceDeliveryMode::PushSnapshot)) => {
            Ok(ResourceDeliveryMode::PushSnapshot as i32)
        }
        Ok(Some(ResourceDeliveryMode::NotifyOnly)) => Ok(ResourceDeliveryMode::NotifyOnly as i32),
        Err(_) => Err(super::super::AppError::bad_request(format!(
            "Invalid deliveryMode '{}'. Expected enum integer {} or {}",
            value.expect("value exists when enum parsing fails"),
            ResourceDeliveryMode::NotifyOnly as i32,
            ResourceDeliveryMode::PushSnapshot as i32
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

fn parse_stream_preference(
    value: Option<i32>,
) -> Result<synctv_proto::client::PlaybackStreamPreference, super::super::AppError> {
    value
        .map(synctv_proto::client::PlaybackStreamPreference::try_from)
        .transpose()
        .map(|value| value.unwrap_or(synctv_proto::client::PlaybackStreamPreference::Unspecified))
        .map_err(|_| super::super::AppError::bad_request("Invalid streamPreference enum integer"))
}

fn parse_subtitle_preference(
    value: Option<i32>,
) -> Result<synctv_proto::client::PlaybackSubtitlePreference, super::super::AppError> {
    value
        .map(synctv_proto::client::PlaybackSubtitlePreference::try_from)
        .transpose()
        .map(|value| value.unwrap_or(synctv_proto::client::PlaybackSubtitlePreference::Unspecified))
        .map_err(|_| super::super::AppError::bad_request("Invalid subtitlePreference enum integer"))
}

fn parse_video_codecs(value: Option<&str>) -> Result<Vec<i32>, super::super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|codec| {
            let value = codec.parse::<i32>().map_err(|_| {
                super::super::AppError::bad_request("Invalid videoCodecs enum integer")
            })?;
            synctv_proto::client::PlaybackVideoCodec::try_from(value)
                .map(|_| value)
                .map_err(|_| {
                    super::super::AppError::bad_request("Invalid videoCodecs enum integer")
                })
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
        .map(|container| {
            let value = container.parse::<i32>().map_err(|_| {
                super::super::AppError::bad_request("Invalid containers enum integer")
            })?;
            synctv_proto::client::PlaybackContainer::try_from(value)
                .map(|_| value)
                .map_err(|_| super::super::AppError::bad_request("Invalid containers enum integer"))
        })
        .collect()
}

fn parse_audio_capability(
    value: Option<i32>,
) -> Result<synctv_proto::client::PlaybackAudioCapability, super::super::AppError> {
    value
        .map(synctv_proto::client::PlaybackAudioCapability::try_from)
        .transpose()
        .map(|value| value.unwrap_or(synctv_proto::client::PlaybackAudioCapability::Unspecified))
        .map_err(|_| super::super::AppError::bad_request("Invalid audioCapability enum integer"))
}

pub(crate) fn build_get_playback_request(
    query: &GetPlaybackQuery,
) -> AppResult<GetPlaybackRequest> {
    let has_profile = query.stream_preference.is_some()
        || query.max_streaming_bitrate.is_some()
        || query.max_audio_channels.is_some()
        || query.video_codecs.is_some()
        || query.containers.is_some()
        || query.audio_capability.is_some()
        || query.subtitle_preference.is_some();

    let playback_client_profile = if has_profile {
        Some(synctv_proto::client::PlaybackClientProfile {
            stream_preference: parse_stream_preference(query.stream_preference)? as i32,
            max_streaming_bitrate: query.max_streaming_bitrate,
            max_audio_channels: query.max_audio_channels,
            supported_video_codecs: parse_video_codecs(query.video_codecs.as_deref())?,
            supported_containers: parse_containers(query.containers.as_deref())?,
            audio_capability: parse_audio_capability(query.audio_capability)? as i32,
            subtitle_preference: parse_subtitle_preference(query.subtitle_preference)? as i32,
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
        stream_preference: query.stream_preference,
        max_streaming_bitrate: query.max_streaming_bitrate,
        max_audio_channels: query.max_audio_channels,
        video_codecs: query.video_codecs.clone(),
        containers: query.containers.clone(),
        audio_capability: query.audio_capability,
        subtitle_preference: query.subtitle_preference,
    })
    .map(|request| request.playback_client_profile)
}
