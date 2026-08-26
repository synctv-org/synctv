//! External Stream Puller
//!
//! Pulls live streams from external RTMP, RTSP, HTTP-FLV, or WHEP endpoints and publishes them
//! to the local `StreamHub` under the local stream identity (`live/{room_id}/{media_id}`).
//!
//! Supports:
//! - **RTMP**: Connects as an RTMP client via xiu's `ClientSession` in Pull mode.
//!   Uses a bridge channel pattern to remap the remote stream identity to our local
//!   `live/{room_id}/{media_id}` identity. The bridge intercepts the `ClientSession`'s
//!   Publish event and returns our local `StreamHub`'s `FrameDataSender`, so all frames
//!   flow directly into the correct stream.
//! - **HTTP-FLV**: Streams FLV data via HTTP GET using reqwest, parses FLV tags
//!   (header + audio/video/metadata tags) in a streaming fashion, and forwards
//!   frames to the local `StreamHub`.
//! - **RTSP**: Uses Retina for DESCRIBE/SETUP/PLAY, RTP transport, authentication,
//!   and codec depacketization. H.264, H.265, and AAC are converted to FLV tag
//!   bodies and published through the same local stream identity.
//! - **WHEP**: Negotiates an H.264/Opus WebRTC session, preserves RTP for local
//!   WHEP subscribers, and remuxes frames for HLS and HTTP-FLV subscribers.
//!
//! Frame-based sources retry established connections. WHEP disconnects end the
//! current publication because a new peer connection starts a new RTP timeline.

use std::sync::Arc;

use anyhow::Result;
use bytes::{Buf, BytesMut};
use synctv_common::{self, ssrf::SsrfGuard};
use synctv_core::models::{
    ExternalLiveSourceConfig, RtmpStreamMode as CoreRtmpStreamMode,
    RtspTrackSelection as CoreRtspTrackSelection, RtspTransport as CoreRtspTransport,
};
use synctv_xiu::rtmp::session::client_session::{
    ClientSession, ClientSessionConfig, ClientSessionType, RtmpStreamMode,
};
use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::rtmp::utils::RtmpUrlParser;
use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTrackSelection, RtspTransport};
use synctv_xiu::streamhub::{
    define::{
        FrameData, FrameDataSender, NotifyInfo, PacketDataSender, PubDataType, PublishType,
        PublisherInfo, StreamHubEvent, StreamHubEventSender,
    },
    send_event_with_backpressure_timeout_for, spawn_event_delivery_with_backpressure_timeout_for,
    stream::StreamIdentifier,
    utils::Uuid,
};
use synctv_xiu::webrtc::{create_whep_client_session, WebRtcConfig};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

const MAX_RETRIES: u32 = 10;
const INITIAL_BACKOFF_MS: u64 = 1000;
const MAX_BACKOFF_MS: u64 = 30_000;
/// Maximum total FLV buffer size (50 MB) to prevent unbounded memory growth
const MAX_FLV_BUFFER_SIZE: usize = 50 * 1024 * 1024;
/// Maximum time to establish the HTTP request and receive response headers.
///
/// Live HTTP-FLV streams can run indefinitely, so this timeout only covers the
/// startup phase. Ongoing liveness is enforced by the per-chunk read timeout.
const HTTP_FLV_REQUEST_START_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);
/// Per-chunk read timeout: if no data arrives for 30s, the stream is dead.
const HTTP_FLV_CHUNK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const WHEP_REQUEST_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const WHEP_RESPONSE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const WHEP_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const WHEP_DELETE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Maximum time between decoded RTSP media frames before reconnecting.
const RTSP_FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const FRAME_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const STREAMHUB_EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
#[error("{0} source ended and requires reconnect")]
struct ReconnectRequired(&'static str);

struct LocalPublication {
    generation_id: Uuid,
    frame_sender: FrameDataSender,
    packet_sender: Option<PacketDataSender>,
}

// FLV format constants
const FLV_HEADER_SIZE: usize = 9;
const FLV_PREV_TAG_SIZE_LEN: usize = 4;
const FLV_TAG_HEADER_SIZE: usize = 11;
const FLV_TAG_AUDIO: u8 = 8;
const FLV_TAG_VIDEO: u8 = 9;
const FLV_TAG_SCRIPT_DATA: u8 = 18;

pub(crate) fn redact_source_url_for_logs(source_url: &str) -> String {
    let Ok(mut parsed) = Url::parse(source_url) else {
        return "<invalid-url>".to_string();
    };
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        warn!("failed to redact external pull source URL credentials");
        return "<redaction-failed>".to_string();
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

fn notify_oneshot<T>(sender: oneshot::Sender<T>, value: T, description: &'static str) {
    if sender.send(value).is_err() {
        debug!(description, "oneshot receiver dropped before notification");
    }
}

async fn log_aborted_task_join(task_name: &'static str, handle: tokio::task::JoinHandle<()>) {
    match handle.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            warn!(
                task = task_name,
                error = %error,
                "aborted task returned join error"
            );
        }
    }
}

async fn send_frame_with_backpressure(
    data_sender: &FrameDataSender,
    frame: FrameData,
) -> Result<()> {
    tokio::time::timeout(FRAME_SEND_TIMEOUT, data_sender.send(frame))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Timed out waiting {}s for local live-stream backpressure to clear",
                FRAME_SEND_TIMEOUT.as_secs()
            )
        })?
        .map_err(|_| anyhow::anyhow!("Local live-stream consumer channel closed"))
}

/// Source type for external streams
#[derive(Debug, Clone)]
pub(crate) enum ExternalSourceType {
    /// RTMP URL (e.g., <rtmp://live.example.com/app/stream>)
    Rtmp,
    /// RTSP URL (e.g., <rtsp://camera.example.com/live>)
    Rtsp,
    /// HTTP-FLV URL (e.g., <http://live.example.com/app/stream.flv>)
    HttpFlv,
    /// WHEP endpoint (HTTP or HTTPS).
    Whep,
}

impl ExternalSourceType {
    const fn can_reconnect_in_place(&self) -> bool {
        !matches!(self, Self::Whep)
    }

    fn from_config(config: &ExternalLiveSourceConfig) -> Result<Self> {
        let (source_type, expected_scheme) = match config {
            ExternalLiveSourceConfig::Rtmp { .. } => (Self::Rtmp, "rtmp"),
            ExternalLiveSourceConfig::Rtsp { .. } => (Self::Rtsp, "rtsp"),
            ExternalLiveSourceConfig::HttpFlv { .. } => (Self::HttpFlv, "http or https"),
            ExternalLiveSourceConfig::Whep { .. } => (Self::Whep, "http or https"),
        };
        let parsed = Url::parse(config.url())?;
        let valid = match source_type {
            Self::Rtmp => parsed.scheme() == "rtmp",
            Self::Rtsp => parsed.scheme() == "rtsp",
            Self::HttpFlv => {
                matches!(parsed.scheme(), "http" | "https") && parsed.path().ends_with(".flv")
            }
            Self::Whep => {
                matches!(parsed.scheme(), "http" | "https")
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.fragment().is_none()
            }
        };
        anyhow::ensure!(
            valid,
            "External source protocol and URL disagree: expected {expected_scheme} source"
        );
        Ok(source_type)
    }
}

const fn map_rtsp_transport(transport: CoreRtspTransport) -> RtspTransport {
    match transport {
        CoreRtspTransport::Tcp => RtspTransport::Tcp,
        CoreRtspTransport::Udp => RtspTransport::Udp,
    }
}

fn map_rtsp_track(selection: CoreRtspTrackSelection) -> RtspTrackSelection {
    match selection {
        CoreRtspTrackSelection::FirstCompatible => RtspTrackSelection::FirstCompatible,
        CoreRtspTrackSelection::Index(index) => {
            RtspTrackSelection::Index(usize::try_from(index).unwrap_or(usize::MAX))
        }
        CoreRtspTrackSelection::Disabled => RtspTrackSelection::Disabled,
    }
}

const fn map_rtmp_mode(mode: CoreRtmpStreamMode) -> RtmpStreamMode {
    match mode {
        CoreRtmpStreamMode::Default => RtmpStreamMode::Default,
        CoreRtmpStreamMode::VideoOnly => RtmpStreamMode::VideoOnly,
        CoreRtmpStreamMode::AudioOnly => RtmpStreamMode::AudioOnly,
    }
}

/// External Stream Puller
///
/// Connects to a remote streaming source and publishes frames to the local
/// `StreamHub` under the local stream identity (`live/{room_id}/{media_id}`).
pub(crate) struct ExternalStreamPuller {
    room_id: String,
    media_id: String,
    source_url: String,
    source_type: ExternalSourceType,
    rtmp_mode: RtmpStreamMode,
    rtsp_options: Option<(RtspTransport, RtspTrackSelection, RtspTrackSelection)>,
    whep_authorization: Option<String>,
    webrtc_config: WebRtcConfig,
    stream_hub_event_sender: StreamHubEventSender,
    generation_id: Uuid,
    /// Optional one-shot channel to signal that the first connection succeeded.
    /// Sent once after the first successful publish + connect, then set to `None`.
    confirm_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    /// Shared HTTP client for FLV connections (reused across retries, supports TLS via rustls).
    http_client: Option<reqwest::Client>,
    /// Pinned resolved address from SSRF validation to prevent DNS rebinding attacks.
    /// Set by `new_async()` after validating the URL; the actual TCP/HTTP connection
    /// uses this address instead of re-resolving the hostname.
    resolved_addr: Option<std::net::SocketAddr>,
    /// Cancellation token for graceful shutdown. When cancelled, the puller
    /// exits the main loop cleanly and unpublishes from the local `StreamHub`.
    cancel_token: CancellationToken,
    /// Maximum FLV tag data size accepted from external HTTP-FLV sources.
    max_flv_tag_size_bytes: usize,
    /// Explicit SSRF policy injected by the application layer.
    ssrf_guard: SsrfGuard,
}

impl ExternalStreamPuller {
    pub const DEFAULT_MAX_FLV_TAG_SIZE_BYTES: usize = 10 * 1024 * 1024;

    /// Create with async DNS-resolved SSRF validation (required for all production use).
    /// Resolves the hostname and validates all resolved IPs against blocklists.
    pub(crate) async fn new_async(
        room_id: String,
        media_id: String,
        source: ExternalLiveSourceConfig,
        stream_hub_event_sender: StreamHubEventSender,
        ssrf_guard: SsrfGuard,
    ) -> Result<Self> {
        Self::new_async_with_resolver(
            room_id,
            media_id,
            source,
            stream_hub_event_sender,
            ssrf_guard,
            |host, port| async move {
                let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|e| anyhow::anyhow!("DNS resolution failed: {e}"))?
                    .collect();
                Ok(addrs)
            },
        )
        .await
    }

    async fn new_async_with_resolver<F, Fut>(
        room_id: String,
        media_id: String,
        source: ExternalLiveSourceConfig,
        stream_hub_event_sender: StreamHubEventSender,
        ssrf_guard: SsrfGuard,
        resolver: F,
    ) -> Result<Self>
    where
        F: Fn(String, u16) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<std::net::SocketAddr>>>,
    {
        let source_type = ExternalSourceType::from_config(&source)?;
        let source_url = source.url().to_string();
        let rtsp_options = match &source {
            ExternalLiveSourceConfig::Rtsp {
                transport,
                video_track,
                audio_track,
                ..
            } => Some((
                map_rtsp_transport(*transport),
                map_rtsp_track(*video_track),
                map_rtsp_track(*audio_track),
            )),
            _ => None,
        };
        let rtmp_mode = match &source {
            ExternalLiveSourceConfig::Rtmp { mode, .. } => map_rtmp_mode(*mode),
            _ => RtmpStreamMode::Default,
        };
        let whep_authorization = match &source {
            ExternalLiveSourceConfig::Whep { authorization, .. } => authorization.clone(),
            _ => None,
        };

        // Resolve hostname and validate IPs against SSRF ACL.
        // For RTMP: the DNS resolver can't be injected, so we check IPs explicitly.
        // For HTTP-FLV: the reqwest DNS resolver handles it, but we still pin the
        // address to prevent DNS rebinding between validation and connection.
        let parsed = Url::parse(&source_url)?;
        let host = parsed.host_str().unwrap_or("");
        let default_port = match source_type {
            ExternalSourceType::Rtmp => 1935,
            ExternalSourceType::Rtsp => 554,
            ExternalSourceType::HttpFlv | ExternalSourceType::Whep => {
                if parsed.scheme() == "https" {
                    443
                } else {
                    80
                }
            }
        };
        let mut resolved_addr = None;
        let port = parsed.port().unwrap_or(default_port);

        if !host.is_empty() {
            ssrf_guard
                .validate_url_target_with_default_port(host, port, default_port)
                .map_err(|error| anyhow::anyhow!("SSRF protection blocked URL: {error}"))?;

            if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                resolved_addr = Some(std::net::SocketAddr::new(ip, port));
            } else {
                // Hostname - resolve and check all IPs
                let addrs = resolver(host.to_string(), port).await?;

                let safe_addrs: Vec<std::net::SocketAddr> = addrs
                    .into_iter()
                    .filter(|addr| !ssrf_guard.is_ip_blocked_for_host(host, &addr.ip()))
                    .collect();

                if safe_addrs.is_empty() {
                    return Err(anyhow::anyhow!(
                        "SSRF protection blocked URL: all resolved IPs for {host} are private/reserved"
                    ));
                }

                resolved_addr = safe_addrs.into_iter().next();
            }
        }

        Ok(Self {
            room_id,
            media_id,
            source_url,
            source_type,
            rtmp_mode,
            rtsp_options,
            whep_authorization,
            webrtc_config: WebRtcConfig::default(),
            stream_hub_event_sender,
            generation_id: Uuid::new(),
            confirm_tx: None,
            http_client: None,
            resolved_addr,
            cancel_token: CancellationToken::new(),
            max_flv_tag_size_bytes: Self::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
            ssrf_guard,
        })
    }

    #[must_use]
    pub(crate) const fn with_max_flv_tag_size_bytes(mut self, max: usize) -> Self {
        self.max_flv_tag_size_bytes = max;
        self
    }

    #[must_use]
    pub(crate) const fn with_generation_id(mut self, generation_id: Uuid) -> Self {
        self.generation_id = generation_id;
        self
    }

    /// Set a one-shot confirmation channel. The puller will signal this channel
    /// after the first successful connection is established (not just after URL validation).
    #[must_use]
    pub(crate) fn with_confirm(
        mut self,
        tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> Self {
        self.confirm_tx = Some(tx);
        self
    }

    /// Set a shared HTTP client for FLV connections (reused across retries, supports TLS).
    #[must_use]
    pub(crate) fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    #[must_use]
    pub(crate) fn with_webrtc_config(mut self, config: WebRtcConfig) -> Self {
        self.webrtc_config = config;
        self
    }

    /// Run the puller with bounded retry logic.
    ///
    /// Startup failures are reported to the caller immediately. After the first
    /// successful connection, frame-based sources retry with exponential backoff
    /// up to `MAX_RETRIES`. A disconnected WHEP source ends its RTP publication so
    /// a later request can establish a new generation with a new RTP timeline.
    /// If the cancellation token is triggered, the loop exits cleanly and returns `Ok(())`.
    pub(crate) async fn run(mut self) -> Result<()> {
        let source_url_for_logs = if matches!(self.source_type, ExternalSourceType::Whep) {
            "<redacted-whep-url>".to_string()
        } else {
            redact_source_url_for_logs(&self.source_url)
        };
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            source_url = %source_url_for_logs,
            source_type = ?self.source_type,
            "Starting external stream puller"
        );

        if self.cancel_token.is_cancelled() {
            return Ok(());
        }

        let publication = match self.publish_to_local_stream_hub().await {
            Ok(publication) => publication,
            Err(error) => {
                if let Some(tx) = self.confirm_tx.take() {
                    notify_oneshot(
                        tx,
                        Err(format!("{error}")),
                        "external pull startup publish failure",
                    );
                }
                return Err(error);
            }
        };
        let unpublish_guard = UnpublishGuard::new(
            self.room_id.clone(),
            self.media_id.clone(),
            publication.generation_id,
            self.stream_hub_event_sender.clone(),
        );

        let result = self
            .run_upstream_connections(
                &publication.frame_sender,
                publication.packet_sender.as_ref(),
            )
            .await;

        unpublish_guard.disarm();
        drop(unpublish_guard);
        self.unpublish_from_local_stream_hub(publication.generation_id);
        result
    }

    async fn run_upstream_connections(
        &mut self,
        data_sender: &FrameDataSender,
        packet_sender: Option<&PacketDataSender>,
    ) -> Result<()> {
        let mut attempt = 0_u32;
        loop {
            if self.cancel_token.is_cancelled() {
                return Ok(());
            }
            attempt += 1;

            let result = match self.source_type {
                ExternalSourceType::Rtmp => self.connect_and_stream_rtmp(data_sender).await,
                ExternalSourceType::Rtsp => self.connect_and_stream_rtsp(data_sender).await,
                ExternalSourceType::HttpFlv => self.connect_and_stream_flv(data_sender).await,
                ExternalSourceType::Whep => {
                    let packet_sender = packet_sender.ok_or_else(|| {
                        anyhow::anyhow!("WHEP publication did not provide an RTP packet channel")
                    })?;
                    self.connect_and_stream_whep(data_sender, packet_sender)
                        .await
                }
            };
            let startup_confirmation_pending = self.confirm_tx.is_some();
            let reconnect_required = result
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<ReconnectRequired>().is_some());

            if self.cancel_token.is_cancelled() {
                return Ok(());
            }

            match result {
                Ok(()) => {
                    info!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        "External stream ended normally"
                    );
                    return Ok(());
                }
                Err(error) => {
                    if startup_confirmation_pending && !reconnect_required {
                        if let Some(tx) = self.confirm_tx.take() {
                            notify_oneshot(
                                tx,
                                Err(format!("{error}")),
                                "external pull startup failure",
                            );
                        }
                        return Err(error);
                    }

                    if reconnect_required && !self.source_type.can_reconnect_in_place() {
                        return Err(anyhow::anyhow!(
                            "WHEP source disconnected; ending the current publication: {error}"
                        ));
                    }

                    if attempt >= MAX_RETRIES {
                        if let Some(tx) = self.confirm_tx.take() {
                            notify_oneshot(
                                tx,
                                Err(format!("{error}")),
                                "external pull startup retry exhaustion",
                            );
                        }
                        return Err(anyhow::anyhow!(
                            "Gave up after {MAX_RETRIES} retries (last error: {error})"
                        ));
                    }

                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        attempt,
                        max_retries = MAX_RETRIES,
                        "External stream pull failed, retrying: {error}"
                    );
                    tokio::select! {
                        () = self.cancel_token.cancelled() => return Ok(()),
                        () = Self::backoff(attempt) => {}
                    }
                }
            }
        }
    }

    /// Connect to remote RTMP server, play the stream, and bridge frames to local `StreamHub`.
    ///
    /// Uses xiu's `ClientSession` in Pull mode with a bridge channel pattern:
    /// 1. A bridge channel replaces the real `StreamHub` event sender for `ClientSession`
    /// 2. When `ClientSession` sends a `Publish` event (on play start), the bridge
    ///    responds with our local `FrameDataSender` instead of creating a new stream
    /// 3. `ClientSession` then sends all received audio/video/metadata frames directly
    ///    through our `FrameDataSender` into the local `StreamHub` under `live/{room_id}/{media_id}`
    async fn connect_and_stream_rtmp(&mut self, data_sender: &FrameDataSender) -> Result<()> {
        // Parse RTMP URL to extract host, port, app_name, stream_name
        let mut parser = RtmpUrlParser::new(self.source_url.clone());
        parser
            .parse_url()
            .map_err(|e| anyhow::anyhow!("Invalid RTMP URL: {e:?}"))?;

        // REQUIRE pinned resolved address - prevents DNS rebinding attacks.
        // This field must be set via new_async(); if using new(), this will fail.
        let connect_addr = self
            .resolved_addr
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Resolved address not available. SSRF validation may have failed. \
                 Use new_async() instead of new() for proper DNS pinning."
                )
            })?
            .to_string();

        info!(
            connect_addr = %connect_addr,
            app_name = %parser.app_name,
            stream_name = %parser.stream_name,
            "Connecting to remote RTMP server"
        );

        // Connect TCP to remote RTMP server with timeout
        let tcp_connect_timeout_secs: u64 = 10;
        let tcp_stream = tokio::time::timeout(
            std::time::Duration::from_secs(tcp_connect_timeout_secs),
            tokio::net::TcpStream::connect(&connect_addr),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "TCP connection to {connect_addr} timed out after {tcp_connect_timeout_secs}s"
            )
        })?
        .map_err(|e| anyhow::anyhow!("Failed to connect to {connect_addr}: {e}"))?;

        // Create bridge channel — ClientSession sends StreamHub events here
        // instead of the real StreamHub. We intercept and redirect.
        let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::channel::<StreamHubEvent>(64);
        let (publish_started_tx, publish_started_rx) = tokio::sync::oneshot::channel::<()>();

        // Clone data_sender for the bridge task
        let bridge_data_sender = data_sender.clone();

        // Spawn bridge task: intercepts ClientSession's Publish event and returns our data_sender.
        // When ClientSession receives "NetStream.Play.Start" from the remote, it calls
        // publish_to_stream_hub() which sends StreamHubEvent::Publish through bridge_tx.
        // The bridge responds with our FrameDataSender (from the real local StreamHub publish).
        // ClientSession then stores it as self.data_sender, so all subsequent on_video_data /
        // on_audio_data calls send frames through our sender into the correct local stream.
        let bridge_handle = tokio::spawn(async move {
            let mut publish_started_tx = Some(publish_started_tx);
            while let Some(event) = bridge_rx.recv().await {
                match event {
                    StreamHubEvent::Publish { result_sender, .. } => {
                        // Respond with our local StreamHub's FrameDataSender
                        notify_oneshot(
                            result_sender,
                            Ok((
                                Some(bridge_data_sender.clone()),
                                None, // No packet data sender needed
                                None, // No statistic data sender needed
                            )),
                            "RTMP bridge publish result",
                        );
                        if let Some(tx) = publish_started_tx.take() {
                            notify_oneshot(tx, (), "RTMP bridge publish started");
                        }
                    }
                    StreamHubEvent::UnPublish { .. } => {
                        // Remote stream ended — exit bridge
                        break;
                    }
                    _ => {
                        // Ignore other events (Subscribe, UnSubscribe, etc.)
                    }
                }
            }
        });

        // Create RTMP client session in Pull mode.
        // ClientSession will: handshake → connect → createStream → play → receive data.
        // The bridge_tx replaces the normal StreamHub event sender, redirecting frames
        // to our local stream identity.
        let mut client = ClientSession::new(
            tcp_stream,
            ClientSessionConfig {
                client_type: ClientSessionType::Pull,
                raw_domain_name: parser.host_with_port.clone(),
                app_name: parser.app_name.clone(),
                raw_stream_name: parser.stream_name_with_query.clone(),
                event_producer: bridge_tx,
                gop_num: 2,
                per_stream_max_bytes: None,
                media_mode: self.rtmp_mode,
            },
        );

        let mut client_run = Box::pin(client.run());
        let mut publish_started_rx = Box::pin(publish_started_rx);
        let mut publish_confirmed = false;

        let result = loop {
            tokio::select! {
                r = &mut client_run => {
                    break r.map_err(|e| anyhow::anyhow!("RTMP client session error: {e}"));
                }
                ready = &mut publish_started_rx, if !publish_confirmed => {
                    if ready.is_ok() {
                        self.send_confirm_ok();
                    }
                    publish_confirmed = true;
                }
                () = self.cancel_token.cancelled() => {
                    info!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        "RTMP stream puller cancelled"
                    );
                    break Ok(());
                }
            }
        };

        // Cleanup: abort bridge task and await to ensure it is fully cleaned up
        bridge_handle.abort();
        log_aborted_task_join("RTMP pull bridge", bridge_handle).await;

        result
    }

    /// Connect to an RTSP source and publish its selected H.264/H.265/AAC tracks.
    async fn connect_and_stream_rtsp(&mut self, data_sender: &FrameDataSender) -> Result<()> {
        let resolved_addr = self.resolved_addr.ok_or_else(|| {
            anyhow::anyhow!("Resolved RTSP address is unavailable after source validation")
        })?;
        let mut config = RtspPullConfig::from_url(&self.source_url)?;
        let (transport, video_track, audio_track) = self.rtsp_options.ok_or_else(|| {
            anyhow::anyhow!("RTSP source is missing transport and track configuration")
        })?;
        config.transport = transport;
        config.video_track = video_track;
        config.audio_track = audio_track;
        config.pin_address(resolved_addr)?;

        info!(
            source_url = %redact_source_url_for_logs(&self.source_url),
            connect_addr = %resolved_addr,
            transport = ?config.transport,
            "Connecting to RTSP source"
        );

        let mut session = RtspPullSession::connect(config).await?;
        let (video_track, audio_track) = session.selected_tracks();
        info!(?video_track, ?audio_track, "RTSP source is playing");
        let mut first_frame_confirmed = false;

        loop {
            let frame = tokio::select! {
                () = self.cancel_token.cancelled() => {
                    info!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        "RTSP stream puller cancelled"
                    );
                    return Ok(());
                }
                result = tokio::time::timeout(RTSP_FRAME_READ_TIMEOUT, session.next_frame()) => {
                    result
                        .map_err(|_| anyhow::anyhow!(
                            "No RTSP media frame received for {}s",
                            RTSP_FRAME_READ_TIMEOUT.as_secs()
                        ))??
                },
            };
            let Some(frame) = frame else {
                return Err(ReconnectRequired("RTSP").into());
            };
            let frame_size = match &frame {
                FrameData::Video { data, .. }
                | FrameData::Audio { data, .. }
                | FrameData::MetaData { data, .. } => data.len(),
                FrameData::MediaInfo { .. } => 0,
            };
            anyhow::ensure!(
                frame_size <= self.max_flv_tag_size_bytes,
                "RTSP frame size {frame_size} exceeds the configured {} byte limit",
                self.max_flv_tag_size_bytes
            );
            send_frame_with_backpressure(data_sender, frame).await?;
            if !first_frame_confirmed {
                self.send_confirm_ok();
                first_frame_confirmed = true;
            }
        }
    }

    async fn connect_and_stream_whep(
        &mut self,
        frame_sender: &FrameDataSender,
        packet_sender: &PacketDataSender,
    ) -> Result<()> {
        let addr = self.resolved_addr.ok_or_else(|| {
            anyhow::anyhow!("Resolved WHEP address is unavailable after source validation")
        })?;
        let client = build_pinned_http_client(&self.source_url, addr, &self.ssrf_guard, "WHEP")?;
        let authorization = self
            .whep_authorization
            .as_deref()
            .map(reqwest::header::HeaderValue::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("WHEP Authorization header is invalid"))?;
        let session = create_whep_client_session(
            frame_sender.clone(),
            packet_sender.clone(),
            &self.webrtc_config,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Failed to create WHEP client session: {error}"))?;

        let mut request = client
            .post(&self.source_url)
            .header(reqwest::header::CONTENT_TYPE, "application/sdp")
            .header(reqwest::header::ACCEPT, "application/sdp")
            .body(session.offer_sdp.clone());
        if let Some(value) = authorization.as_ref() {
            request = request.header(reqwest::header::AUTHORIZATION, value.clone());
        }

        let response = tokio::select! {
            () = self.cancel_token.cancelled() => {
                let _ = session.close().await;
                return Ok(());
            }
            result = tokio::time::timeout(WHEP_REQUEST_START_TIMEOUT, request.send()) => {
                match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        let _ = session.close().await;
                        return Err(anyhow::anyhow!(
                            "WHEP request failed: {}",
                            error.without_url()
                        ));
                    }
                    Err(_) => {
                        let _ = session.close().await;
                        anyhow::bail!(
                            "WHEP endpoint did not respond within {}s",
                            WHEP_REQUEST_START_TIMEOUT.as_secs()
                        );
                    }
                }
            }
        };

        if response.status() != reqwest::StatusCode::CREATED {
            let status = response.status();
            let _ = session.close().await;
            anyhow::bail!("WHEP endpoint returned {status}; expected 201 Created");
        }
        if !response_has_sdp_content_type(&response) {
            let _ = session.close().await;
            anyhow::bail!("WHEP endpoint response Content-Type must be application/sdp");
        }
        let session_url = match resolve_whep_session_url(&self.source_url, &response) {
            Ok(url) => url,
            Err(error) => {
                let _ = session.close().await;
                return Err(error);
            }
        };

        let stream_result = async {
            let answer = read_limited_whep_answer(
                response,
                self.webrtc_config.max_sdp_bytes,
                &self.cancel_token,
            )
            .await?;
            session
                .apply_answer(&answer)
                .await
                .map_err(|error| anyhow::anyhow!("Invalid WHEP answer: {error}"))?;
            session
                .wait_connected(WHEP_CONNECTION_TIMEOUT)
                .await
                .map_err(|error| anyhow::anyhow!("WHEP media transport failed: {error}"))?;
            self.send_confirm_ok();

            tokio::select! {
                () = self.cancel_token.cancelled() => Ok(()),
                () = session.wait_closed() => Err(ReconnectRequired("WHEP").into()),
            }
        }
        .await;

        delete_whep_resource(&client, &session_url, authorization.as_ref()).await;
        if let Err(error) = session.close().await {
            debug!(%error, "failed to close local WHEP peer connection");
        }
        stream_result
    }

    /// Connect to remote HTTP-FLV source and stream frames to local `StreamHub`.
    ///
    /// Performs HTTP GET on the FLV URL, reads the response body in chunks, and
    /// parses FLV tags in a streaming fashion:
    /// 1. FLV header (9 bytes) + `PreviousTagSize0` (4 bytes)
    /// 2. Repeating: tag header (11 bytes) + tag data + `PreviousTagSize` (4 bytes)
    ///
    /// Each parsed tag is converted to a `FrameData` and sent through `data_sender`.
    async fn connect_and_stream_flv(&mut self, data_sender: &FrameDataSender) -> Result<()> {
        info!(
            source_url = %redact_source_url_for_logs(&self.source_url),
            "Connecting to HTTP-FLV source"
        );

        // REQUIRE pinned resolved address - prevents DNS rebinding attacks.
        // This field must be set via new_async(); if using new(), this will fail.
        let addr = self.resolved_addr.ok_or_else(|| {
            anyhow::anyhow!(
                "Resolved address not available. SSRF validation may have failed. \
                 Use new_async() instead of new() for proper DNS pinning."
            )
        })?;

        let client =
            build_pinned_http_client(&self.source_url, addr, &self.ssrf_guard, "HTTP-FLV")?;

        let mut response =
            send_http_flv_request(&client, &self.source_url, HTTP_FLV_REQUEST_START_TIMEOUT)
                .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        let mut buffer = BytesMut::new();
        let mut header_parsed = false;
        // Read response body in chunks and parse FLV tags.
        // Use per-chunk timeout instead of total request timeout so live streams
        // can run indefinitely as long as data keeps flowing.
        loop {
            // Race chunk read against cancellation so shutdown is responsive
            let chunk = tokio::select! {
                () = self.cancel_token.cancelled() => {
                    info!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        "HTTP-FLV stream puller cancelled"
                    );
                    return Ok(());
                }
                result = tokio::time::timeout(HTTP_FLV_CHUNK_READ_TIMEOUT, response.chunk()) => {
                    match result {
                        Ok(Ok(Some(c))) => c,
                        Ok(Ok(None)) => {
                            if !header_parsed {
                                return Err(anyhow::anyhow!(
                                    "HTTP-FLV source closed before sending a complete FLV header"
                                ));
                            }
                            if !buffer.is_empty() {
                                return Err(anyhow::anyhow!(
                                    "HTTP-FLV source closed with an incomplete FLV tag buffered"
                                ));
                            }
                            return Err(ReconnectRequired("HTTP-FLV").into());
                        }
                        Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to read HTTP chunk: {e}")),
                        Err(_) => return Err(anyhow::anyhow!("No data received for {}s, stream appears dead", HTTP_FLV_CHUNK_READ_TIMEOUT.as_secs())),
                    }
                }
            };

            if buffer.len() + chunk.len() > MAX_FLV_BUFFER_SIZE {
                return Err(anyhow::anyhow!(
                    "FLV buffer exceeded {} MB limit — likely a slow consumer or malformed stream",
                    MAX_FLV_BUFFER_SIZE / (1024 * 1024)
                ));
            }
            buffer.extend_from_slice(&chunk);

            // Parse FLV header on first data arrival
            if !header_parsed {
                if buffer.len() < FLV_HEADER_SIZE + FLV_PREV_TAG_SIZE_LEN {
                    continue;
                }

                // Validate FLV signature ("FLV")
                if &buffer[0..3] != b"FLV" {
                    return Err(anyhow::anyhow!(
                        "Invalid FLV header: expected 'FLV' signature, got {:?}",
                        &buffer[0..3]
                    ));
                }

                debug!(
                    version = buffer[3],
                    has_audio = (buffer[4] & 0x04) != 0,
                    has_video = (buffer[4] & 0x01) != 0,
                    "FLV header parsed"
                );

                // Skip FLV header (9 bytes) + PreviousTagSize0 (4 bytes)
                buffer.advance(FLV_HEADER_SIZE + FLV_PREV_TAG_SIZE_LEN);
                header_parsed = true;
                self.send_confirm_ok();
            }

            // Parse as many complete tags as possible from the buffer
            loop {
                if buffer.len() < FLV_TAG_HEADER_SIZE {
                    break; // Need more data for tag header
                }

                // Peek at tag header to determine total size needed
                //   [0]     = TagType (8=audio, 9=video, 18=script)
                //   [1..4]  = DataSize (24-bit big-endian)
                //   [4..7]  = Timestamp lower 24 bits (big-endian)
                //   [7]     = TimestampExtended (upper 8 bits)
                //   [8..11] = StreamID (always 0)
                let tag_type = buffer[0];
                let data_size = ((buffer[1] as usize) << 16)
                    | ((buffer[2] as usize) << 8)
                    | (buffer[3] as usize);

                // Reject unreasonably large tags to prevent OOM.
                let max_flv_tag_size = self.max_flv_tag_size_bytes;
                if data_size > max_flv_tag_size {
                    anyhow::bail!(
                        "FLV tag data_size too large: {data_size} bytes (max {max_flv_tag_size}), likely corrupted stream"
                    );
                }

                let total_tag_size = FLV_TAG_HEADER_SIZE + data_size + FLV_PREV_TAG_SIZE_LEN;
                if buffer.len() < total_tag_size {
                    break; // Need more data for tag body + PreviousTagSize
                }

                // Parse timestamp: [7] is upper 8 bits, [4..7] is lower 24 bits
                let timestamp = (u32::from(buffer[7]) << 24)
                    | (u32::from(buffer[4]) << 16)
                    | (u32::from(buffer[5]) << 8)
                    | u32::from(buffer[6]);

                // Zero-copy extraction: skip tag header, split out data, freeze to Bytes.
                // This avoids a memcpy compared to the old copy_from_slice approach.
                drop(buffer.split_to(FLV_TAG_HEADER_SIZE)); // discard tag header
                let tag_data = buffer.split_to(data_size).freeze(); // zero-copy Bytes
                buffer.advance(FLV_PREV_TAG_SIZE_LEN); // skip PreviousTagSize

                // Convert to FrameData based on tag type and send to StreamHub
                let frame = match tag_type {
                    FLV_TAG_VIDEO => FrameData::Video {
                        timestamp,
                        data: tag_data,
                    },
                    FLV_TAG_AUDIO => FrameData::Audio {
                        timestamp,
                        data: tag_data,
                    },
                    FLV_TAG_SCRIPT_DATA => FrameData::MetaData {
                        timestamp,
                        data: tag_data,
                    },
                    _ => {
                        debug!("Skipping unknown FLV tag type: {tag_type}");
                        continue;
                    }
                };

                if let Err(error) = send_frame_with_backpressure(data_sender, frame).await {
                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        %error,
                        "HTTP-FLV frame delivery failed"
                    );
                    return Err(error);
                }
            }
        }
    }

    /// Send the one-shot confirmation if still pending.
    fn send_confirm_ok(&mut self) {
        if let Some(tx) = self.confirm_tx.take() {
            notify_oneshot(tx, Ok(()), "external pull startup success");
        }
    }

    /// Exponential backoff with jitter (delegated to shared utility).
    async fn backoff(attempt: u32) {
        crate::util::backoff(attempt, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS).await;
    }

    /// Publish to local `StreamHub` under `live/{room_id}/{media_id}`.
    ///
    /// Sends a `StreamHubEvent::Publish` to register this stream in the local `StreamHub`,
    /// then receives back a `FrameDataSender` that can be used to push frames into the stream.
    async fn publish_to_local_stream_hub(&mut self) -> Result<LocalPublication> {
        let generation_id = self.generation_id;
        let pub_data_type = if matches!(self.source_type, ExternalSourceType::Whep) {
            PubDataType::Both
        } else {
            PubDataType::Frame
        };
        let request_url = if matches!(self.source_type, ExternalSourceType::Whep) {
            "external://whep".to_string()
        } else {
            format!(
                "external://{}",
                redact_source_url_for_logs(&self.source_url)
            )
        };

        let publisher_info = PublisherInfo {
            id: generation_id,
            pub_type: PublishType::ExternalPull,
            pub_data_type,
            notify_info: NotifyInfo {
                request_url,
                remote_addr: String::new(),
            },
        };

        // Use canonical (room_id, media_id) format matching RTMP publish identifier
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        let stream_handler = Arc::new(RtmpStreamHandler::new());

        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let publish_event = StreamHubEvent::Publish {
            identifier,
            info: publisher_info,
            stream_handler,
            result_sender: event_result_sender,
        };

        send_event_with_backpressure_timeout_for(
            &self.stream_hub_event_sender,
            publish_event,
            STREAMHUB_EVENT_SEND_TIMEOUT,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Failed to send publish event to StreamHub: {error}"))?;

        let result = event_result_receiver
            .await
            .map_err(|_| anyhow::anyhow!("Publish result channel closed"))?
            .map_err(|e| {
                // If the stream already exists, treat it as non-fatal so the caller
                // can handle it.
                anyhow::anyhow!("Publish failed: {e}")
            })?;

        let data_sender = result
            .0
            .ok_or_else(|| anyhow::anyhow!("No data sender from publish result"))?;
        let packet_sender = result.1;
        if matches!(self.source_type, ExternalSourceType::Whep) && packet_sender.is_none() {
            anyhow::bail!("No packet sender from WHEP publish result");
        }

        info!("Successfully published external stream to local StreamHub");
        Ok(LocalPublication {
            generation_id,
            frame_sender: data_sender,
            packet_sender,
        })
    }

    /// Unpublish from local `StreamHub`.
    fn unpublish_from_local_stream_hub(&mut self, generation_id: Uuid) {
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        let unpublish_event = StreamHubEvent::UnPublish {
            identifier,
            generation_id,
        };

        spawn_event_delivery_with_backpressure_timeout_for(
            self.stream_hub_event_sender.clone(),
            unpublish_event,
            STREAMHUB_EVENT_SEND_TIMEOUT,
        );
    }
}

fn build_pinned_http_client(
    source_url: &str,
    resolved_addr: std::net::SocketAddr,
    ssrf_guard: &SsrfGuard,
    source_name: &'static str,
) -> Result<reqwest::Client> {
    let parsed = reqwest::Url::parse(source_url)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{source_name} source URL is missing a host"))?;

    let mut builder = synctv_common::http::SsrfSafeClientBuilder::new()
        .ssrf_guard(ssrf_guard.clone())
        .connect_timeout(std::time::Duration::from_secs(10))
        .disable_request_timeout()
        .disable_read_timeout()
        .pool_max_idle_per_host(100)
        .pool_idle_timeout(std::time::Duration::from_secs(30));

    if host.parse::<std::net::IpAddr>().is_err() {
        builder = builder.resolve(host.to_string(), resolved_addr);
    }

    builder
        .build()
        .map_err(|error| anyhow::anyhow!("Failed to create {source_name} HTTP client: {error}"))
}

#[cfg(test)]
fn build_http_flv_client(
    source_url: &str,
    resolved_addr: std::net::SocketAddr,
    ssrf_guard: &SsrfGuard,
) -> Result<reqwest::Client> {
    build_pinned_http_client(source_url, resolved_addr, ssrf_guard, "HTTP-FLV")
}

fn response_has_sdp_content_type(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/sdp"))
}

fn resolve_whep_session_url(source_url: &str, response: &reqwest::Response) -> Result<Url> {
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .ok_or_else(|| anyhow::anyhow!("WHEP endpoint response is missing Location"))?
        .to_str()
        .map_err(|_| anyhow::anyhow!("WHEP endpoint returned an invalid Location header"))?;
    resolve_whep_session_location(source_url, location)
}

fn resolve_whep_session_location(source_url: &str, location: &str) -> Result<Url> {
    let source = Url::parse(source_url)?;
    let session = source
        .join(location)
        .map_err(|_| anyhow::anyhow!("WHEP endpoint returned an invalid Location header"))?;
    let same_origin = source.scheme() == session.scheme()
        && source.host_str() == session.host_str()
        && source.port_or_known_default() == session.port_or_known_default();
    anyhow::ensure!(
        same_origin,
        "WHEP endpoint returned a cross-origin Location header"
    );
    anyhow::ensure!(
        matches!(session.scheme(), "http" | "https")
            && session.username().is_empty()
            && session.password().is_none()
            && session.fragment().is_none(),
        "WHEP endpoint returned an unsafe Location header"
    );
    Ok(session)
}

async fn read_limited_whep_answer(
    mut response: reqwest::Response,
    max_bytes: usize,
    cancel_token: &CancellationToken,
) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        anyhow::bail!("WHEP answer exceeds the configured SDP size limit");
    }
    let mut body = Vec::with_capacity(max_bytes.min(16 * 1024));
    loop {
        let chunk = tokio::select! {
            () = cancel_token.cancelled() => {
                anyhow::bail!("WHEP negotiation was cancelled");
            }
            result = tokio::time::timeout(WHEP_RESPONSE_READ_TIMEOUT, response.chunk()) => {
                match result {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(error)) => {
                        return Err(anyhow::anyhow!(
                            "Failed to read WHEP answer: {}",
                            error.without_url()
                        ));
                    }
                    Err(_) => anyhow::bail!(
                        "WHEP answer body stalled for {}s",
                        WHEP_RESPONSE_READ_TIMEOUT.as_secs()
                    ),
                }
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= max_bytes,
            "WHEP answer exceeds the configured SDP size limit"
        );
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| anyhow::anyhow!("WHEP answer is not valid UTF-8"))
}

async fn delete_whep_resource(
    client: &reqwest::Client,
    session_url: &Url,
    authorization: Option<&reqwest::header::HeaderValue>,
) {
    let mut request = client.delete(session_url.clone());
    if let Some(value) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, value.clone());
    }
    match tokio::time::timeout(WHEP_DELETE_TIMEOUT, request.send()).await {
        Ok(Ok(response))
            if response.status().is_success()
                || matches!(
                    response.status(),
                    reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE
                ) => {}
        Ok(Ok(response)) => {
            warn!(
                status = %response.status(),
                "WHEP resource deletion returned an unexpected status"
            );
        }
        Ok(Err(error)) => {
            warn!(
                error = %error.without_url(),
                "failed to delete remote WHEP resource"
            );
        }
        Err(_) => warn!("timed out deleting remote WHEP resource"),
    }
}

async fn send_http_flv_request(
    client: &reqwest::Client,
    source_url: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Response> {
    tokio::time::timeout(timeout, client.get(source_url).send())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "HTTP-FLV source did not respond within {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("HTTP request failed: {e}"))
}

/// Drop guard that sends UnPublish to StreamHub when dropped.
///
/// Ensures cleanup happens even if the puller task is aborted (not just cancelled).
struct UnpublishGuard {
    room_id: String,
    media_id: String,
    generation_id: Uuid,
    stream_hub_event_sender: StreamHubEventSender,
    /// Set to true when the puller has already sent UnPublish (e.g., during normal retry).
    disarmed: std::sync::atomic::AtomicBool,
}

impl UnpublishGuard {
    const fn new(
        room_id: String,
        media_id: String,
        generation_id: Uuid,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self {
            room_id,
            media_id,
            generation_id,
            stream_hub_event_sender,
            disarmed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn disarm(&self) {
        self.disarmed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for UnpublishGuard {
    fn drop(&mut self) {
        if self.disarmed.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        debug!(
            "UnpublishGuard: scheduling UnPublish for {}/{}",
            self.room_id, self.media_id
        );
        spawn_event_delivery_with_backpressure_timeout_for(
            self.stream_hub_event_sender.clone(),
            StreamHubEvent::UnPublish {
                identifier,
                generation_id: self.generation_id,
            },
            STREAMHUB_EVENT_SEND_TIMEOUT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use synctv_core_testing::RtmpPublisher;
    use synctv_xiu::{
        httpflv::HttpFlvSession,
        rtmp::server::RtmpServer,
        streamhub::{define::BroadcastEvent, StreamsHub},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn rtmp_source(url: &str) -> ExternalLiveSourceConfig {
        ExternalLiveSourceConfig::Rtmp {
            url: url.to_string(),
            mode: CoreRtmpStreamMode::Default,
        }
    }

    fn http_flv_source(url: &str) -> ExternalLiveSourceConfig {
        ExternalLiveSourceConfig::HttpFlv {
            url: url.to_string(),
        }
    }

    fn whep_source(url: &str) -> ExternalLiveSourceConfig {
        ExternalLiveSourceConfig::Whep {
            url: url.to_string(),
            authorization: Some("Bearer upstream-token".to_string()),
        }
    }

    #[test]
    fn test_source_type_detection() {
        assert!(matches!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::Rtmp {
                url: "rtmp://live.example.com/app/stream".to_string(),
                mode: CoreRtmpStreamMode::Default,
            }),
            Ok(ExternalSourceType::Rtmp)
        ));
        assert!(matches!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::Rtsp {
                url: "rtsp://camera.example.com/live".to_string(),
                transport: CoreRtspTransport::Tcp,
                video_track: CoreRtspTrackSelection::FirstCompatible,
                audio_track: CoreRtspTrackSelection::FirstCompatible,
            }),
            Ok(ExternalSourceType::Rtsp)
        ));
        assert!(matches!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::HttpFlv {
                url: "http://live.example.com/app/stream.flv".to_string(),
            }),
            Ok(ExternalSourceType::HttpFlv)
        ));
        assert!(matches!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::HttpFlv {
                url: "https://live.example.com/app/stream.flv?token=abc".to_string(),
            }),
            Ok(ExternalSourceType::HttpFlv)
        ));
        assert!(matches!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::Whep {
                url: "https://live.example.com/whep/stream".to_string(),
                authorization: Some("Bearer secret".to_string()),
            }),
            Ok(ExternalSourceType::Whep)
        ));
        assert!(!ExternalSourceType::Whep.can_reconnect_in_place());
        assert!(ExternalSourceType::Rtmp.can_reconnect_in_place());
        assert!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::HttpFlv {
                url: "https://live.example.com/app/stream/index.m3u8".to_string(),
            })
            .is_err()
        );
        assert!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::Rtsp {
                url: "rtmp://camera.example.com/live".to_string(),
                transport: CoreRtspTransport::Tcp,
                video_track: CoreRtspTrackSelection::FirstCompatible,
                audio_track: CoreRtspTrackSelection::Disabled,
            })
            .is_err()
        );
        assert!(
            ExternalSourceType::from_config(&ExternalLiveSourceConfig::Whep {
                url: "rtmp://live.example.com/app/stream".to_string(),
                authorization: None,
            })
            .is_err()
        );
        for url in [
            "https://user:password@live.example.com/whep",
            "https://live.example.com/whep#fragment",
        ] {
            assert!(
                ExternalSourceType::from_config(&ExternalLiveSourceConfig::Whep {
                    url: url.to_string(),
                    authorization: None,
                })
                .is_err(),
                "unsafe WHEP source URL was accepted: {url}"
            );
        }
    }

    #[test]
    fn whep_session_location_accepts_relative_same_origin_urls() -> TestResult {
        let session = resolve_whep_session_location(
            "https://media.example.com/live/whep",
            "/whep/sessions/session-1",
        )?;
        assert_eq!(
            session.as_str(),
            "https://media.example.com/whep/sessions/session-1"
        );
        Ok(())
    }

    #[test]
    fn whep_session_location_rejects_cross_origin_and_unsafe_urls() {
        for location in [
            "https://other.example.com/session-1",
            "https://user@media.example.com/session-1",
            "/session-1#fragment",
        ] {
            assert!(
                resolve_whep_session_location("https://media.example.com/whep", location).is_err(),
                "unsafe WHEP Location was accepted: {location}"
            );
        }
    }

    #[test]
    fn test_redact_source_url_for_logs_removes_sensitive_parts() {
        let redacted = redact_source_url_for_logs(
            "https://user:pass@live.example.com:8443/app/stream.flv?token=secret#frag",
        );
        assert_eq!(redacted, "https://live.example.com:8443/app/stream.flv");
        assert!(!redacted.contains("user"));
        assert!(!redacted.contains("pass"));
        assert!(!redacted.contains("token"));
        assert!(!redacted.contains("secret"));
    }

    /// Note: SSRF protection is enforced at the network level (DNS resolution time),
    /// not at source type detection time.
    /// Tests for SSRF protection during async creation are in test_external_puller_async_ssrf_protection.
    #[test]
    fn test_ssrf_urls_are_valid_format() {
        // URLs with private IPs are valid URL formats
        // SSRF protection happens at network level, not URL validation
        for url in [
            "rtmp://10.0.0.1/app/stream",
            "rtmp://192.168.1.1/app/stream",
            "rtmp://172.16.0.1/app/stream",
            "rtmp://localhost/app/stream",
        ] {
            assert!(
                ExternalSourceType::from_config(&ExternalLiveSourceConfig::Rtmp {
                    url: url.to_string(),
                    mode: CoreRtmpStreamMode::Default,
                })
                .is_ok()
            );
        }
    }

    fn spawn_stream_hub(
        mut receiver: tokio::sync::mpsc::Receiver<StreamHubEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let StreamHubEvent::Publish { result_sender, .. } = event {
                    let (data_sender, _) = tokio::sync::mpsc::channel(8);
                    notify_oneshot(
                        result_sender,
                        Ok((Some(FrameDataSender::bounded(data_sender)), None, None)),
                        "test stream hub publish result",
                    );
                }
            }
        })
    }

    struct TestRtmpSource {
        address: std::net::SocketAddr,
        shutdown: CancellationToken,
        server_task: tokio::task::JoinHandle<()>,
        event_relay_task: tokio::task::JoinHandle<()>,
        hub_task: tokio::task::JoinHandle<()>,
        lifecycle: tokio::sync::broadcast::Receiver<BroadcastEvent>,
        subscription_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    }

    impl TestRtmpSource {
        async fn start() -> TestResult<Self> {
            let (hub_event_sender, event_receiver) = tokio::sync::mpsc::channel(
                synctv_xiu::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY,
            );
            let mut hub = StreamsHub::new(hub_event_sender.clone(), event_receiver);
            let lifecycle = hub.get_client_event_consumer();
            let hub_task = tokio::spawn(async move {
                let _ = hub.run().await;
            });
            let (event_sender, mut server_event_receiver) = tokio::sync::mpsc::channel(
                synctv_xiu::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY,
            );
            let (subscription_tx, subscription_rx) = tokio::sync::mpsc::unbounded_channel();
            let event_relay_task = tokio::spawn(async move {
                while let Some(event) = server_event_receiver.recv().await {
                    if matches!(event, StreamHubEvent::Subscribe { .. }) {
                        let _ = subscription_tx.send(());
                    }
                    if hub_event_sender.send(event).await.is_err() {
                        break;
                    }
                }
            });
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let shutdown = CancellationToken::new();
            let mut server = RtmpServer::new(address.to_string(), event_sender, 2, None, None)
                .with_listener(listener)
                .with_cancellation_token(&shutdown)
                .with_shutdown_grace_period(StdDuration::from_millis(50));
            let server_task = tokio::spawn(async move {
                if let Err(error) = server.run().await {
                    tracing::error!(%error, "test RTMP source failed");
                }
            });
            Ok(Self {
                address,
                shutdown,
                server_task,
                event_relay_task,
                hub_task,
                lifecycle,
                subscription_rx,
            })
        }

        async fn next_lifecycle_event(&mut self) -> TestResult<BroadcastEvent> {
            tokio::time::timeout(StdDuration::from_secs(2), self.lifecycle.recv())
                .await
                .map_err(|_| test_error("test RTMP source lifecycle event timed out"))?
                .map_err(anyhow::Error::from)
        }

        async fn wait_for_subscription(&mut self) -> TestResult {
            tokio::time::timeout(StdDuration::from_secs(2), self.subscription_rx.recv())
                .await
                .map_err(|_| test_error("test RTMP source subscription timed out"))?
                .ok_or_else(|| test_error("test RTMP source subscription observer closed"))
        }

        async fn stop(self) -> TestResult {
            self.shutdown.cancel();
            tokio::time::timeout(StdDuration::from_secs(2), self.server_task)
                .await
                .map_err(|_| test_error("test RTMP source did not stop"))??;
            tokio::time::timeout(StdDuration::from_secs(2), self.event_relay_task)
                .await
                .map_err(|_| test_error("test RTMP source event relay did not stop"))??;
            self.hub_task.abort();
            let _ = self.hub_task.await;
            Ok(())
        }
    }

    fn make_test_rtmp_puller(
        address: std::net::SocketAddr,
        mode: RtmpStreamMode,
        sender: StreamHubEventSender,
        confirm_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
        cancel_token: CancellationToken,
    ) -> ExternalStreamPuller {
        ExternalStreamPuller {
            generation_id: Uuid::new(),
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            source_url: format!("rtmp://{address}/upstream/source"),
            source_type: ExternalSourceType::Rtmp,
            rtmp_mode: mode,
            rtsp_options: None,
            whep_authorization: None,
            webrtc_config: WebRtcConfig::default(),
            stream_hub_event_sender: sender,
            confirm_tx: Some(confirm_tx),
            http_client: None,
            resolved_addr: Some(address),
            cancel_token,
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
            ssrf_guard: SsrfGuard::disabled(),
        }
    }

    async fn start_flv_viewer(
        sender: StreamHubEventSender,
    ) -> TestResult<(
        tokio::sync::mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    )> {
        let (flv_tx, flv_rx) = tokio::sync::mpsc::channel(64);
        let mut session =
            HttpFlvSession::new("room".to_string(), "media".to_string(), sender, flv_tx);
        session.start().await?;
        let task = tokio::spawn(async move { session.run_after_start().await });
        Ok((flv_rx, task))
    }

    async fn read_http_request_headers(stream: &mut tokio::net::TcpStream) -> anyhow::Result<()> {
        let mut buffer = [0_u8; 1024];
        let mut received = Vec::new();

        loop {
            let read = tokio::time::timeout(StdDuration::from_secs(2), stream.read(&mut buffer))
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for HTTP request headers"))??;
            if read == 0 {
                break;
            }

            received.extend_from_slice(&buffer[..read]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }

            if received.len() >= 16 * 1024 {
                anyhow::bail!("HTTP request headers exceeded test server limit");
            }
        }

        Ok(())
    }

    async fn spawn_http_response_server(
        response: Vec<u8>,
    ) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                if let Err(error) = read_http_request_headers(&mut stream).await {
                    debug!(%error, "test HTTP response server failed to read request headers");
                }
                if let Err(error) = stream.write_all(&response).await {
                    debug!(%error, "test HTTP response server failed to write response");
                }
                if let Err(error) = stream.shutdown().await {
                    debug!(%error, "test HTTP response server failed to shut down stream");
                }
            }
        });
        Ok((addr, handle))
    }

    fn flv_video_response(marker: [u8; 3]) -> Vec<u8> {
        let payload = [
            0x17, 0x01, 0, 0, 0, 0, 0, 0, 3, marker[0], marker[1], marker[2],
        ];
        let mut body = vec![b'F', b'L', b'V', 0x01, 0x01, 0x00, 0x00, 0x00, 0x09];
        body.extend_from_slice(&[0, 0, 0, 0]);
        // Send enough frames for the downstream HTTP-FLV session to identify
        // a video-only stream while the local publication stays open across
        // upstream reconnects.
        for timestamp in 0_u32..12 {
            body.push(FLV_TAG_VIDEO);
            body.extend_from_slice(&[0, 0, u8::try_from(payload.len()).expect("test FLV payload")]);
            let timestamp = timestamp * 40;
            body.extend_from_slice(&[
                ((timestamp >> 16) & 0xff) as u8,
                ((timestamp >> 8) & 0xff) as u8,
                (timestamp & 0xff) as u8,
                ((timestamp >> 24) & 0xff) as u8,
            ]);
            body.extend_from_slice(&[0, 0, 0]);
            body.extend_from_slice(&payload);
            body.extend_from_slice(
                &u32::try_from(FLV_TAG_HEADER_SIZE + payload.len())
                    .expect("test FLV tag size")
                    .to_be_bytes(),
            );
        }
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: video/x-flv\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        response
    }

    async fn spawn_reconnecting_http_flv_source() -> TestResult<(
        std::net::SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<TestResult>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
        let (second_request_tx, second_request_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let responses = [
                flv_video_response([0x65, 0xf1, 0xf1]),
                flv_video_response([0x65, 0xf2, 0xf2]),
            ];
            let mut second_request_tx = Some(second_request_tx);
            let mut release_first_rx = Some(release_first_rx);
            for (index, response) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().await?;
                read_http_request_headers(&mut stream).await?;
                if index == 0 {
                    release_first_rx
                        .take()
                        .ok_or_else(|| test_error("first HTTP-FLV release used twice"))?
                        .await
                        .map_err(|_| test_error("first HTTP-FLV release sender dropped"))?;
                } else {
                    notify_oneshot(
                        second_request_tx
                            .take()
                            .ok_or_else(|| test_error("second HTTP-FLV request sent twice"))?,
                        (),
                        "second HTTP-FLV request",
                    );
                }
                stream.write_all(&response).await?;
                stream.shutdown().await?;
            }
            Ok(())
        });
        Ok((address, release_first_tx, second_request_rx, task))
    }

    async fn spawn_delayed_http_response_server(
        delay: StdDuration,
        response: Vec<u8>,
    ) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                if let Err(error) = read_http_request_headers(&mut stream).await {
                    debug!(%error, "delayed test HTTP response server failed to read request headers");
                }
                tokio::time::sleep(delay).await;
                if let Err(error) = stream.write_all(&response).await {
                    debug!(%error, "delayed test HTTP response server failed to write response");
                }
                if let Err(error) = stream.shutdown().await {
                    debug!(%error, "delayed test HTTP response server failed to shut down stream");
                }
            }
        });
        Ok((addr, handle))
    }

    async fn read_rtsp_request(stream: &mut tokio::net::TcpStream) -> anyhow::Result<String> {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            anyhow::ensure!(stream.read(&mut byte).await? == 1, "RTSP client closed");
            request.push(byte[0]);
            anyhow::ensure!(request.len() <= 16 * 1024, "RTSP request is too large");
        }
        Ok(String::from_utf8(request)?)
    }

    fn rtsp_cseq(request: &str) -> anyhow::Result<&str> {
        request
            .lines()
            .find_map(|line| line.strip_prefix("CSeq: "))
            .ok_or_else(|| anyhow::anyhow!("RTSP request is missing CSeq"))
    }

    async fn write_interleaved_h264(
        stream: &mut tokio::net::TcpStream,
        sequence: u16,
        timestamp: u32,
        nal: &[u8],
    ) -> anyhow::Result<()> {
        let mut rtp = Vec::with_capacity(12 + nal.len());
        rtp.extend_from_slice(&[0x80, 0xe0]);
        rtp.extend_from_slice(&sequence.to_be_bytes());
        rtp.extend_from_slice(&timestamp.to_be_bytes());
        rtp.extend_from_slice(&[1, 2, 3, 4]);
        rtp.extend_from_slice(nal);
        stream.write_all(&[b'$', 0]).await?;
        stream
            .write_all(&u16::try_from(rtp.len())?.to_be_bytes())
            .await?;
        stream.write_all(&rtp).await?;
        Ok(())
    }

    fn rtp_packet(
        payload_type: u8,
        marker: bool,
        sequence: u16,
        timestamp: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = Vec::with_capacity(12 + payload.len());
        packet.extend_from_slice(&[0x80, payload_type | if marker { 0x80 } else { 0 }]);
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&[1, 2, 3, 4]);
        packet.extend_from_slice(payload);
        packet
    }

    async fn spawn_scripted_rtsp_source(
        sdp: String,
        expected_track: &'static str,
        packets: Vec<Vec<u8>>,
        expected_basic_authorization: Option<&'static str>,
    ) -> anyhow::Result<(
        std::net::SocketAddr,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut describe = read_rtsp_request(&mut stream).await?;
            anyhow::ensure!(describe.starts_with("DESCRIBE "), "expected DESCRIBE");

            if let Some(expected) = expected_basic_authorization {
                anyhow::ensure!(
                    !describe.contains("Authorization:"),
                    "credentials should follow the server challenge"
                );
                let response = format!(
                    "RTSP/1.0 401 Unauthorized\r\nCSeq: {}\r\nWWW-Authenticate: Basic realm=\"SyncTV test\"\r\n\r\n",
                    rtsp_cseq(&describe)?
                );
                stream.write_all(response.as_bytes()).await?;
                describe = read_rtsp_request(&mut stream).await?;
                if !describe.contains(expected) {
                    let response = format!(
                        "RTSP/1.0 401 Unauthorized\r\nCSeq: {}\r\nWWW-Authenticate: Basic realm=\"SyncTV test\"\r\n\r\n",
                        rtsp_cseq(&describe)?
                    );
                    stream.write_all(response.as_bytes()).await?;
                    stream.shutdown().await?;
                    return Ok(());
                }
            }

            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
                rtsp_cseq(&describe)?,
                sdp.len()
            );
            stream.write_all(response.as_bytes()).await?;
            let setup = read_rtsp_request(&mut stream).await?;
            anyhow::ensure!(setup.contains(expected_track), "unexpected SETUP track");
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: scripted-test;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=01020304;mode=\"play\"\r\n\r\n",
                rtsp_cseq(&setup)?
            );
            stream.write_all(response.as_bytes()).await?;
            let play = read_rtsp_request(&mut stream).await?;
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: scripted-test\r\nRTP-Info: url=rtsp://{address}/live/{expected_track};seq=1;rtptime=0\r\n\r\n",
                rtsp_cseq(&play)?
            );
            stream.write_all(response.as_bytes()).await?;
            for packet in packets {
                stream.write_all(&[b'$', 0]).await?;
                stream
                    .write_all(&u16::try_from(packet.len())?.to_be_bytes())
                    .await?;
                stream.write_all(&packet).await?;
            }
            stream.shutdown().await?;
            Ok(())
        });
        Ok((address, task))
    }

    struct ReconnectingRtspSource {
        address: std::net::SocketAddr,
        release_first_media: tokio::sync::oneshot::Sender<()>,
        second_played: tokio::sync::oneshot::Receiver<()>,
        release_second_media: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    async fn spawn_reconnecting_rtsp_source() -> anyhow::Result<ReconnectingRtspSource> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
        let (release_second_tx, release_second_rx) = tokio::sync::oneshot::channel();
        let (second_played_tx, second_played_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let mut release_first_rx = Some(release_first_rx);
            let mut release_second_rx = Some(release_second_rx);
            let mut second_played_tx = Some(second_played_tx);
            for attempt in 1..=2 {
                let (mut stream, _) = listener.accept().await?;
                let describe = read_rtsp_request(&mut stream).await?;
                anyhow::ensure!(describe.starts_with("DESCRIBE "), "expected DESCRIBE");
                let sdp = concat!(
                    "v=0\r\n",
                    "o=- 1 1 IN IP4 127.0.0.1\r\n",
                    "s=SyncTV reconnect source\r\n",
                    "t=0 0\r\n",
                    "a=control:*\r\n",
                    "m=video 0 RTP/AVP 96\r\n",
                    "c=IN IP4 0.0.0.0\r\n",
                    "a=rtpmap:96 H264/90000\r\n",
                    "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
                    "a=control:trackID=1\r\n",
                );
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
                    rtsp_cseq(&describe)?,
                    sdp.len()
                );
                stream.write_all(response.as_bytes()).await?;

                let setup = read_rtsp_request(&mut stream).await?;
                anyhow::ensure!(setup.starts_with("SETUP "), "expected SETUP");
                let session = format!("reconnect-test-{attempt}");
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: {session};timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=01020304;mode=\"play\"\r\n\r\n",
                    rtsp_cseq(&setup)?
                );
                stream.write_all(response.as_bytes()).await?;

                let play = read_rtsp_request(&mut stream).await?;
                anyhow::ensure!(play.starts_with("PLAY "), "expected PLAY");
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: {session}\r\nRTP-Info: url=rtsp://{address}/live/trackID=1;seq=1;rtptime=0\r\n\r\n",
                    rtsp_cseq(&play)?
                );
                stream.write_all(response.as_bytes()).await?;

                if attempt == 1 {
                    release_first_rx
                        .take()
                        .ok_or_else(|| test_error("first RTSP media release already used"))?
                        .await
                        .map_err(|_| test_error("first RTSP media release sender dropped"))?;
                    for index in 0_u16..10 {
                        let marker = u8::try_from(index)?;
                        let nal = if index == 0 {
                            [0x65, 0xf1, 0xf1]
                        } else {
                            [0x41, 0x10, marker]
                        };
                        write_interleaved_h264(
                            &mut stream,
                            index + 1,
                            u32::from(index) * 3_000,
                            &nal,
                        )
                        .await?;
                    }
                    stream.shutdown().await?;
                    continue;
                }

                notify_oneshot(
                    second_played_tx
                        .take()
                        .ok_or_else(|| test_error("second PLAY notifier already used"))?,
                    (),
                    "second RTSP PLAY",
                );
                release_second_rx
                    .take()
                    .ok_or_else(|| test_error("second RTSP media release already used"))?
                    .await
                    .map_err(|_| test_error("second RTSP media release sender dropped"))?;
                write_interleaved_h264(&mut stream, 1, 0, &[0x65, 0xf2, 0xf2]).await?;

                let mut buffer = [0_u8; 256];
                while stream.read(&mut buffer).await? != 0 {}
            }
            Ok(())
        });
        Ok(ReconnectingRtspSource {
            address,
            release_first_media: release_first_tx,
            second_played: second_played_rx,
            release_second_media: release_second_tx,
            task: handle,
        })
    }

    async fn wait_for_flv_marker(
        receiver: &mut tokio::sync::mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>,
        marker: &[u8],
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + StdDuration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let chunk = tokio::time::timeout(remaining, receiver.recv())
                .await
                .map_err(|_| test_error("timed out waiting for HTTP-FLV marker"))?
                .ok_or_else(|| test_error("HTTP-FLV response channel closed"))??;
            if chunk.windows(marker.len()).any(|window| window == marker) {
                return Ok(());
            }
        }
    }

    fn make_test_http_puller(
        addr: std::net::SocketAddr,
        sender: StreamHubEventSender,
        confirm_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> ExternalStreamPuller {
        ExternalStreamPuller {
            generation_id: Uuid::new(),
            room_id: "room123".to_string(),
            media_id: "media456".to_string(),
            source_url: format!("http://{addr}/stream.flv"),
            source_type: ExternalSourceType::HttpFlv,
            rtmp_mode: RtmpStreamMode::Default,
            rtsp_options: None,
            whep_authorization: None,
            webrtc_config: WebRtcConfig::default(),
            stream_hub_event_sender: sender,
            confirm_tx: Some(confirm_tx),
            http_client: Some(reqwest::Client::new()),
            resolved_addr: Some(addr),
            cancel_token: CancellationToken::new(),
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
            ssrf_guard: SsrfGuard::disabled(),
        }
    }

    #[tokio::test]
    async fn test_http_flv_confirmation_waits_for_valid_flv_header() -> TestResult {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nbadflvheader!"
                .to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;
        let (hub_sender, hub_receiver) = tokio::sync::mpsc::channel(8);
        let hub_handle = spawn_stream_hub(hub_receiver);
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();

        let puller = make_test_http_puller(addr, hub_sender, confirm_tx);
        let result = puller.run().await;

        assert!(result.is_err(), "invalid FLV header must fail startup");
        let confirm = confirm_rx.await?;
        let err = confirm.expect_err("startup should not be confirmed for invalid FLV");
        assert!(
            err.contains("Invalid FLV header"),
            "unexpected confirmation error: {err}"
        );

        server_handle.abort();
        hub_handle.abort();
        Ok(())
    }

    async fn exercise_rtmp_pull_mode(
        mode: RtmpStreamMode,
        expect_audio: bool,
        expect_video: bool,
    ) -> TestResult {
        let mut source = TestRtmpSource::start().await?;
        let mut upstream = RtmpPublisher::connect(source.address, "upstream", "source").await?;
        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(64);
        let mut hub = StreamsHub::new(event_sender.clone(), event_receiver);
        let mut lifecycle = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move { hub.run().await });
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();
        let cancel_token = CancellationToken::new();
        let puller = make_test_rtmp_puller(
            source.address,
            mode,
            event_sender.clone(),
            confirm_tx,
            cancel_token.clone(),
        );
        let pull_task = tokio::spawn(puller.run());

        assert!(matches!(
            tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
                .await
                .map_err(|_| test_error("RTMP puller did not publish locally"))??,
            BroadcastEvent::Publish { .. }
        ));
        tokio::time::timeout(StdDuration::from_secs(2), confirm_rx)
            .await
            .map_err(|_| test_error("RTMP puller did not confirm upstream playback"))??
            .map_err(anyhow::Error::msg)?;
        source.wait_for_subscription().await?;
        let (mut flv_rx, flv_task) = start_flv_viewer(event_sender).await?;

        upstream.send_video(0, true).await?;
        upstream.send_audio(0).await?;
        upstream.send_video(1, true).await?;
        upstream.send_audio(1).await?;
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        cancel_token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), pull_task)
            .await
            .map_err(|_| test_error("RTMP mode puller did not stop"))???;
        tokio::time::timeout(StdDuration::from_secs(2), flv_task)
            .await
            .map_err(|_| test_error("RTMP mode FLV viewer did not stop"))???;

        let mut audio = false;
        let mut video = false;
        while let Some(chunk) = flv_rx.recv().await {
            match chunk?.first().copied() {
                Some(FLV_TAG_AUDIO) => audio = true,
                Some(FLV_TAG_VIDEO) => video = true,
                _ => {}
            }
        }
        assert_eq!(audio, expect_audio);
        assert_eq!(video, expect_video);

        upstream.close();
        source.stop().await?;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn external_rtmp_default_mode_forwards_audio_and_video() -> TestResult {
        exercise_rtmp_pull_mode(RtmpStreamMode::Default, true, true).await
    }

    #[tokio::test]
    async fn external_rtmp_video_only_mode_filters_audio() -> TestResult {
        exercise_rtmp_pull_mode(RtmpStreamMode::VideoOnly, false, true).await
    }

    #[tokio::test]
    async fn external_rtmp_audio_only_mode_filters_video() -> TestResult {
        exercise_rtmp_pull_mode(RtmpStreamMode::AudioOnly, true, false).await
    }

    #[tokio::test]
    async fn external_rtmp_reconnect_keeps_publication_and_two_flv_viewers() -> TestResult {
        const FIRST_MARKER: &[u8] = &[0x65, 0xf1, 0xf1];
        const SECOND_MARKER: &[u8] = &[0x65, 0xf2, 0xf2];

        let mut source = TestRtmpSource::start().await?;
        let mut first_upstream =
            RtmpPublisher::connect(source.address, "upstream", "source").await?;
        assert!(matches!(
            source.next_lifecycle_event().await?,
            BroadcastEvent::Publish { .. }
        ));
        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(64);
        let mut hub = StreamsHub::new(event_sender.clone(), event_receiver);
        let mut lifecycle = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move { hub.run().await });
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();
        let cancel_token = CancellationToken::new();
        let puller = make_test_rtmp_puller(
            source.address,
            RtmpStreamMode::Default,
            event_sender.clone(),
            confirm_tx,
            cancel_token.clone(),
        );
        let pull_task = tokio::spawn(puller.run());
        let published_id = match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("RTMP puller did not publish locally"))??
        {
            BroadcastEvent::Publish { generation_id, .. } => generation_id,
            BroadcastEvent::UnPublish { .. } => {
                return Err(test_error("RTMP puller unpublished during startup"));
            }
        };
        tokio::time::timeout(StdDuration::from_secs(2), confirm_rx)
            .await
            .map_err(|_| test_error("RTMP puller did not confirm upstream playback"))??
            .map_err(anyhow::Error::msg)?;
        source.wait_for_subscription().await?;

        let (mut first_flv_rx, first_flv_task) = start_flv_viewer(event_sender.clone()).await?;
        let (mut second_flv_rx, second_flv_task) = start_flv_viewer(event_sender.clone()).await?;
        first_upstream.send_video(0, true).await?;
        first_upstream.send_audio(0).await?;
        first_upstream
            .send_raw_video(1, &[0x17, 0x01, 0, 0, 0, 0, 0, 0, 3, 0x65, 0xf1, 0xf1])
            .await?;
        first_upstream.send_audio(1).await?;
        tokio::try_join!(
            wait_for_flv_marker(&mut first_flv_rx, FIRST_MARKER),
            wait_for_flv_marker(&mut second_flv_rx, FIRST_MARKER),
        )?;

        first_upstream.close();
        assert!(matches!(
            source.next_lifecycle_event().await?,
            BroadcastEvent::UnPublish { .. }
        ));
        let mut second_upstream =
            RtmpPublisher::connect(source.address, "upstream", "source").await?;
        assert!(matches!(
            source.next_lifecycle_event().await?,
            BroadcastEvent::Publish { .. }
        ));
        source.wait_for_subscription().await?;
        second_upstream.send_video(0, true).await?;
        second_upstream
            .send_raw_video(1, &[0x17, 0x01, 0, 0, 0, 0, 0, 0, 3, 0x65, 0xf2, 0xf2])
            .await?;

        tokio::try_join!(
            wait_for_flv_marker(&mut first_flv_rx, SECOND_MARKER),
            wait_for_flv_marker(&mut second_flv_rx, SECOND_MARKER),
        )?;
        assert!(!first_flv_task.is_finished());
        assert!(!second_flv_task.is_finished());
        assert!(
            tokio::time::timeout(StdDuration::from_millis(50), lifecycle.recv())
                .await
                .is_err(),
            "RTMP upstream reconnect changed the local publication"
        );

        cancel_token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), pull_task)
            .await
            .map_err(|_| test_error("RTMP reconnect puller did not stop"))???;
        match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("RTMP reconnect final UnPublish was not delivered"))??
        {
            BroadcastEvent::UnPublish { generation_id, .. } => {
                assert_eq!(generation_id, published_id);
            }
            BroadcastEvent::Publish { .. } => {
                return Err(test_error("RTMP reconnect created another publication"));
            }
        }
        tokio::time::timeout(StdDuration::from_secs(2), first_flv_task)
            .await
            .map_err(|_| test_error("first RTMP reconnect viewer did not stop"))???;
        tokio::time::timeout(StdDuration::from_secs(2), second_flv_task)
            .await
            .map_err(|_| test_error("second RTMP reconnect viewer did not stop"))???;

        second_upstream.close();
        source.stop().await?;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn external_http_flv_reconnect_keeps_publication_and_two_flv_viewers() -> TestResult {
        let (source_address, release_first_response, second_request, source_task) =
            spawn_reconnecting_http_flv_source().await?;
        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(64);
        let mut hub = StreamsHub::new(event_sender.clone(), event_receiver);
        let mut lifecycle = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move { hub.run().await });
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();
        let mut puller = make_test_http_puller(source_address, event_sender.clone(), confirm_tx);
        puller.room_id = "room".to_string();
        puller.media_id = "media".to_string();
        let cancel_token = puller.cancel_token.clone();
        let pull_task = tokio::spawn(puller.run());
        let published_id = match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("HTTP-FLV puller did not publish locally"))??
        {
            BroadcastEvent::Publish { generation_id, .. } => generation_id,
            BroadcastEvent::UnPublish { .. } => {
                return Err(test_error("HTTP-FLV puller unpublished during startup"));
            }
        };
        let (mut first_flv_rx, first_flv_task) = start_flv_viewer(event_sender.clone()).await?;
        let (mut second_flv_rx, second_flv_task) = start_flv_viewer(event_sender).await?;

        release_first_response
            .send(())
            .map_err(|()| test_error("HTTP-FLV source closed before first response release"))?;
        tokio::time::timeout(StdDuration::from_secs(2), confirm_rx)
            .await
            .map_err(|_| test_error("HTTP-FLV puller did not confirm first response"))??
            .map_err(anyhow::Error::msg)?;
        tokio::try_join!(
            wait_for_flv_marker(&mut first_flv_rx, &[0x65, 0xf1, 0xf1]),
            wait_for_flv_marker(&mut second_flv_rx, &[0x65, 0xf1, 0xf1]),
        )?;
        tokio::time::timeout(StdDuration::from_secs(3), second_request)
            .await
            .map_err(|_| test_error("HTTP-FLV puller did not reconnect"))
            .and_then(|result| result.map_err(anyhow::Error::from))?;
        tokio::try_join!(
            wait_for_flv_marker(&mut first_flv_rx, &[0x65, 0xf2, 0xf2]),
            wait_for_flv_marker(&mut second_flv_rx, &[0x65, 0xf2, 0xf2]),
        )?;
        assert!(!first_flv_task.is_finished());
        assert!(!second_flv_task.is_finished());
        assert!(
            tokio::time::timeout(StdDuration::from_millis(50), lifecycle.recv())
                .await
                .is_err(),
            "HTTP-FLV reconnect changed the local publication"
        );

        cancel_token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), pull_task)
            .await
            .map_err(|_| test_error("HTTP-FLV reconnect puller did not stop"))???;
        match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("HTTP-FLV final UnPublish was not delivered"))??
        {
            BroadcastEvent::UnPublish { generation_id, .. } => {
                assert_eq!(generation_id, published_id);
            }
            BroadcastEvent::Publish { .. } => {
                return Err(test_error("HTTP-FLV reconnect created another publication"));
            }
        }
        tokio::time::timeout(StdDuration::from_secs(2), first_flv_task)
            .await
            .map_err(|_| test_error("first HTTP-FLV reconnect viewer did not stop"))???;
        tokio::time::timeout(StdDuration::from_secs(2), second_flv_task)
            .await
            .map_err(|_| test_error("second HTTP-FLV reconnect viewer did not stop"))???;
        source_task.await??;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn rtsp_reconnects_after_clean_eof() -> TestResult {
        let source = spawn_reconnecting_rtsp_source().await?;
        let address = source.address;
        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(8);
        let mut hub = StreamsHub::new(event_sender.clone(), event_receiver);
        let mut lifecycle = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move { hub.run().await });
        let (confirm_tx, mut confirm_rx) = tokio::sync::oneshot::channel();
        let cancel_token = CancellationToken::new();
        let puller = ExternalStreamPuller {
            generation_id: Uuid::new(),
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            source_url: format!("rtsp://{address}/live"),
            source_type: ExternalSourceType::Rtsp,
            rtmp_mode: RtmpStreamMode::Default,
            rtsp_options: Some((
                RtspTransport::Tcp,
                RtspTrackSelection::FirstCompatible,
                RtspTrackSelection::Disabled,
            )),
            whep_authorization: None,
            webrtc_config: WebRtcConfig::default(),
            stream_hub_event_sender: event_sender.clone(),
            confirm_tx: Some(confirm_tx),
            http_client: None,
            resolved_addr: Some(address),
            cancel_token: cancel_token.clone(),
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
            ssrf_guard: SsrfGuard::disabled(),
        };
        let pull_task = tokio::spawn(puller.run());

        let published_id = match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("RTSP puller did not publish to StreamHub"))??
        {
            BroadcastEvent::Publish { generation_id, .. } => generation_id,
            BroadcastEvent::UnPublish { .. } => {
                return Err(test_error("RTSP stream unpublished before startup"));
            }
        };

        let (flv_tx_1, mut flv_rx_1) = tokio::sync::mpsc::channel(64);
        let mut flv_session_1 = HttpFlvSession::new(
            "room".to_string(),
            "media".to_string(),
            event_sender.clone(),
            flv_tx_1,
        );
        flv_session_1.start().await?;
        let flv_task_1 = tokio::spawn(async move { flv_session_1.run_after_start().await });

        let (flv_tx_2, mut flv_rx_2) = tokio::sync::mpsc::channel(64);
        let mut flv_session_2 = HttpFlvSession::new(
            "room".to_string(),
            "media".to_string(),
            event_sender,
            flv_tx_2,
        );
        flv_session_2.start().await?;
        let flv_task_2 = tokio::spawn(async move { flv_session_2.run_after_start().await });

        assert!(
            tokio::time::timeout(StdDuration::from_millis(50), &mut confirm_rx)
                .await
                .is_err(),
            "RTSP PLAY without media must keep startup pending"
        );
        source
            .release_first_media
            .send(())
            .map_err(|()| test_error("first RTSP connection closed before media release"))?;
        tokio::time::timeout(StdDuration::from_secs(2), &mut confirm_rx)
            .await
            .map_err(|_| test_error("RTSP first media frame did not confirm startup"))?
            .map_err(|_| test_error("RTSP confirmation sender dropped"))?
            .map_err(anyhow::Error::msg)?;
        tokio::try_join!(
            wait_for_flv_marker(&mut flv_rx_1, &[0x65, 0xf1, 0xf1]),
            wait_for_flv_marker(&mut flv_rx_2, &[0x65, 0xf1, 0xf1]),
        )?;

        tokio::time::timeout(StdDuration::from_secs(5), source.second_played)
            .await
            .map_err(|_| test_error("RTSP puller did not establish its second connection"))?
            .map_err(|_| test_error("second RTSP PLAY notifier dropped"))?;
        assert!(!flv_task_1.is_finished());
        assert!(!flv_task_2.is_finished());
        assert!(
            tokio::time::timeout(StdDuration::from_millis(50), lifecycle.recv())
                .await
                .is_err(),
            "upstream reconnect must keep the local StreamHub publication alive"
        );

        source
            .release_second_media
            .send(())
            .map_err(|()| test_error("second RTSP connection closed before media release"))?;
        tokio::try_join!(
            wait_for_flv_marker(&mut flv_rx_1, &[0x65, 0xf2, 0xf2]),
            wait_for_flv_marker(&mut flv_rx_2, &[0x65, 0xf2, 0xf2]),
        )?;

        cancel_token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), pull_task)
            .await
            .map_err(|_| test_error("RTSP puller did not stop after cancellation"))???;

        match tokio::time::timeout(StdDuration::from_secs(2), lifecycle.recv())
            .await
            .map_err(|_| test_error("final RTSP UnPublish was not delivered"))??
        {
            BroadcastEvent::UnPublish { generation_id, .. } => {
                assert_eq!(generation_id, published_id);
            }
            BroadcastEvent::Publish { .. } => {
                return Err(test_error(
                    "RTSP reconnect created a second local publication",
                ));
            }
        }

        tokio::time::timeout(StdDuration::from_secs(2), flv_task_1)
            .await
            .map_err(|_| test_error("first HTTP-FLV session did not close"))???;
        tokio::time::timeout(StdDuration::from_secs(2), flv_task_2)
            .await
            .map_err(|_| test_error("second HTTP-FLV session did not close"))???;
        source.task.await??;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn rtsp_audio_only_exact_index_disables_video() -> TestResult {
        let sdp = concat!(
            "v=0\r\n",
            "o=- 1 1 IN IP4 127.0.0.1\r\n",
            "s=SyncTV audio track test\r\n",
            "t=0 0\r\n",
            "a=control:*\r\n",
            "m=video 0 RTP/AVP 96\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtpmap:96 H264/90000\r\n",
            "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
            "a=control:trackID=1\r\n",
            "m=audio 0 RTP/AVP 97\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtpmap:97 mpeg4-generic/44100/2\r\n",
            "a=fmtp:97 streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1210\r\n",
            "a=control:trackID=2\r\n",
        );
        let aac_access_unit = [0x00, 0x10, 0x00, 0x18, 0x21, 0x10, 0x56];
        let (address, source_task) = spawn_scripted_rtsp_source(
            sdp.to_string(),
            "trackID=2",
            vec![rtp_packet(97, true, 1, 0, &aac_access_unit)],
            None,
        )
        .await?;
        let mut config = RtspPullConfig::from_url(&format!("rtsp://{address}/live"))?;
        config.video_track = RtspTrackSelection::Disabled;
        config.audio_track = RtspTrackSelection::Index(1);
        let mut session = RtspPullSession::connect(config).await?;
        assert_eq!(session.selected_tracks(), (None, Some(1)));

        let sequence = session
            .next_frame()
            .await?
            .ok_or_else(|| test_error("missing AAC sequence header"))?;
        let raw = session
            .next_frame()
            .await?
            .ok_or_else(|| test_error("missing AAC access unit"))?;
        assert!(matches!(
            sequence,
            FrameData::Audio { ref data, .. } if data.get(..2) == Some(&[0xaf, 0][..])
        ));
        assert!(matches!(
            raw,
            FrameData::Audio { ref data, .. }
                if data.get(..2) == Some(&[0xaf, 1][..]) && data.ends_with(&[0x21, 0x10, 0x56])
        ));
        source_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn rtsp_h264_fu_a_fragments_reassemble_into_one_flv_frame() -> TestResult {
        let sdp = concat!(
            "v=0\r\n",
            "o=- 1 1 IN IP4 127.0.0.1\r\n",
            "s=SyncTV FU-A test\r\n",
            "t=0 0\r\n",
            "a=control:*\r\n",
            "m=video 0 RTP/AVP 96\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtpmap:96 H264/90000\r\n",
            "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
            "a=control:trackID=1\r\n",
        );
        let packets = vec![
            rtp_packet(96, false, 1, 0, &[0x7c, 0x85, 0xaa, 0xbb]),
            rtp_packet(96, true, 2, 0, &[0x7c, 0x45, 0xcc, 0xdd]),
        ];
        let (address, source_task) =
            spawn_scripted_rtsp_source(sdp.to_string(), "trackID=1", packets, None).await?;
        let config = RtspPullConfig::from_url(&format!("rtsp://{address}/live"))?;
        let mut session = RtspPullSession::connect(config).await?;
        let _sequence = session
            .next_frame()
            .await?
            .ok_or_else(|| test_error("missing AVC sequence header"))?;
        let frame = session
            .next_frame()
            .await?
            .ok_or_else(|| test_error("missing reassembled AVC frame"))?;
        assert!(matches!(
            frame,
            FrameData::Video { ref data, .. }
                if data.windows(9).any(|window| window == [0, 0, 0, 5, 0x65, 0xaa, 0xbb, 0xcc, 0xdd])
        ));
        source_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn rtsp_basic_auth_accepts_valid_and_rejects_invalid_credentials() -> TestResult {
        let sdp = concat!(
            "v=0\r\n",
            "o=- 1 1 IN IP4 127.0.0.1\r\n",
            "s=SyncTV auth test\r\n",
            "t=0 0\r\n",
            "a=control:*\r\n",
            "m=video 0 RTP/AVP 96\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtpmap:96 H264/90000\r\n",
            "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
            "a=control:trackID=1\r\n",
        );
        let expected = "Authorization: Basic dXNlcjpwYXNz";
        let media = vec![rtp_packet(96, true, 1, 0, &[0x65, 0xa1, 0xa1])];
        let (valid_address, valid_source) =
            spawn_scripted_rtsp_source(sdp.to_string(), "trackID=1", media, Some(expected)).await?;
        let valid = RtspPullConfig::from_url(&format!("rtsp://user:pass@{valid_address}/live"))?;
        let mut session = RtspPullSession::connect(valid).await?;
        assert!(matches!(
            session.next_frame().await?,
            Some(FrameData::Video { .. })
        ));
        valid_source.await??;

        let (invalid_address, invalid_source) =
            spawn_scripted_rtsp_source(sdp.to_string(), "trackID=1", Vec::new(), Some(expected))
                .await?;
        let invalid =
            RtspPullConfig::from_url(&format!("rtsp://user:wrong@{invalid_address}/live"))?;
        let Err(error) = RtspPullSession::connect(invalid).await else {
            return Err(test_error("invalid RTSP Basic credentials were accepted"));
        };
        let error_chain = format!("{error:#}");
        assert!(error_chain.contains("401") || error_chain.contains("Unauthorized"));
        invalid_source.await??;
        Ok(())
    }

    #[tokio::test]
    async fn test_http_flv_truncated_header_fails_startup() -> TestResult {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nFLV".to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;
        let (hub_sender, hub_receiver) = tokio::sync::mpsc::channel(8);
        let hub_handle = spawn_stream_hub(hub_receiver);
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();

        let puller = make_test_http_puller(addr, hub_sender, confirm_tx);
        let result = puller.run().await;

        assert!(result.is_err(), "truncated FLV header must fail startup");
        let confirm = confirm_rx.await?;
        let err = confirm.expect_err("startup should not be confirmed for truncated header");
        assert!(
            err.contains("complete FLV header"),
            "unexpected confirmation error: {err}"
        );

        server_handle.abort();
        hub_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_http_flv_confirmation_succeeds_after_valid_header() -> TestResult {
        let mut body = vec![b'F', b'L', b'V', 0x01, 0x00, 0x00, 0x00, 0x00, 0x09];
        body.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        let mut response = response;
        response.extend_from_slice(&body);
        let (addr, server_handle) = spawn_http_response_server(response).await?;
        let (hub_sender, hub_receiver) = tokio::sync::mpsc::channel(8);
        let hub_handle = spawn_stream_hub(hub_receiver);
        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();

        let puller = make_test_http_puller(addr, hub_sender, confirm_tx);
        let cancel_token = puller.cancel_token.clone();
        let pull_task = tokio::spawn(puller.run());
        let confirm = confirm_rx.await?;
        assert!(
            confirm.is_ok(),
            "startup should be confirmed after FLV header"
        );
        cancel_token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), pull_task)
            .await
            .map_err(|_| test_error("valid HTTP-FLV puller did not stop after cancellation"))???;

        server_handle.abort();
        hub_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_http_flv_frame_send_waits_for_backpressure_instead_of_dropping() -> TestResult {
        let (data_sender, mut data_receiver) = tokio::sync::mpsc::channel(1);
        data_sender
            .try_send(FrameData::MetaData {
                timestamp: 0,
                data: bytes::Bytes::from_static(b"queued"),
            })
            .map_err(|_| test_error("test channel should start full"))?;

        let sender = FrameDataSender::bounded(data_sender);
        let send_task = tokio::spawn(async move {
            send_frame_with_backpressure(
                &sender,
                FrameData::Video {
                    timestamp: 1,
                    data: bytes::Bytes::from_static(b"video"),
                },
            )
            .await
        });

        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !send_task.is_finished(),
            "HTTP-FLV send should wait for bounded StreamHub backpressure"
        );

        data_receiver
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("data receiver closed before releasing backpressure"))?;
        send_task.await??;

        let received = data_receiver
            .recv()
            .await
            .ok_or_else(|| test_error("backpressured frame should be delivered"))?;
        assert!(matches!(received, FrameData::Video { timestamp: 1, .. }));
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_creation_rtmp() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://93.184.216.34/app/stream"),
            sender,
            SsrfGuard::strict_policy(),
        )
        .await?;

        assert_eq!(puller.room_id, "room123");
        assert_eq!(puller.media_id, "media456");
        assert!(matches!(puller.source_type, ExternalSourceType::Rtmp));
        assert_eq!(
            puller.resolved_addr,
            Some(std::net::SocketAddr::from(([93, 184, 216, 34], 1935)))
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_creation_flv() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            http_flv_source("http://93.184.216.34/app/stream.flv"),
            sender,
            SsrfGuard::strict_policy(),
        )
        .await?;

        assert!(matches!(puller.source_type, ExternalSourceType::HttpFlv));
        assert_eq!(
            puller.resolved_addr,
            Some(std::net::SocketAddr::from(([93, 184, 216, 34], 80)))
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_creation_whep_pins_dns_and_keeps_authorization() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let resolved = std::net::SocketAddr::from(([93, 184, 216, 34], 443));
        let puller = ExternalStreamPuller::new_async_with_resolver(
            "room123".to_string(),
            "media456".to_string(),
            whep_source("https://media.example.com/whep/channel"),
            sender,
            SsrfGuard::strict_policy(),
            move |host, port| async move {
                assert_eq!(host, "media.example.com");
                assert_eq!(port, 443);
                Ok(vec![resolved])
            },
        )
        .await?;

        assert!(matches!(puller.source_type, ExternalSourceType::Whep));
        assert_eq!(puller.resolved_addr, Some(resolved));
        assert_eq!(
            puller.whep_authorization.as_deref(),
            Some("Bearer upstream-token")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_whep_response_helpers_accept_standard_response() -> TestResult {
        let body = "v=0\r\n";
        let response = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/sdp; charset=utf-8\r\nLocation: /whep/sessions/session-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let (addr, server_handle) = spawn_http_response_server(response).await?;
        let source_url = format!("http://{addr}/whep/channel");
        let client = build_pinned_http_client(&source_url, addr, &SsrfGuard::disabled(), "WHEP")?;
        let response = client.post(&source_url).body("offer").send().await?;

        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        assert!(response_has_sdp_content_type(&response));
        assert_eq!(
            resolve_whep_session_url(&source_url, &response)?.as_str(),
            format!("http://{addr}/whep/sessions/session-1")
        );
        assert_eq!(
            read_limited_whep_answer(response, 32, &CancellationToken::new()).await?,
            body
        );
        server_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_whep_answer_content_length_enforces_sdp_limit() -> TestResult {
        let response = b"HTTP/1.1 201 Created\r\nContent-Type: application/sdp\r\nLocation: /session-1\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345".to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;
        let source_url = format!("http://{addr}/whep");
        let client = build_pinned_http_client(&source_url, addr, &SsrfGuard::disabled(), "WHEP")?;
        let response = client.post(&source_url).body("offer").send().await?;
        let error = read_limited_whep_answer(response, 4, &CancellationToken::new())
            .await
            .expect_err("oversized WHEP answer must be rejected");

        assert!(error.to_string().contains("SDP size limit"));
        server_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_invalid_url() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            http_flv_source("http://example.com/video.mp4"),
            sender,
            SsrfGuard::strict_policy(),
        )
        .await;

        assert!(puller.is_err());
    }

    #[tokio::test]
    async fn test_external_puller_m3u8_rejected() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            http_flv_source("https://live.example.com/app/stream/index.m3u8"),
            sender,
            SsrfGuard::strict_policy(),
        )
        .await;

        assert!(puller.is_err());
    }

    #[tokio::test]
    async fn test_external_puller_async_sets_resolved_addr() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let resolved = std::net::SocketAddr::from(([93, 184, 216, 34], 1935));

        let puller = ExternalStreamPuller::new_async_with_resolver(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://example.com/app/stream"),
            sender,
            SsrfGuard::strict_policy(),
            move |host, port| {
                let expected = resolved;
                async move {
                    assert_eq!(host, "example.com");
                    assert_eq!(port, 1935);
                    Ok(vec![expected])
                }
            },
        )
        .await?;

        assert!(
            puller.resolved_addr.is_some(),
            "resolved_addr should be set by new_async"
        );
        assert_eq!(puller.resolved_addr, Some(resolved));
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_allows_private_ip_for_allowed_hostname() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let resolved = std::net::SocketAddr::from(([10, 0, 0, 42], 1935));
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();

        let puller = ExternalStreamPuller::new_async_with_resolver(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://internal.example/app/stream"),
            sender,
            guard,
            move |host, port| {
                let expected = resolved;
                async move {
                    assert_eq!(host, "internal.example");
                    assert_eq!(port, 1935);
                    Ok(vec![expected])
                }
            },
        )
        .await?;

        assert_eq!(puller.resolved_addr, Some(resolved));
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_allows_custom_port_for_allowed_hostname() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let resolved = std::net::SocketAddr::from(([10, 0, 0, 42], 18000));
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();

        let puller = ExternalStreamPuller::new_async_with_resolver(
            "room123".to_string(),
            "media456".to_string(),
            http_flv_source("http://internal.example:18000/live.flv"),
            sender,
            guard,
            move |host, port| {
                let expected = resolved;
                async move {
                    assert_eq!(host, "internal.example");
                    assert_eq!(port, 18000);
                    Ok(vec![expected])
                }
            },
        )
        .await?;

        assert_eq!(puller.resolved_addr, Some(resolved));
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_blocks_metadata_ip_for_allowed_hostname() -> TestResult {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();

        let Err(error) = ExternalStreamPuller::new_async_with_resolver(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://internal.example/app/stream"),
            sender,
            guard,
            |_, _| async {
                Ok(vec![std::net::SocketAddr::from((
                    [169, 254, 169, 254],
                    1935,
                ))])
            },
        )
        .await
        else {
            return Err(test_error(
                "hostname allowlist must not allow metadata/link-local targets",
            ));
        };

        assert!(
            error.to_string().contains("all resolved IPs"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_build_http_flv_client_pins_hostname_to_resolved_address() -> TestResult {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;

        let client = build_http_flv_client(
            &format!("http://example.com:{}/stream.flv", addr.port()),
            addr,
            &SsrfGuard::disabled(),
        )?;

        let response = client
            .get(format!("http://example.com:{}/stream.flv", addr.port()))
            .send()
            .await?;

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_build_http_flv_client_keeps_redirects_disabled() -> TestResult {
        let response = b"HTTP/1.1 302 Found\r\nLocation: /next.flv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;

        let client = build_http_flv_client(
            &format!("http://example.com:{}/stream.flv", addr.port()),
            addr,
            &SsrfGuard::disabled(),
        )?;

        let response = client
            .get(format!("http://example.com:{}/stream.flv", addr.port()))
            .send()
            .await?;

        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_build_http_flv_client_keeps_redirects_disabled_for_ip_literals() -> TestResult {
        let response = b"HTTP/1.1 302 Found\r\nLocation: /next.flv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (addr, server_handle) = spawn_http_response_server(response).await?;

        let client = build_http_flv_client(
            &format!("http://{addr}/stream.flv"),
            addr,
            &SsrfGuard::disabled(),
        )?;

        let response = client
            .get(format!("http://{addr}/stream.flv"))
            .send()
            .await?;

        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server_handle.abort();
        Ok(())
    }

    #[test]
    fn test_build_http_flv_client_disables_inherited_reqwest_timeouts_for_live_streams(
    ) -> TestResult {
        let client = build_http_flv_client(
            "http://example.com:8080/stream.flv",
            std::net::SocketAddr::from(([203, 0, 113, 10], 8080)),
            &SsrfGuard::strict_policy(),
        )?;

        let request = client.get("http://example.com:8080/stream.flv").build()?;
        assert_eq!(
            request.timeout(),
            None,
            "live HTTP-FLV requests must not inherit a total request timeout"
        );

        let debug_repr = format!("{client:?}");
        assert!(
            !debug_repr.contains("read_timeout: Some(30s)"),
            "live HTTP-FLV client must not inherit the proxy preset read timeout: {debug_repr}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_send_http_flv_request_times_out_before_headers() -> TestResult {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (addr, server_handle) =
            spawn_delayed_http_response_server(StdDuration::from_millis(200), response).await?;

        let client = build_http_flv_client(
            &format!("http://{addr}/stream.flv"),
            addr,
            &SsrfGuard::disabled(),
        )?;

        let err = send_http_flv_request(
            &client,
            &format!("http://{addr}/stream.flv"),
            StdDuration::from_millis(50),
        )
        .await
        .expect_err("request should time out before response headers arrive");

        assert!(
            err.to_string().contains("did not respond within"),
            "unexpected error: {err}"
        );
        server_handle.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_external_puller_async_creation_allows_private_addresses_when_ssrf_is_explicitly_disabled(
    ) {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        // Localhost is allowed when the injected SSRF policy is disabled.
        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://localhost/app/stream"),
            sender.clone(),
            SsrfGuard::disabled(),
        )
        .await;
        assert!(
            puller.is_ok(),
            "localhost should be allowed when SSRF protection is explicitly disabled"
        );

        // Literal loopback IPs are also allowed by the disabled policy.
        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            http_flv_source("http://127.0.0.1/stream.flv"),
            sender.clone(),
            SsrfGuard::disabled(),
        )
        .await;
        assert!(
            puller.is_ok(),
            "127.0.0.1 should be allowed when SSRF protection is explicitly disabled"
        );

        // Private IPs are likewise allowed unless a strict SSRF policy is injected.
        let puller = ExternalStreamPuller::new_async(
            "room123".to_string(),
            "media456".to_string(),
            rtmp_source("rtmp://192.168.1.1/app/stream"),
            sender.clone(),
            SsrfGuard::disabled(),
        )
        .await;
        assert!(
            puller.is_ok(),
            "192.168.1.1 should be allowed when SSRF protection is explicitly disabled"
        );
    }
}
