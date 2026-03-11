use crate::flv::define::{
    AacProfile, AvcCodecId, AvcLevel, AvcProfile, HevcLevel, HevcProfile, SoundFormat,
};
use chrono::{DateTime, Local};

use {
    super::errors::StreamHubError,
    super::stream::StreamIdentifier,
    super::utils::Uuid,
    async_trait::async_trait,
    bytes::Bytes,
    serde::ser::SerializeStruct,
    serde::Serialize,
    serde::Serializer,
    std::fmt,
    std::sync::Arc,
    tokio::sync::{broadcast, mpsc, oneshot},
};

/* Subscribe streams from stream hub */
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum SubscribeType {
    /* Remote client request pulling(play) a rtmp stream.*/
    RtmpPull,
    /* Remote request to play httpflv triggers remux from RTMP to httpflv. */
    RtmpRemux2HttpFlv,
    /* The publishing of RTMP stream triggers remuxing from RTMP to HLS protocol.(NOTICE:It is not triggerred by players.)*/
    RtmpRemux2Hls,
    /* Relay(Push) local RTMP stream from stream hub to other RTMP nodes.*/
    RtmpRelay,
}

/* Publish streams to stream hub */
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum PublishType {
    /* Receive rtmp stream from remote push client. */
    RtmpPush,
    /* Relay(Pull) remote RTMP stream to local stream hub. */
    RtmpRelay,
}

#[derive(Debug, Serialize, Clone)]
pub struct NotifyInfo {
    pub request_url: String,
    pub remote_addr: String,
}

#[derive(Debug, Clone)]
pub struct SubscriberInfo {
    pub id: Uuid,
    pub sub_type: SubscribeType,
    pub notify_info: NotifyInfo,
    pub sub_data_type: SubDataType,
}

impl Serialize for SubscriberInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 3 is the number of fields in the struct.
        let mut state = serializer.serialize_struct("SubscriberInfo", 3)?;

        state.serialize_field("id", &self.id.to_string())?;
        state.serialize_field("sub_type", &self.sub_type)?;
        state.serialize_field("notify_info", &self.notify_info)?;
        state.end()
    }
}

#[derive(Debug, Clone)]
pub struct PublisherInfo {
    pub id: Uuid,
    pub pub_type: PublishType,
    pub pub_data_type: PubDataType,
    pub notify_info: NotifyInfo,
}

impl Serialize for PublisherInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 3 is the number of fields in the struct.
        let mut state = serializer.serialize_struct("PublisherInfo", 3)?;

        state.serialize_field("id", &self.id.to_string())?;
        state.serialize_field("pub_type", &self.pub_type)?;
        state.serialize_field("notify_info", &self.notify_info)?;
        state.end()
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoCodecType {
    H264,
    H265,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaInfo {
    pub audio_clock_rate: u32,
    pub video_clock_rate: u32,
    pub vcodec: VideoCodecType,
}

impl MediaInfo {
    /// Return the number of heap-allocated bytes owned by this `MediaInfo`.
    ///
    /// Currently all fields are stack-only (`u32`, enum), so this returns 0.
    /// If heap-allocated fields (e.g., `String`) are added in the future,
    /// their `.len()` / `.capacity()` should be included here so that
    /// `Gop::frame_memory_size` remains accurate.
    #[must_use]
    pub const fn heap_size(&self) -> usize {
        // No heap-allocated fields at present.
        0
    }
}

/// Frame data using `Bytes` for zero-copy fan-out.
///
/// `Bytes::clone()` is O(1) -- only bumps Arc reference count, no data copy.
/// Publishers create `BytesMut` and call `.freeze()` before wrapping in `FrameData`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum FrameData {
    Video {
        timestamp: u32,
        #[serde(with = "bytes_serde")]
        data: Bytes,
    },
    Audio {
        timestamp: u32,
        #[serde(with = "bytes_serde")]
        data: Bytes,
    },
    MetaData {
        timestamp: u32,
        #[serde(with = "bytes_serde")]
        data: Bytes,
    },
    MediaInfo {
        media_info: MediaInfo,
    },
}

/// Serde support for Bytes (serialize as Vec<u8>)
mod bytes_serde {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec = Vec::<u8>::deserialize(deserializer)?;
        Ok(Bytes::from(vec))
    }
}

/// Used to pass RTP raw data.
/// Uses `Bytes` (immutable, O(1) clone) since packets are never mutated after creation.
#[derive(Clone)]
pub enum PacketData {
    Video { timestamp: u32, data: Bytes },
    Audio { timestamp: u32, data: Bytes },
}

//used to transfer a/v frame between different protocols(rtmp/rtsp/webrtc/http-flv/hls)
//or send a/v frame data from publisher to subscribers.
// Bounded to provide backpressure - when full, packets are dropped.
pub type FrameDataSender = mpsc::Sender<FrameData>;
pub type FrameDataReceiver = mpsc::Receiver<FrameData>;

/// Default capacity for frame data channels.
///
/// Must be large enough to absorb bursts (keyframe + B-frames) without
/// dropping. 4096 frames ≈ ~160 s at 25 fps, keeping memory bounded while
/// avoiding silent frame loss under load.
pub const FRAME_DATA_CHANNEL_CAPACITY: usize = 4096;

//used to transfer rtp packet data,it includles the following directions:
// rtsp(publisher)->stream hub->rtsp(subscriber)
// webrtc(publisher whip)->stream hub->webrtc(subscriber whep)
// Bounded to provide backpressure - when full, packets are dropped.
pub type PacketDataSender = mpsc::Sender<PacketData>;
pub type PacketDataReceiver = mpsc::Receiver<PacketData>;

/// Default capacity for packet data channels.
/// Limits memory usage while allowing enough buffer for normal operation.
/// When full, new packets are dropped (non-blocking behavior).
pub const PACKET_DATA_CHANNEL_CAPACITY: usize = 256;

pub type StreamHubEventSender = mpsc::Sender<StreamHubEvent>;
pub type StreamHubEventReceiver = mpsc::Receiver<StreamHubEvent>;

/// Default capacity for the bounded `StreamHub` event channel.
/// Large enough for normal operation but prevents unbounded memory growth.
pub const STREAM_HUB_EVENT_CHANNEL_CAPACITY: usize = 4096;

pub type BroadcastEventSender = broadcast::Sender<BroadcastEvent>;
pub type BroadcastEventReceiver = broadcast::Receiver<BroadcastEvent>;

pub type TransceiverEventSender = mpsc::Sender<TransceiverEvent>;
pub type TransceiverEventReceiver = mpsc::Receiver<TransceiverEvent>;

/// Capacity for bounded transceiver event channels.
pub const TRANSCEIVER_EVENT_CHANNEL_CAPACITY: usize = 1024;

pub type StatisticDataSender = mpsc::Sender<StatisticData>;
pub type StatisticDataReceiver = mpsc::Receiver<StatisticData>;

/// Capacity for bounded statistic data channels.
pub const STATISTIC_DATA_CHANNEL_CAPACITY: usize = 1024;

pub type SubEventExecuteResultSender =
    oneshot::Sender<Result<(DataReceiver, Option<StatisticDataSender>), StreamHubError>>;
pub type PubEventExecuteResultSender = oneshot::Sender<
    Result<
        (
            Option<FrameDataSender>,
            Option<PacketDataSender>,
            Option<StatisticDataSender>,
        ),
        StreamHubError,
    >,
>;
pub type TransceiverEventExecuteResultSender = oneshot::Sender<StatisticDataSender>;

#[async_trait]
pub trait TStreamHandler: Send + Sync {
    async fn send_prior_data(
        &self,
        sender: DataSender,
        sub_type: SubscribeType,
    ) -> Result<(), StreamHubError>;
}

//A publisher can publish one or two kinds of av stream at a time.
#[derive(Debug)]
pub struct DataReceiver {
    pub frame_receiver: Option<FrameDataReceiver>,
    pub packet_receiver: Option<PacketDataReceiver>,
}

//A subscriber only needs to subscribe to one type of stream at a time
#[derive(Debug, Clone)]
pub enum DataSender {
    Frame { sender: FrameDataSender },
    Packet { sender: PacketDataSender },
}
//we can only sub one kind of stream.
#[derive(Debug, Clone, Serialize)]
pub enum SubDataType {
    Frame,
    Packet,
}
//we can pub frame or packet or both.
#[derive(Debug, Clone, Serialize)]
pub enum PubDataType {
    Frame,
    Packet,
    Both,
}

#[derive(Serialize)]
pub enum StreamHubEvent {
    Subscribe {
        identifier: StreamIdentifier,
        info: SubscriberInfo,
        #[serde(skip_serializing)]
        result_sender: SubEventExecuteResultSender,
    },
    UnSubscribe {
        identifier: StreamIdentifier,
        info: SubscriberInfo,
    },
    Publish {
        identifier: StreamIdentifier,
        info: PublisherInfo,
        #[serde(skip_serializing)]
        result_sender: PubEventExecuteResultSender,
        #[serde(skip_serializing)]
        stream_handler: Arc<dyn TStreamHandler>,
    },
    UnPublish {
        identifier: StreamIdentifier,
    },
}

impl fmt::Debug for StreamHubEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subscribe {
                identifier, info, ..
            } => f
                .debug_struct("StreamHubEvent::Subscribe")
                .field("identifier", identifier)
                .field("info", info)
                .finish(),
            Self::UnSubscribe { identifier, info } => f
                .debug_struct("StreamHubEvent::UnSubscribe")
                .field("identifier", identifier)
                .field("info", info)
                .finish(),
            Self::Publish {
                identifier, info, ..
            } => f
                .debug_struct("StreamHubEvent::Publish")
                .field("identifier", identifier)
                .field("info", info)
                .finish(),
            Self::UnPublish { identifier } => f
                .debug_struct("StreamHubEvent::UnPublish")
                .field("identifier", identifier)
                .finish(),
        }
    }
}

#[derive(Debug)]
pub enum TransceiverEvent {
    Subscribe {
        sender: DataSender,
        info: SubscriberInfo,
        result_sender: TransceiverEventExecuteResultSender,
    },
    UnSubscribe {
        info: SubscriberInfo,
    },
    UnPublish {},
}

impl fmt::Display for TransceiverEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", *self)
    }
}

#[derive(Debug, Clone)]
pub enum BroadcastEvent {
    Publish {
        identifier: StreamIdentifier,
        pub_type: PublishType,
    },
    UnPublish {
        identifier: StreamIdentifier,
    },
}

pub enum StatisticData {
    AudioCodec {
        sound_format: SoundFormat,
        profile: AacProfile,
        samplerate: u32,
        channels: u8,
    },
    VideoCodec {
        codec: AvcCodecId,
        profile: AvcProfile,
        level: AvcLevel,
        width: u32,
        height: u32,
    },
    HevcCodec {
        codec: AvcCodecId,
        profile: HevcProfile,
        level: HevcLevel,
        width: u32,
        height: u32,
    },
    Audio {
        uuid: Option<Uuid>,
        data_size: usize,
        aac_packet_type: u8,
        duration: usize,
    },
    Video {
        uuid: Option<Uuid>,
        data_size: usize,
        frame_count: usize,
        is_key_frame: Option<bool>,
        duration: usize,
    },
    Publisher {
        id: Uuid,
        remote_addr: String,
        start_time: DateTime<Local>,
    },
    Subscriber {
        id: Uuid,
        remote_addr: String,
        sub_type: SubscribeType,
        start_time: DateTime<Local>,
    },
}
