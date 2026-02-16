//! External Stream Puller
//!
//! Pulls live streams from external RTMP or HTTP-FLV URLs and publishes them
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
//!
//! Both modes include retry logic with exponential backoff (matching `GrpcStreamPuller`).

use std::sync::Arc;

use anyhow::Result;
use bytes::{Buf, BytesMut};
use synctv_core::validation::SSRFValidator;
use synctv_xiu::rtmp::session::client_session::{ClientSession, ClientSessionType};
use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::rtmp::utils::RtmpUrlParser;
use synctv_xiu::streamhub::{
    define::{
        FrameData, FrameDataSender, NotifyInfo, PublishType, PublisherInfo, StreamHubEvent,
        StreamHubEventSender,
    },
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

const MAX_RETRIES: u32 = 10;
const INITIAL_BACKOFF_MS: u64 = 1000;
const MAX_BACKOFF_MS: u64 = 30_000;
/// Maximum total FLV buffer size (50 MB) to prevent unbounded memory growth
const MAX_FLV_BUFFER_SIZE: usize = 50 * 1024 * 1024;

// FLV format constants
const FLV_HEADER_SIZE: usize = 9;
const FLV_PREV_TAG_SIZE_LEN: usize = 4;
const FLV_TAG_HEADER_SIZE: usize = 11;
const FLV_TAG_AUDIO: u8 = 8;
const FLV_TAG_VIDEO: u8 = 9;
const FLV_TAG_SCRIPT_DATA: u8 = 18;

/// Source type for external streams
#[derive(Debug, Clone)]
pub enum ExternalSourceType {
    /// RTMP URL (e.g., <rtmp://live.example.com/app/stream>)
    Rtmp,
    /// HTTP-FLV URL (e.g., <http://live.example.com/app/stream.flv>)
    HttpFlv,
}

impl ExternalSourceType {
    /// Detect source type from URL
    #[must_use] 
    pub fn from_url(url: &str) -> Option<Self> {
        if url.starts_with("rtmp://") {
            Some(Self::Rtmp)
        } else if url.ends_with(".flv") || url.contains(".flv?") {
            Some(Self::HttpFlv)
        } else {
            None
        }
    }
}

/// External Stream Puller
///
/// Connects to a remote streaming source and publishes frames to the local
/// `StreamHub` under the local stream identity (`live/{room_id}/{media_id}`).
pub struct ExternalStreamPuller {
    room_id: String,
    media_id: String,
    source_url: String,
    source_type: ExternalSourceType,
    stream_hub_event_sender: StreamHubEventSender,
    /// Optional one-shot channel to signal that the first connection succeeded.
    /// Sent once after the first successful publish + connect, then set to `None`.
    confirm_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    /// Shared HTTP client for FLV connections (reused across retries, supports TLS via rustls).
    http_client: Option<reqwest::Client>,
}

impl ExternalStreamPuller {
    pub fn new(
        room_id: String,
        media_id: String,
        source_url: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Result<Self> {
        let source_type = ExternalSourceType::from_url(&source_url)
            .ok_or_else(|| anyhow::anyhow!(
                "Unsupported source URL format: {source_url}. Expected rtmp:// or *.flv"
            ))?;

        // SSRF validation: block private IPs, loopback, link-local, metadata endpoints
        SSRFValidator::new().validate_url(&source_url)
            .map_err(|e| anyhow::anyhow!("SSRF protection blocked URL: {e}"))?;

        Ok(Self {
            room_id,
            media_id,
            source_url,
            source_type,
            stream_hub_event_sender,
            confirm_tx: None,
            http_client: None,
        })
    }

    /// Create with async DNS-resolved SSRF validation (preferred for production use).
    /// Resolves the hostname and validates all resolved IPs against blocklists.
    pub async fn new_async(
        room_id: String,
        media_id: String,
        source_url: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Result<Self> {
        let source_type = ExternalSourceType::from_url(&source_url)
            .ok_or_else(|| anyhow::anyhow!(
                "Unsupported source URL format: {source_url}. Expected rtmp:// or *.flv"
            ))?;

        // Async SSRF validation: resolves hostname and checks all IPs
        SSRFValidator::new().validate_url_async(&source_url).await
            .map_err(|e| anyhow::anyhow!("SSRF protection blocked URL: {e}"))?;

        Ok(Self {
            room_id,
            media_id,
            source_url,
            source_type,
            stream_hub_event_sender,
            confirm_tx: None,
            http_client: None,
        })
    }

    /// Set a one-shot confirmation channel. The puller will signal this channel
    /// after the first successful connection is established (not just after URL validation).
    #[must_use]
    pub fn with_confirm(mut self, tx: tokio::sync::oneshot::Sender<Result<(), String>>) -> Self {
        self.confirm_tx = Some(tx);
        self
    }

    /// Set a shared HTTP client for FLV connections (reused across retries, supports TLS).
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Run the puller with retry logic.
    ///
    /// On transient failures (connection refused, stream interrupted), retries with exponential
    /// backoff (1s initial, 30s max, with jitter). Gives up after 10 consecutive failures.
    pub async fn run(mut self) -> Result<()> {
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            source_url = %self.source_url,
            source_type = ?self.source_type,
            "Starting external stream puller"
        );

        let mut attempt: u32 = 0;

        loop {
            attempt += 1;

            // Publish to local StreamHub (re-publish on each retry to get a fresh sender)
            let data_sender = match self.publish_to_local_stream_hub().await {
                Ok(sender) => sender,
                Err(e) => {
                    let err_msg = format!("{e}");
                    // M-8: If publish fails with "Exists" (stale entry from failed unpublish),
                    // force-unpublish first and retry immediately
                    if err_msg.contains("Exists") || err_msg.contains("exists") {
                        warn!(
                            room_id = %self.room_id,
                            "Stream already published (stale entry), force-unpublishing and retrying"
                        );
                        let _ = self.unpublish_from_local_stream_hub().await;
                        // Retry publish immediately (don't count as a separate attempt)
                        match self.publish_to_local_stream_hub().await {
                            Ok(sender) => sender,
                            Err(e2) => {
                                error!(
                                    room_id = %self.room_id,
                                    attempt = attempt,
                                    "Failed to publish after force-unpublish: {e2}"
                                );
                                if attempt > MAX_RETRIES {
                                    return Err(anyhow::anyhow!(
                                        "Gave up after {MAX_RETRIES} retries (last error: {e2})"
                                    ));
                                }
                                Self::backoff(attempt).await;
                                continue;
                            }
                        }
                    } else {
                        error!(
                            room_id = %self.room_id,
                            attempt = attempt,
                            "Failed to publish to local StreamHub: {e}"
                        );
                        if attempt > MAX_RETRIES {
                            return Err(anyhow::anyhow!(
                                "Gave up after {MAX_RETRIES} retries (last error: publish to StreamHub: {e})"
                            ));
                        }
                        Self::backoff(attempt).await;
                        continue;
                    }
                }
            };

            let connect_start = std::time::Instant::now();
            let result = match self.source_type {
                ExternalSourceType::Rtmp => self.connect_and_stream_rtmp(&data_sender).await,
                ExternalSourceType::HttpFlv => self.connect_and_stream_flv(&data_sender).await,
            };
            let stream_duration = connect_start.elapsed();

            // If the connection failed on the first attempt and we have a pending
            // confirm_tx, signal the failure so the caller doesn't wait forever.
            if result.is_err() {
                if let Some(tx) = self.confirm_tx.take() {
                    let _ = tx.send(Err(format!("{}", result.as_ref().unwrap_err())));
                }
            }

            // Always clean up local StreamHub before retry or exit
            if let Err(e) = self.unpublish_from_local_stream_hub().await {
                warn!("Failed to unpublish from local StreamHub: {e}");
            }

            /// Minimum connection duration to consider "successful" for retry reset
            const MIN_SUCCESSFUL_DURATION: std::time::Duration = std::time::Duration::from_secs(60);

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
                    // Reset retry counter if the connection was up for a meaningful duration
                    if stream_duration > MIN_SUCCESSFUL_DURATION {
                        info!(
                            room_id = %self.room_id,
                            duration_secs = stream_duration.as_secs(),
                            "Resetting retry counter after successful long connection"
                        );
                        attempt = 0;
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

                    Self::backoff(attempt).await;
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
        parser.parse_url()
            .map_err(|e| anyhow::anyhow!("Invalid RTMP URL: {e:?}"))?;

        // Ensure port is present (default RTMP port is 1935)
        let connect_addr = if parser.port.is_none() {
            format!("{}:1935", parser.host)
        } else {
            parser.host_with_port.clone()
        };

        info!(
            connect_addr = %connect_addr,
            app_name = %parser.app_name,
            stream_name = %parser.stream_name,
            "Connecting to remote RTMP server"
        );

        // Connect TCP to remote RTMP server with timeout
        const TCP_CONNECT_TIMEOUT_SECS: u64 = 10;
        let tcp_stream = tokio::time::timeout(
            std::time::Duration::from_secs(TCP_CONNECT_TIMEOUT_SECS),
            tokio::net::TcpStream::connect(&connect_addr)
        )
        .await
        .map_err(|_| anyhow::anyhow!("TCP connection to {connect_addr} timed out after {TCP_CONNECT_TIMEOUT_SECS}s"))?
        .map_err(|e| anyhow::anyhow!("Failed to connect to {connect_addr}: {e}"))?;

        // TCP connection established — signal confirmation
        self.send_confirm_ok();

        // Create bridge channel — ClientSession sends StreamHub events here
        // instead of the real StreamHub. We intercept and redirect.
        let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::channel::<StreamHubEvent>(64);

        // Clone data_sender for the bridge task
        let bridge_data_sender = data_sender.clone();

        // Spawn bridge task: intercepts ClientSession's Publish event and returns our data_sender.
        // When ClientSession receives "NetStream.Play.Start" from the remote, it calls
        // publish_to_stream_hub() which sends StreamHubEvent::Publish through bridge_tx.
        // The bridge responds with our FrameDataSender (from the real local StreamHub publish).
        // ClientSession then stores it as self.data_sender, so all subsequent on_video_data /
        // on_audio_data calls send frames through our sender into the correct local stream.
        let bridge_handle = tokio::spawn(async move {
            while let Some(event) = bridge_rx.recv().await {
                match event {
                    StreamHubEvent::Publish { result_sender, .. } => {
                        // Respond with our local StreamHub's FrameDataSender
                        let _ = result_sender.send(Ok((
                            Some(bridge_data_sender.clone()),
                            None, // No packet data sender needed
                            None, // No statistic data sender needed
                        )));
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
            ClientSessionType::Pull,
            parser.host_with_port.clone(),
            parser.app_name.clone(),
            parser.stream_name_with_query.clone(),
            bridge_tx,
            2, // gop_num (GOP cache on bridge side; real caching happens in local StreamHub)
        );

        let result = client.run().await;

        // Cleanup: abort bridge task and await to ensure it is fully cleaned up
        bridge_handle.abort();
        let _ = bridge_handle.await;

        result.map_err(|e| anyhow::anyhow!("RTMP client session error: {e}"))
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
            source_url = %self.source_url,
            "Connecting to HTTP-FLV source"
        );

        // Use the shared HTTP client if available, otherwise create one.
        // The shared client supports TLS (via rustls) and is reused across retries.
        let client = match &self.http_client {
            Some(c) => c.clone(),
            None => {
                let c = reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {e}"))?;
                self.http_client = Some(c.clone());
                c
            }
        };

        let mut response = client
            .get(&self.source_url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        // HTTP connection established and response OK — signal confirmation
        self.send_confirm_ok();

        let mut buffer = BytesMut::new();
        let mut header_parsed = false;
        let mut dropped_frames: u64 = 0;
        const DROP_LOG_INTERVAL: u64 = 100;
        /// Per-chunk read timeout: if no data arrives for 30s, the stream is dead.
        const CHUNK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        // Read response body in chunks and parse FLV tags.
        // Use per-chunk timeout instead of total request timeout so live streams
        // can run indefinitely as long as data keeps flowing.
        loop {
            let chunk = match tokio::time::timeout(CHUNK_READ_TIMEOUT, response.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break, // Stream ended normally
                Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to read HTTP chunk: {e}")),
                Err(_) => return Err(anyhow::anyhow!("No data received for {}s, stream appears dead", CHUNK_READ_TIMEOUT.as_secs())),
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

                // Reject unreasonably large tags to prevent OOM (max 10 MB)
                const MAX_FLV_TAG_SIZE: usize = 10 * 1024 * 1024;
                if data_size > MAX_FLV_TAG_SIZE {
                    anyhow::bail!(
                        "FLV tag data_size too large: {data_size} bytes (max {MAX_FLV_TAG_SIZE}), likely corrupted stream"
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
                let _ = buffer.split_to(FLV_TAG_HEADER_SIZE); // discard tag header
                let tag_data = buffer.split_to(data_size).freeze(); // zero-copy Bytes
                buffer.advance(FLV_PREV_TAG_SIZE_LEN); // skip PreviousTagSize

                // Convert to FrameData based on tag type and send to StreamHub
                let frame = match tag_type {
                    FLV_TAG_VIDEO => FrameData::Video { timestamp, data: tag_data },
                    FLV_TAG_AUDIO => FrameData::Audio { timestamp, data: tag_data },
                    FLV_TAG_SCRIPT_DATA => FrameData::MetaData { timestamp, data: tag_data },
                    _ => {
                        debug!("Skipping unknown FLV tag type: {tag_type}");
                        continue;
                    }
                };

                // Use try_send for non-blocking behavior
                // If channel is full, drop the packet (backpressure)
                if let Err(mpsc::error::TrySendError::Full(_)) = data_sender.try_send(frame) {
                    dropped_frames += 1;
                    if dropped_frames % DROP_LOG_INTERVAL == 1 {
                        warn!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            total_dropped = dropped_frames,
                            "FLV frame dropped due to backpressure"
                        );
                    }
                }
            }
        }
        info!("HTTP-FLV stream ended");
        Ok(())
    }

    /// Send the one-shot confirmation if still pending.
    fn send_confirm_ok(&mut self) {
        if let Some(tx) = self.confirm_tx.take() {
            let _ = tx.send(Ok(()));
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
            pub_type: PublishType::RtmpRelay,
            pub_data_type: synctv_xiu::streamhub::define::PubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!("external://{}", self.source_url),
                remote_addr: self.source_url.clone(),
            },
        };

        let stream_name = format!("{}/{}", self.room_id, self.media_id);
        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: stream_name.clone(),
        };

        let stream_handler = Arc::new(RtmpStreamHandler::new());

        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let publish_event = StreamHubEvent::Publish {
            identifier,
            info: publisher_info,
            stream_handler,
            result_sender: event_result_sender,
        };

        self.stream_hub_event_sender
            .try_send(publish_event)
            .map_err(|_| anyhow::anyhow!("Failed to send publish event"))?;

        let result = event_result_receiver
            .await
            .map_err(|_| anyhow::anyhow!("Publish result channel closed"))?
            .map_err(|e| {
                // M-8: If the stream already exists (e.g., unpublish failed on previous retry),
                // treat it as a non-fatal error so the caller can handle it
                anyhow::anyhow!("Publish failed: {e}")
            })?;

        let data_sender = result
            .0
            .ok_or_else(|| anyhow::anyhow!("No data sender from publish result"))?;

        info!("Successfully published external stream to local StreamHub");
        Ok(data_sender)
    }

    /// Unpublish from local `StreamHub`.
    async fn unpublish_from_local_stream_hub(&mut self) -> Result<()> {
        let stream_name = format!("{}/{}", self.room_id, self.media_id);
        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name,
        };

        let unpublish_event = StreamHubEvent::UnPublish { identifier };

        if let Err(e) = self.stream_hub_event_sender.try_send(unpublish_event) {
            warn!("Failed to send unpublish event: {}", e);
        }

        Ok(())
    }
}

/// Validate that a URL is a supported external source format and is SSRF-safe
pub fn validate_source_url(url: &str) -> Result<ExternalSourceType, String> {
    let source_type = ExternalSourceType::from_url(url)
        .ok_or_else(|| format!(
            "Unsupported source URL: {url}. Expected rtmp:// or *.flv"
        ))?;

    // SSRF validation: block private IPs, loopback, link-local, metadata endpoints
    SSRFValidator::new().validate_url(url)
        .map_err(|e| format!("SSRF protection blocked URL: {e}"))?;

    Ok(source_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_type_detection() {
        assert!(matches!(
            ExternalSourceType::from_url("rtmp://live.example.com/app/stream"),
            Some(ExternalSourceType::Rtmp)
        ));
        assert!(matches!(
            ExternalSourceType::from_url("http://live.example.com/app/stream.flv"),
            Some(ExternalSourceType::HttpFlv)
        ));
        assert!(matches!(
            ExternalSourceType::from_url("https://live.example.com/app/stream.flv?token=abc"),
            Some(ExternalSourceType::HttpFlv)
        ));
        // m3u8/HLS is not supported
        assert!(ExternalSourceType::from_url("https://live.example.com/app/stream/index.m3u8").is_none());
        assert!(ExternalSourceType::from_url("http://example.com/video.mp4").is_none());
    }

    #[test]
    fn test_validate_source_url() {
        assert!(validate_source_url("rtmp://live.example.com/app/stream").is_ok());
        assert!(validate_source_url("http://live.example.com/app/stream.flv").is_ok());
        // m3u8/HLS is not supported
        assert!(validate_source_url("https://live.example.com/app/stream/index.m3u8").is_err());
        assert!(validate_source_url("http://example.com/video.mp4").is_err());
        assert!(validate_source_url("not-a-url").is_err());
    }

    #[test]
    fn test_ssrf_blocked_urls() {
        // Private IPs should be blocked
        assert!(validate_source_url("rtmp://10.0.0.1/app/stream").is_err());
        assert!(validate_source_url("rtmp://192.168.1.1/app/stream").is_err());
        assert!(validate_source_url("rtmp://172.16.0.1/app/stream").is_err());
        assert!(validate_source_url("http://127.0.0.1/stream.flv").is_err());
        assert!(validate_source_url("http://169.254.169.254/stream.flv").is_err());
        assert!(validate_source_url("rtmp://localhost/app/stream").is_err());
    }

    #[tokio::test]
    async fn test_external_puller_creation_rtmp() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "rtmp://live.example.com/app/stream".to_string(),
            sender,
        );

        assert!(puller.is_ok());
        let puller = puller.unwrap();
        assert_eq!(puller.room_id, "room123");
        assert_eq!(puller.media_id, "media456");
        assert!(matches!(puller.source_type, ExternalSourceType::Rtmp));
    }

    #[tokio::test]
    async fn test_external_puller_creation_flv() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "http://live.example.com/app/stream.flv".to_string(),
            sender,
        );

        assert!(puller.is_ok());
        assert!(matches!(puller.unwrap().source_type, ExternalSourceType::HttpFlv));
    }

    #[tokio::test]
    async fn test_external_puller_invalid_url() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "http://example.com/video.mp4".to_string(),
            sender,
        );

        assert!(puller.is_err());
    }

    #[tokio::test]
    async fn test_external_puller_m3u8_rejected() {
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let puller = ExternalStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "https://live.example.com/app/stream/index.m3u8".to_string(),
            sender,
        );

        assert!(puller.is_err());
    }
}
