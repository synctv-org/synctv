// Custom HLS remuxer using xiu's libraries but pluggable storage abstraction
// Architecture:
// - Uses xiu's FlvVideoTagDemuxer/FlvAudioTagDemuxer for FLV parsing
// - Uses xiu's TsMuxer for TS segment generation
// - Uses xiu-storage's HlsStorage trait for segment/playlist storage
// - Generates M3U8 dynamically in memory, no file writes

use crate::flv::define::AvcCodecId;
use crate::flv::{
    define::{frame_type, FlvData},
    demuxer::{FlvAudioTagDemuxer, FlvVideoTagDemuxer},
};
use crate::hls::playlist::{HlsPlaylist, SegmentInfo};
use crate::hls::segment_manager::SegmentManager;
use crate::mpegts::{
    define::{epsi_stream_type, MPEG_FLAG_IDR_FRAME},
    ts::TsMuxer,
};
use crate::storage::HlsStorage;
use crate::streamhub::{
    define::{
        BroadcastEvent, BroadcastEventReceiver, FrameData, FrameDataReceiver, NotifyInfo,
        StreamHubEvent, StreamHubEventSender, SubscribeType, SubscriberInfo,
    },
    send_event_with_backpressure_timeout, spawn_event_delivery_with_backpressure_timeout,
    stream::StreamIdentifier,
    subscribe_with_rollback_on_timeout,
    utils::Uuid,
    SubscribeWithRollbackError,
};
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
const STREAM_SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);
const RECV_TIMEOUT_MS: u64 = 65000;
const ACTIVITY_RECORD_INTERVAL: Duration = Duration::from_secs(10);
const DTS_REGRESSION_THRESHOLD_MS: i64 = 1000;

fn now_epoch_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

fn current_segment_minute_bucket() -> String {
    let epoch_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(err) => {
            tracing::warn!(
                "system clock is before UNIX_EPOCH while generating HLS segment bucket: {err}"
            );
            0
        }
    };
    (epoch_secs / 60).to_string()
}

fn generate_ts_name() -> String {
    format!(
        "{}_{}",
        current_segment_minute_bucket(),
        synctv_common::snanoid!(12)
    )
}

/// Registry of active streams (for M3U8 generation)
pub type StreamRegistry = Arc<DashMap<String, Arc<parking_lot::RwLock<StreamProcessorState>>>>;
/// Stream processor state that can be accessed by HTTP server
pub struct StreamProcessorState {
    pub app_name: String,
    pub stream_name: String,
    pub playlist: HlsPlaylist,
    /// Creation timestamp used to detect if this entry was replaced by a new handler
    pub created_at: Instant,
    /// When set, this stream's segments can be cleaned up immediately.
    /// Set when the stream handler ends to allow memory-conscious cleanup
    /// rather than waiting for the 60-second grace period to elapse.
    pub marked_for_cleanup: bool,
    /// Segment names captured at cleanup-mark time. Only these specific segments
    /// should be deleted by the cleanup task, avoiding a race where a new handler
    /// has already started writing new segments for the same stream key.
    pub cleanup_segment_names: Vec<String>,
}

/// Implementation of `StreamCleanupChecker` for the HLS stream registry.
///
/// The cleanup task uses this to find streams that are ready for cleanup.
/// Crucially, it returns the specific segment names captured at mark time
/// rather than performing a blanket delete, preventing the race where
/// new handler segments get deleted.
pub struct RegistryCleanupChecker {
    pub registry: StreamRegistry,
}

impl crate::hls::segment_manager::StreamCleanupChecker for RegistryCleanupChecker {
    fn get_streams_marked_for_cleanup(
        &self,
    ) -> Vec<crate::hls::segment_manager::MarkedStreamCleanup> {
        let mut result = Vec::new();
        for entry in self.registry.iter() {
            let state = entry.value().read();
            if state.marked_for_cleanup {
                result.push(crate::hls::segment_manager::MarkedStreamCleanup {
                    app_name: state.app_name.clone(),
                    stream_name: state.stream_name.clone(),
                    segment_names: state.cleanup_segment_names.clone(),
                });
            }
        }
        result
    }
}

impl StreamProcessorState {
    /// Generate M3U8 content dynamically with custom TS URL generator
    ///
    /// # Arguments
    /// * `gen_ts_url` - Closure that takes TS name and returns full URL (can add auth tokens)
    ///
    /// # Example
    /// ```text
    /// let m3u8 = state.generate_m3u8(|ts_name| {
    /// format!("/api/room/live/hls/data/{}/{}/{}?token={}", room_id, movie_id, ts_name, token)
    /// });
    /// ```
    pub fn generate_m3u8<F>(&self, mut gen_ts_url: F) -> String
    where
        F: FnMut(&str) -> String,
    {
        self.playlist.generate_m3u8(&mut gen_ts_url)
    }
}

fn mark_stream_state_for_cleanup(state: &mut StreamProcessorState) {
    state.cleanup_segment_names = state
        .playlist
        .segments
        .iter()
        .map(|segment| segment.ts_name.clone())
        .collect();
    state.marked_for_cleanup = true;
}

/// Callback invoked when media data is received from a publisher.
/// Arguments: (`app_name/room_id`, `stream_name/media_id`).
/// Used to record publisher activity for silent publisher detection.
pub type PublisherActivityCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Callback that returns the list of active RTMP publishers as
/// `(app_name, stream_name)` pairs. Used for reconciliation after
/// broadcast channel lag.
pub type ActivePublishersSource = Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>;

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
    /// Optional callback to record publisher data activity
    activity_callback: Option<PublisherActivityCallback>,
    /// Optional source of currently active publishers for post-lag reconciliation
    active_publishers_source: Option<ActivePublishersSource>,
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
            activity_callback: None,
            active_publishers_source: None,
        }
    }

    /// Set a callback that is invoked when media frames are received from a publisher.
    /// This is used by `PublisherManager` to track publisher liveness and prevent
    /// silent publisher timeouts.
    #[must_use]
    pub fn with_activity_callback(mut self, callback: PublisherActivityCallback) -> Self {
        self.activity_callback = Some(callback);
        self
    }

    /// Set a source of active publishers for post-lag reconciliation.
    ///
    /// After broadcast channel lag causes a resubscribe, the remuxer queries
    /// this callback for the current list of active RTMP publishers and starts
    /// HLS handlers for any that are missing.
    #[must_use]
    pub fn with_active_publishers_source(mut self, source: ActivePublishersSource) -> Self {
        self.active_publishers_source = Some(source);
        self
    }

    pub async fn run(&mut self) -> Result<(), HlsRemuxerError> {
        tracing::info!("Custom HLS remuxer started");

        // Clean up only segments older than the configured retention window.
        // Shared storage backends may be used by multiple replicas, so startup
        // must not purge fresh objects written by another live publisher.
        match self.segment_manager.cleanup_expired().await {
            Ok(deleted) => {
                if deleted > 0 {
                    tracing::info!("Cleaned up {} expired HLS segments on startup", deleted);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to cleanup orphaned segments on startup: {}", e);
            }
        }

        loop {
            let val = tokio::select! {
                           () = self.cancel_token.cancelled() => {
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
                                           "HLS remuxer lagged behind by {n} broadcast events; re-subscribing. \
                                            {} active handler tasks remain unaffected.",
                                           self.handler_tasks.len()
                                       );
            // Existing handler tasks are NOT aborted — they have independent
            // data channels. Only Publish events in the skipped window are lost.
                                       self.client_event_consumer = self.client_event_consumer.resubscribe();

            // Reconcile: start HLS handlers for any active publishers
            // that don't already have a running handler.
                                       if let Some(ref source) = self.active_publishers_source {
                                           let active = source();
                                           let mut started = 0u32;
                                           for (app_name, stream_name) in active {
                                               let key = format!("{app_name}/{stream_name}");
                                               if self.stream_registry.contains_key(&key) {
                                                   continue; // handler already running
                                               }
                                               tracing::info!(
                                                   "HLS remuxer reconciliation: starting handler for {}/{}",
                                                   app_name, stream_name
                                               );
                                               let stream_handler = StreamHandler::new(
                                                   app_name,
                                                   stream_name,
                                                   self.event_producer.clone(),
                                                   Arc::clone(&self.segment_manager),
                                                   self.stream_registry.clone(),
                                                   self.activity_callback.clone(),
                                               );
                                               self.handler_tasks.spawn(async move {
                                                   if let Err(e) = stream_handler.run().await {
                                                       tracing::error!("HLS stream handler error (reconciliation): {}", e);
                                                   }
                                               });
                                               started += 1;
                                           }
                                           if started > 0 {
                                               tracing::info!(
                                                   "HLS remuxer reconciliation: started {} new handlers after lag",
                                                   started
                                               );
                                           }
                                       }
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
                BroadcastEvent::Publish {
                    identifier,
                    pub_type,
                } => {
                    // Process streams owned by this node. Cross-node relay
                    // streams are mirrored from the publisher node, where HLS
                    // segment generation already happens.
                    if pub_type == crate::streamhub::define::PublishType::RtmpRelay {
                        tracing::debug!("HLS remuxer: skipping relayed stream (RtmpRelay)");
                        continue;
                    }

                    let StreamIdentifier::Rtmp {
                        app_name,
                        stream_name,
                    } = identifier;

                    tracing::info!("HLS remuxer: new stream {}/{}", app_name, stream_name);

                    let stream_handler = StreamHandler::new(
                        app_name,
                        stream_name,
                        self.event_producer.clone(),
                        Arc::clone(&self.segment_manager),
                        self.stream_registry.clone(),
                        self.activity_callback.clone(),
                    );

                    self.handler_tasks.spawn(async move {
                        if let Err(e) = stream_handler.run().await {
                            tracing::error!("HLS stream handler error: {}", e);
                        }
                    });
                }
                BroadcastEvent::UnPublish { .. } => {
                    tracing::trace!("HLS remuxer: stream unpublished");
                }
            }
        }
    }
}

/// Drop guard that sends `UnSubscribe` to `StreamHub` on drop.
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
        tracing::info!(
            subscriber_id = %self.subscriber_id,
            "UnsubscribeGuard: scheduling unsubscribe on drop"
        );
        spawn_event_delivery_with_backpressure_timeout(self.event_producer.clone(), event);
    }
}

/// Drop guard that removes the stream registry entry on drop.
/// Used to prevent registry leaks if the handler panics.
/// On the normal path, `active` is set to `false` before the delayed remove.
struct StreamRegistryGuard {
    registry: StreamRegistry,
    key: String,
    /// The creation timestamp of the entry this guard owns.
    /// Only removes the entry if it still matches (prevents removing a newer handler's entry).
    created_at: Instant,
    active: bool,
}

impl Drop for StreamRegistryGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        // Only remove if the entry still belongs to this handler
        if let Some(entry) = self.registry.get(&self.key) {
            if entry.read().created_at == self.created_at {
                drop(entry);
                self.registry.remove(&self.key);
                tracing::warn!(
                    "StreamRegistryGuard: removed registry entry for {} on panic/early-return",
                    self.key
                );
            }
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
    /// Optional callback to record publisher data activity
    activity_callback: Option<PublisherActivityCallback>,
}

impl StreamHandler {
    fn new(
        app_name: String,
        stream_name: String,
        event_producer: StreamHubEventSender,
        segment_manager: Arc<SegmentManager>,
        stream_registry: StreamRegistry,
        activity_callback: Option<PublisherActivityCallback>,
    ) -> Self {
        let (_, data_consumer) =
            mpsc::channel(crate::streamhub::define::FRAME_DATA_CHANNEL_CAPACITY);
        let subscriber_id = Uuid::new();

        Self {
            app_name,
            stream_name,
            event_producer,
            segment_manager,
            stream_registry,
            data_consumer: crate::streamhub::define::FrameDataReceiver::bounded(data_consumer),
            subscriber_id,
            activity_callback,
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

        let playlist = HlsPlaylist::new();

        // Register stream in registry
        let handler_created_at = Instant::now();
        let state = Arc::new(parking_lot::RwLock::new(StreamProcessorState {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
            playlist,
            created_at: handler_created_at,
            marked_for_cleanup: false,
            cleanup_segment_names: Vec::new(),
        }));
        self.stream_registry
            .insert(registry_key.clone(), state.clone());

        // Process FLV data and generate HLS segments
        let mut processor = StreamProcessor::new(
            &self.app_name,
            &self.stream_name,
            Arc::clone(&self.segment_manager),
            state.clone(),
        )?;

        // Install registry guard: if the handler panics, immediately remove the
        // registry entry so it doesn't leak. On the normal path we disarm it
        // and do a delayed remove instead.
        let mut registry_guard = StreamRegistryGuard {
            registry: self.stream_registry.clone(),
            key: registry_key.clone(),
            created_at: handler_created_at,
            active: true,
        };

        processor
            .process_stream(&mut self.data_consumer, self.activity_callback.clone())
            .await?;

        // Unsubscribe when done
        self.unsubscribe_from_stream_hub().await?;

        // Deactivate drop guard only after the explicit unsubscribe succeeds.
        // If unsubscribe fails, keep the guards armed so drop-based cleanup still runs.
        unsub_guard.active = false;
        // Disarm registry guard only after the explicit unsubscribe succeeds.
        // This prevents registry leaks on the early-return error path above.
        registry_guard.active = false;

        // Mark the stream state as ended. Storage cleanup is intentionally
        // age-based and asynchronous; deleting by stream prefix is unsafe for
        // shared backends because another replica can republish the same
        // room/media key while this handler is in its grace period.
        {
            let mut state_guard = state.write();
            mark_stream_state_for_cleanup(&mut state_guard);
        }

        // Remove from registry after some delay (allow clients to finish).
        // We capture handler_created_at before sleeping so that when the timer fires
        // we can detect if a new publisher has already replaced this handler.
        tokio::time::sleep(tokio::time::Duration::from_mins(1)).await;

        // Only remove the registry entry if it still belongs to this handler.
        // If a new publisher started within the 60s window, its registry entry
        // will have a different created_at.
        let is_still_owner = self
            .stream_registry
            .get(&registry_key)
            .is_some_and(|entry| entry.read().created_at == handler_created_at);

        if is_still_owner {
            self.stream_registry.remove(&registry_key);
        } else {
            tracing::info!(
                "HLS registry cleanup for {}/{}: skipped because a new publisher has taken over within the grace period",
                self.app_name,
                self.stream_name,
            );
        }

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

        let (data_receiver, _stat_sender) = subscribe_with_rollback_on_timeout(
            &self.event_producer,
            identifier,
            sub_info,
            STREAM_SUBSCRIBE_TIMEOUT,
        )
        .await
        .map_err(|err| match err {
            SubscribeWithRollbackError::Timeout => HlsRemuxerError::SubscribeTimeout,
            SubscribeWithRollbackError::StreamHub(err) => HlsRemuxerError::SubscribeError(err),
        })?;

        let receiver = data_receiver
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

    async fn unsubscribe_from_stream_hub(&self) -> Result<(), HlsRemuxerError> {
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

        send_event_with_backpressure_timeout(&self.event_producer, unsubscribe_event)
            .await
            .map_err(HlsRemuxerError::StreamHubEventSendError)?;

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
    storage: Arc<dyn HlsStorage>,
    app: String,
    stream: String,
    name: String,
    data: Bytes,
) -> std::io::Result<()> {
    use backon::{ExponentialBuilder, Retryable};

    (|| {
        let storage = storage.clone();
        let app = app.clone();
        let stream = stream.clone();
        let name = name.clone();
        let data = data.clone();
        async move { storage.write(&app, &stream, &name, data).await }
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

    // Codec detection: tracks the video codec detected from the stream.
    // When HEVC is detected, the TsMuxer is re-initialized with PSI_STREAM_HEVC.
    video_codec_id: Option<u8>,
    /// True after a decodable video frame has been received. Audio-only streams
    /// use audio DTS to rotate live segments because they have no keyframes.
    video_seen: bool,

    // Segment tracking
    sequence_no: u64,

    // Timing
    segment_duration_ms: i64, // Target segment duration (e.g., 10000ms = 10s)
    last_segment_dts: i64,
    last_dts: i64,
    last_pts: i64,

    /// When set, the next HLS segment will include an #EXT-X-DISCONTINUITY tag.
    /// This is set when frames are dropped (buffer overflow, timestamp issues)
    /// so that HLS players know to reset their PTS decoder.
    discontinuity_pending: bool,
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

        let (sequence_no, discontinuity_pending) = {
            let state = state.read();
            (
                state
                    .playlist
                    .segments
                    .back()
                    .map_or(0, |segment| segment.sequence.saturating_add(1)),
                !state.playlist.segments.is_empty(),
            )
        };

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
            video_codec_id: None,
            video_seen: false,
            sequence_no,
            segment_duration_ms: 10000, // 10 seconds
            last_segment_dts: 0,
            last_dts: 0,
            last_pts: 0,
            discontinuity_pending,
        })
    }

    async fn process_stream(
        &mut self,
        data_consumer: &mut FrameDataReceiver,
        activity_callback: Option<PublisherActivityCallback>,
    ) -> Result<(), HlsRemuxerError> {
        // Use a longer timeout for stream end detection
        // The original logic had a flaw: it would increment retry_count on any
        // recv() returning None, even during brief network pauses.
        // Now we use a timeout-based approach instead.
        // Must not be less than the silent publisher timeout (60s) to avoid
        // false stream-end detection while the publisher is still connected.
        // Throttle activity callbacks to avoid excessive overhead.
        // The silent publisher timeout is 60s, so recording every 10s is sufficient.
        let mut last_activity_record = Instant::now();

        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(RECV_TIMEOUT_MS),
                data_consumer.recv(),
            )
            .await
            {
                Ok(Some(frame_data)) => {
                    let flv_data = match frame_data {
                        FrameData::Audio { timestamp, data } => FlvData::Audio {
                            timestamp,
                            data: BytesMut::from(data),
                        },
                        FrameData::Video { timestamp, data } => FlvData::Video {
                            timestamp,
                            data: BytesMut::from(data),
                        },
                        _ => continue,
                    };

                    // Record publisher activity (throttled) to prevent silent publisher timeout
                    if let Some(callback) = activity_callback.as_ref() {
                        if last_activity_record.elapsed() >= ACTIVITY_RECORD_INTERVAL {
                            callback(&self.app_name, &self.stream_name);
                            last_activity_record = Instant::now();
                        }
                    }

                    self.process_flv_data(flv_data).await?;
                }
                Ok(None) => {
                    // Channel closed - stream truly ended
                    tracing::info!(
                        "Stream channel closed: {}/{}",
                        self.app_name,
                        self.stream_name
                    );
                    self.flush_remaining_segment().await?;
                    break;
                }
                Err(_timeout) => {
                    // Timeout - no data for RECV_TIMEOUT_MS, consider stream ended
                    tracing::info!(
                        "Stream timeout (no data for {}s): {}/{}",
                        RECV_TIMEOUT_MS / 1000,
                        self.app_name,
                        self.stream_name
                    );
                    self.flush_remaining_segment().await?;
                    break;
                }
            }
        }

        Ok(())
    }

    async fn process_flv_data(&mut self, flv_data: FlvData) -> Result<(), HlsRemuxerError> {
        let previous_dts = self.last_dts;

        let (pid, pts, dts, flags, payload) = match flv_data {
            FlvData::Video { timestamp, data } => {
                let video_data = self.video_demuxer.demux(timestamp, data).map_err(|e| {
                    HlsRemuxerError::DemuxError(format!("Video demux error: {e:?}"))
                })?;

                let Some(video_data) = video_data else {
                    return Ok(());
                };
                self.video_seen = true;

                // Detect codec on first video frame and re-initialize TsMuxer if HEVC.
                // This ensures the PMT advertises the correct stream_type (0x24 for HEVC,
                // 0x1B for H.264), preventing players from failing to decode with no error.
                if self.video_codec_id.is_none() {
                    self.video_codec_id = Some(video_data.codec_id);
                    if video_data.codec_id == AvcCodecId::HEVC as u8 {
                        tracing::info!(
                            "Detected HEVC video codec for {}/{}, re-initializing TsMuxer with PSI_STREAM_HEVC",
                            self.app_name, self.stream_name
                        );
                        let mut new_muxer = TsMuxer::new();
                        let audio_pid = new_muxer
                            .add_stream(epsi_stream_type::PSI_STREAM_AAC, BytesMut::new())
                            .map_err(|e| {
                                HlsRemuxerError::MuxError(format!(
                                    "Failed to add audio stream: {e:?}"
                                ))
                            })?;
                        let video_pid = new_muxer
                            .add_stream(epsi_stream_type::PSI_STREAM_HEVC, BytesMut::new())
                            .map_err(|e| {
                                HlsRemuxerError::MuxError(format!(
                                    "Failed to add HEVC video stream: {e:?}"
                                ))
                            })?;
                        self.ts_muxer = new_muxer;
                        self.audio_pid = audio_pid;
                        self.video_pid = video_pid;
                    }
                }

                let mut flags = 0;
                let payload = video_data.data;

                // Check if keyframe and if we need new segment
                if video_data.frame_type == frame_type::KEY_FRAME {
                    flags = MPEG_FLAG_IDR_FRAME;

                    if video_data.dts - self.last_segment_dts >= self.segment_duration_ms {
                        self.finalize_segment(video_data.dts, false).await?;
                    }
                }

                (
                    self.video_pid,
                    video_data.pts,
                    video_data.dts,
                    flags,
                    payload,
                )
            }
            FlvData::Audio { timestamp, data } => {
                let audio_data = self.audio_demuxer.demux(timestamp, data).map_err(|e| {
                    HlsRemuxerError::DemuxError(format!("Audio demux error: {e:?}"))
                })?;

                if !audio_data.has_data {
                    return Ok(());
                }

                let payload = audio_data.data;

                // Video streams rotate on keyframes. An audio-only RTMP/RTSP
                // source has no keyframes, so use the audio clock to maintain
                // the same live HLS sliding-window behavior.
                if !self.video_seen
                    && audio_data.dts - self.last_segment_dts >= self.segment_duration_ms
                    && self.last_dts > self.last_segment_dts
                {
                    self.finalize_segment(audio_data.dts, false).await?;
                }

                (self.audio_pid, audio_data.pts, audio_data.dts, 0, payload)
            }
            FlvData::MetaData { .. } => return Ok(()),
        };

        // Detect DTS regression (timestamp going backward) which indicates dropped frames
        // or a stream discontinuity. Set the flag so the next segment gets #EXT-X-DISCONTINUITY.
        if previous_dts > 0 && dts < previous_dts - DTS_REGRESSION_THRESHOLD_MS {
            tracing::warn!(
                "DTS regression detected for {}/{}: last_dts={}, current_dts={} — marking discontinuity",
                self.app_name, self.stream_name, previous_dts, dts
            );
            self.discontinuity_pending = true;
        }

        self.last_dts = dts;
        self.last_pts = pts;

        // Write to TS muxer
        self.ts_muxer
            .write(pid, pts * 90, dts * 90, flags, payload)
            .map_err(|e| HlsRemuxerError::MuxError(format!("TS mux error: {e:?}")))?;

        Ok(())
    }

    async fn finalize_segment(
        &mut self,
        current_dts: i64,
        is_eof: bool,
    ) -> Result<(), HlsRemuxerError> {
        let ts_data = self.ts_muxer.get_data();
        // Guard against zero or negative segment durations caused by
        // DTS non-monotonicity, first-segment edge cases, or encoder anomalies.
        // A segment with zero/negative duration would produce an invalid M3U8 that
        // players reject with a playlist parse error.
        let raw_duration_ms = current_dts - self.last_segment_dts;
        let duration_ms = if raw_duration_ms <= 0 {
            tracing::warn!(
                "Invalid segment duration {}ms for {}/{} (current_dts={}, last_segment_dts={}). \
                 Using target segment duration {}ms as fallback.",
                raw_duration_ms,
                self.app_name,
                self.stream_name,
                current_dts,
                self.last_segment_dts,
                self.segment_duration_ms,
            );
            self.segment_duration_ms
        } else {
            raw_duration_ms
        };
        let ts_data_len = ts_data.len();

        // Generate TS filename with a minute bucket prefix. OSS storage maps
        // this prefix to a real directory while HTTP routes keep a slash-free
        // opaque segment name.
        let ts_name = generate_ts_name();

        // Write segment to storage with retry using structured (app, stream, name)
        let storage = self.segment_manager.storage().clone();
        let data: Bytes = ts_data.into();
        write_with_retry(
            storage,
            self.app_name.clone(),
            self.stream_name.clone(),
            ts_name.clone(),
            data,
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                "HLS segment write failed after retries: {}/{}/{} - {}",
                self.app_name,
                self.stream_name,
                ts_name,
                e
            );
            HlsRemuxerError::StorageError(e.to_string())
        })?;

        tracing::debug!(
            "Wrote segment: {}/{}/{} ({}ms, {} bytes)",
            self.app_name,
            self.stream_name,
            ts_name,
            duration_ms,
            ts_data_len
        );

        // Consume the discontinuity flag: if frames were dropped or a DTS regression
        // was detected, mark this segment so the M3U8 playlist includes
        // #EXT-X-DISCONTINUITY before it, telling players to reset their PTS decoder.
        let discontinuity = self.discontinuity_pending;
        self.discontinuity_pending = false;

        let started_at_ms = now_epoch_ms().saturating_sub(duration_ms);

        // Track segment metadata
        let segment_info = SegmentInfo {
            sequence: self.sequence_no,
            duration_ms,
            started_at_ms,
            ts_name,
            discontinuity,
        };

        // Update the in-memory playlist. Storage cleanup runs independently using
        // the configured retention window so clients can fetch an older segment
        // after it leaves the playlist window.
        {
            let mut state = self.state.write();
            state.playlist.push_segment(segment_info);
            if is_eof {
                state.playlist.mark_ended();
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
        } else {
            self.state.write().playlist.mark_ended();
        }
        Ok(())
    }
}

// Error types
#[derive(Debug, thiserror::Error)]
pub enum HlsRemuxerError {
    #[error("StreamHub event send error: {0}")]
    StreamHubEventSendError(crate::streamhub::errors::StreamHubError),

    #[error("Subscribe error: {0}")]
    SubscribeError(crate::streamhub::errors::StreamHubError),

    #[error("Subscribe timed out")]
    SubscribeTimeout,

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
    fn from(error: crate::streamhub::errors::StreamHubError) -> Self {
        Self::SubscribeError(error)
    }
}
