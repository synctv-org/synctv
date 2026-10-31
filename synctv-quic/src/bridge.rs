//! Bridges h3 request streams onto a tower HTTP service.
//!
//! Request bodies are buffered (SyncTV endpoints take bounded bodies; the
//! global limit is 10 MB), then dispatched through the service. Response
//! bodies stream frame-by-frame onto the QUIC stream — including trailers,
//! so SSE and other long-lived responses as well as gRPC work unchanged.

use bytes::{Buf, Bytes};
use http::Request;
use http_body::Body as HttpBody;
use http_body_util::BodyExt;
use tower::Service;

/// Errors while serving one HTTP/3 request.
#[derive(Debug)]
pub enum BridgeError {
    /// The QUIC stream or connection failed.
    Transport(String),
    /// The service (router) failed to produce a response.
    Service(String),
    /// The request body exceeded the buffer limit.
    BodyTooLarge { limit: usize },
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "HTTP/3 transport error: {error}"),
            Self::Service(error) => write!(f, "HTTP/3 service error: {error}"),
            Self::BodyTooLarge { limit } => {
                write!(f, "HTTP/3 request body exceeded {limit} bytes")
            }
        }
    }
}

impl std::error::Error for BridgeError {}

/// Default request-body buffer limit: matches the HTTP API's global cap.
pub const REQUEST_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// Serves one HTTP/3 request through `service` and writes the response onto
/// `stream`.
///
/// `ReqBody` lets the caller pick the service's request body type; axum's
/// `Body` accepts `Bytes` via `From`.
pub async fn serve_request<S, ReqBody, ResBody>(
    mut stream: h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    request: Request<()>,
    service: &mut S,
) -> Result<(), BridgeError>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Send,
    S::Future: Send,
    S::Error: std::fmt::Display,
    ReqBody: From<Bytes> + Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display,
{
    // Buffer the request body (bounded; SyncTV takes no streaming uploads).
    let mut body = Vec::new();
    while let Some(chunk) = stream
        .recv_data()
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?
    {
        let chunk: Bytes = Bytes::copy_from_slice(chunk.chunk());
        if body.len() + chunk.len() > REQUEST_BODY_LIMIT {
            return Err(BridgeError::BodyTooLarge {
                limit: REQUEST_BODY_LIMIT,
            });
        }
        body.extend_from_slice(&chunk);
    }

    let (parts, ()) = request.into_parts();
    let request = Request::from_parts(parts, ReqBody::from(Bytes::from(body)));

    let response = service
        .call(request)
        .await
        .map_err(|error| BridgeError::Service(error.to_string()))?;

    let (parts, mut body) = response.into_parts();
    stream
        .send_response(http::Response::from_parts(parts, ()))
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;

    let mut trailers = None;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| BridgeError::Service(error.to_string()))?;
        match frame.into_data() {
            Ok(data) => {
                if !data.is_empty() {
                    stream
                        .send_data(data)
                        .await
                        .map_err(|error| BridgeError::Transport(error.to_string()))?;
                }
            }
            Err(frame) => {
                if let Ok(headers) = frame.into_trailers() {
                    trailers = Some(headers);
                }
            }
        }
    }

    if let Some(trailers) = trailers {
        stream
            .send_trailers(trailers)
            .await
            .map_err(|error| BridgeError::Transport(error.to_string()))?;
    }
    stream
        .finish()
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;
    Ok(())
}
