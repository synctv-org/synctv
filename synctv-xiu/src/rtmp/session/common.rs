use crate::streamhub::define::{DataSender, StatisticData, StatisticDataSender};
use tokio::sync::oneshot;

use {
    super::{
        define::SessionType,
        errors::{SessionError, SessionErrorValue},
    },
    crate::rtmp::{
        cache::errors::CacheError,
        cache::SplitCache,
        chunk::{
            define::{chunk_type, csid_type},
            packetizer::ChunkPacketizer,
            ChunkInfo,
        },
        messages::define::msg_type_id,
    },
    crate::streamhub::{
        define::{
            FrameData, FrameDataReceiver, FrameDataSender, FrameTrySendError, NotifyInfo,
            PublishType, PublisherInfo, StreamHubEvent, StreamHubEventSender, SubscribeType,
            SubscriberInfo, TStreamHandler,
        },
        errors::{StreamHubError, StreamHubErrorValue},
        send_event_with_backpressure_timeout,
        stream::StreamIdentifier,
        subscribe_with_rollback_on_timeout,
        utils::Uuid,
        SubscribeWithRollbackError,
    },
    async_trait::async_trait,
    bytes::BytesMut,
    parking_lot::RwLock,
    std::collections::VecDeque,
    std::fmt,
    std::time::{Duration, Instant},
    std::{net::SocketAddr, sync::Arc},
};

// Rate limiting constants for DoS prevention
const MAX_VIDEO_FRAMES_PER_SECOND: usize = 120; // Max 120 FPS video (generous for 60 FPS + margin)
const MAX_AUDIO_FRAMES_PER_SECOND: usize = 200; // Max 200 FPS audio (AAC 48kHz ~47fps, with generous margin)
const MAX_METADATA_FRAMES_PER_SECOND: usize = 10; // Metadata updates are infrequent; 10/s is generous
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);
const STREAM_SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Distinguishes audio vs video for per-track rate limiting.
#[derive(Clone, Copy)]
enum FrameType {
    Video,
    Audio,
    Metadata,
}

impl FrameType {
    const fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Metadata => "metadata",
        }
    }
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn invalid_enhanced_video_data(reason: impl Into<String>) -> SessionError {
    SessionError {
        value: SessionErrorValue::InvalidEnhancedVideoData(reason.into()),
    }
}

fn normalize_enhanced_video_data(data: &mut BytesMut) -> Result<bool, SessionError> {
    const EX_HEADER: u8 = 0x80;
    const PACKET_TYPE_SEQUENCE_START: u8 = 0;
    const PACKET_TYPE_CODED_FRAMES: u8 = 1;
    const PACKET_TYPE_SEQUENCE_END: u8 = 2;
    const PACKET_TYPE_CODED_FRAMES_X: u8 = 3;
    const PACKET_TYPE_METADATA: u8 = 4;

    let Some(&flags) = data.first() else {
        return Err(invalid_enhanced_video_data("empty video message"));
    };
    if flags & EX_HEADER == 0 {
        return Ok(true);
    }
    if data.len() < 5 {
        return Err(invalid_enhanced_video_data(
            "extended header is shorter than the FourCC",
        ));
    }

    let packet_type = flags & 0x0f;
    let frame_type = (flags >> 4) & 0x07;
    let codec_id = match &data[1..5] {
        b"avc1" => crate::flv::define::AvcCodecId::H264 as u8,
        b"hvc1" | b"hev1" => crate::flv::define::AvcCodecId::HEVC as u8,
        fourcc => {
            tracing::warn!(fourcc = ?fourcc, "dropping unsupported enhanced RTMP video codec");
            return Ok(false);
        }
    };

    let legacy_packet_type = match packet_type {
        PACKET_TYPE_SEQUENCE_START => crate::flv::define::avc_packet_type::AVC_SEQHDR,
        PACKET_TYPE_CODED_FRAMES | PACKET_TYPE_CODED_FRAMES_X => {
            crate::flv::define::avc_packet_type::AVC_NALU
        }
        PACKET_TYPE_SEQUENCE_END => crate::flv::define::avc_packet_type::AVC_EOS,
        PACKET_TYPE_METADATA => return Ok(false),
        unsupported => {
            return Err(invalid_enhanced_video_data(format!(
                "packet type {unsupported} is unsupported"
            )));
        }
    };

    let mut normalized = BytesMut::with_capacity(data.len());
    normalized.extend_from_slice(&[frame_type << 4 | codec_id, legacy_packet_type]);
    match packet_type {
        PACKET_TYPE_CODED_FRAMES => {
            if data.len() < 8 {
                return Err(invalid_enhanced_video_data(
                    "coded frame is missing its composition time",
                ));
            }
            normalized.extend_from_slice(&data[5..]);
        }
        PACKET_TYPE_SEQUENCE_START | PACKET_TYPE_SEQUENCE_END | PACKET_TYPE_CODED_FRAMES_X => {
            normalized.extend_from_slice(&[0, 0, 0]);
            normalized.extend_from_slice(&data[5..]);
        }
        _ => unreachable!("enhanced RTMP packet type was validated above"),
    }
    *data = normalized;
    Ok(true)
}

fn try_send_prior(
    sender: &FrameDataSender,
    data: FrameData,
    name: &str,
) -> Result<(), StreamHubError> {
    match sender.try_send(data) {
        Ok(()) => Ok(()),
        Err(FrameTrySendError::Full(_)) => {
            tracing::warn!("send_prior_data: {} dropped due to channel full", name);
            Ok(())
        }
        Err(FrameTrySendError::Closed(_)) => Err(StreamHubError {
            value: StreamHubErrorValue::SubscriberClosed,
        }),
    }
}

fn remote_addr_to_string(remote_addr: Option<SocketAddr>) -> String {
    remote_addr.map_or_else(String::new, |addr| addr.to_string())
}

pub struct Common {
    // Stable subscriber/publisher id used by StreamHub maps and statistics.
    session_id: Uuid,
    // Present for sessions that write RTMP chunks to a peer.
    packetizer: Option<ChunkPacketizer>,

    // Set when the session actually subscribes (receiver) or publishes (sender).
    data_receiver: Option<FrameDataReceiver>,
    data_sender: Option<FrameDataSender>,

    event_producer: StreamHubEventSender,
    pub session_type: SessionType,

    // Remote peer address used in subscriber/publisher statistics.
    remote_addr: Option<SocketAddr>,
    // Original request URL from the RTMP peer.
    pub request_url: String,
    pub stream_handler: Arc<RtmpStreamHandler>,
    // StreamHub statistics sink returned by subscribe/publish.
    statistic_data_sender: Option<StatisticDataSender>,

    // Separate per-track rate limiting for DoS prevention (sliding window).
    // Audio and video use independent rate limiters so that high audio frame rates
    // (e.g. AAC 48kHz) do not exhaust the video budget and vice versa.
    video_timestamps: VecDeque<Instant>,
    audio_timestamps: VecDeque<Instant>,
    metadata_timestamps: VecDeque<Instant>,
}

impl Common {
    #[must_use]
    pub fn new(
        packetizer: Option<ChunkPacketizer>,
        event_producer: StreamHubEventSender,
        session_type: SessionType,
        remote_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            session_id: Uuid::new(),
            packetizer,

            data_sender: None,
            data_receiver: None,

            event_producer,
            session_type,
            remote_addr,
            request_url: String::default(),
            stream_handler: Arc::new(RtmpStreamHandler::new()),
            statistic_data_sender: None,
            video_timestamps: VecDeque::with_capacity(MAX_VIDEO_FRAMES_PER_SECOND),
            audio_timestamps: VecDeque::with_capacity(MAX_AUDIO_FRAMES_PER_SECOND),
            metadata_timestamps: VecDeque::with_capacity(MAX_METADATA_FRAMES_PER_SECOND),
        }
    }

    #[must_use]
    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    // Check per-track frame rate limit before accepting new frame.
    // Audio and video have independent sliding-window counters so that one track's
    // high frame rate cannot starve the other.
    fn check_rate_limit(&mut self, frame_type: FrameType) -> bool {
        let (timestamps, max_fps) = match frame_type {
            FrameType::Video => (&mut self.video_timestamps, MAX_VIDEO_FRAMES_PER_SECOND),
            FrameType::Audio => (&mut self.audio_timestamps, MAX_AUDIO_FRAMES_PER_SECOND),
            FrameType::Metadata => (
                &mut self.metadata_timestamps,
                MAX_METADATA_FRAMES_PER_SECOND,
            ),
        };

        let now = Instant::now();

        // Remove timestamps outside the sliding window
        while let Some(&oldest) = timestamps.front() {
            if now.duration_since(oldest) > RATE_LIMIT_WINDOW {
                timestamps.pop_front();
            } else {
                break;
            }
        }

        // Check if we've exceeded the rate limit
        if timestamps.len() >= max_fps {
            return false;
        }

        // Add current timestamp and allow frame
        timestamps.push_back(now);
        true
    }

    fn accept_frame_with_rate_limit(&mut self, frame_type: FrameType) -> bool {
        if self.check_rate_limit(frame_type) {
            return true;
        }

        tracing::warn!(
            frame_type = frame_type.name(),
            "frame dropped because the per-track rate limit was exceeded"
        );
        false
    }

    fn send_frame_to_channel(
        &self,
        channel_data: FrameData,
        label: &str,
    ) -> Result<(), SessionError> {
        if let Some(sender) = &self.data_sender {
            match sender.try_send(channel_data) {
                Ok(()) => {}
                Err(FrameTrySendError::Full(_)) => {
                    tracing::warn!("{label} frame dropped due to channel full");
                }
                Err(FrameTrySendError::Closed(_)) => {
                    return Err(SessionError {
                        value: SessionErrorValue::SendFrameDataErr,
                    });
                }
            }
        } else {
            return Err(SessionError {
                value: SessionErrorValue::NoneFrameDataSender,
            });
        }

        Ok(())
    }

    pub async fn send_channel_data(&mut self) -> Result<(), SessionError> {
        let mut receiver = self.data_receiver.take().ok_or(SessionError {
            value: SessionErrorValue::NoneFrameDataReceiver,
        })?;
        loop {
            if let Some(data) = receiver.recv().await {
                match data {
                    FrameData::Audio { timestamp, data } => {
                        let data_size = data.len();
                        self.send_audio(BytesMut::from(data), timestamp).await?;

                        if let Some(sender) = &self.statistic_data_sender {
                            let statistic_audio_data = StatisticData::Audio {
                                uuid: Some(self.session_id),
                                aac_packet_type: 1,
                                data_size,
                                duration: 0,
                            };
                            if let Err(err) = sender.send(statistic_audio_data) {
                                tracing::error!("send statistic_data err: {err}");
                            }
                        }
                    }
                    FrameData::Video { timestamp, data } => {
                        let data_size = data.len();
                        self.send_video(BytesMut::from(data), timestamp).await?;

                        if let Some(sender) = &self.statistic_data_sender {
                            let statistic_video_data = StatisticData::Video {
                                uuid: Some(self.session_id),
                                frame_count: 1,
                                data_size,
                                is_key_frame: None,
                                duration: 0,
                            };
                            if let Err(err) = sender.send(statistic_video_data) {
                                tracing::error!("send statistic_data err: {err}");
                            }
                        }
                    }
                    FrameData::MetaData { timestamp, data } => {
                        self.send_metadata(BytesMut::from(data), timestamp).await?;
                    }
                    FrameData::MediaInfo { .. } => {}
                }
            } else {
                // recv() returning None means all senders are dropped -- channel is permanently closed.
                return Err(SessionError {
                    value: SessionErrorValue::NoMediaDataReceived,
                });
            }
        }
    }

    pub async fn send_audio(&mut self, data: BytesMut, timestamp: u32) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::AUDIO,
            chunk_type::TYPE_0,
            timestamp,
            usize_to_u32_saturating(data.len()),
            msg_type_id::AUDIO,
            0,
            data,
        );

        if let Some(packetizer) = &mut self.packetizer {
            packetizer.write_chunk(&mut chunk_info).await?;
        }

        Ok(())
    }

    pub async fn send_video(&mut self, data: BytesMut, timestamp: u32) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::VIDEO,
            chunk_type::TYPE_0,
            timestamp,
            usize_to_u32_saturating(data.len()),
            msg_type_id::VIDEO,
            0,
            data,
        );

        if let Some(packetizer) = &mut self.packetizer {
            packetizer.write_chunk(&mut chunk_info).await?;
        }

        Ok(())
    }

    pub async fn send_metadata(
        &mut self,
        data: BytesMut,
        timestamp: u32,
    ) -> Result<(), SessionError> {
        let mut chunk_info = ChunkInfo::new(
            csid_type::DATA_AMF0_AMF3,
            chunk_type::TYPE_0,
            timestamp,
            usize_to_u32_saturating(data.len()),
            msg_type_id::DATA_AMF0,
            0,
            data,
        );

        if let Some(packetizer) = &mut self.packetizer {
            packetizer.write_chunk(&mut chunk_info).await?;
        }

        Ok(())
    }

    pub(crate) fn on_video_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: u32,
    ) -> Result<(), SessionError> {
        if !normalize_enhanced_video_data(data)? {
            return Ok(());
        }
        if !self.accept_frame_with_rate_limit(FrameType::Video) {
            return Ok(());
        }

        let frame = data.split().freeze();
        let channel_data = FrameData::Video {
            timestamp,
            data: frame,
        };

        self.send_frame_to_channel(channel_data, "Video")
    }

    pub(crate) fn on_audio_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: u32,
    ) -> Result<(), SessionError> {
        if !self.accept_frame_with_rate_limit(FrameType::Audio) {
            return Ok(());
        }

        let frame = data.split().freeze();
        let channel_data = FrameData::Audio {
            timestamp,
            data: frame,
        };

        self.send_frame_to_channel(channel_data, "Audio")
    }

    pub(crate) fn on_meta_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: u32,
    ) -> Result<(), SessionError> {
        if !self.accept_frame_with_rate_limit(FrameType::Metadata) {
            return Ok(());
        }

        let frame = data.split().freeze();
        let channel_data = FrameData::MetaData {
            timestamp,
            data: frame,
        };

        self.send_frame_to_channel(channel_data, "Metadata")
    }

    fn get_subscriber_info(&self) -> SubscriberInfo {
        let remote_addr = remote_addr_to_string(self.remote_addr);

        let sub_type = match self.session_type {
            SessionType::Client => SubscribeType::RtmpRelay,
            SessionType::Server => SubscribeType::RtmpPull,
        };

        SubscriberInfo {
            id: self.session_id,
            sub_type,
            sub_data_type: crate::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: self.request_url.clone(),
                remote_addr,
            },
        }
    }

    fn get_publisher_info(&self) -> PublisherInfo {
        let remote_addr = remote_addr_to_string(self.remote_addr);

        let pub_type = match self.session_type {
            SessionType::Client => PublishType::RtmpRelay,
            SessionType::Server => PublishType::RtmpPush,
        };

        PublisherInfo {
            id: self.session_id,
            pub_type,
            pub_data_type: crate::streamhub::define::PubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: self.request_url.clone(),
                remote_addr,
            },
        }
    }

    pub async fn subscribe_from_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
    ) -> Result<(), SessionError> {
        tracing::info!(
            "subscribe_from_stream_hub, app_name: {} stream_name: {} subscribe_id: {}",
            app_name,
            stream_name,
            self.session_id
        );

        let identifier = StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        };

        let subscriber_info = self.get_subscriber_info();
        let result = subscribe_with_rollback_on_timeout(
            &self.event_producer,
            identifier,
            subscriber_info,
            STREAM_SUBSCRIBE_TIMEOUT,
        )
        .await
        .map_err(|err| match err {
            SubscribeWithRollbackError::Timeout => SessionError {
                value: SessionErrorValue::Timeout,
            },
            SubscribeWithRollbackError::StreamHub(_) => SessionError {
                value: SessionErrorValue::StreamHubEventSendErr,
            },
        })?;
        self.data_receiver = Some(result.0.frame_receiver.ok_or(SessionError {
            value: SessionErrorValue::StreamHubEventSendErr,
        })?);

        let statistic_data_sender: Option<StatisticDataSender> = result.1;

        if let Some(sender) = &statistic_data_sender {
            let statistic_subscriber = StatisticData::Subscriber {
                id: self.session_id,
                remote_addr: remote_addr_to_string(self.remote_addr),
                start_time: chrono::Local::now(),
                sub_type: SubscribeType::RtmpPull,
            };
            if let Err(err) = sender.send(statistic_subscriber) {
                tracing::error!("send statistic_subscriber err: {err}");
            }
        }

        self.statistic_data_sender = statistic_data_sender;

        Ok(())
    }

    pub async fn unsubscribe_from_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
    ) -> Result<(), SessionError> {
        let identifier = StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        };

        let subscribe_event = StreamHubEvent::UnSubscribe {
            identifier,
            info: self.get_subscriber_info(),
        };
        send_event_with_backpressure_timeout(&self.event_producer, subscribe_event)
            .await
            .map_err(|err| {
                tracing::error!("unsubscribe_from_stream_hub err {err}");
                SessionError {
                    value: SessionErrorValue::ChannelError(err),
                }
            })?;

        Ok(())
    }

    pub async fn publish_to_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
        gop_num: usize,
        per_stream_max_bytes: Option<usize>,
    ) -> Result<(), SessionError> {
        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let info = self.get_publisher_info();
        let remote_addr = info.notify_info.remote_addr.clone();

        let publish_event = StreamHubEvent::Publish {
            identifier: StreamIdentifier::Rtmp {
                app_name: app_name.clone(),
                stream_name: stream_name.clone(),
            },
            info,
            stream_handler: self.stream_handler.clone(),
            result_sender: event_result_sender,
        };

        send_event_with_backpressure_timeout(&self.event_producer, publish_event)
            .await
            .map_err(|err| SessionError {
                value: SessionErrorValue::ChannelError(err),
            })?;

        let result = event_result_receiver.await??;
        self.data_sender = Some(result.0.ok_or(SessionError {
            value: SessionErrorValue::StreamHubEventSendErr,
        })?);

        let statistic_data_sender: Option<StatisticDataSender> = result.2;

        if let Some(sender) = &statistic_data_sender {
            let statistic_publisher = StatisticData::Publisher {
                id: self.session_id,
                remote_addr,
                start_time: chrono::Local::now(),
            };
            if let Err(err) = sender.send(statistic_publisher) {
                tracing::error!("send statistic_publisher err: {err}");
            }
        }

        let cache = SplitCache::new(gop_num, per_stream_max_bytes, statistic_data_sender);
        self.stream_handler.set_cache(cache);
        Ok(())
    }

    pub async fn unpublish_to_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
    ) -> Result<(), SessionError> {
        tracing::info!("unpublish_to_stream_hub, app_name:{app_name}, stream_name:{stream_name}");
        let unpublish_event = StreamHubEvent::UnPublish {
            identifier: StreamIdentifier::Rtmp {
                app_name: app_name.clone(),
                stream_name: stream_name.clone(),
            },
            generation_id: self.session_id,
        };

        match send_event_with_backpressure_timeout(&self.event_producer, unpublish_event).await {
            Err(err) => {
                tracing::error!(
                    "unpublish_to_stream_hub error.app_name: {app_name}, stream_name: {stream_name}, err: {err}"
                );
                return Err(SessionError {
                    value: SessionErrorValue::ChannelError(err),
                });
            }
            Ok(()) => {
                tracing::info!(
                    "unpublish_to_stream_hub succeeded, app_name: {app_name}, stream_name: {stream_name}"
                );
            }
        }
        Ok(())
    }
}

/// RTMP stream handler with split cache for reduced lock contention.
///
/// Uses parking_lot::RwLock instead of `tokio::sync::Mutex` because:
/// 1. Cache operations are synchronous (no async points inside)
/// 2. RwLock allows concurrent reads from multiple subscribers
/// 3. parking_lot has better performance under contention
///
/// The cache is split into independent components:
/// - video_seq: Video sequence header (infrequent updates)
/// - audio_seq: Audio sequence header (infrequent updates)
/// - `metadata`: Stream metadata (infrequent updates)
/// - `gops`: GOP cache (frequent updates, shared by audio and video)
///
/// This design allows:
/// - Concurrent reads from different subscribers
/// - Video and audio saves to proceed in parallel (except for GOP)
/// - Metadata reads without blocking frame processing
pub struct RtmpStreamHandler {
    /// Cached sequence headers, metadata, and GOPs shared by publisher and subscribers.
    pub cache: RwLock<Option<Arc<SplitCache>>>,
}

impl RtmpStreamHandler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(Some(Arc::new(SplitCache::new(2, None, None)))),
        }
    }

    /// Set the cache (called once when publishing starts).
    /// Uses a blocking write lock but is only called once per stream.
    pub(crate) fn set_cache(&self, cache: SplitCache) {
        *self.cache.write() = Some(Arc::new(cache));
    }

    /// Save video data to cache.
    /// Acquires write locks only on video_seq and gops, not on audio_seq or metadata.
    pub(crate) fn save_video_data(
        &self,
        chunk_body: &bytes::Bytes,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &*self.cache.read() {
            cache.save_video_data(chunk_body, timestamp)?;
        }
        Ok(())
    }

    /// Save audio data to cache.
    /// Acquires write locks only on audio_seq and gops, not on video_seq or metadata.
    pub(crate) fn save_audio_data(
        &self,
        chunk_body: &bytes::Bytes,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &*self.cache.read() {
            cache.save_audio_data(chunk_body, timestamp)?;
        }
        Ok(())
    }

    /// Save metadata to cache.
    /// Acquires write lock only on metadata, not on video_seq, audio_seq, or gops.
    pub(crate) fn save_metadata(&self, chunk_body: &bytes::Bytes, timestamp: u32) {
        if let Some(cache) = &*self.cache.read() {
            cache.save_metadata(chunk_body, timestamp);
        }
    }
}

impl Default for RtmpStreamHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TStreamHandler for RtmpStreamHandler {
    fn save_frame_data(&self, frame: &FrameData) -> Result<(), StreamHubError> {
        let result = match frame {
            FrameData::Video { timestamp, data } => self.save_video_data(data, *timestamp),
            FrameData::Audio { timestamp, data } => self.save_audio_data(data, *timestamp),
            FrameData::MetaData { timestamp, data } => {
                self.save_metadata(data, *timestamp);
                return Ok(());
            }
            FrameData::MediaInfo { .. } => return Ok(()),
        };
        result.map_err(|error| StreamHubError {
            value: StreamHubErrorValue::InternalTaskError(format!(
                "publisher frame cache rejected media: {error}"
            )),
        })
    }

    async fn send_prior_data(
        &self,
        data_sender: DataSender,
        sub_type: SubscribeType,
    ) -> Result<(), StreamHubError> {
        let sender = match data_sender {
            DataSender::Frame { sender } => sender,
            DataSender::Packet { sender: _ } => {
                return Err(StreamHubError {
                    value: StreamHubErrorValue::NotCorrectDataSenderType,
                });
            }
        };

        // Read cache reference with minimal lock time
        let cache_ref = {
            let guard = self.cache.read();
            guard.clone()
        };

        if let Some(cache) = cache_ref {
            // Send metadata (uses separate read lock from audio/video seq)
            if let Some(meta_body_data) = cache.get_metadata() {
                tracing::info!("send_prior_data: meta_body_data: ");
                try_send_prior(&sender, meta_body_data, "metadata")?;
            }

            // Send audio sequence header (uses separate read lock)
            if let Some(audio_seq_data) = cache.get_audio_seq() {
                tracing::info!("send_prior_data: audio_seq_data: ",);
                try_send_prior(&sender, audio_seq_data, "audio seq")?;
            }

            // Send video sequence header (uses separate read lock)
            if let Some(video_seq_data) = cache.get_video_seq() {
                tracing::info!("send_prior_data: video_seq_data:");
                try_send_prior(&sender, video_seq_data, "video seq")?;
            }

            // Send GOP data for relevant subscriber types
            match sub_type {
                SubscribeType::RtmpPull
                | SubscribeType::RtmpRemux2HttpFlv
                | SubscribeType::RtmpRemux2Hls => {
                    // get_gops_data clones the GOPs, so we can send without holding the lock
                    if let Some(gops_data) = cache.get_gops_data() {
                        for gop in gops_data {
                            for channel_data in gop.frame_data() {
                                try_send_prior(&sender, channel_data.clone(), "gop frame")?;
                            }
                        }
                    }
                }
                SubscribeType::RtmpRelay | SubscribeType::WhepPull => {}
            }
        }

        Ok(())
    }
}

impl fmt::Debug for Common {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(fmt, "S2 {{ member: {:?} }}", self.request_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtmp::session::define::SessionType;
    use crate::streamhub::define::FrameDataSender;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn send_publish_result(event: StreamHubEvent) {
        let StreamHubEvent::Publish { result_sender, .. } = event else {
            panic!("expected publish event");
        };

        let (frame_sender, _frame_receiver) = mpsc::channel(8);
        let (stat_sender, _stat_receiver) = mpsc::unbounded_channel();
        result_sender
            .send(Ok((
                Some(FrameDataSender::bounded(frame_sender)),
                None,
                Some(stat_sender),
            )))
            .expect("publish result should be delivered");
    }

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

    fn assert_unpublish_event(event: StreamHubEvent, expected_app: &str, expected_stream: &str) {
        let StreamHubEvent::UnPublish { identifier, .. } = event else {
            panic!("expected unpublish event, got {event:?}");
        };
        assert_rtmp_identifier(identifier, expected_app, expected_stream);
    }

    #[test]
    fn remote_addr_to_string_uses_empty_string_for_absent_address() {
        assert_eq!(remote_addr_to_string(None), "");
    }

    #[test]
    fn remote_addr_to_string_formats_present_address() {
        let addr = "127.0.0.1:1935".parse().expect("valid socket address");
        assert_eq!(remote_addr_to_string(Some(addr)), "127.0.0.1:1935");
    }

    #[test]
    fn enhanced_hevc_sequence_start_is_normalized_to_legacy_hvcc() {
        let mut data = BytesMut::from(&b"\x90hvc1\x01\x02\x03"[..]);
        assert!(normalize_enhanced_video_data(&mut data).expect("valid sequence start"));
        assert_eq!(&data[..], b"\x1c\x00\x00\x00\x00\x01\x02\x03");
    }

    #[test]
    fn enhanced_hevc_coded_frames_x_is_normalized_with_zero_composition_time() {
        let mut data = BytesMut::from(&b"\xa3hvc1\x00\x00\x00\x02\x26\x01"[..]);
        assert!(normalize_enhanced_video_data(&mut data).expect("valid coded frame"));
        assert_eq!(&data[..], b"\x2c\x01\x00\x00\x00\x00\x00\x00\x02\x26\x01");
    }

    #[test]
    fn enhanced_hevc_coded_frames_preserves_signed_composition_time_bytes() {
        let mut data = BytesMut::from(&b"\x91hev1\xff\xff\xfe\x00\x00\x00\x02\x26\x01"[..]);
        assert!(normalize_enhanced_video_data(&mut data).expect("valid coded frame"));
        assert_eq!(&data[..], b"\x1c\x01\xff\xff\xfe\x00\x00\x00\x02\x26\x01");
    }

    #[test]
    fn enhanced_video_rejects_truncated_and_unknown_packet_types() {
        let truncated = normalize_enhanced_video_data(&mut BytesMut::from(&b"\x90hvc"[..]))
            .expect_err("truncated FourCC must fail");
        assert!(matches!(
            truncated.value,
            SessionErrorValue::InvalidEnhancedVideoData(_)
        ));

        let unknown = normalize_enhanced_video_data(&mut BytesMut::from(&b"\x96hvc1"[..]))
            .expect_err("multitrack packet must fail until multitrack is supported");
        assert!(matches!(
            unknown.value,
            SessionErrorValue::InvalidEnhancedVideoData(_)
        ));
    }

    #[test]
    fn rate_limit_rejects_frames_after_track_budget_is_exhausted() {
        let (event_sender, _event_rx) = mpsc::channel(1);
        let mut common = Common::new(None, event_sender, SessionType::Server, None);

        for _ in 0..MAX_METADATA_FRAMES_PER_SECOND {
            assert!(common.accept_frame_with_rate_limit(FrameType::Metadata));
        }

        assert!(!common.accept_frame_with_rate_limit(FrameType::Metadata));
    }

    #[tokio::test]
    async fn test_unsubscribe_retries_when_event_channel_is_temporarily_full() {
        let (event_sender, mut event_rx) = mpsc::channel(1);
        let mut common = Common::new(None, event_sender.clone(), SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        event_sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "live".to_string(),
                    stream_name: "blocker".to_string(),
                },
                generation_id: Uuid::new(),
            })
            .expect("prefill event channel");

        let unsubscribe_task = tokio::spawn(async move {
            common
                .unsubscribe_from_stream_hub("live".to_string(), "room/stream".to_string())
                .await
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !unsubscribe_task.is_finished(),
            "unsubscribe should wait for temporary backpressure instead of succeeding early"
        );

        let first = event_rx
            .recv()
            .await
            .expect("blocked event should be readable");
        assert!(matches!(first, StreamHubEvent::UnPublish { .. }));

        let unsubscribe = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
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
        let (event_sender, event_rx) = mpsc::channel(1);
        drop(event_rx);

        let mut common = Common::new(None, event_sender, SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();
        common.stream_handler = Arc::new(RtmpStreamHandler::new());

        let err = common
            .unsubscribe_from_stream_hub("live".to_string(), "room/stream".to_string())
            .await
            .expect_err("closed event channel must surface unsubscribe failure");

        assert!(matches!(err.value, SessionErrorValue::ChannelError(_)));
    }

    #[tokio::test]
    async fn test_publish_retries_when_event_channel_is_temporarily_full() {
        let (event_sender, mut event_rx) = mpsc::channel(1);
        let mut common = Common::new(None, event_sender.clone(), SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        event_sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "live".to_string(),
                    stream_name: "blocker".to_string(),
                },
                generation_id: Uuid::new(),
            })
            .expect("prefill event channel");

        let publish_task = tokio::spawn(async move {
            common
                .publish_to_stream_hub("live".to_string(), "room/stream".to_string(), 1, None)
                .await
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !publish_task.is_finished(),
            "publish should wait for temporary backpressure"
        );

        let first = event_rx
            .recv()
            .await
            .expect("blocked event should be readable");
        assert!(matches!(first, StreamHubEvent::UnPublish { .. }));

        let publish = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("publish should eventually be delivered")
            .expect("event channel should stay open");
        send_publish_result(publish);

        let result = publish_task.await.expect("publish task should join");
        assert!(
            result.is_ok(),
            "publish should succeed after capacity frees"
        );
    }

    #[tokio::test]
    async fn test_publish_returns_error_when_event_channel_is_closed() {
        let (event_sender, event_rx) = mpsc::channel(1);
        drop(event_rx);

        let mut common = Common::new(None, event_sender, SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        let err = common
            .publish_to_stream_hub("live".to_string(), "room/stream".to_string(), 1, None)
            .await
            .expect_err("closed event channel must surface publish failure");

        assert!(matches!(err.value, SessionErrorValue::ChannelError(_)));
    }

    #[tokio::test]
    async fn test_unpublish_retries_when_event_channel_is_temporarily_full() {
        let (event_sender, mut event_rx) = mpsc::channel(1);
        let mut common = Common::new(None, event_sender.clone(), SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        event_sender
            .try_send(StreamHubEvent::UnSubscribe {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "live".to_string(),
                    stream_name: "blocker".to_string(),
                },
                info: common.get_subscriber_info(),
            })
            .expect("prefill event channel");

        let unpublish_task = tokio::spawn(async move {
            common
                .unpublish_to_stream_hub("live".to_string(), "room/stream".to_string())
                .await
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !unpublish_task.is_finished(),
            "unpublish should wait for temporary backpressure instead of failing early"
        );

        let first = event_rx
            .recv()
            .await
            .expect("blocked event should be readable");
        assert!(matches!(first, StreamHubEvent::UnSubscribe { .. }));

        let unpublish = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("unpublish should eventually be delivered")
            .expect("event channel should stay open");

        let result = unpublish_task.await.expect("unpublish task should join");
        assert!(
            result.is_ok(),
            "unpublish should succeed after capacity frees"
        );

        assert_unpublish_event(unpublish, "live", "room/stream");
    }

    #[tokio::test]
    async fn test_unpublish_returns_error_when_event_channel_is_closed() {
        let (event_sender, event_rx) = mpsc::channel(1);
        drop(event_rx);

        let mut common = Common::new(None, event_sender, SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        let err = common
            .unpublish_to_stream_hub("live".to_string(), "room/stream".to_string())
            .await
            .expect_err("closed event channel must surface unpublish failure");

        assert!(matches!(err.value, SessionErrorValue::ChannelError(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_subscribe_from_stream_hub_times_out_when_result_never_arrives() {
        let (event_sender, mut event_rx) = mpsc::channel(1);
        let mut common = Common::new(None, event_sender, SessionType::Server, None);
        common.request_url = "/live/room/stream".to_string();

        let subscribe_task = tokio::spawn(async move {
            common
                .subscribe_from_stream_hub("live".to_string(), "room/stream".to_string())
                .await
        });

        let event = event_rx
            .recv()
            .await
            .expect("subscribe event should be delivered");
        assert!(matches!(event, StreamHubEvent::Subscribe { .. }));

        tokio::time::advance(STREAM_SUBSCRIBE_TIMEOUT + Duration::from_secs(1)).await;

        let err = subscribe_task
            .await
            .expect("subscribe task should join")
            .expect_err("subscribe should time out when the streamhub result never arrives");

        assert!(matches!(err.value, SessionErrorValue::Timeout));

        let rollback = event_rx
            .recv()
            .await
            .expect("timed-out subscribe should emit rollback unsubscribe");
        assert_unsubscribe_event(rollback, "live", "room/stream");
    }
}
