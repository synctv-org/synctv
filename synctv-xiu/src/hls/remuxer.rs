// Custom HLS remuxer using xiu's libraries but pluggable storage abstraction
//
// Architecture:
// - Uses xiu's FlvVideoTagDemuxer/FlvAudioTagDemuxer for FLV parsing
// - Uses xiu's TsMuxer for TS segment generation
// - Uses xiu-storage's HlsStorage trait for segment/playlist storage
// - Generates M3U8 dynamically in memory, no file writes

use crate::hls::segment_manager::SegmentManager;
use crate::storage::HlsStorage;
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::streamhub::{
    define::{
        BroadcastEvent, BroadcastEventReceiver, FrameData, FrameDataReceiver,
        NotifyInfo, StreamHubEvent, StreamHubEventSender, SubscribeType, SubscriberInfo,
    },
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::flv::{
    define::{frame_type, FlvData},
    demuxer::{FlvAudioTagDemuxer, FlvVideoTagDemuxer},
};
use crate::mpegts::{
    define::{epsi_stream_type, MPEG_FLAG_IDR_FRAME},
    ts::TsMuxer,
};

/// Segment metadata for M3U8 generation
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    /// Segment sequence number
    pub sequence: u64,
    /// Segment duration in milliseconds
    pub duration: i64,
    /// TS filename (nanoid, e.g., "a1b2c3d4e5f6")
    pub ts_name: String,
    /// Storage key (e.g., "`live/room_123/a1b2c3d4e5f6.ts`")
    pub storage_key: String,
    /// Whether this is a discontinuity point
    pub discontinuity: bool,
    /// Creation time (for cleanup)
    pub created_at: Instant,
}

/// Registry of active streams (for M3U8 generation)
pub type StreamRegistry = Arc<DashMap<String, Arc<parking_lot::RwLock<StreamProcessorState>>>>;

/// Stream processor state that can be accessed by HTTP server
pub struct StreamProcessorState {
    pub app_name: String,
    pub stream_name: String,
    pub segments: VecDeque<SegmentInfo>,
    pub is_ended: bool,
}

impl StreamProcessorState {
    /// Generate M3U8 content dynamically with custom TS URL generator
    ///
    /// # Arguments
    /// * `gen_ts_url` - Closure that takes TS name and returns full URL (can add auth tokens)
    ///
    /// # Example
    /// ```ignore
    /// let m3u8 = state.generate_m3u8(|ts_name| {
    ///     format!("/api/room/live/hls/data/{}/{}/{}?token={}", room_id, movie_id, ts_name, token)
    /// });
    /// ```
    pub fn generate_m3u8<F>(&self, mut gen_ts_url: F) -> String
    where
        F: FnMut(&str) -> String,
    {
        let mut m3u8_content = String::new();

        // Header
        m3u8_content.push_str("#EXTM3U\n");
        m3u8_content.push_str("#EXT-X-VERSION:3\n");

        // Target duration (max segment duration in seconds, rounded up)
        let max_duration_sec = self.segments
            .iter()
            .map(|s| (s.duration + 999) / 1000)
            .max()
            .unwrap_or(10);
        m3u8_content.push_str(&format!("#EXT-X-TARGETDURATION:{max_duration_sec}\n"));

        // Media sequence (first segment in playlist)
        let first_seq = self.segments.front().map_or(0, |s| s.sequence);
        m3u8_content.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{first_seq}\n"));

        // Segments
        for segment in &self.segments {
            if segment.discontinuity {
                m3u8_content.push_str("#EXT-X-DISCONTINUITY\n");
            }

            let duration_sec = segment.duration as f64 / 1000.0;
            m3u8_content.push_str(&format!("#EXTINF:{duration_sec:.3},\n"));

            // Use closure to generate segment URL (allows custom auth, CDN URLs, etc)
            let segment_url = gen_ts_url(&segment.ts_name);
            m3u8_content.push_str(&format!("{segment_url}\n"));
        }

        if self.is_ended {
            m3u8_content.push_str("#EXT-X-ENDLIST\n");
        }

        m3u8_content
    }
}

/// Custom HLS remuxer with storage abstraction
pub struct CustomHlsRemuxer {
    /// Event receiver from `StreamHub`
    client_event_consumer: BroadcastEventReceiver,
    /// Event sender to `StreamHub`
    event_producer: StreamHubEventSender,
    /// Segment manager with storage backend
    segment_manager: Arc<SegmentManager>,
    /// Stream registry for M3U8 generation
    stream_registry: StreamRegistry,
    /// Cancellation token for graceful shutdown
    cancel_token: CancellationToken,
    /// Tracked spawned stream handler tasks
    handler_tasks: tokio::task::JoinSet<()>,
}

impl CustomHlsRemuxer {
    #[must_use]
    pub fn new(
        consumer: BroadcastEventReceiver,
        event_producer: StreamHubEventSender,
        segment_manager: Arc<SegmentManager>,
        stream_registry: StreamRegistry,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            client_event_consumer: consumer,
            event_producer,
            segment_manager,
            stream_registry,
            cancel_token,
            handler_tasks: tokio::task::JoinSet::new(),
        }
    }

    pub async fn run(&mut self) -> Result<(), HlsRemuxerError> {
        tracing::info!("Custom HLS remuxer started");

        loop {
            let val = tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("HLS remuxer cancelled (shutdown), draining {} handler tasks", self.handler_tasks.len());
                    self.handler_tasks.abort_all();
                    while self.handler_tasks.join_next().await.is_some() {}
                    return Ok(());
                }
                // Reap completed handler tasks without blocking
                Some(result) = self.handler_tasks.join_next(), if !self.handler_tasks.is_empty() => {
                    if let Err(e) = result {
                        if !e.is_cancelled() {
                            tracing::error!("HLS stream handler task panicked: {}", e);
                        }
                    }
                    continue;
                }
                result = self.client_event_consumer.recv() => {
                    match result {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                "HLS remuxer lagged behind by {n} broadcast events; re-subscribing to avoid permanent stream loss"
                            );
                            // Re-subscribe to get a fresh receiver that is
                            // caught up with the broadcast tail. Without this,
                            // any Publish events in the skipped window would be
                            // permanently missed.
                            self.client_event_consumer = self.client_event_consumer.resubscribe();
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(HlsRemuxerError::ReceiveError(
                                tokio::sync::broadcast::error::RecvError::Closed,
                            ));
                        }
                    }
                }
            };
            match val {
                BroadcastEvent::Publish { identifier, pub_type } => {
                    // Only process locally published RTMP streams (RtmpPush).
                    // Skip relayed streams (RtmpRelay) — in cluster mode,
                    // only the publisher node generates HLS segments.
                    if pub_type == crate::streamhub::define::PublishType::RtmpRelay {
                        tracing::debug!("HLS remuxer: skipping relayed stream (RtmpRelay)");
                        continue;
                    }

                    if let StreamIdentifier::Rtmp {
                        app_name,
                        stream_name,
                    } = identifier
                    {
                        tracing::info!("HLS remuxer: new stream {}/{}", app_name, stream_name);

                        let stream_handler = StreamHandler::new(
                            app_name,
                            stream_name,
                            self.event_producer.clone(),
                            Arc::clone(&self.segment_manager),
                            self.stream_registry.clone(),
                        );

                        self.handler_tasks.spawn(async move {
                            if let Err(e) = stream_handler.run().await {
                                tracing::error!("HLS stream handler error: {}", e);
                            }
                        });
                    }
                }
                BroadcastEvent::UnPublish { .. } => {
                    tracing::trace!("HLS remuxer: stream unpublished");
                }
            }
        }
    }
}

/// Drop guard that sends UnSubscribe to StreamHub on drop.
/// Ensures cleanup even if the handler panics or returns early.
struct UnsubscribeGuard {
    event_producer: StreamHubEventSender,
    subscriber_id: Uuid,
    app_name: String,
    stream_name: String,
    active: bool,
}

impl Drop for UnsubscribeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2Hls,
            sub_data_type: crate::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };
        let event = StreamHubEvent::UnSubscribe {
            identifier,
            info: sub_info,
        };
        if let Err(e) = self.event_producer.try_send(event) {
            tracing::warn!(
                subscriber_id = %self.subscriber_id,
                "UnsubscribeGuard: failed to send unsubscribe on drop: {e}"
            );
        } else {
            tracing::info!(
                subscriber_id = %self.subscriber_id,
                "UnsubscribeGuard: sent unsubscribe on drop"
            );
        }
    }
}

/// Handler for a single HLS stream
struct StreamHandler {
    app_name: String,
    stream_name: String,
    event_producer: StreamHubEventSender,
    segment_manager: Arc<SegmentManager>,
    stream_registry: StreamRegistry,
    data_consumer: FrameDataReceiver,
    subscriber_id: Uuid,
}

impl StreamHandler {
    fn new(
        app_name: String,
        stream_name: String,
        event_producer: StreamHubEventSender,
        segment_manager: Arc<SegmentManager>,
        stream_registry: StreamRegistry,
    ) -> Self {
        let (_, data_consumer) = mpsc::channel(crate::streamhub::define::FRAME_DATA_CHANNEL_CAPACITY);
        let subscriber_id = Uuid::new();

        Self {
            app_name,
            stream_name,
            event_producer,
            segment_manager,
            stream_registry,
            data_consumer,
            subscriber_id,
        }
    }

    async fn run(mut self) -> Result<(), HlsRemuxerError> {
        // Subscribe to stream
        self.subscribe_from_stream_hub().await?;

        // Install drop guard to ensure unsubscribe on panic or early return
        let mut unsub_guard = UnsubscribeGuard {
            event_producer: self.event_producer.clone(),
            subscriber_id: self.subscriber_id,
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
            active: true,
        };

        // Create registry key
        let registry_key = format!("{}/{}", self.app_name, self.stream_name);

        // Register stream in registry
        let state = Arc::new(parking_lot::RwLock::new(StreamProcessorState {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
            segments: VecDeque::new(),
            is_ended: false,
        }));
        self.stream_registry.insert(registry_key.clone(), state.clone());

        // Process FLV data and generate HLS segments
        let mut processor = StreamProcessor::new(
            &self.app_name,
            &self.stream_name,
            Arc::clone(&self.segment_manager),
            state.clone(),
        )?;

        processor.process_stream(&mut self.data_consumer).await?;

        // Deactivate drop guard - we'll unsubscribe explicitly
        unsub_guard.active = false;

        // Unsubscribe when done
        self.unsubscribe_from_stream_hub().await?;

        // Remove from registry after some delay (allow clients to finish)
        tokio::time::sleep(tokio::time::Duration::from_mins(1)).await;
        self.stream_registry.remove(&registry_key);

        Ok(())
    }

    async fn subscribe_from_stream_hub(&mut self) -> Result<(), HlsRemuxerError> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2Hls,
            sub_data_type: crate::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };

        let (event_result_sender, event_result_receiver) = tokio::sync::oneshot::channel();

        let subscribe_event = StreamHubEvent::Subscribe {
            identifier,
            info: sub_info,
            result_sender: event_result_sender,
        };

        self.event_producer
            .try_send(subscribe_event)
            .map_err(|_| HlsRemuxerError::StreamHubEventSendError)?;

        let receiver = event_result_receiver
            .await
            .map_err(|_| HlsRemuxerError::SubscribeError)??
            .0
            .frame_receiver
            .ok_or(HlsRemuxerError::NoFrameReceiver)?;

        self.data_consumer = receiver;

        tracing::info!(
            "Subscribed to stream: {}/{}",
            self.app_name,
            self.stream_name
        );

        Ok(())
    }

    async fn unsubscribe_from_stream_hub(&mut self) -> Result<(), HlsRemuxerError> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2Hls,
            sub_data_type: crate::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };

        let unsubscribe_event = StreamHubEvent::UnSubscribe {
            identifier,
            info: sub_info,
        };

        if let Err(e) = self.event_producer.try_send(unsubscribe_event) {
            tracing::error!("Unsubscribe error: {}", e);
        }

        tracing::info!(
            "Unsubscribed from stream: {}/{}",
            self.app_name,
            self.stream_name
        );

        Ok(())
    }
}

/// Write data to storage with exponential backoff retry (via `backon` crate)
///
/// Retries transient storage failures (timeouts, connection errors) up to
/// 3 times with exponential backoff (100ms base, 2s max, with jitter).
async fn write_with_retry(
    storage: &Arc<dyn HlsStorage>,
    key: &str,
    data: Bytes,
) -> std::io::Result<()> {
    use backon::{ExponentialBuilder, Retryable};

    let storage = Arc::clone(storage);
    let key = key.to_owned();

    (|| {
        let storage = storage.clone();
        let key = key.clone();
        let data = data.clone();
        async move { storage.write(&key, data).await }
    })
    .retry(
        ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_secs(2))
            .with_max_times(3)
            .with_jitter(),
    )
    .when(is_transient_error)
    .notify(|e, dur| {
        tracing::warn!("HLS storage write failed: {e} - retrying in {dur:?}");
    })
    .await
}

/// Check if an I/O error is transient and worth retrying
fn is_transient_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    )
}

/// Processes FLV data and generates HLS segments
struct StreamProcessor {
    app_name: String,
    stream_name: String,
    segment_manager: Arc<SegmentManager>,
    state: Arc<parking_lot::RwLock<StreamProcessorState>>,

    // Demuxers
    video_demuxer: FlvVideoTagDemuxer,
    audio_demuxer: FlvAudioTagDemuxer,

    // TS muxer
    ts_muxer: TsMuxer,
    video_pid: u16,
    audio_pid: u16,

    // Segment tracking
    sequence_no: u64,
    max_segments: usize, // Keep last N segments in M3U8

    // Timing
    segment_duration_ms: i64, // Target segment duration (e.g., 10000ms = 10s)
    last_segment_dts: i64,
    last_dts: i64,
    last_pts: i64,
}

impl StreamProcessor {
    fn new(
        app_name: &str,
        stream_name: &str,
        segment_manager: Arc<SegmentManager>,
        state: Arc<parking_lot::RwLock<StreamProcessorState>>,
    ) -> Result<Self, HlsRemuxerError> {
        let mut ts_muxer = TsMuxer::new();
        let audio_pid = ts_muxer
            .add_stream(epsi_stream_type::PSI_STREAM_AAC, BytesMut::new())
            .map_err(|e| HlsRemuxerError::MuxError(format!("Failed to add audio stream: {e:?}")))?;
        let video_pid = ts_muxer
            .add_stream(epsi_stream_type::PSI_STREAM_H264, BytesMut::new())
            .map_err(|e| HlsRemuxerError::MuxError(format!("Failed to add video stream: {e:?}")))?;

        Ok(Self {
            app_name: app_name.to_string(),
            stream_name: stream_name.to_string(),
            segment_manager,
            state,
            video_demuxer: FlvVideoTagDemuxer::new(),
            audio_demuxer: FlvAudioTagDemuxer::new(),
            ts_muxer,
            video_pid,
            audio_pid,
            sequence_no: 0,
            max_segments: 6, // Keep last 6 segments
            segment_duration_ms: 10000, // 10 seconds
            last_segment_dts: 0,
            last_dts: 0,
            last_pts: 0,
        })
    }

    async fn process_stream(
        &mut self,
        data_consumer: &mut FrameDataReceiver,
    ) -> Result<(), HlsRemuxerError> {
        // Use a longer timeout for stream end detection
        // The original logic had a flaw: it would increment retry_count on any
        // recv() returning None, even during brief network pauses.
        // Now we use a timeout-based approach instead.
        const RECV_TIMEOUT_MS: u64 = 5000; // 5 seconds of no data = stream ended

        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(RECV_TIMEOUT_MS),
                data_consumer.recv()
            ).await {
                Ok(Some(frame_data)) => {
                    let flv_data = match frame_data {
                        FrameData::Audio { timestamp, data } => FlvData::Audio { timestamp, data: BytesMut::from(&data[..]) },
                        FrameData::Video { timestamp, data } => FlvData::Video { timestamp, data: BytesMut::from(&data[..]) },
                        _ => continue,
                    };
                    self.process_flv_data(flv_data).await?;
                }
                Ok(None) => {
                    // Channel closed - stream truly ended
                    tracing::info!("Stream channel closed: {}/{}", self.app_name, self.stream_name);
                    self.flush_remaining_segment().await?;
                    break;
                }
                Err(_timeout) => {
                    // Timeout - no data for 5 seconds, consider stream ended
                    tracing::info!("Stream timeout (no data for {}s): {}/{}",
                        RECV_TIMEOUT_MS / 1000, self.app_name, self.stream_name);
                    self.flush_remaining_segment().await?;
                    break;
                }
            }
        }

        Ok(())
    }

    async fn process_flv_data(&mut self, flv_data: FlvData) -> Result<(), HlsRemuxerError> {
        let (pid, pts, dts, flags, payload) = match flv_data {
            FlvData::Video { timestamp, data } => {
                let video_data = self
                    .video_demuxer
                    .demux(timestamp, data)
                    .map_err(|e| HlsRemuxerError::DemuxError(format!("Video demux error: {e:?}")))?;

                let video_data = match video_data {
                    Some(data) => data,
                    None => return Ok(()),
                };

                let mut flags = 0;
                let mut payload = BytesMut::new();
                payload.extend_from_slice(&video_data.data);

                // Check if keyframe and if we need new segment
                if video_data.frame_type == frame_type::KEY_FRAME {
                    flags = MPEG_FLAG_IDR_FRAME;

                    if video_data.dts - self.last_segment_dts >= self.segment_duration_ms {
                        self.finalize_segment(video_data.dts, false).await?;
                    }
                }

                self.last_dts = video_data.dts;
                self.last_pts = video_data.pts;

                (self.video_pid, video_data.pts, video_data.dts, flags, payload)
            }
            FlvData::Audio { timestamp, data } => {
                let audio_data = self
                    .audio_demuxer
                    .demux(timestamp, data)
                    .map_err(|e| HlsRemuxerError::DemuxError(format!("Audio demux error: {e:?}")))?;

                if !audio_data.has_data {
                    return Ok(());
                }

                let mut payload = BytesMut::new();
                payload.extend_from_slice(&audio_data.data);

                self.last_dts = audio_data.dts;
                self.last_pts = audio_data.pts;

                (self.audio_pid, audio_data.pts, audio_data.dts, 0, payload)
            }
            _ => return Ok(()),
        };

        // Write to TS muxer
        self.ts_muxer
            .write(pid, pts * 90, dts * 90, flags, payload)
            .map_err(|e| HlsRemuxerError::MuxError(format!("TS mux error: {e:?}")))?;

        Ok(())
    }

    async fn finalize_segment(&mut self, current_dts: i64, is_eof: bool) -> Result<(), HlsRemuxerError> {
        let ts_data = self.ts_muxer.get_data();
        let duration_ms = current_dts - self.last_segment_dts;
        let ts_data_len = ts_data.len();

        // Generate TS filename using nanoid (12 chars, like Go's SortUUID)
        let ts_name = nanoid::nanoid!(12);

        // Generate storage key: app_name-stream_name-ts_name
        // stream_name format is "room_id:media_id", replace : with - for flat key
        let storage_key = format!(
            "{}-{}-{}",
            self.app_name,
            self.stream_name.replace(':', "-"),
            ts_name
        );

        // Write segment to storage with retry
        let storage = self.segment_manager.storage().clone();
        let data: Bytes = ts_data.into();
        write_with_retry(&storage, &storage_key, data)
            .await
            .map_err(|e| {
                tracing::warn!(
                    "HLS segment write failed after retries: {} - {}",
                    storage_key,
                    e
                );
                HlsRemuxerError::StorageError(e.to_string())
            })?;

        tracing::debug!(
            "Wrote segment: {} ({}ms, {} bytes)",
            storage_key,
            duration_ms,
            ts_data_len
        );

        // Track segment metadata
        let segment_info = SegmentInfo {
            sequence: self.sequence_no,
            duration: duration_ms,
            ts_name,
            storage_key,
            discontinuity: false,
            created_at: Instant::now(),
        };

        // Update shared state with new segment
        {
            let mut state = self.state.write();
            state.segments.push_back(segment_info);

            // Remove old segments from list (but keep in storage for now, cleanup task will handle)
            if state.segments.len() > self.max_segments {
                state.segments.pop_front();
            }

            // Mark stream as ended if this is the last segment
            if is_eof {
                state.is_ended = true;
                tracing::info!("Stream ended: {}/{}", self.app_name, self.stream_name);
            }
        }

        // Reset for next segment
        self.ts_muxer.reset();
        self.last_segment_dts = current_dts;
        self.sequence_no += 1;

        Ok(())
    }

    async fn flush_remaining_segment(&mut self) -> Result<(), HlsRemuxerError> {
        if self.last_dts > self.last_segment_dts {
            self.finalize_segment(self.last_dts, true).await?;
        }
        Ok(())
    }
}

// Error types
#[derive(Debug, thiserror::Error)]
pub enum HlsRemuxerError {
    #[error("StreamHub event send error")]
    StreamHubEventSendError,

    #[error("Subscribe error")]
    SubscribeError,

    #[error("No frame receiver")]
    NoFrameReceiver,

    #[error("Demux error: {0}")]
    DemuxError(String),

    #[error("Mux error: {0}")]
    MuxError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Receive error: {0}")]
    ReceiveError(#[from] tokio::sync::broadcast::error::RecvError),
}

impl From<crate::streamhub::errors::StreamHubError> for HlsRemuxerError {
    fn from(_: crate::streamhub::errors::StreamHubError) -> Self {
        Self::SubscribeError
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_info_creation() {
        let segment = SegmentInfo {
            sequence: 1,
            duration: 10000,
            ts_name: "test_segment".to_string(),
            storage_key: "live/room123/test_segment".to_string(),
            discontinuity: false,
            created_at: Instant::now(),
        };

        assert_eq!(segment.sequence, 1);
        assert_eq!(segment.duration, 10000);
        assert!(!segment.discontinuity);
    }

    #[test]
    fn test_stream_processor_state_generate_m3u8() {
        let mut state = StreamProcessorState {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
            segments: VecDeque::new(),
            is_ended: false,
        };

        // Add some segments
        state.segments.push_back(SegmentInfo {
            sequence: 0,
            duration: 10000,
            ts_name: "segment0.ts".to_string(),
            storage_key: "live/room123/segment0.ts".to_string(),
            discontinuity: false,
            created_at: Instant::now(),
        });

        state.segments.push_back(SegmentInfo {
            sequence: 1,
            duration: 10000,
            ts_name: "segment1.ts".to_string(),
            storage_key: "live/room123/segment1.ts".to_string(),
            discontinuity: false,
            created_at: Instant::now(),
        });

        // Generate M3U8
        let m3u8 = state.generate_m3u8(|ts_name| format!("/api/hls/{}", ts_name));

        // Verify M3U8 content
        assert!(m3u8.contains("#EXTM3U"));
        assert!(m3u8.contains("#EXT-X-VERSION:3"));
        assert!(m3u8.contains("#EXT-X-TARGETDURATION:"));
        assert!(m3u8.contains("#EXT-X-MEDIA-SEQUENCE:0"));
        assert!(m3u8.contains("segment0.ts"));
        assert!(m3u8.contains("segment1.ts"));
    }

    #[test]
    fn test_stream_processor_state_with_discontinuity() {
        let mut state = StreamProcessorState {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
            segments: VecDeque::new(),
            is_ended: false,
        };

        // Add segment with discontinuity
        state.segments.push_back(SegmentInfo {
            sequence: 0,
            duration: 10000,
            ts_name: "segment0.ts".to_string(),
            storage_key: "live/room123/segment0.ts".to_string(),
            discontinuity: true,
            created_at: Instant::now(),
        });

        // Generate M3U8
        let m3u8 = state.generate_m3u8(|ts_name| format!("/api/hls/{}", ts_name));

        // Verify discontinuity tag is present
        assert!(m3u8.contains("#EXT-X-DISCONTINUITY"));
    }

    #[test]
    fn test_stream_processor_state_ended() {
        let mut state = StreamProcessorState {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
            segments: VecDeque::new(),
            is_ended: true,
        };

        // Add a segment
        state.segments.push_back(SegmentInfo {
            sequence: 0,
            duration: 10000,
            ts_name: "segment0.ts".to_string(),
            storage_key: "live/room123/segment0.ts".to_string(),
            discontinuity: false,
            created_at: Instant::now(),
        });

        // Generate M3U8
        let m3u8 = state.generate_m3u8(|ts_name| format!("/api/hls/{}", ts_name));

        // Verify ENDLIST tag is present
        assert!(m3u8.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn test_stream_processor_state_custom_url_generator() {
        let mut state = StreamProcessorState {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
            segments: VecDeque::new(),
            is_ended: false,
        };

        // Add segment
        state.segments.push_back(SegmentInfo {
            sequence: 0,
            duration: 10000,
            ts_name: "segment0.ts".to_string(),
            storage_key: "live/room123/segment0.ts".to_string(),
            discontinuity: false,
            created_at: Instant::now(),
        });

        // Generate M3U8 with custom URL generator (e.g., adding auth token)
        let m3u8 = state.generate_m3u8(|ts_name| {
            format!("/api/room/live/hls/data/room123/media456/{}?token=abc123", ts_name)
        });

        // Verify custom URL format is used
        assert!(m3u8.contains("?token=abc123"));
        assert!(m3u8.contains("/api/room/live/hls/data/room123/media456/"));
    }

    #[test]
    fn test_hls_remuxer_error_display() {
        let error = HlsRemuxerError::DemuxError("test error".to_string());
        assert_eq!(error.to_string(), "Demux error: test error");

        let error = HlsRemuxerError::StorageError("storage failed".to_string());
        assert_eq!(error.to_string(), "Storage error: storage failed");
    }

    #[test]
    fn test_is_transient_error() {
        // Transient errors - should retry
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "aborted")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::Interrupted, "interrupted")));
        assert!(is_transient_error(&std::io::Error::new(std::io::ErrorKind::WouldBlock, "would block")));

        // Non-transient errors - should not retry
        assert!(!is_transient_error(&std::io::Error::new(std::io::ErrorKind::NotFound, "not found")));
        assert!(!is_transient_error(&std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied")));
        assert!(!is_transient_error(&std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid input")));
    }

    #[tokio::test]
    async fn test_write_with_retry_success() {
        use crate::storage::MemoryStorage;

        let storage = Arc::new(MemoryStorage::new()) as Arc<dyn HlsStorage>;
        let data = Bytes::from_static(b"test segment data");

        // Should succeed immediately
        let result = write_with_retry(&storage, "test-key", data.clone()).await;
        assert!(result.is_ok());

        // Verify data was written
        let read_data = storage.read("test-key").await.unwrap();
        assert_eq!(data, read_data);
    }
}
