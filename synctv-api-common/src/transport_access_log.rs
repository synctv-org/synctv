use std::{
    net::{IpAddr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::{ConnectInfo, MatchedPath, Request},
    http::{header::ToStrError, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};
use hyper::body::{Frame, SizeHint};

use crate::{
    observability::metrics, request_context::CURRENT_REQUEST_ID, AccessLogSettings,
    ApiServerSettings,
};

const ACCESS_LOG_TARGET: &str = "synctv::access";
const MAX_LOGGED_ROUTE_LEN: usize = 512;
const UNKNOWN_CLIENT_IP: &str = "-";
const GRPC_STATUS: HeaderName = HeaderName::from_static("grpc-status");
const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Debug)]
struct AccessLogContext {
    method: Method,
    route: String,
    route_matched: bool,
    client_ip: Option<IpAddr>,
    request_id: String,
    started_at: Instant,
    access_log: AccessLogSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl AccessLogContext {
    fn from_http_request(
        request: &Request,
        server: &ApiServerSettings,
        access_log: &AccessLogSettings,
    ) -> Self {
        if let Some(route) = request.extensions().get::<MatchedPath>() {
            Self::from_request(request, server, access_log, route.as_str().to_string())
        } else {
            Self::from_unmatched_http_request(request, server, access_log)
        }
    }

    fn from_grpc_request(
        request: &Request,
        server: &ApiServerSettings,
        access_log: &AccessLogSettings,
    ) -> Self {
        Self::from_request(
            request,
            server,
            access_log,
            request.uri().path().to_string(),
        )
    }

    fn from_request(
        request: &Request,
        server: &ApiServerSettings,
        access_log: &AccessLogSettings,
        route: String,
    ) -> Self {
        Self {
            method: request.method().clone(),
            route,
            route_matched: true,
            client_ip: effective_client_ip(request, server),
            request_id: request_id(request.headers()),
            started_at: Instant::now(),
            access_log: access_log.clone(),
        }
    }

    fn from_unmatched_http_request(
        request: &Request,
        server: &ApiServerSettings,
        access_log: &AccessLogSettings,
    ) -> Self {
        let mut context = Self::from_request(
            request,
            server,
            access_log,
            route_for_access_log(request.uri().path()),
        );
        context.route_matched = false;
        context
    }

    fn client_ip_display(&self) -> String {
        self.client_ip.map_or_else(
            || UNKNOWN_CLIENT_IP.to_string(),
            |client_ip| client_ip.to_string(),
        )
    }

    fn log_http_completion(
        &self,
        status: StatusCode,
        handler_latency: Duration,
        response_bytes: u64,
        completion: &'static str,
    ) {
        if !self.access_log.enabled {
            return;
        }
        let client_ip = self.client_ip_display();
        let elapsed = self.started_at.elapsed();
        let latency_ms = duration_ms(elapsed);
        let handler_latency_ms = duration_ms(handler_latency);
        let slow = self.access_log.slow_request_threshold_ms != 0
            && handler_latency >= Duration::from_millis(self.access_log.slow_request_threshold_ms);
        macro_rules! emit {
            ($macro:ident) => {
                tracing::$macro!(
                    target: ACCESS_LOG_TARGET,
                    protocol = "http",
                    method = %self.method,
                    route = %self.route,
                    route_matched = self.route_matched,
                    status = status.as_u16(),
                    latency_ms,
                    handler_latency_ms,
                    response_bytes,
                    slow,
                    client_ip = %client_ip,
                    request_id = %self.request_id,
                    completion,
                    "request completed"
                )
            };
        }
        match http_access_log_level(status, slow, completion) {
            AccessLogLevel::Debug => emit!(debug),
            AccessLogLevel::Info => emit!(info),
            AccessLogLevel::Warn => emit!(warn),
            AccessLogLevel::Error => emit!(error),
        }
    }

    fn log_grpc_completion(
        &self,
        http_status: StatusCode,
        grpc_code: i32,
        grpc_status: &str,
        completion: &'static str,
    ) {
        let elapsed = self.started_at.elapsed();
        record_grpc_metrics(&self.route, grpc_code, grpc_status, elapsed);
        if !self.access_log.enabled {
            return;
        }
        let client_ip = self.client_ip_display();
        let latency_ms = duration_ms(elapsed);
        macro_rules! emit {
            ($macro:ident) => {
                tracing::$macro!(
                    target: ACCESS_LOG_TARGET,
                    protocol = "grpc",
                    rpc = %self.route,
                    http_status = http_status.as_u16(),
                    grpc_status,
                    grpc_code,
                    latency_ms,
                    client_ip = %client_ip,
                    request_id = %self.request_id,
                    completion,
                    "request completed"
                )
            };
        }
        match grpc_access_log_level(http_status, grpc_code, completion) {
            AccessLogLevel::Debug => emit!(debug),
            AccessLogLevel::Info => emit!(info),
            AccessLogLevel::Warn => emit!(warn),
            AccessLogLevel::Error => emit!(error),
        }
    }
}

/// Add request correlation and emit one completion log for an HTTP request.
pub async fn http_access_log_middleware(
    request: Request,
    next: Next,
    server: &ApiServerSettings,
    access_log: &AccessLogSettings,
) -> Response {
    let context = AccessLogContext::from_http_request(&request, server, access_log);

    complete_http_request(request, next, context).await
}

async fn complete_http_request(
    request: Request,
    next: Next,
    context: AccessLogContext,
) -> Response {
    let request_id = context.request_id.clone();
    let mut response = CURRENT_REQUEST_ID
        .scope(request_id, async move { next.run(request).await })
        .await;

    insert_request_id_header(&mut response, &context.request_id);
    if !context.access_log.enabled {
        return response;
    }

    let status = response.status();
    let handler_latency = context.started_at.elapsed();
    let expected_response_bytes = response
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    response.map(|body| {
        Body::new(HttpAccessLogBody::new(
            body,
            context,
            status,
            handler_latency,
            expected_response_bytes,
        ))
    })
}

/// Add request correlation and emit a completion log with the final gRPC status.
/// Non-gRPC requests reaching the transport fallback are logged as unmatched HTTP.
pub async fn grpc_access_log_middleware(
    request: Request,
    next: Next,
    server: &ApiServerSettings,
    access_log: &AccessLogSettings,
) -> Response {
    if !request_targets_grpc_transport(request.headers()).unwrap_or(false) {
        let context = AccessLogContext::from_unmatched_http_request(&request, server, access_log);
        return complete_http_request(request, next, context).await;
    }

    let context = AccessLogContext::from_grpc_request(&request, server, access_log);

    let request_id = context.request_id.clone();
    let mut response = CURRENT_REQUEST_ID
        .scope(request_id, async move { next.run(request).await })
        .await;

    insert_request_id_header(&mut response, &context.request_id);
    let http_status = response.status();
    if let Some((grpc_code, grpc_status)) = grpc_status(response.headers()) {
        context.log_grpc_completion(http_status, grpc_code, grpc_status, "finished");
        return response;
    }

    if !http_status.is_success() {
        context.log_grpc_completion(http_status, -1, "HTTP_ERROR", "finished");
        return response;
    }

    response.map(|body| Body::new(GrpcAccessLogBody::new(body, context, http_status)))
}

/// Return whether a request targets a gRPC or gRPC-Web transport.
pub fn request_targets_grpc_transport(headers: &HeaderMap) -> Result<bool, ToStrError> {
    let Some(value) = headers.get(axum::http::header::CONTENT_TYPE) else {
        return Ok(false);
    };
    let media_type = value
        .to_str()?
        .trim()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Ok(media_type.starts_with("application/grpc"))
}

#[derive(Debug)]
struct HttpAccessLogBody {
    inner: Body,
    context: Option<AccessLogContext>,
    status: StatusCode,
    handler_latency: Duration,
    expected_response_bytes: Option<u64>,
    response_bytes: u64,
}

impl HttpAccessLogBody {
    fn new(
        inner: Body,
        context: AccessLogContext,
        status: StatusCode,
        handler_latency: Duration,
        expected_response_bytes: Option<u64>,
    ) -> Self {
        let completes_without_body_poll = context.method == Method::HEAD
            || status.is_informational()
            || matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED)
            || expected_response_bytes == Some(0);
        let mut body = Self {
            inner,
            context: Some(context),
            status,
            handler_latency,
            expected_response_bytes,
            response_bytes: 0,
        };
        if completes_without_body_poll || body.inner.is_end_stream() {
            body.complete("finished");
        }
        body
    }

    fn complete(&mut self, completion: &'static str) {
        if let Some(context) = self.context.take() {
            context.log_http_completion(
                self.status,
                self.handler_latency,
                self.response_bytes,
                completion,
            );
        }
    }
}

impl HttpBody for HttpAccessLogBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.response_bytes = this
                        .response_bytes
                        .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
                }
                if this.inner.is_end_stream()
                    || this
                        .expected_response_bytes
                        .is_some_and(|expected| this.response_bytes >= expected)
                {
                    this.complete("finished");
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.complete("body_error");
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.complete("finished");
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for HttpAccessLogBody {
    fn drop(&mut self) {
        self.complete("response_dropped");
    }
}

#[derive(Debug)]
struct GrpcAccessLogBody {
    inner: Body,
    context: Option<AccessLogContext>,
    http_status: StatusCode,
}

impl GrpcAccessLogBody {
    fn new(inner: Body, context: AccessLogContext, http_status: StatusCode) -> Self {
        let mut body = Self {
            inner,
            context: Some(context),
            http_status,
        };
        if body.inner.is_end_stream() {
            body.complete(2, "UNKNOWN", "missing_status");
        }
        body
    }

    fn complete(&mut self, grpc_code: i32, grpc_status: &str, completion: &'static str) {
        if let Some(context) = self.context.take() {
            context.log_grpc_completion(self.http_status, grpc_code, grpc_status, completion);
        }
    }
}

impl HttpBody for GrpcAccessLogBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(trailers) = frame.trailers_ref() {
                    if let Some((grpc_code, grpc_status)) = grpc_status(trailers) {
                        this.complete(grpc_code, grpc_status, "finished");
                    } else {
                        this.complete(2, "UNKNOWN", "missing_status");
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.complete(2, "UNKNOWN", "body_error");
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.complete(2, "UNKNOWN", "missing_status");
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for GrpcAccessLogBody {
    fn drop(&mut self) {
        self.complete(1, "CANCELLED", "response_dropped");
    }
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(&X_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map_or_else(|| synctv_common::snanoid!(12), str::to_owned)
}

fn route_for_access_log(path: &str) -> String {
    if path.len() <= MAX_LOGGED_ROUTE_LEN {
        return path.to_string();
    }

    let mut end = MAX_LOGGED_ROUTE_LEN - 3;
    while !path.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &path[..end])
}

fn effective_client_ip(request: &Request, server: &ApiServerSettings) -> Option<IpAddr> {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip())?;
    Some(
        synctv_adapter::client_ip::extract_client_ip_from_headers(
            |ip| server.is_trusted_proxy(ip),
            peer_ip,
            request.headers(),
        )
        .unwrap_or(peer_ip),
    )
}

fn insert_request_id_header(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }
}

fn grpc_status(headers: &HeaderMap) -> Option<(i32, &'static str)> {
    let code = headers
        .get(&GRPC_STATUS)?
        .to_str()
        .ok()?
        .parse::<i32>()
        .ok()?;
    Some((code, grpc_status_name(code)))
}

fn http_access_log_level(status: StatusCode, slow: bool, completion: &str) -> AccessLogLevel {
    if completion == "body_error" || status.is_server_error() {
        AccessLogLevel::Error
    } else if status.is_client_error() || slow {
        AccessLogLevel::Warn
    } else if completion == "response_dropped" {
        AccessLogLevel::Debug
    } else {
        AccessLogLevel::Info
    }
}

fn grpc_access_log_level(
    http_status: StatusCode,
    grpc_code: i32,
    completion: &str,
) -> AccessLogLevel {
    if !http_status.is_success() || matches!(completion, "body_error" | "missing_status") {
        return AccessLogLevel::Error;
    }
    match grpc_code {
        1 => AccessLogLevel::Debug,
        0 | 3 | 5 | 6 | 7 | 9 | 11 | 12 | 16 => AccessLogLevel::Info,
        4 | 8 | 10 | 14 => AccessLogLevel::Warn,
        _ => AccessLogLevel::Error,
    }
}

fn record_grpc_metrics(route: &str, grpc_code: i32, grpc_status: &str, elapsed: Duration) {
    let (service, method) = grpc_metric_labels(route, grpc_code);
    metrics::REMOTE_TRANSPORT_REQUESTS_TOTAL
        .with_label_values(&[service, method, grpc_status])
        .inc();
    metrics::REMOTE_TRANSPORT_REQUEST_DURATION
        .with_label_values(&[service, method, grpc_status])
        .observe(elapsed.as_secs_f64());
}

fn grpc_metric_labels(route: &str, grpc_code: i32) -> (&str, &str) {
    let mut segments = route.strip_prefix('/').unwrap_or(route).split('/');
    let service = segments.next().filter(|value| !value.is_empty());
    let method = segments.next().filter(|value| !value.is_empty());
    if segments.next().is_some() {
        return ("<invalid>", "<invalid>");
    }
    match (service, method) {
        (Some(_), Some(_)) if grpc_code == 12 => ("<unimplemented>", "<unimplemented>"),
        (Some(service), Some(method)) => (service, method),
        _ => ("<invalid>", "<invalid>"),
    }
}

const fn grpc_status_name(code: i32) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN_CODE",
    }
}

fn duration_ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1_000_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{convert::Infallible, io::Write, sync::Arc};

    use axum::{routing::get, Router};
    use futures::stream;
    use http_body_util::{BodyExt, StreamBody};
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[derive(Clone, Default)]
    struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogCapture {
        fn contents(&self) -> String {
            String::from_utf8(
                self.0
                    .lock()
                    .expect("log capture lock should not be poisoned")
                    .clone(),
            )
            .expect("captured logs should be UTF-8")
        }
    }

    struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("log capture lock should not be poisoned")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for LogCapture {
        type Writer = LogWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            LogWriter(self.0.clone())
        }
    }

    fn json_subscriber(capture: LogCapture) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_writer(capture)
            .finish()
    }

    #[test]
    fn request_id_accepts_safe_values_and_replaces_unsafe_values() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert(&X_REQUEST_ID, HeaderValue::from_static("request_ABC-123"));
        assert_eq!(request_id(&headers), "request_ABC-123");

        headers.insert(&X_REQUEST_ID, HeaderValue::from_static("contains spaces"));
        let generated = request_id(&headers);
        assert_eq!(generated.len(), 12);
        assert!(generated.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        Ok(())
    }

    #[test]
    fn client_ip_uses_forwarded_header_only_for_trusted_proxy() -> TestResult {
        let peer = "192.0.2.10:8080".parse::<SocketAddr>()?;
        let mut request = Request::builder()
            .header("x-forwarded-for", "203.0.113.20")
            .body(Body::empty())?;
        request.extensions_mut().insert(ConnectInfo(peer));

        let untrusted = ApiServerSettings::default();
        assert_eq!(effective_client_ip(&request, &untrusted), Some(peer.ip()));

        let mut trusted = ApiServerSettings::default();
        trusted.trusted_proxies.push(peer.ip().to_string());
        assert_eq!(
            effective_client_ip(&request, &trusted),
            Some("203.0.113.20".parse::<IpAddr>()?)
        );
        Ok(())
    }

    #[test]
    fn grpc_status_names_cover_standard_and_unknown_codes() {
        assert_eq!(grpc_status_name(0), "OK");
        assert_eq!(grpc_status_name(13), "INTERNAL");
        assert_eq!(grpc_status_name(99), "UNKNOWN_CODE");
    }

    #[test]
    fn duration_ms_is_rounded_to_microsecond_precision() {
        assert!((duration_ms(Duration::from_nanos(45_250)) - 0.045).abs() < f64::EPSILON);
        assert!((duration_ms(Duration::from_micros(45)) - 0.045).abs() < f64::EPSILON);
    }

    #[test]
    fn access_log_levels_follow_transport_semantics() {
        assert_eq!(
            http_access_log_level(StatusCode::OK, false, "finished"),
            AccessLogLevel::Info
        );
        assert_eq!(
            http_access_log_level(StatusCode::OK, true, "finished"),
            AccessLogLevel::Warn
        );
        assert_eq!(
            http_access_log_level(StatusCode::OK, false, "response_dropped"),
            AccessLogLevel::Debug
        );
        assert_eq!(
            http_access_log_level(StatusCode::OK, false, "body_error"),
            AccessLogLevel::Error
        );
        assert_eq!(
            http_access_log_level(StatusCode::BAD_REQUEST, false, "finished"),
            AccessLogLevel::Warn
        );
        assert_eq!(
            http_access_log_level(StatusCode::INTERNAL_SERVER_ERROR, false, "finished"),
            AccessLogLevel::Error
        );
        assert_eq!(
            grpc_access_log_level(StatusCode::OK, 1, "response_dropped"),
            AccessLogLevel::Debug
        );
        assert_eq!(
            grpc_access_log_level(StatusCode::OK, 3, "finished"),
            AccessLogLevel::Info
        );
        assert_eq!(
            grpc_access_log_level(StatusCode::OK, 14, "finished"),
            AccessLogLevel::Warn
        );
        assert_eq!(
            grpc_access_log_level(StatusCode::OK, 13, "finished"),
            AccessLogLevel::Error
        );
    }

    #[test]
    fn grpc_metric_labels_bound_unimplemented_and_invalid_methods() {
        assert_eq!(
            grpc_metric_labels("/package.Service/Stream", 0),
            ("package.Service", "Stream")
        );
        assert_eq!(
            grpc_metric_labels("/package.Service/attacker-controlled", 12),
            ("<unimplemented>", "<unimplemented>")
        );
        assert_eq!(
            grpc_metric_labels("/attacker-controlled.Service/Method", 12),
            ("<unimplemented>", "<unimplemented>")
        );
        assert_eq!(
            grpc_metric_labels("/invalid/too/many", 0),
            ("<invalid>", "<invalid>")
        );
    }

    #[test]
    fn grpc_fallback_logs_unmatched_http_path_without_query() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let server = ApiServerSettings::default();
                let access_log = AccessLogSettings::default();
                let app = Router::new()
                    .route(
                        "/private-resource-id",
                        get(|| async { StatusCode::NOT_FOUND }),
                    )
                    .layer(axum::middleware::from_fn(move |request, next| {
                        let server = server.clone();
                        let access_log = access_log.clone();
                        async move {
                            grpc_access_log_middleware(request, next, &server, &access_log).await
                        }
                    }));
                let request = Request::builder()
                    .uri("/private-resource-id?secret=must-not-appear")
                    .header(&X_REQUEST_ID, "http-fallback-001")
                    .body(Body::empty())?;

                let response = app.oneshot(request).await?;
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert_eq!(
                    response.headers().get(&X_REQUEST_ID),
                    Some(&HeaderValue::from_static("http-fallback-001"))
                );
                response.into_body().collect().await?;
                TestResult::Ok(())
            })
        })?;

        let output = capture.contents();
        assert!(output.contains(r#""level":"WARN""#), "{output}");
        assert!(output.contains(r#""protocol":"http""#), "{output}");
        assert!(output.contains(r#""route":"/private-resource-id""#));
        assert!(output.contains(r#""route_matched":false"#));
        assert!(output.contains(r#""status":404"#));
        assert!(!output.contains(r#""protocol":"grpc""#));
        assert!(!output.contains("must-not-appear"));
        Ok(())
    }

    #[test]
    fn http_access_log_uses_matched_route_and_omits_query() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let server = ApiServerSettings::default();
                let access_log = AccessLogSettings::default();
                let app = Router::new()
                    .route(
                        "/items/{item_id}",
                        get(|| async {
                            let frames = stream::iter([Ok::<_, Infallible>(Frame::data(
                                Bytes::from_static(b"response-body"),
                            ))]);
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .header(axum::http::header::CONTENT_LENGTH, "13")
                                .body(Body::new(StreamBody::new(frames)))
                                .expect("streaming response should build")
                        }),
                    )
                    .layer(axum::middleware::from_fn(move |request, next| {
                        let server = server.clone();
                        let access_log = access_log.clone();
                        async move {
                            http_access_log_middleware(request, next, &server, &access_log).await
                        }
                    }));
                let mut request = Request::builder()
                    .uri("/items/private-item?token=secret-value")
                    .header(&X_REQUEST_ID, "http-request-123")
                    .body(Body::empty())?;
                request
                    .extensions_mut()
                    .insert(ConnectInfo("192.0.2.10:8080".parse::<SocketAddr>()?));

                let response = app.oneshot(request).await?;
                assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
                assert_eq!(
                    response.headers().get(&X_REQUEST_ID),
                    Some(&HeaderValue::from_static("http-request-123"))
                );
                assert!(capture.contents().is_empty());
                let mut body = response.into_body();
                let frame = body
                    .frame()
                    .await
                    .ok_or("response body should contain one frame")??;
                assert_eq!(
                    frame.data_ref(),
                    Some(&Bytes::from_static(b"response-body"))
                );
                assert!(!body.is_end_stream());
                drop(body);
                TestResult::Ok(())
            })
        })?;

        let output = capture.contents();
        assert!(output.contains(r#""level":"ERROR""#), "{output}");
        assert!(output.contains(r#""protocol":"http""#));
        assert!(output.contains(r#""method":"GET""#));
        assert!(output.contains(r#""route":"/items/{item_id}""#));
        assert!(output.contains(r#""route_matched":true"#));
        assert!(output.contains(r#""status":500"#));
        assert!(output.contains(r#""response_bytes":13"#));
        assert!(output.contains(r#""slow":false"#));
        assert!(output.contains(r#""completion":"finished""#));
        assert!(output.contains(r#""client_ip":"192.0.2.10""#));
        assert!(output.contains(r#""request_id":"http-request-123""#));
        assert!(!output.contains("private-item"));
        assert!(!output.contains("secret-value"));
        Ok(())
    }

    #[test]
    fn http_access_log_reports_body_errors_after_partial_delivery() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let server = ApiServerSettings::default();
                let access_log = AccessLogSettings::default();
                let app = Router::new()
                    .route(
                        "/stream",
                        get(|| async {
                            let frames = stream::iter([
                                Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(
                                    b"partial",
                                ))),
                                Err(std::io::Error::other("stream failed")),
                            ]);
                            Response::builder()
                                .body(Body::new(StreamBody::new(frames)))
                                .expect("streaming response should build")
                        }),
                    )
                    .layer(axum::middleware::from_fn(move |request, next| {
                        let server = server.clone();
                        let access_log = access_log.clone();
                        async move {
                            http_access_log_middleware(request, next, &server, &access_log).await
                        }
                    }));
                let request = Request::builder()
                    .uri("/stream")
                    .header(&X_REQUEST_ID, "http-stream-error")
                    .body(Body::empty())?;

                let response = app.oneshot(request).await?;
                assert!(capture.contents().is_empty());
                assert!(response.into_body().collect().await.is_err());
                TestResult::Ok(())
            })
        })?;

        let output = capture.contents();
        assert!(output.contains(r#""level":"ERROR""#), "{output}");
        assert!(output.contains(r#""response_bytes":7"#));
        assert!(output.contains(r#""completion":"body_error""#));
        assert!(output.contains(r#""request_id":"http-stream-error""#));
        Ok(())
    }

    #[test]
    fn http_access_log_completes_responses_that_have_no_wire_body() -> TestResult {
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            for (method, status, expected_response_bytes) in [
                (Method::HEAD, StatusCode::OK, Some(13)),
                (Method::GET, StatusCode::NO_CONTENT, None),
                (Method::GET, StatusCode::OK, Some(0)),
            ] {
                let request = Request::builder()
                    .method(method)
                    .uri("/resource")
                    .body(Body::empty())?;
                let context = AccessLogContext::from_request(
                    &request,
                    &ApiServerSettings::default(),
                    &AccessLogSettings::default(),
                    "/resource".to_string(),
                );
                let frames = stream::pending::<Result<Frame<Bytes>, Infallible>>();
                let body = HttpAccessLogBody::new(
                    Body::new(StreamBody::new(frames)),
                    context,
                    status,
                    Duration::ZERO,
                    expected_response_bytes,
                );
                drop(body);
            }
            TestResult::Ok(())
        })?;

        let output = capture.contents();
        assert_eq!(output.matches(r#""completion":"finished""#).count(), 3);
        assert_eq!(output.matches(r#""response_bytes":0"#).count(), 3);
        assert!(!output.contains("response_dropped"));
        Ok(())
    }

    #[test]
    fn disabled_access_log_still_returns_request_id() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let server = ApiServerSettings::default();
                let access_log = AccessLogSettings {
                    enabled: false,
                    ..AccessLogSettings::default()
                };
                let app = Router::new()
                    .route("/health", get(|| async { StatusCode::OK }))
                    .layer(axum::middleware::from_fn(move |request, next| {
                        let server = server.clone();
                        let access_log = access_log.clone();
                        async move {
                            http_access_log_middleware(request, next, &server, &access_log).await
                        }
                    }));
                let request = Request::builder()
                    .uri("/health")
                    .header(&X_REQUEST_ID, "request-without-log")
                    .body(Body::empty())?;

                let response = app.oneshot(request).await?;
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(
                    response.headers().get(&X_REQUEST_ID),
                    Some(&HeaderValue::from_static("request-without-log"))
                );
                TestResult::Ok(())
            })
        })?;

        assert!(capture.contents().is_empty());
        Ok(())
    }

    #[test]
    fn grpc_access_log_waits_for_trailers_and_reports_final_status() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let capture = LogCapture::default();
        let subscriber = json_subscriber(capture.clone());

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let server = ApiServerSettings::default();
                let access_log = AccessLogSettings::default();
                let app = Router::new()
                    .route(
                        "/package.Service/{method}",
                        axum::routing::post(|| async {
                            let mut trailers = HeaderMap::new();
                            trailers.insert(GRPC_STATUS, HeaderValue::from_static("13"));
                            let frames =
                                stream::iter([Ok::<_, Infallible>(Frame::trailers(trailers))]);
                            Response::builder()
                                .header(axum::http::header::CONTENT_TYPE, "application/grpc")
                                .body(Body::new(StreamBody::new(frames)))
                                .expect("gRPC test response should build")
                        }),
                    )
                    .layer(axum::middleware::from_fn(move |request, next| {
                        let server = server.clone();
                        let access_log = access_log.clone();
                        async move {
                            grpc_access_log_middleware(request, next, &server, &access_log).await
                        }
                    }));
                let request = Request::builder()
                    .method(Method::POST)
                    .uri("/package.Service/Stream?authorization=secret-value")
                    .header(axum::http::header::CONTENT_TYPE, "application/grpc")
                    .header(&X_REQUEST_ID, "grpc-request-123")
                    .body(Body::empty())?;

                let response = app.oneshot(request).await?;
                assert!(capture.contents().is_empty());
                assert_eq!(
                    response.headers().get(&X_REQUEST_ID),
                    Some(&HeaderValue::from_static("grpc-request-123"))
                );
                response.into_body().collect().await?;
                TestResult::Ok(())
            })
        })?;

        let output = capture.contents();
        assert!(output.contains(r#""level":"ERROR""#), "{output}");
        assert!(output.contains(r#""protocol":"grpc""#));
        assert!(output.contains(r#""rpc":"/package.Service/Stream""#));
        assert!(output.contains(r#""grpc_status":"INTERNAL""#));
        assert!(output.contains(r#""grpc_code":13"#));
        assert!(output.contains(r#""completion":"finished""#));
        assert!(output.contains(r#""request_id":"grpc-request-123""#));
        assert!(!output.contains("authorization"));
        assert!(!output.contains("secret-value"));
        Ok(())
    }
}
