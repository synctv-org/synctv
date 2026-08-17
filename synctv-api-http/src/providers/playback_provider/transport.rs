use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{sse::Event, Response},
};
use futures::{Stream, StreamExt};
use std::convert::Infallible;
use synctv_proto::{client::ErrorMessage, playback_provider::common::StreamChunk};

use crate::http::{
    error::{map_api_error, AppResult},
    AppError, AppState,
};
use synctv_api_common::impls::{ApiError, EndpointRateLimitCategory};

pub trait PlaybackProviderHttpResponse {
    fn chunk(self) -> Option<StreamChunk>;
}

/// Map Bilibili live danmaku events to SSE format.
pub fn bilibili_danmaku_sse_event(
    event: Result<synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize Bilibili live danmaku SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize danmaku event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn twitch_chat_sse_event(
    event: Result<synctv_proto::playback_provider::twitch::TwitchChatEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize Twitch chat SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize Twitch chat event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn huya_danmaku_sse_event(
    event: Result<synctv_proto::playback_provider::huya::HuyaDanmakuEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize Huya danmaku SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize Huya danmaku event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn douyu_danmaku_sse_event(
    event: Result<synctv_proto::playback_provider::douyu::DouyuDanmakuEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize Douyu danmaku SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize Douyu danmaku event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn douyin_danmaku_sse_event(
    event: Result<synctv_proto::playback_provider::douyin::DouyinDanmakuEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize Douyin danmaku SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize Douyin danmaku event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn acfun_danmaku_sse_event(
    event: Result<synctv_proto::playback_provider::acfun::AcFunDanmakuEvent, ApiError>,
) -> Result<Event, Infallible> {
    let event = match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Event::default().event("danmaku").data(data),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to serialize AcFun danmaku SSE event");
                Event::default()
                    .event("error")
                    .data(r#"{"message":"Failed to serialize AcFun danmaku event"}"#)
            }
        },
        Err(error) => Event::default().event("error").data(
            serde_json::to_string(&ErrorMessage {
                message: error.to_string(),
                code: error.code(),
                detail: String::new(),
                client_operation_id: String::new(),
            })
            .unwrap_or_else(|_| r#"{"message":"Failed to serialize provider error"}"#.to_string()),
        ),
    };
    Ok(event)
}

pub fn query(raw_query: axum::extract::RawQuery) -> String {
    raw_query.0.unwrap_or_default()
}

pub fn signed_query_fields(
    query: &str,
    room_id: &str,
) -> Result<(String, String, String, i64), ApiError> {
    signed_query_fields_with_resource_params(query, room_id, &[])
}

pub fn signed_query_fields_with_resource_params(
    query: &str,
    room_id: &str,
    resource_params: &[&str],
) -> Result<(String, String, String, i64), ApiError> {
    let mut sig = None;
    let mut uid = None;
    let mut exp = None;

    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "sig" => sig = Some(value.into_owned()),
            "uid" => uid = Some(value.into_owned()),
            "exp" => {
                let value = value.into_owned();
                exp = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| ApiError::InvalidInput("exp is invalid".to_string()))?,
                );
            }
            "targetUrl" => {}
            key if resource_params.contains(&key) => {}
            _ => {
                return Err(ApiError::InvalidInput(format!(
                    "unknown signed query parameter '{key}'"
                )));
            }
        }
    }

    Ok((
        sig.filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ApiError::Authentication("sig is required".to_string()))?,
        uid.filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ApiError::Authentication("uid is required".to_string()))?,
        non_empty_path_room_id(room_id)?,
        exp.ok_or_else(|| ApiError::Authentication("exp is required".to_string()))?,
    ))
}

fn non_empty_path_room_id(room_id: &str) -> Result<String, ApiError> {
    let room_id = room_id.trim();
    if room_id.is_empty() {
        return Err(ApiError::InvalidInput("roomId is required".to_string()));
    }
    Ok(room_id.to_string())
}

pub fn unsigned_query_field(query: &str, allowed_key: &str) -> Result<Option<String>, ApiError> {
    let mut found = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "sig" | "uid" | "exp" | "targetUrl" => {}
            key if key == allowed_key => found = Some(value.into_owned()),
            _ => {
                return Err(ApiError::InvalidInput(format!(
                    "unknown query parameter '{key}'"
                )));
            }
        }
    }
    Ok(found)
}

pub fn target_url(query: &str) -> Result<String, ApiError> {
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "targetUrl").then(|| value.into_owned()))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::InvalidInput("targetUrl is required".to_string()))
}

pub fn range_header(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    headers
        .get(header::RANGE)
        .map(|value| {
            value
                .to_str()
                .map(str::to_string)
                .map_err(|_| ApiError::InvalidInput("Range header is invalid".to_string()))
        })
        .transpose()
}

pub async fn stream_http_response<T, S>(
    state: AppState,
    request_meta: crate::http::middleware::RequestMetadata,
    method: Method,
    build_stream: impl FnOnce(
            synctv_core::provider::ExecutionControl,
        ) -> futures::future::BoxFuture<'static, Result<S, ApiError>>
        + Send
        + 'static,
) -> AppResult<Response>
where
    T: PlaybackProviderHttpResponse + Send + 'static,
    S: Stream<Item = Result<T, ApiError>> + Send + Unpin + 'static,
{
    if method != Method::GET && method != Method::HEAD {
        return Err(AppError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Playback provider route does not support this HTTP method",
        ));
    }

    let request_executor = state.shared_api_runtime.request_executor.clone();
    let resolve_request_meta = request_meta.0.with_timeout(None);
    let (stream, cancellation) = request_executor
        .execute_public_with_control(
            &resolve_request_meta,
            EndpointRateLimitCategory::Streaming,
            move |request_control| async move {
                let cancellation = request_control.cancellation_token();
                let stream = build_stream(request_control).await?;
                Ok::<_, ApiError>((stream, cancellation))
            },
        )
        .await
        .map_err(map_api_error)?;

    let response = response_from_playback_provider_stream::<T, S>(stream, method).await?;
    let action_control = synctv_core::provider::ExecutionControl::from_parts(None, cancellation);
    action_control
        .check_active()
        .map_err(|err| super::super::app_error_from_control(&err))?;
    Ok(response)
}

pub(crate) async fn stream_chunk_http_response<S>(stream: S, method: Method) -> AppResult<Response>
where
    S: Stream<Item = Result<StreamChunk, ApiError>> + Send + Unpin + 'static,
{
    response_from_stream_chunks(stream, method).await
}

async fn response_from_playback_provider_stream<T, S>(
    mut stream: S,
    method: Method,
) -> AppResult<Response>
where
    T: PlaybackProviderHttpResponse + Send + 'static,
    S: Stream<Item = Result<T, ApiError>> + Send + Unpin + 'static,
{
    let first = stream
        .next()
        .await
        .ok_or_else(|| AppError::internal_server_error("Playback provider stream was empty"))?
        .map_err(map_api_error)?;
    let first_chunk = first.chunk().ok_or_else(|| {
        AppError::internal_server_error("Playback provider response missing chunk")
    })?;

    response_from_first_chunk_and_rest(
        first_chunk,
        stream.map(|item| {
            item.and_then(|response| {
                response.chunk().ok_or_else(|| {
                    ApiError::Internal("Playback provider response missing chunk".to_string())
                })
            })
        }),
        &method,
    )
}

async fn response_from_stream_chunks<S>(mut stream: S, method: Method) -> AppResult<Response>
where
    S: Stream<Item = Result<StreamChunk, ApiError>> + Send + Unpin + 'static,
{
    let first_chunk = stream
        .next()
        .await
        .ok_or_else(|| AppError::internal_server_error("Playback provider stream was empty"))?
        .map_err(map_api_error)?;

    response_from_first_chunk_and_rest(first_chunk, stream, &method)
}

fn response_from_first_chunk_and_rest<S>(
    first_chunk: StreamChunk,
    stream: S,
    method: &Method,
) -> AppResult<Response>
where
    S: Stream<Item = Result<StreamChunk, ApiError>> + Send + 'static,
{
    let status = if first_chunk.status == 0 {
        StatusCode::OK
    } else {
        StatusCode::from_u16(
            u16::try_from(first_chunk.status)
                .map_err(|_| AppError::internal_server_error("Invalid playback provider status"))?,
        )
        .map_err(|_| AppError::internal_server_error("Invalid playback provider status"))?
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    apply_stream_chunk_metadata(response.headers_mut(), &first_chunk)?;

    let response = if method == Method::HEAD {
        response
    } else {
        let first_data = first_chunk.data;
        let body_stream = futures::stream::once(async move { Ok::<_, std::io::Error>(first_data) })
            .chain(stream.map(|item| match item {
                Ok(chunk) => Ok(chunk.data),
                Err(error) => Err(std::io::Error::other(error.message().to_string())),
            }));
        *response.body_mut() = Body::from_stream(body_stream);
        response
    };
    Ok(response)
}

fn apply_stream_chunk_metadata(headers: &mut HeaderMap, chunk: &StreamChunk) -> AppResult<()> {
    insert_optional_header(headers, header::CONTENT_TYPE, chunk.content_type.as_deref())?;
    insert_optional_header(
        headers,
        header::CONTENT_ENCODING,
        chunk.content_encoding.as_deref(),
    )?;
    if let Some(content_length) = chunk.content_length {
        insert_optional_header(
            headers,
            header::CONTENT_LENGTH,
            Some(&content_length.to_string()),
        )?;
    }
    insert_optional_header(
        headers,
        header::CONTENT_RANGE,
        chunk.content_range.as_deref(),
    )?;
    insert_optional_header(
        headers,
        header::ACCEPT_RANGES,
        chunk.accept_ranges.as_deref(),
    )?;
    insert_optional_header(
        headers,
        header::CACHE_CONTROL,
        chunk.cache_control.as_deref(),
    )?;
    insert_optional_header(headers, header::ETAG, chunk.etag.as_deref())?;
    insert_optional_header(
        headers,
        header::LAST_MODIFIED,
        chunk.last_modified.as_deref(),
    )?;
    insert_optional_header(headers, header::EXPIRES, chunk.expires.as_deref())?;
    insert_optional_header(
        headers,
        header::CONTENT_DISPOSITION,
        chunk.content_disposition.as_deref(),
    )?;
    insert_optional_header(headers, header::LOCATION, chunk.location.as_deref())?;
    Ok(())
}

fn insert_optional_header(
    headers: &mut HeaderMap,
    name: header::HeaderName,
    value: Option<&str>,
) -> AppResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = HeaderValue::from_str(value).map_err(|_| {
        AppError::internal_server_error("Invalid playback provider response metadata")
    })?;
    headers.insert(name, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_url_query_uses_lower_camel_case() {
        let parsed = target_url("targetUrl=https%3A%2F%2Fcdn.example%2Fseg.ts&sig=s")
            .expect("targetUrl should parse");
        assert_eq!(parsed, "https://cdn.example/seg.ts");

        let error = target_url("sig=s").expect_err("missing targetUrl should be rejected");
        assert!(
            matches!(error, ApiError::InvalidInput(message) if message == "targetUrl is required")
        );
    }

    #[test]
    fn signed_query_fields_rejects_unknown_params() {
        let error = signed_query_fields("sig=s&uid=u&exp=1&extra=1", "room")
            .expect_err("unknown query parameter should be rejected");
        assert!(matches!(error, ApiError::InvalidInput(message) if message.contains("extra")));
    }

    #[test]
    fn signed_query_fields_rejects_legacy_room_query() {
        let error = signed_query_fields("sig=s&uid=u&rid=r&exp=1", "room")
            .expect_err("playback provider query should reject rid");
        assert!(matches!(error, ApiError::InvalidInput(message) if message.contains("rid")));
    }

    #[test]
    fn stream_chunk_metadata_preserves_content_encoding() {
        let mut headers = HeaderMap::new();
        let chunk = StreamChunk {
            content_encoding: Some("deflate".to_string()),
            ..Default::default()
        };

        apply_stream_chunk_metadata(&mut headers, &chunk)
            .expect("content encoding metadata should be a valid HTTP header");

        assert_eq!(
            headers
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("deflate")
        );
    }
}
