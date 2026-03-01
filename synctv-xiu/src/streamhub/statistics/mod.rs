use {
    super::stream::StreamIdentifier,
    crate::flv::define::{AacProfile, AvcCodecId, AvcLevel, AvcProfile, SoundFormat},
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
    pub width: u32,
    pub height: u32,
    /*used for calculate the bitrate*/
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    #[serde(rename = "bitrate(kbits/s)")]
    pub bitrate: usize,
    /*used for calculate the frame rate*/
    #[serde(skip_serializing)]
    pub recv_frame_count: usize,
    pub frame_rate: usize,
    /*used for calculate the GOP*/
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
    /*used for calculate the bitrate*/
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    #[serde(rename = "bitrate(kbits/s)")]
    pub bitrate: usize,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct StatisticsStream {
    /*publisher infomation */
    pub publisher: StatisticPublisher,
    /*subscriber infomation */
    pub subscribers: HashMap<Uuid, StatisticSubscriber>,
    /*How many clients are subscribing to this stream.*/
    pub subscriber_count: usize,
    /*calculate upstream traffic, now equals audio and video traffic received by this server*/
    pub total_recv_bytes: usize,
    /*calculate downstream traffic, now equals audio and video traffic sent to all subscribers*/
    pub total_send_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct StatisticPublisher {
    pub id: Uuid,
    identifier: StreamIdentifier,
    pub start_time: DateTime<Local>,
    pub video: VideoInfo,
    pub audio: AudioInfo,
    pub remote_address: String,
    /*used for calculate the recv_bitrate*/
    #[serde(skip_serializing)]
    pub recv_bytes: usize,
    /*the bitrate at which the server receives streaming data*/
    #[serde(rename = "recv_bitrate(kbits/s)")]
    pub recv_bitrate: usize,
}

impl StatisticPublisher {
    #[must_use]
    pub fn new(identifier: StreamIdentifier) -> Self {
        Self {
            identifier,
            ..Default::default()
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct StatisticSubscriber {
    pub id: Uuid,
    pub start_time: DateTime<Local>,
    pub remote_address: String,
    pub sub_type: SubscribeType,
    /*used for calculate the send_bitrate*/
    #[serde(skip_serializing)]
    pub send_bytes: usize,
    /*the bitrate at which the server send streaming data to a client*/
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
            ..Default::default()
        }
    }

    fn get_publisher(&self) -> Self {
        let mut statistic_stream = self.clone();
        statistic_stream.subscribers.clear();
        statistic_stream
    }

    fn get_subscriber(&self, uuid: Uuid) -> Self {
        let mut statistic_stream = self.clone();
        statistic_stream.subscribers.retain(|&id, _| uuid == id);
        statistic_stream
    }

    #[must_use]
    pub fn query_by_uuid(&self, uuid: Uuid) -> Self {
        if uuid == self.publisher.id {
            self.get_publisher()
        } else {
            self.get_subscriber(uuid)
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
