//! Bridges h3 request streams onto a tower HTTP service.
//!
//! Request bodies stream frame-by-frame out of the receive half of the QUIC
//! stream (see [`H3RecvBody`]) — buffering them would deadlock bidirectional
//! RPCs like gRPC, where the service starts replying long before the client
//! finishes uploading. Response bodies likewise stream frame-by-frame onto
//! the send half — including trailers, so SSE and other long-lived responses
//! as well as gRPC work unchanged.

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

/// Default request-body cap: matches the HTTP API's global limit. Streaming
/// bodies enforce it incrementally per chunk.
pub const REQUEST_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// How long a dropped-but-unread request body keeps draining in the
/// background. Responding without reading the body is common (early
/// rejections, gRPC trailers); the drain keeps the client's stream from
/// being reset mid-response, and the bound reaps parked uploaders.
const BODY_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Streaming HTTP/3 request body backed by the receive half of the request
/// stream. Pulled by the service on demand, so QUIC flow control provides
/// backpressure and an unread body never buffers beyond one chunk.
pub struct H3RecvBody {
    /// `None` once the body reached EOF: a fully-read receive half drops
    /// cleanly, an unread one must be drained by [`Drop`] instead of
    /// resetting the stream.
    stream: Option<
        h3::server::RequestStream<
            crate::frame_budget::BudgetedRecvStream<h3_quinn::RecvStream>,
            Bytes,
        >,
    >,
    received: usize,
}

impl H3RecvBody {
    /// Wraps the receive half of a request stream with the global byte cap.
    pub fn new(
        stream: h3::server::RequestStream<
            crate::frame_budget::BudgetedRecvStream<h3_quinn::RecvStream>,
            Bytes,
        >,
    ) -> Self {
        Self {
            stream: Some(stream),
            received: 0,
        }
    }
}

impl Drop for H3RecvBody {
    fn drop(&mut self) {
        let Some(mut stream) = self.stream.take() else {
            return;
        };
        let mut received = self.received;
        // Dropping an unread QUIC receive half resets the stream, which
        // tears down the response the client may still be reading. Drain
        // the remainder instead — the standard respond-then-drain shape of
        // HTTP servers — bounded by the global byte cap and a wall clock.
        tokio::spawn(async move {
            let drain = async {
                while let Ok(Some(chunk)) = stream.recv_data().await {
                    received = received.saturating_add(chunk.chunk().len());
                    if received > REQUEST_BODY_LIMIT {
                        return;
                    }
                }
            };
            let _ = tokio::time::timeout(BODY_DRAIN_TIMEOUT, drain).await;
        });
    }
}

impl HttpBody for H3RecvBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        use std::task::Poll;
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(None);
        };
        match stream.poll_recv_data(cx) {
            Poll::Ready(Ok(Some(chunk))) => {
                let chunk = Bytes::copy_from_slice(chunk.chunk());
                self.received = self.received.saturating_add(chunk.len());
                if self.received > REQUEST_BODY_LIMIT {
                    // Protocol violation: reset the rest of the upload.
                    self.stream = None;
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        BridgeError::BodyTooLarge {
                            limit: REQUEST_BODY_LIMIT,
                        },
                    ))));
                }
                Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
            }
            Poll::Ready(Ok(None)) => {
                // Clean EOF: the receive half now drops without a reset.
                self.stream = None;
                Poll::Ready(None)
            }
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(std::io::Error::other(
                BridgeError::Transport(error.to_string()),
            )))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Serves one HTTP/3 request through `service` and writes the response onto
/// `stream`.
///
/// `make_body` wraps the streaming [`H3RecvBody`] into the service's request
/// body type (the bridge itself is axum-agnostic); the caller keeps control
/// over buffering policy. `send_response` is only issued after the service
/// produces one, so bidirectional streaming works end to end.
pub async fn serve_request<S, ReqBody, ResBody, F>(
    stream: h3::server::RequestStream<crate::frame_budget::BudgetedBidiStream<Bytes>, Bytes>,
    request: Request<()>,
    make_body: F,
    service: &mut S,
) -> Result<(), BridgeError>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Send,
    S::Future: Send,
    S::Error: std::fmt::Display,
    F: FnOnce(H3RecvBody) -> ReqBody,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display,
{
    // Split first: the receive half becomes the streaming request body, the
    // send half carries the response, and both directions progress
    // concurrently — a service that replies before reading the full request
    // (gRPC, early error responses) no longer waits on the upload.
    let (mut send_stream, recv_stream) = stream.split();
    let (parts, ()) = request.into_parts();
    let request = Request::from_parts(parts, make_body(H3RecvBody::new(recv_stream)));

    let response = service
        .call(request)
        .await
        .map_err(|error| BridgeError::Service(error.to_string()))?;

    let (parts, mut body) = response.into_parts();
    send_stream
        .send_response(http::Response::from_parts(parts, ()))
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;

    let mut trailers = None;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| BridgeError::Service(error.to_string()))?;
        match frame.into_data() {
            Ok(data) => {
                if !data.is_empty() {
                    send_stream
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
        send_stream
            .send_trailers(trailers)
            .await
            .map_err(|error| BridgeError::Transport(error.to_string()))?;
    }
    send_stream
        .finish()
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;
    Ok(())
}
