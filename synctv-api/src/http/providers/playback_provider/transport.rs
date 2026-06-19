use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use futures::{Stream, StreamExt};
use synctv_proto::playback_provider::common::StreamChunk;

use crate::http::{
    error::{map_api_error, AppResult},
    AppError, AppState,
};
use crate::impls::{ApiError, EndpointRateLimitCategory};

pub trait PlaybackProviderHttpResponse {
    fn chunk(self) -> Option<StreamChunk>;
}

pub fn query(raw_query: axum::extract::RawQuery) -> String {
    raw_query.0.unwrap_or_default()
}

pub fn signed_query_fields(query: &str) -> Result<(String, String, String, i64), ApiError> {
    let mut sig = None;
    let mut uid = None;
    let mut rid = None;
    let mut exp = None;

    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "sig" => sig = Some(value.into_owned()),
            "uid" => uid = Some(value.into_owned()),
            "rid" => rid = Some(value.into_owned()),
            "exp" => {
                let value = value.into_owned();
                exp = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| ApiError::InvalidInput("exp is invalid".to_string()))?,
                );
            }
            _ => {}
        }
    }

    Ok((
        sig.filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ApiError::Authentication("sig is required".to_string()))?,
        uid.filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ApiError::Authentication("uid is required".to_string()))?,
        rid.filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ApiError::Authentication("rid is required".to_string()))?,
        exp.ok_or_else(|| ApiError::Authentication("exp is required".to_string()))?,
    ))
}

pub fn target_url(query: &str) -> Result<String, ApiError> {
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "target_url").then(|| value.into_owned()))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::InvalidInput("target_url is required".to_string()))
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
        let first_data = bytes::Bytes::from(first_chunk.data);
        let body_stream = futures::stream::once(async move { Ok::<_, std::io::Error>(first_data) })
            .chain(stream.map(|item| match item {
                Ok(chunk) => Ok(bytes::Bytes::from(chunk.data)),
                Err(error) => Err(std::io::Error::other(error.message().to_string())),
            }));
        *response.body_mut() = Body::from_stream(body_stream);
        response
    };
    Ok(response)
}

fn apply_stream_chunk_metadata(headers: &mut HeaderMap, chunk: &StreamChunk) -> AppResult<()> {
    insert_optional_header(headers, header::CONTENT_TYPE, chunk.content_type.as_deref())?;
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
