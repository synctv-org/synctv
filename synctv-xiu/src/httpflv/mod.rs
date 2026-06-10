// HTTP-FLV session: subscribes to StreamHub and sends FLV data over a bounded channel
// This is a generic, reusable component. The HTTP routing layer
// (which may depend on application-specific state like Redis) lives
// in the downstream crate (e.g., synctv-livestream).

use crate::flv::amf0::amf0_writer::Amf0Writer;
use crate::flv::muxer::{FlvMuxer, HEADER_LENGTH};
use crate::streamhub::{
    define::{
        FrameData, FrameDataReceiver, NotifyInfo, StreamHubEvent, StreamHubEventSender,
        SubDataType, SubscribeType, SubscriberInfo,
    },
    send_event_with_backpressure_timeout,
    stream::StreamIdentifier,
    subscribe_with_rollback_on_timeout,
    utils::Uuid,
    SubscribeWithRollbackError,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Capacity for the HTTP response channel (bounded to prevent OOM with slow clients).
/// At ~8KB per FLV tag (typical video frame), 512 entries ≈ 4MB buffer per client.
pub const FLV_RESPONSE_CHANNEL_CAPACITY: usize = 512;

/// Maximum number of consecutive dropped frames before disconnecting a slow subscriber.
///
/// At 30fps video, 150 consecutive drops ≈ 5 seconds of missed content.
/// The subscriber's playback is unrecoverable at this point.
pub const MAX_CONSECUTIVE_DROPPED_FRAMES: u32 = 150;
const STREAM_SUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// HTTP-FLV session (per-client connection)
pub struct HttpFlvSession {
    pub app_name: String,
    pub stream_name: String,
    event_producer: StreamHubEventSender,
    /// Initialized to None; set by `subscribe_from_stream_hub`.
    /// Calling `send_media_stream` without subscribing first is an error.
    data_receiver: Option<FrameDataReceiver>,
    response_producer: mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    subscriber_id: Uuid,
    muxer: FlvMuxer,
    pub has_audio: bool,
    pub has_video: bool,
    pub has_send_header: bool,
    /// Track consecutive dropped frames for slow client detection.
    /// Reset to 0 on each successful send. When this reaches
    /// `MAX_CONSECUTIVE_DROPPED_FRAMES`, the session is disconnected.
    consecutive_dropped_frames: u32,
    /// Total number of dropped frames for monitoring/logging.
    pub total_dropped_frames: u64,
}

impl HttpFlvSession {
    #[must_use]
    pub fn new(
        app_name: String,
        stream_name: String,
        event_producer: StreamHubEventSender,
        response_producer: mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    ) -> Self {
        let subscriber_id = Uuid::new();

        Self {
            app_name,
            stream_name,
            event_producer,
            data_receiver: None,
            response_producer,
            subscriber_id,
            muxer: FlvMuxer::new(),
            has_audio: false,
            has_video: false,
            has_send_header: false,
            consecutive_dropped_frames: 0,
            total_dropped_frames: 0,
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        self.start().await?;
        self.run_after_start().await
    }

    pub async fn run_after_start(&mut self) -> anyhow::Result<()> {
        let result = self.send_media_stream().await;
        let unsubscribe_result = self.unsubscribe_from_stream_hub().await;

        match (result, unsubscribe_result) {
            (Err(stream_err), Ok(())) => Err(stream_err),
            (Ok(()), Err(unsubscribe_err)) => Err(unsubscribe_err),
            (Err(stream_err), Err(unsubscribe_err)) => {
                warn!(
                    stream = %self.stream_name,
                    stream_error = %stream_err,
                    unsubscribe_error = %unsubscribe_err,
                    "HTTP-FLV session failed and unsubscribe cleanup also failed"
                );
                Err(stream_err)
            }
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub async fn start(&mut self) -> anyhow::Result<()> {
        self.subscribe_from_stream_hub().await
    }

    async fn send_media_stream(&mut self) -> anyhow::Result<()> {
        let mut data_receiver = self.data_receiver.take().ok_or_else(|| {
            anyhow::anyhow!("send_media_stream called before subscribe_from_stream_hub")
        })?;

        let mut max_av_frame_num_to_guess_av = 0;
        let mut cached_frames = Vec::new();

        loop {
            if let Some(data) = data_receiver.recv().await {
                // Detect audio/video before sending header
                if !self.has_send_header {
                    max_av_frame_num_to_guess_av += 1;

                    match data {
                        FrameData::Audio { .. } => {
                            self.has_audio = true;
                            cached_frames.push(data);
                        }
                        FrameData::Video { .. } => {
                            self.has_video = true;
                            cached_frames.push(data);
                        }
                        FrameData::MetaData { .. } => cached_frames.push(data),
                        _ => {}
                    }

                    // Send header after detecting A/V or after 10 frames
                    if (self.has_audio && self.has_video) || max_av_frame_num_to_guess_av > 10 {
                        self.has_send_header = true;

                        // Write FLV header
                        self.muxer
                            .write_flv_header(self.has_audio, self.has_video)
                            .map_err(|e| anyhow::anyhow!("Failed to write FLV header: {e:?}"))?;
                        self.muxer
                            .write_previous_tag_size(0)
                            .map_err(|e| anyhow::anyhow!("Failed to write tag size: {e:?}"))?;
                        self.flush_response_data()?;

                        // Write cached frames
                        for frame in &cached_frames {
                            self.write_flv_tag(frame)?;
                        }
                        cached_frames.clear();
                    }

                    continue;
                }

                // Write FLV tag. Slow-subscriber disconnects and closed
                // response channels must terminate the session so the
                // StreamHub subscription is released promptly.
                self.write_flv_tag(&data)?;
            } else {
                // Channel closed - stream truly ended
                info!("Stream channel closed");
                break;
            }
        }
        Ok(())
    }

    fn write_flv_tag(&mut self, frame_data: &FrameData) -> anyhow::Result<()> {
        let (data, timestamp, tag_type) = match frame_data {
            FrameData::Audio { timestamp, data } => (&data[..], *timestamp, 8), // AUDIO
            FrameData::Video { timestamp, data } => (&data[..], *timestamp, 9), // VIDEO
            FrameData::MetaData { timestamp, data } => {
                // Remove @setDataFrame from RTMP's metadata
                let mut amf_writer = Amf0Writer::new();
                amf_writer
                    .write_string(&String::from("@setDataFrame"))
                    .map_err(|e| anyhow::anyhow!("Failed to write AMF string: {e:?}"))?;
                let right = &data[amf_writer.len()..];
                (right, *timestamp, 18) // SCRIPT_DATA_AMF
            }
            _ => return Ok(()),
        };

        let data_len =
            u32::try_from(data.len()).map_err(|_| anyhow::anyhow!("FLV tag body too large"))?;

        self.muxer
            .write_flv_tag_header(tag_type, data_len, timestamp)
            .map_err(|e| anyhow::anyhow!("Failed to write FLV tag header: {e:?}"))?;
        self.muxer
            .write_flv_tag_body(data)
            .map_err(|e| anyhow::anyhow!("Failed to write FLV tag body: {e:?}"))?;
        self.muxer
            .write_previous_tag_size(data_len + HEADER_LENGTH)
            .map_err(|e| anyhow::anyhow!("Failed to write tag size: {e:?}"))?;

        self.flush_response_data()?;

        Ok(())
    }

    fn flush_response_data(&mut self) -> anyhow::Result<()> {
        let bytes = self.muxer.writer.extract_current_bytes_frozen();

        // Use try_send to apply backpressure. Track consecutive dropped frames
        // and disconnect the subscriber after too many drops to prevent a slow client
        // from permanently consuming publisher resources.
        match self.response_producer.try_send(Ok(bytes)) {
            Ok(()) => {
                // Successful send resets the consecutive drop counter
                self.consecutive_dropped_frames = 0;
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.consecutive_dropped_frames += 1;
                self.total_dropped_frames += 1;
                if self.consecutive_dropped_frames >= MAX_CONSECUTIVE_DROPPED_FRAMES {
                    warn!(
                        stream = %self.stream_name,
                        consecutive_drops = self.consecutive_dropped_frames,
                        total_drops = self.total_dropped_frames,
                        "Disconnecting slow FLV subscriber: too many consecutive dropped frames"
                    );
                    return Err(anyhow::anyhow!(
                        "Slow subscriber disconnected after {} consecutive dropped frames",
                        self.consecutive_dropped_frames
                    ));
                }
                warn!(
                    stream = %self.stream_name,
                    consecutive_drops = self.consecutive_dropped_frames,
                    "FLV response channel full, dropping frame (slow client)"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(anyhow::anyhow!("Response channel closed"));
            }
        }

        Ok(())
    }

    async fn subscribe_from_stream_hub(&mut self) -> anyhow::Result<()> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2HttpFlv,
            sub_data_type: SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!("/live/{}.flv", self.stream_name),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };

        let result = subscribe_with_rollback_on_timeout(
            &self.event_producer,
            identifier,
            sub_info,
            STREAM_SUBSCRIBE_TIMEOUT,
        )
        .await
        .map_err(|err| match err {
            SubscribeWithRollbackError::Timeout => {
                anyhow::anyhow!(
                    "Subscribe timed out after {}s",
                    STREAM_SUBSCRIBE_TIMEOUT.as_secs()
                )
            }
            SubscribeWithRollbackError::StreamHub(e) => {
                anyhow::anyhow!("Subscribe failed: {e:?}")
            }
        })?;
        self.data_receiver = Some(
            result
                .0
                .frame_receiver
                .ok_or_else(|| anyhow::anyhow!("No frame receiver"))?,
        );

        info!(
            subscriber_id = %self.subscriber_id,
            stream = %self.stream_name,
            "Subscribed to StreamHub"
        );

        Ok(())
    }

    async fn unsubscribe_from_stream_hub(&self) -> anyhow::Result<()> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2HttpFlv,
            sub_data_type: SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!("/live/{}.flv", self.stream_name),
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
            .map_err(|err| {
                warn!("Failed to send unsubscribe event: {err}");
                anyhow::Error::new(err)
            })?;

        info!(
            subscriber_id = %self.subscriber_id,
            stream = %self.stream_name,
            "Unsubscribed from StreamHub"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streamhub::errors::{StreamHubError, StreamHubErrorValue};

    fn assert_rtmp_identifier(
        identifier: StreamIdentifier,
        expected_app: &str,
        expected_stream: &str,
    ) {
        let StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        } = identifier;
        assert_eq!(app_name, expected_app);
        assert_eq!(stream_name, expected_stream);
    }

    fn assert_unsubscribe_event(event: StreamHubEvent, expected_app: &str, expected_stream: &str) {
        let StreamHubEvent::UnSubscribe { identifier, .. } = event else {
            panic!("expected unsubscribe event, got {event:?}");
        };
        assert_rtmp_identifier(identifier, expected_app, expected_stream);
    }

    fn subscribe_result_sender(
        event: StreamHubEvent,
    ) -> tokio::sync::oneshot::Sender<
        Result<
            (
                crate::streamhub::define::DataReceiver,
                Option<crate::streamhub::define::StatisticDataSender>,
            ),
            StreamHubError,
        >,
    > {
        let StreamHubEvent::Subscribe { result_sender, .. } = event else {
            panic!("expected subscribe event, got {event:?}");
        };
        result_sender
    }

    #[test]
    fn test_http_flv_session_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "room123/media456".to_string(),
            event_sender,
            response_tx,
        );

        assert_eq!(session.app_name, "live");
        assert_eq!(session.stream_name, "room123/media456");
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
    }

    #[test]
    fn test_flv_session_defaults() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        // Verify default states
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
        assert_eq!(session.consecutive_dropped_frames, 0);
        assert_eq!(session.total_dropped_frames, 0);
    }

    /// Test that flush_response_data drops frames when channel is full and tracks the drop count.
    #[test]
    fn test_flush_drops_frames_when_channel_full() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        // Create a channel with capacity 1 so the first pending response fills it.
        let (response_tx, _response_rx) = mpsc::channel(1);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        // Write FLV header data to the muxer buffer so flush has data to send
        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        session
            .flush_response_data()
            .expect("first response flush should fill the channel");
        assert_eq!(session.consecutive_dropped_frames, 0);

        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        let result = session.flush_response_data();
        // Channel is full (capacity 1, one message pending), frame should be dropped
        assert!(result.is_ok());
        assert_eq!(session.consecutive_dropped_frames, 1);
        assert_eq!(session.total_dropped_frames, 1);
    }

    /// Test that consecutive drops beyond the threshold disconnects the subscriber.
    #[test]
    fn test_slow_subscriber_disconnected_after_max_drops() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        // Channel capacity 1, but we won't read from it
        let (response_tx, _response_rx) = mpsc::channel(1);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        session
            .flush_response_data()
            .expect("initial response flush should fill the channel");

        for i in 0..MAX_CONSECUTIVE_DROPPED_FRAMES {
            session
                .muxer
                .write_flv_header(true, true)
                .expect("header write");
            let result = session.flush_response_data();
            if i < MAX_CONSECUTIVE_DROPPED_FRAMES - 1 {
                assert!(
                    result.is_ok(),
                    "Should not disconnect before reaching threshold (drop #{})",
                    i + 1
                );
            } else {
                assert!(
                    result.is_err(),
                    "Should disconnect after {MAX_CONSECUTIVE_DROPPED_FRAMES} consecutive drops"
                );
                let err_msg = result.unwrap_err().to_string();
                assert!(
                    err_msg.contains("Slow subscriber disconnected"),
                    "Error should mention slow subscriber: {err_msg}"
                );
            }
        }

        assert_eq!(
            session.total_dropped_frames,
            u64::from(MAX_CONSECUTIVE_DROPPED_FRAMES)
        );
    }

    /// Test that consecutive drop counter resets on successful send.
    #[tokio::test]
    async fn test_drop_counter_resets_on_successful_send() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        // Channel capacity 2 so we can control fill/drain
        let (response_tx, mut response_rx) = mpsc::channel(2);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        session
            .flush_response_data()
            .expect("first response flush should fill one channel slot");
        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        session
            .flush_response_data()
            .expect("second response flush should fill one channel slot");

        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        session
            .flush_response_data()
            .expect("full response channel should drop one frame below disconnect threshold");
        assert_eq!(session.consecutive_dropped_frames, 1);

        // Drain the channel
        response_rx.recv().await;
        response_rx.recv().await;

        // Next send should succeed and reset the counter
        session
            .muxer
            .write_flv_header(true, true)
            .expect("header write");
        let result = session.flush_response_data();
        assert!(result.is_ok());
        assert_eq!(
            session.consecutive_dropped_frames, 0,
            "Counter should reset after successful send"
        );
        assert_eq!(
            session.total_dropped_frames, 1,
            "Total should still reflect the one drop"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_session_waits_for_stream_without_idle_timeout_disconnect() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);
        let (frame_tx, frame_rx) = mpsc::channel(8);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender,
            response_tx,
        );
        session.data_receiver = Some(crate::streamhub::define::FrameDataReceiver::bounded(
            frame_rx,
        ));

        let session_task = tokio::spawn(async move { session.send_media_stream().await });

        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        assert!(
            !session_task.is_finished(),
            "session must stay alive during temporary publisher silence"
        );

        drop(frame_tx);

        let result = session_task.await.expect("session task should join");
        assert!(
            result.is_ok(),
            "session should exit cleanly when the stream channel closes: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_send_media_stream_disconnects_slow_subscriber() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(1);
        let (frame_tx, frame_rx) = mpsc::channel(256);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender,
            response_tx,
        );
        session.data_receiver = Some(crate::streamhub::define::FrameDataReceiver::bounded(
            frame_rx,
        ));
        session.has_send_header = true;
        session.has_audio = true;
        session.has_video = true;

        for _ in 0..=MAX_CONSECUTIVE_DROPPED_FRAMES {
            frame_tx
                .send(FrameData::Video {
                    timestamp: 0,
                    data: bytes::Bytes::from_static(b"frame"),
                })
                .await
                .expect("frame send should succeed while receiver is alive");
        }
        drop(frame_tx);

        let err = session
            .send_media_stream()
            .await
            .expect_err("slow subscriber should terminate the session");

        assert!(
            err.to_string().contains("Slow subscriber disconnected"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_unsubscribes_after_slow_subscriber_disconnect() {
        let (event_sender, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (response_tx, _response_rx) = mpsc::channel(1);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender,
            response_tx,
        );

        let session_task = tokio::spawn(async move { session.run().await });

        let subscribe = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("subscribe event should be emitted")
            .expect("event channel should stay open");

        let result_sender = subscribe_result_sender(subscribe);

        let (frame_tx, frame_rx) = mpsc::channel(256);
        result_sender
            .send(Ok((
                crate::streamhub::define::DataReceiver {
                    frame_receiver: Some(crate::streamhub::define::FrameDataReceiver::bounded(
                        frame_rx,
                    )),
                    packet_receiver: None,
                },
                None,
            )))
            .expect("subscribe response should be delivered");

        for _ in 0..=MAX_CONSECUTIVE_DROPPED_FRAMES {
            frame_tx
                .send(FrameData::Video {
                    timestamp: 0,
                    data: bytes::Bytes::from_static(b"frame"),
                })
                .await
                .expect("frame send should succeed while receiver is alive");
        }
        drop(frame_tx);

        let err = tokio::time::timeout(std::time::Duration::from_secs(1), session_task)
            .await
            .expect("session task should finish")
            .expect("session task should join")
            .expect_err("slow subscriber should terminate the session");
        assert!(
            err.to_string().contains("Slow subscriber disconnected"),
            "unexpected error: {err}"
        );

        let unsubscribe = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("unsubscribe event should be emitted")
            .expect("event channel should stay open");

        assert_unsubscribe_event(unsubscribe, "live", "room/stream");
    }

    #[tokio::test]
    async fn test_unsubscribe_retries_when_event_channel_is_temporarily_full() {
        let (event_sender, mut event_rx) = tokio::sync::mpsc::channel(1);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender.clone(),
            response_tx,
        );

        event_sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "live".to_string(),
                    stream_name: "blocker".to_string(),
                },
            })
            .expect("prefill event channel");

        let unsubscribe_task =
            tokio::spawn(async move { session.unsubscribe_from_stream_hub().await });

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !unsubscribe_task.is_finished(),
            "unsubscribe should wait for temporary backpressure instead of succeeding early"
        );

        let first = event_rx
            .recv()
            .await
            .expect("blocked event should still be readable");
        assert!(matches!(first, StreamHubEvent::UnPublish { .. }));

        let unsubscribe = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("unsubscribe should eventually be delivered")
            .expect("event channel should stay open");

        let result = unsubscribe_task
            .await
            .expect("unsubscribe task should join");
        assert!(
            result.is_ok(),
            "unsubscribe should succeed after capacity frees"
        );

        assert_unsubscribe_event(unsubscribe, "live", "room/stream");
    }

    #[tokio::test]
    async fn test_unsubscribe_returns_error_when_event_channel_is_closed() {
        let (event_sender, event_rx) = tokio::sync::mpsc::channel(1);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);
        drop(event_rx);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender,
            response_tx,
        );

        let err = session
            .unsubscribe_from_stream_hub()
            .await
            .expect_err("closed event channel must surface unsubscribe failure");

        let streamhub_err = err
            .downcast_ref::<StreamHubError>()
            .expect("error should preserve streamhub context");
        assert!(matches!(
            streamhub_err.value,
            StreamHubErrorValue::EventChannelClosed
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn test_subscribe_from_stream_hub_times_out_when_result_never_arrives() {
        let (event_sender, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

        let mut session = HttpFlvSession::new(
            "live".to_string(),
            "room/stream".to_string(),
            event_sender,
            response_tx,
        );

        let subscribe_task = tokio::spawn(async move { session.subscribe_from_stream_hub().await });

        let event = event_rx
            .recv()
            .await
            .expect("subscribe event should be emitted");
        assert!(matches!(event, StreamHubEvent::Subscribe { .. }));

        tokio::time::advance(STREAM_SUBSCRIBE_TIMEOUT + std::time::Duration::from_secs(1)).await;

        let err = subscribe_task
            .await
            .expect("task should join")
            .expect_err("subscribe should time out when no result arrives");
        assert!(
            err.to_string().contains("Subscribe timed out"),
            "unexpected error: {err}"
        );

        let rollback = event_rx
            .recv()
            .await
            .expect("timed-out subscribe should emit rollback unsubscribe");
        assert_unsubscribe_event(rollback, "live", "room/stream");
    }
}
