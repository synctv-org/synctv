use axum::http::HeaderMap;
use base64::Engine as _;
use prost::Message as _;

use super::AppResult;
use synctv_proto::client::{ChatMessageType, GetPlaybackRequest, ResourceDeliveryMode};

pub(super) fn validate_include_message_types(values: Vec<i32>) -> AppResult<Vec<i32>> {
    values
        .into_iter()
        .map(|raw| match ChatMessageType::try_from(raw) {
            Ok(ChatMessageType::Unspecified) | Err(_) => Err(super::super::AppError::bad_request(
                "Invalid includeMessageTypes entry",
            )),
            Ok(_) => Ok(raw),
        })
        .collect()
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(
    feature = "openapi",
    into_params(parameter_in = Query, rename_all = "camelCase")
)]
pub struct GetPlaybackQuery {
    /// Base64url-encoded PlaybackClientProfile protobuf.
    pub client_profile: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
    #[serde(default)]
    pub include_message_types: Vec<i32>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPlaybackStateQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub event_sequence: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPlaylistItemsQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
    pub playlist_id: Option<String>,
    pub page: Option<u32>,
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
    pub search: Option<String>,
    pub source_provider: Option<i32>,
    pub provider_instance_name: Option<String>,
    pub sort_by: Option<i32>,
    pub sort_direction: Option<i32>,
    pub availability: Option<i32>,
    pub refresh: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPlaybackQuery {
    pub delivery_mode: Option<i32>,
    pub format: Option<String>,
    pub client_profile: Option<String>,
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

fn decode_client_profile(
    encoded: &str,
) -> Result<synctv_proto::client::PlaybackClientProfile, super::super::AppError> {
    const MAX_ENCODED_PROFILE_BYTES: usize = 16 * 1024;
    let encoded = encoded.trim();
    if encoded.is_empty() || encoded.len() > MAX_ENCODED_PROFILE_BYTES {
        return Err(super::super::AppError::bad_request(
            "Invalid clientProfile length",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| super::super::AppError::bad_request("Invalid clientProfile encoding"))?;
    synctv_proto::client::PlaybackClientProfile::decode(bytes.as_slice())
        .map_err(|_| super::super::AppError::bad_request("Invalid clientProfile protobuf"))
}

pub(crate) fn build_get_playback_request(
    query: &GetPlaybackQuery,
) -> AppResult<GetPlaybackRequest> {
    Ok(GetPlaybackRequest {
        playback_client_profile: query
            .client_profile
            .as_deref()
            .map(decode_client_profile)
            .transpose()?,
    })
}

pub(crate) fn build_playback_client_profile_from_watch_query(
    query: &WatchPlaybackQuery,
) -> AppResult<Option<synctv_proto::client::PlaybackClientProfile>> {
    build_get_playback_request(&GetPlaybackQuery {
        client_profile: query.client_profile.clone(),
    })
    .map(|request| request.playback_client_profile)
}
