//! External Stream Puller
//!
//! Pulls live streams from external RTMP, RTSP, or HTTP-FLV URLs and publishes them
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
//!
//! Both modes include bounded retry logic for established streams.

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
        FrameData, FrameDataSender, NotifyInfo, PublishType, PublisherInfo, StreamHubEvent,
        StreamHubEventSender,
    },
    send_event_with_backpressure_timeout_for, spawn_event_delivery_with_backpressure_timeout_for,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
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
/// Maximum time between decoded RTSP media frames before reconnecting.
const RTSP_FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const FRAME_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const STREAMHUB_EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
}

impl ExternalSourceType {
    fn from_config(config: &ExternalLiveSourceConfig) -> Result<Self> {
        let (source_type, expected_scheme) = match config {
            ExternalLiveSourceConfig::Rtmp { .. } => (Self::Rtmp, "rtmp"),
            ExternalLiveSourceConfig::Rtsp { .. } => (Self::Rtsp, "rtsp"),
            ExternalLiveSourceConfig::HttpFlv { .. } => (Self::HttpFlv, "http or https"),
        };
        let parsed = Url::parse(config.url())?;
        let valid = match source_type {
            Self::Rtmp => parsed.scheme() == "rtmp",
            Self::Rtsp => parsed.scheme() == "rtsp",
            Self::HttpFlv => {
                matches!(parsed.scheme(), "http" | "https") && parsed.path().ends_with(".flv")
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
    stream_hub_event_sender: StreamHubEventSender,
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

        // Resolve hostname and validate IPs against SSRF ACL.
        // For RTMP: the DNS resolver can't be injected, so we check IPs explicitly.
        // For HTTP-FLV: the reqwest DNS resolver handles it, but we still pin the
        // address to prevent DNS rebinding between validation and connection.
        let parsed = Url::parse(&source_url)?;
        let host = parsed.host_str().unwrap_or("");
        let default_port = match source_type {
            ExternalSourceType::Rtmp => 1935,
            ExternalSourceType::Rtsp => 554,
            ExternalSourceType::HttpFlv => {
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
            stream_hub_event_sender,
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

    /// Run the puller with bounded retry logic.
    ///
    /// Startup failures are reported to the caller immediately. After the first
    /// successful connection, interrupted streams retry with exponential backoff
    /// up to `MAX_RETRIES`.
    /// If the cancellation token is triggered, the loop exits cleanly and returns `Ok(())`.
    pub(crate) async fn run(mut self) -> Result<()> {
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            source_url = %redact_source_url_for_logs(&self.source_url),
            source_type = ?self.source_type,
            "Starting external stream puller"
        );

        let mut attempt: u32 = 0;

        loop {
            // Exit cleanly if cancellation has been requested before starting a new attempt
            if self.cancel_token.is_cancelled() {
                info!(
                    room_id = %self.room_id,
                    media_id = %self.media_id,
                    "External stream puller cancelled before attempt"
                );
                return Ok(());
            }

            attempt += 1;

            // Publish to local StreamHub (re-publish on each retry to get a fresh sender)
            let data_sender = match self.publish_to_local_stream_hub().await {
                Ok(sender) => sender,
                Err(e) => {
                    if let Some(tx) = self.confirm_tx.take() {
                        notify_oneshot(
                            tx,
                            Err(format!("{e}")),
                            "external pull startup publish failure",
                        );
                        error!(
                            room_id = %self.room_id,
                            "Failed to publish to local StreamHub: {e}"
                        );
                        return Err(e);
                    }

                    if attempt >= MAX_RETRIES {
                        error!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            attempt = attempt,
                            "Gave up after {MAX_RETRIES} retries publishing to local StreamHub: {e}"
                        );
                        return Err(anyhow::anyhow!(
                            "Gave up after {MAX_RETRIES} retries (last error: publish to StreamHub: {e})"
                        ));
                    }

                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        "External stream publish failed, retrying: {e}"
                    );

                    tokio::select! {
                        () = self.cancel_token.cancelled() => {
                            info!(
                                room_id = %self.room_id,
                                media_id = %self.media_id,
                                "External stream puller cancelled during retry backoff"
                            );
                            return Ok(());
                        }
                        () = Self::backoff(attempt) => {}
                    }
                    continue;
                }
            };

            // Create a Drop guard that ensures UnPublish is sent even if this
            // task is aborted (not just cancelled). The guard is disarmed before
            // the explicit unpublish call below to prevent double-send.
            let unpublish_guard = UnpublishGuard::new(
                self.room_id.clone(),
                self.media_id.clone(),
                self.stream_hub_event_sender.clone(),
            );

            let result = match self.source_type {
                ExternalSourceType::Rtmp => self.connect_and_stream_rtmp(&data_sender).await,
                ExternalSourceType::Rtsp => self.connect_and_stream_rtsp(&data_sender).await,
                ExternalSourceType::HttpFlv => self.connect_and_stream_flv(&data_sender).await,
            };
            let startup_confirmation_pending = self.confirm_tx.is_some();

            // If the connection failed on the first attempt and we have a pending
            // confirm_tx, signal the failure so the caller doesn't wait forever.
            if let Err(ref e) = result {
                if let Some(tx) = self.confirm_tx.take() {
                    notify_oneshot(tx, Err(format!("{e}")), "external pull startup failure");
                }
            }

            // Disarm the guard before explicit unpublish to prevent double-send
            unpublish_guard.disarm();
            drop(unpublish_guard);

            // Always clean up local StreamHub before retry or exit
            self.unpublish_from_local_stream_hub();

            // Exit cleanly if cancellation was triggered during streaming
            if self.cancel_token.is_cancelled() {
                info!(
                    room_id = %self.room_id,
                    media_id = %self.media_id,
                    "External stream puller cancelled after stream ended"
                );
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
                Err(e) => {
                    if startup_confirmation_pending {
                        error!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            "External stream startup failed before first confirmation: {e}"
                        );
                        return Err(e);
                    }

                    if attempt >= MAX_RETRIES {
                        error!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            attempt = attempt,
                            "Gave up after {MAX_RETRIES} retries: {e}"
                        );
                        return Err(anyhow::anyhow!(
                            "Gave up after {MAX_RETRIES} retries (last error: {e})"
                        ));
                    }

                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        "External stream pull failed, retrying: {e}"
                    );

                    // Respect cancellation during backoff
                    tokio::select! {
                        () = self.cancel_token.cancelled() => {
                            info!(
                                room_id = %self.room_id,
                                media_id = %self.media_id,
                                "External stream puller cancelled during retry backoff"
                            );
                            return Ok(());
                        }
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
        self.send_confirm_ok();

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
                info!(
                    room_id = %self.room_id,
                    media_id = %self.media_id,
                    "RTSP source ended"
                );
                return Ok(());
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
        }
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

        let client = build_http_flv_client(&self.source_url, addr, &self.ssrf_guard)?;

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
                            break; // Stream ended normally
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
        if !header_parsed {
            anyhow::bail!("HTTP-FLV stream ended before a complete FLV header was received");
        }
        if !buffer.is_empty() {
            anyhow::bail!(
                "HTTP-FLV stream ended with {} buffered bytes of an incomplete FLV tag",
                buffer.len()
            );
        }

        info!("HTTP-FLV stream ended");
        Ok(())
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
    async fn publish_to_local_stream_hub(&mut self) -> Result<FrameDataSender> {
        let publisher_id = Uuid::new();

        let publisher_info = PublisherInfo {
            id: publisher_id,
            pub_type: PublishType::ExternalPull,
            pub_data_type: synctv_xiu::streamhub::define::PubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!(
                    "external://{}",
                    redact_source_url_for_logs(&self.source_url)
                ),
                remote_addr: redact_source_url_for_logs(&self.source_url),
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

        info!("Successfully published external stream to local StreamHub");
        Ok(data_sender)
    }

    /// Unpublish from local `StreamHub`.
    fn unpublish_from_local_stream_hub(&mut self) {
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        let unpublish_event = StreamHubEvent::UnPublish { identifier };

        spawn_event_delivery_with_backpressure_timeout_for(
            self.stream_hub_event_sender.clone(),
            unpublish_event,
            STREAMHUB_EVENT_SEND_TIMEOUT,
        );
    }
}

fn build_http_flv_client(
    source_url: &str,
    resolved_addr: std::net::SocketAddr,
    ssrf_guard: &SsrfGuard,
) -> Result<reqwest::Client> {
    let parsed = reqwest::Url::parse(source_url)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("HTTP-FLV source URL is missing a host"))?;

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
        .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {e}"))
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
    stream_hub_event_sender: StreamHubEventSender,
    /// Set to true when the puller has already sent UnPublish (e.g., during normal retry).
    disarmed: std::sync::atomic::AtomicBool,
}

impl UnpublishGuard {
    const fn new(
        room_id: String,
        media_id: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self {
            room_id,
            media_id,
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
            StreamHubEvent::UnPublish { identifier },
            STREAMHUB_EVENT_SEND_TIMEOUT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type TestResult = anyhow::Result<()>;

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

    fn make_test_http_puller(
        addr: std::net::SocketAddr,
        sender: StreamHubEventSender,
        confirm_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> ExternalStreamPuller {
        ExternalStreamPuller {
            room_id: "room123".to_string(),
            media_id: "media456".to_string(),
            source_url: format!("http://{addr}/stream.flv"),
            source_type: ExternalSourceType::HttpFlv,
            rtmp_mode: RtmpStreamMode::Default,
            rtsp_options: None,
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
        let result = puller.run().await;

        assert!(result.is_ok(), "valid FLV header should allow startup");
        let confirm = confirm_rx.await?;
        assert!(
            confirm.is_ok(),
            "startup should be confirmed after FLV header"
        );

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
