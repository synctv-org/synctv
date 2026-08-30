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
    tokio::sync::{broadcast, mpsc, oneshot, OwnedSemaphorePermit, Semaphore},
};

/// How a consumer subscribes to a stream in `StreamsHub`.
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum SubscribeType {
    /// RTMP player pulls frames from a local stream.
    RtmpPull,
    /// HTTP-FLV session remuxes RTMP frames to FLV.
    RtmpRemux2HttpFlv,
    /// HLS remuxer subscribes after an RTMP publish event.
    RtmpRemux2Hls,
    /// Relay publisher pushes a local stream to another RTMP node.
    RtmpRelay,
    /// WHEP player consumes RTP packets from a local stream.
    WhepPull,
}

/// How a producer publishes a stream into `StreamsHub`.
#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub enum PublishType {
    /// Remote RTMP client pushes into this node.
    RtmpPush,
    /// Standards-based WHIP client pushes into this node.
    WhipPush,
    /// This node pulls a remote RTMP stream and republishes it locally.
    RtmpRelay,
    /// This node owns an external RTMP, RTSP, or HTTP-FLV pull and republishes it locally.
    ExternalPull,
}

impl PublishType {
    #[must_use]
    pub const fn is_user_push(&self) -> bool {
        matches!(self, Self::RtmpPush | Self::WhipPush)
    }
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
        // Runtime-only routing fields are omitted from serialized diagnostics.
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
        // Runtime-only routing fields are omitted from serialized diagnostics.
        let mut state = serializer.serialize_struct("PublisherInfo", 3)?;

        state.serialize_field("id", &self.id.to_string())?;
        state.serialize_field("pub_type", &self.pub_type)?;
        state.serialize_field("notify_info", &self.notify_info)?;
        state.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoCodecType {
    H264,
    H265,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug)]
pub enum FrameTrySendError<T> {
    Full(T),
    Closed(T),
}

/// Sender used for frame fan-out between publishers, subscribers, and remuxers.
#[derive(Debug, Clone)]
pub enum FrameDataSender {
    Bounded(mpsc::Sender<FrameData>),
    /// Production frame channels use a byte budget in addition to the frame
    /// count bound. This keeps large keyframes from filling many gigabytes of
    /// queue memory for a slow subscriber.
    Budgeted(Arc<BudgetedFrameSender>),
    Unbounded(mpsc::UnboundedSender<FrameData>),
}

#[derive(Debug)]
#[doc(hidden)]
pub struct BudgetedFrameSender {
    sender: mpsc::Sender<BudgetedFrame>,
    budget: Arc<Semaphore>,
    max_bytes: usize,
}

#[derive(Debug)]
#[doc(hidden)]
pub struct BudgetedFrame {
    data: FrameData,
    _permit: OwnedSemaphorePermit,
}

impl FrameDataSender {
    #[must_use]
    pub const fn bounded(sender: mpsc::Sender<FrameData>) -> Self {
        Self::Bounded(sender)
    }

    pub(crate) fn budgeted(capacity: usize, max_bytes: usize) -> (Self, FrameDataReceiver) {
        let (sender, receiver) = mpsc::channel(capacity);
        let budget = Arc::new(Semaphore::new(max_bytes));
        let channel = Arc::new(BudgetedFrameSender {
            sender,
            budget: Arc::clone(&budget),
            max_bytes,
        });
        (
            Self::Budgeted(channel),
            FrameDataReceiver::Budgeted { receiver },
        )
    }

    #[must_use]
    pub const fn unbounded(sender: mpsc::UnboundedSender<FrameData>) -> Self {
        Self::Unbounded(sender)
    }

    pub fn try_send(&self, value: FrameData) -> Result<(), FrameTrySendError<FrameData>> {
        match self {
            Self::Bounded(sender) => match sender.try_send(value) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(value)) => Err(FrameTrySendError::Full(value)),
                Err(mpsc::error::TrySendError::Closed(value)) => {
                    Err(FrameTrySendError::Closed(value))
                }
            },
            Self::Budgeted(channel) => {
                let permits = frame_data_bytes(&value);
                if permits > channel.max_bytes {
                    return Err(FrameTrySendError::Full(value));
                }
                let permits = u32::try_from(permits).unwrap_or(u32::MAX);
                let Ok(permit) = channel.budget.clone().try_acquire_many_owned(permits) else {
                    return Err(FrameTrySendError::Full(value));
                };
                match channel.sender.try_send(BudgetedFrame {
                    data: value,
                    _permit: permit,
                }) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(item)) => {
                        Err(FrameTrySendError::Full(item.data))
                    }
                    Err(mpsc::error::TrySendError::Closed(item)) => {
                        Err(FrameTrySendError::Closed(item.data))
                    }
                }
            }
            Self::Unbounded(sender) => sender
                .send(value)
                .map_err(|err| FrameTrySendError::Closed(err.0)),
        }
    }

    pub async fn send(&self, value: FrameData) -> Result<(), FrameTrySendError<FrameData>> {
        match self {
            Self::Bounded(sender) => sender
                .send(value)
                .await
                .map_err(|err| FrameTrySendError::Closed(err.0)),
            Self::Budgeted(channel) => {
                let permits = frame_data_bytes(&value);
                if permits > channel.max_bytes {
                    return Err(FrameTrySendError::Full(value));
                }
                let permits = u32::try_from(permits).unwrap_or(u32::MAX);
                let Ok(permit) = channel.budget.clone().acquire_many_owned(permits).await else {
                    return Err(FrameTrySendError::Closed(value));
                };
                channel
                    .sender
                    .send(BudgetedFrame {
                        data: value,
                        _permit: permit,
                    })
                    .await
                    .map_err(|err| FrameTrySendError::Closed(err.0.data))
            }
            Self::Unbounded(sender) => sender
                .send(value)
                .map_err(|err| FrameTrySendError::Closed(err.0)),
        }
    }
}

#[derive(Debug)]
pub enum FrameDataReceiver {
    Bounded(mpsc::Receiver<FrameData>),
    Budgeted {
        receiver: mpsc::Receiver<BudgetedFrame>,
    },
    Unbounded(mpsc::UnboundedReceiver<FrameData>),
}

impl FrameDataReceiver {
    #[must_use]
    pub const fn bounded(receiver: mpsc::Receiver<FrameData>) -> Self {
        Self::Bounded(receiver)
    }

    #[must_use]
    pub const fn unbounded(receiver: mpsc::UnboundedReceiver<FrameData>) -> Self {
        Self::Unbounded(receiver)
    }

    pub async fn recv(&mut self) -> Option<FrameData> {
        match self {
            Self::Bounded(receiver) => receiver.recv().await,
            Self::Budgeted { receiver } => receiver.recv().await.map(|item| item.data),
            Self::Unbounded(receiver) => receiver.recv().await,
        }
    }

    pub fn try_recv(&mut self) -> Result<FrameData, mpsc::error::TryRecvError> {
        match self {
            Self::Bounded(receiver) => receiver.try_recv(),
            Self::Budgeted { receiver } => receiver.try_recv().map(|item| item.data),
            Self::Unbounded(receiver) => receiver.try_recv(),
        }
    }
}

fn frame_data_bytes(data: &FrameData) -> usize {
    match data {
        FrameData::Video { data, .. }
        | FrameData::Audio { data, .. }
        | FrameData::MetaData { data, .. } => data.len(),
        FrameData::MediaInfo { .. } => 0,
    }
}

/// Default capacity for frame data channels.
///
/// Must be large enough to absorb bursts (keyframe + B-frames) without
/// dropping. 4096 frames ≈ ~160 s at 25 fps, keeping memory bounded while
/// avoiding silent frame loss under load.
pub const FRAME_DATA_CHANNEL_CAPACITY: usize = 4096;

/// Maximum total media payload retained by one production frame channel.
pub const FRAME_DATA_CHANNEL_MAX_BYTES: usize = 32 * 1024 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn budgeted_frame_channel_releases_bytes_after_receive() {
        let (sender, mut receiver) = FrameDataSender::budgeted(4096, 8);
        let first = FrameData::Video {
            timestamp: 0,
            data: Bytes::from_static(b"123456"),
        };
        sender
            .try_send(first)
            .expect("first frame should fit budget");

        let second = FrameData::Audio {
            timestamp: 1,
            data: Bytes::from_static(b"123"),
        };
        assert!(matches!(
            sender.try_send(second),
            Err(FrameTrySendError::Full(_))
        ));

        let received = receiver.recv().await.expect("first frame should arrive");
        assert_eq!(frame_data_bytes(&received), 6);

        sender
            .try_send(FrameData::Audio {
                timestamp: 2,
                data: Bytes::from_static(b"123"),
            })
            .expect("released permits should be reusable");
    }
}

/// Called when media enters a locally owned publisher transceiver.
///
/// The callback runs before subscriber fan-out, so publisher liveness remains
/// independent from HLS/FLV subscriber availability.
pub type PublisherActivityCallback = Arc<dyn Fn(&str, &str, Uuid) + Send + Sync>;

pub type TransceiverEventSender = mpsc::UnboundedSender<TransceiverEvent>;
pub type TransceiverEventReceiver = mpsc::UnboundedReceiver<TransceiverEvent>;

pub type StatisticDataSender = mpsc::UnboundedSender<StatisticData>;
pub type StatisticDataReceiver = mpsc::UnboundedReceiver<StatisticData>;

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
pub type TransceiverEventExecuteResultSender =
    oneshot::Sender<Result<StatisticDataSender, StreamHubError>>;

#[async_trait]
pub trait TStreamHandler: Send + Sync {
    fn save_frame_data(&self, _frame: &FrameData) -> Result<(), StreamHubError> {
        Ok(())
    }

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
#[derive(Debug, Clone, Copy, Serialize, Eq, PartialEq)]
pub enum SubDataType {
    Frame,
    Packet,
}
//we can pub frame or packet or both.
#[derive(Debug, Clone, Copy, Serialize, Eq, PartialEq)]
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
    SubscribeWithGeneration {
        identifier: StreamIdentifier,
        info: SubscriberInfo,
        expected_generation_id: Uuid,
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
        generation_id: Uuid,
    },
    ForceUnPublish {
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
            Self::SubscribeWithGeneration {
                identifier,
                info,
                expected_generation_id,
                ..
            } => f
                .debug_struct("StreamHubEvent::SubscribeWithGeneration")
                .field("identifier", identifier)
                .field("info", info)
                .field("expected_generation_id", expected_generation_id)
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
            Self::UnPublish {
                identifier,
                generation_id,
            } => f
                .debug_struct("StreamHubEvent::UnPublish")
                .field("identifier", identifier)
                .field("generation_id", generation_id)
                .finish(),
            Self::ForceUnPublish { identifier } => f
                .debug_struct("StreamHubEvent::ForceUnPublish")
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
        generation_id: Uuid,
    },
    UnPublish {
        identifier: StreamIdentifier,
        generation_id: Uuid,
    },
}

#[derive(Debug)]
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
