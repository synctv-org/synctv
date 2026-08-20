use {
    super::stream::StreamIdentifier,
    crate::flv::define::{
        AacProfile, AvcCodecId, AvcLevel, AvcProfile, HevcLevel, HevcProfile, SoundFormat,
    },
    crate::streamhub::{define::SubscribeType, utils::Uuid},
    chrono::{DateTime, Local},
    serde::Serialize,
    std::{collections::HashMap, sync::Arc, time::Duration},
    tokio::{
        sync::{broadcast::Receiver, Mutex},
        time,
    },
};

#[derive(Debug, Clone, Serialize, Default)]
pub struct VideoInfo {
    pub codec: AvcCodecId,
    pub profile: AvcProfile,
    pub level: AvcLevel,
    /// HEVC profile (only set when codec is HEVC)
    pub hevc_profile: Option<HevcProfile>,
    /// HEVC level (only set when codec is HEVC)
    pub hevc_level: Option<HevcLevel>,
    pub width: u32,
    pub height: u32,
    /// Bytes accumulated during the current bitrate interval.
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    #[serde(rename = "bitrate(kbits/s)")]
    pub bitrate: usize,
    /// Frames accumulated during the current frame-rate interval.
    #[serde(skip_serializing)]
    pub recv_frame_count: usize,
    pub frame_rate: usize,
    /// Frames accumulated since the previous keyframe.
    #[serde(skip_serializing)]
    pub recv_frame_count_for_gop: usize,
    pub gop: usize,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct AudioInfo {
    pub sound_format: SoundFormat,
    pub profile: AacProfile,
    pub samplerate: u32,
    pub channels: u8,
    /// Bytes accumulated during the current bitrate interval.
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    #[serde(rename = "bitrate(kbits/s)")]
    pub bitrate: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct StatisticsStream {
    /// Publisher-side statistics for this stream.
    pub publisher: StatisticPublisher,
    /// Per-subscriber downstream statistics.
    pub subscribers: HashMap<Uuid, StatisticSubscriber>,
    /// Number of active subscribers for this stream.
    pub subscriber_count: usize,
    /// Total upstream audio/video bytes received by this node.
    pub total_recv_bytes: usize,
    /// Total downstream audio/video bytes sent to subscribers.
    pub total_send_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct StatisticPublisher {
    pub id: Uuid,
    identifier: StreamIdentifier,
    pub start_time: DateTime<Local>,
    pub video: VideoInfo,
    pub audio: AudioInfo,
    pub remote_address: String,
    /// Bytes accumulated during the current publisher bitrate interval.
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    /// Current upstream bitrate from publisher to this node.
    #[serde(rename = "recv_bitrate(kbits/s)")]
    pub recv_bitrate: usize,
}

impl StatisticPublisher {
    #[must_use]
    pub fn new(identifier: StreamIdentifier) -> Self {
        Self {
            id: Uuid::default(),
            identifier,
            start_time: Local::now(),
            video: VideoInfo::default(),
            audio: AudioInfo::default(),
            remote_address: String::new(),
            recv_bytes: 0,
            recv_bitrate: 0,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct StatisticSubscriber {
    pub id: Uuid,
    pub start_time: DateTime<Local>,
    pub remote_address: String,
    pub sub_type: SubscribeType,
    /// Bytes accumulated during the current subscriber bitrate interval.
    #[serde(skip_serializing)]
    pub send_bytes: usize,
    /// Current downstream bitrate from this node to the subscriber.
    #[serde(rename = "send_bitrate(kbits/s)")]
    pub send_bitrate: usize,
    #[serde(rename = "total_send_bytes(kbits/s)")]
    pub total_send_bytes: usize,
}

impl StatisticsStream {
    #[must_use]
    pub fn new(identifier: StreamIdentifier) -> Self {
        Self {
            publisher: StatisticPublisher::new(identifier),
            subscribers: HashMap::new(),
            subscriber_count: 0,
            total_recv_bytes: 0,
            total_send_bytes: 0,
        }
    }
}

pub struct StatisticsCalculate {
    stream: Arc<Mutex<StatisticsStream>>,
    exit: Receiver<()>,
}

impl StatisticsCalculate {
    pub const fn new(stream: Arc<Mutex<StatisticsStream>>, exit: Receiver<()>) -> Self {
        Self { stream, exit }
    }

    async fn calculate(&self) {
        let stream_statistics_clone = &mut self.stream.lock().await;

        stream_statistics_clone.publisher.video.bitrate =
            stream_statistics_clone.publisher.video.recv_bytes * 8 / 5000;
        stream_statistics_clone.publisher.video.recv_bytes = 0;

        stream_statistics_clone.publisher.video.frame_rate =
            stream_statistics_clone.publisher.video.recv_frame_count / 5;
        stream_statistics_clone.publisher.video.recv_frame_count = 0;

        stream_statistics_clone.publisher.audio.bitrate =
            stream_statistics_clone.publisher.audio.recv_bytes * 8 / 5000;
        stream_statistics_clone.publisher.audio.recv_bytes = 0;

        stream_statistics_clone.publisher.recv_bitrate =
            stream_statistics_clone.publisher.recv_bytes * 8 / 5000;
        stream_statistics_clone.publisher.recv_bytes = 0;

        for subscriber in stream_statistics_clone.subscribers.values_mut() {
            subscriber.send_bitrate = subscriber.send_bytes * 8 / 5000;
            subscriber.send_bytes = 0;
        }
    }
    pub async fn start(&mut self) {
        let mut interval = time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
               _ = interval.tick() => {
                self.calculate().await;
               },
               _ = self.exit.recv() => {
                    tracing::info!("avstatistics shutting down");
                    return
               },
            }
        }
    }
}
