use crate::streamhub::define::{DataSender, StatisticData, StatisticDataSender};
use tokio::sync::oneshot;

use {
    super::{
        define::SessionType,
        errors::{SessionError, SessionErrorValue},
    },
    crate::rtmp::{
        cache::errors::CacheError,
        cache::Cache,
        chunk::{
            define::{chunk_type, csid_type},
            packetizer::ChunkPacketizer,
            ChunkInfo,
        },
        messages::define::msg_type_id,
    },
    async_trait::async_trait,
    bytes::BytesMut,
    std::fmt,
    std::{net::SocketAddr, sync::Arc},
    std::collections::VecDeque,
    std::time::{Duration, Instant},
    crate::streamhub::{
        define::{
            FrameData, FrameDataReceiver, FrameDataSender, NotifyInfo,
            PublishType, PublisherInfo, StreamHubEvent, StreamHubEventSender, SubscribeType,
            SubscriberInfo, TStreamHandler,
        },
        errors::{StreamHubError, StreamHubErrorValue},
        stream::StreamIdentifier,
        utils::Uuid,
    },
    tokio::sync::{mpsc, Mutex},
};

// Fixed #107: Rate limiting constants for DoS prevention
const MAX_FRAMES_PER_SECOND: usize = 120; // Max 120 FPS (generous for 60 FPS + margin)
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

pub struct Common {
    /* Used to mark the subscriber's the data producer
    in channels and delete it from map when unsubscribe
    is called. */
    session_id: Uuid,
    //only Server Subscriber or Client Publisher needs to send out trunck data.
    packetizer: Option<ChunkPacketizer>,

    // L-6: Use Option to avoid allocating unused channels at construction.
    // Set when the session actually subscribes (receiver) or publishes (sender).
    data_receiver: Option<FrameDataReceiver>,
    data_sender: Option<FrameDataSender>,

    event_producer: StreamHubEventSender,
    pub session_type: SessionType,

    /*save the client side socket connected to the SeverSession */
    remote_addr: Option<SocketAddr>,
    /*request URL from client*/
    pub request_url: String,
    pub stream_handler: Arc<RtmpStreamHandler>,
    /* now used for subscriber session */
    statistic_data_sender: Option<StatisticDataSender>,

    // Fixed #107: Frame rate limiting for DoS prevention (sliding window)
    frame_timestamps: VecDeque<Instant>,
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
            frame_timestamps: VecDeque::with_capacity(MAX_FRAMES_PER_SECOND),
        }
    }

    // Fixed #107: Check frame rate limit before accepting new frame
    fn check_rate_limit(&mut self) -> bool {
        let now = Instant::now();

        // Remove timestamps outside the sliding window
        while let Some(&oldest) = self.frame_timestamps.front() {
            if now.duration_since(oldest) > RATE_LIMIT_WINDOW {
                self.frame_timestamps.pop_front();
            } else {
                break;
            }
        }

        // Check if we've exceeded the rate limit
        if self.frame_timestamps.len() >= MAX_FRAMES_PER_SECOND {
            return false;
        }

        // Add current timestamp and allow frame
        self.frame_timestamps.push_back(now);
        true
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
                        self.send_audio(BytesMut::from(&data[..]), timestamp).await?;

                        if let Some(sender) = &self.statistic_data_sender {
                            let statistic_audio_data = StatisticData::Audio {
                                uuid: Some(self.session_id),
                                aac_packet_type: 1,
                                data_size,
                                duration: 0,
                            };
                            if let Err(err) = sender.try_send(statistic_audio_data) {
                                tracing::error!("send statistic_data err: {err}");
                            }
                        }
                    }
                    FrameData::Video { timestamp, data } => {
                        let data_size = data.len();
                        self.send_video(BytesMut::from(&data[..]), timestamp).await?;

                        if let Some(sender) = &self.statistic_data_sender {
                            let statistic_video_data = StatisticData::Video {
                                uuid: Some(self.session_id),
                                frame_count: 1,
                                data_size,
                                is_key_frame: None,
                                duration: 0,
                            };
                            if let Err(err) = sender.try_send(statistic_video_data) {
                                tracing::error!("send statistic_data err: {err}");
                            }
                        }
                    }
                    FrameData::MetaData { timestamp, data } => {
                        self.send_metadata(BytesMut::from(&data[..]), timestamp).await?;
                    }
                    _ => {}
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
            data.len() as u32,
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
            data.len() as u32,
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
            data.len() as u32,
            msg_type_id::DATA_AMF0,
            0,
            data,
        );

        if let Some(packetizer) = &mut self.packetizer {
            packetizer.write_chunk(&mut chunk_info).await?;
        }

        Ok(())
    }

    pub async fn on_video_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        // Fixed #107: Apply frame rate limiting to prevent DoS attacks
        if !self.check_rate_limit() {
            tracing::warn!(
                "Video frame dropped: rate limit exceeded ({} FPS max)",
                MAX_FRAMES_PER_SECOND
            );
            return Ok(()); // Drop frame silently
        }

        let channel_data = FrameData::Video {
            timestamp: *timestamp,
            data: bytes::Bytes::copy_from_slice(data),
        };

        if let Some(sender) = &self.data_sender {
            match sender.try_send(channel_data) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("Video frame dropped due to channel full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
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

        self.stream_handler
            .save_video_data(data, *timestamp)
            .await?;

        Ok(())
    }

    pub async fn on_audio_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        // Fixed #107: Apply frame rate limiting to prevent DoS attacks
        if !self.check_rate_limit() {
            tracing::warn!(
                "Audio frame dropped: rate limit exceeded ({} FPS max)",
                MAX_FRAMES_PER_SECOND
            );
            return Ok(()); // Drop frame silently
        }

        let channel_data = FrameData::Audio {
            timestamp: *timestamp,
            data: bytes::Bytes::copy_from_slice(data),
        };

        if let Some(sender) = &self.data_sender {
            match sender.try_send(channel_data) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("Audio frame dropped due to channel full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
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

        self.stream_handler
            .save_audio_data(data, *timestamp)
            .await?;

        Ok(())
    }

    pub async fn on_meta_data(
        &mut self,
        data: &mut BytesMut,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        let channel_data = FrameData::MetaData {
            timestamp: *timestamp,
            data: bytes::Bytes::copy_from_slice(data),
        };

        if let Some(sender) = &self.data_sender {
            match sender.try_send(channel_data) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("Metadata frame dropped due to channel full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
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

        self.stream_handler.save_metadata(data, *timestamp).await;

        Ok(())
    }

    fn get_subscriber_info(&mut self) -> SubscriberInfo {
        let remote_addr = if let Some(addr) = self.remote_addr {
            addr.to_string()
        } else {
            String::from("unknown")
        };

        let sub_type = match self.session_type {
            SessionType::Client => SubscribeType::RtmpRelay,
            SessionType::Server => SubscribeType::RtmpPull,
        };

        SubscriberInfo {
            id: self.session_id,
            /*rtmp local client subscribe from local rtmp session
            and publish(relay) the rtmp steam to remote RTMP server*/
            sub_type,
            sub_data_type: crate::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: self.request_url.clone(),
                remote_addr,
            },
        }
    }

    fn get_publisher_info(&mut self) -> PublisherInfo {
        let remote_addr = if let Some(addr) = self.remote_addr {
            addr.to_string()
        } else {
            String::from("unknown")
        };

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

    /* Subscribe from stream hub and push stream data to players or other rtmp nodes */
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

        let (event_result_sender, event_result_receiver) = oneshot::channel();

        let subscribe_event = StreamHubEvent::Subscribe {
            identifier,
            info: self.get_subscriber_info(),
            result_sender: event_result_sender,
        };
        let rv = self.event_producer.try_send(subscribe_event);

        if rv.is_err() {
            return Err(SessionError {
                value: SessionErrorValue::StreamHubEventSendErr,
            });
        }

        let result = event_result_receiver.await??;
        self.data_receiver = Some(result.0.frame_receiver.ok_or(SessionError {
            value: SessionErrorValue::StreamHubEventSendErr,
        })?);

        let statistic_data_sender: Option<StatisticDataSender> = result.1;

        if let Some(sender) = &statistic_data_sender {
            let statistic_subscriber = StatisticData::Subscriber {
                id: self.session_id,
                remote_addr: self.remote_addr.map_or_else(|| "unknown".to_string(), |a| a.to_string()),
                start_time: chrono::Local::now(),
                sub_type: SubscribeType::RtmpPull,
            };
            if let Err(err) = sender.try_send(statistic_subscriber) {
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
        if let Err(err) = self.event_producer.try_send(subscribe_event) {
            tracing::error!("unsubscribe_from_stream_hub err {err}");
        }

        Ok(())
    }

    /* Publish RTMP streams to stream hub, the streams can be pushed from remote or pulled from remote to local */
    pub async fn publish_to_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
        gop_num: usize,
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

        if self.event_producer.try_send(publish_event).is_err() {
            return Err(SessionError {
                value: SessionErrorValue::StreamHubEventSendErr,
            });
        }

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
            if let Err(err) = sender.try_send(statistic_publisher) {
                tracing::error!("send statistic_publisher err: {err}");
            }
        }

        self.stream_handler
            .set_cache(Cache::new(gop_num, None, statistic_data_sender))
            .await;
        Ok(())
    }

    pub async fn unpublish_to_stream_hub(
        &mut self,
        app_name: String,
        stream_name: String,
    ) -> Result<(), SessionError> {
        tracing::info!(
            "unpublish_to_stream_hub, app_name:{app_name}, stream_name:{stream_name}"
        );
        let unpublish_event = StreamHubEvent::UnPublish {
            identifier: StreamIdentifier::Rtmp {
                app_name: app_name.clone(),
                stream_name: stream_name.clone(),
            },
        };

        match self.event_producer.try_send(unpublish_event) {
            Err(_) => {
                tracing::error!(
                    "unpublish_to_stream_hub error.app_name: {app_name}, stream_name: {stream_name}"
                );
                return Err(SessionError {
                    value: SessionErrorValue::StreamHubEventSendErr,
                });
            }
            _ => {
                tracing::info!(
                    "unpublish_to_stream_hub successfully.app_name: {app_name}, stream_name: {stream_name}"
                );
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct RtmpStreamHandler {
    /*cache is used to save RTMP sequence/gops/meta data
    which needs to be send to client(player) */
    /*The cache will be used in different threads(save
    cache in one thread and send cache data to different clients
    in other threads) */
    pub cache: Mutex<Option<Cache>>,
}

impl RtmpStreamHandler {
    #[must_use] 
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(None),
        }
    }

    pub async fn set_cache(&self, cache: Cache) {
        *self.cache.lock().await = Some(cache);
    }

    pub async fn save_video_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_video_data(chunk_body, timestamp).await?;
        }
        Ok(())
    }

    pub async fn save_audio_data(
        &self,
        chunk_body: &BytesMut,
        timestamp: u32,
    ) -> Result<(), CacheError> {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_audio_data(chunk_body, timestamp).await?;
        }
        Ok(())
    }

    pub async fn save_metadata(&self, chunk_body: &BytesMut, timestamp: u32) {
        if let Some(cache) = &mut *self.cache.lock().await {
            cache.save_metadata(chunk_body, timestamp);
        }
    }
}

#[async_trait]
impl TStreamHandler for RtmpStreamHandler {
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

        /// Helper to send prior data, returning error on closed channel
        fn try_send_prior<T>(sender: &mpsc::Sender<T>, data: T, name: &str) -> Result<(), StreamHubError> {
            match sender.try_send(data) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("send_prior_data: {} dropped due to channel full", name);
                    Ok(())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    Err(StreamHubError {
                        value: StreamHubErrorValue::SubscriberClosed,
                    })
                }
            }
        }

        if let Some(cache) = &mut *self.cache.lock().await {
            if let Some(meta_body_data) = cache.get_metadata() {
                tracing::info!("send_prior_data: meta_body_data: ");
                try_send_prior(&sender, meta_body_data, "metadata")?;
            }
            if let Some(audio_seq_data) = cache.get_audio_seq() {
                tracing::info!("send_prior_data: audio_seq_data: ",);
                try_send_prior(&sender, audio_seq_data, "audio seq")?;
            }
            if let Some(video_seq_data) = cache.get_video_seq() {
                tracing::info!("send_prior_data: video_seq_data:");
                try_send_prior(&sender, video_seq_data, "video seq")?;
            }
            match sub_type {
                SubscribeType::RtmpPull
                | SubscribeType::RtmpRemux2HttpFlv
                | SubscribeType::RtmpRemux2Hls => {
                    if let Some(gops_data) = cache.get_gops_data() {
                        for gop in gops_data {
                            for channel_data in gop.frame_data() {
                                try_send_prior(&sender, channel_data.clone(), "gop frame")?;
                            }
                        }
                    }
                }
                _ => {}
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
