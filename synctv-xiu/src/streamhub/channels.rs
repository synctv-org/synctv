use super::define::{
    self, DataReceiver, DataSender, FrameDataReceiver, FrameDataSender, PacketDataSender,
    PubDataType, SubDataType, SubscribeType, SubscriberInfo,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FrameChannelPolicy {
    Bounded,
    Unbounded,
}

impl FrameChannelPolicy {
    pub(crate) fn build(self) -> (FrameDataSender, FrameDataReceiver) {
        match self {
            Self::Bounded => {
                let (sender, receiver) = mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
                (
                    FrameDataSender::bounded(sender),
                    FrameDataReceiver::bounded(receiver),
                )
            }
            Self::Unbounded => {
                let (sender, receiver) = mpsc::unbounded_channel();
                (
                    FrameDataSender::unbounded(sender),
                    FrameDataReceiver::unbounded(receiver),
                )
            }
        }
    }
}

pub(crate) fn frame_policy_for_subscriber(sub_type: &SubscribeType) -> FrameChannelPolicy {
    match sub_type {
        SubscribeType::RtmpPull => FrameChannelPolicy::Bounded,
        SubscribeType::RtmpRemux2HttpFlv
        | SubscribeType::RtmpRemux2Hls
        | SubscribeType::RtmpRelay => FrameChannelPolicy::Unbounded,
    }
}

pub(crate) fn build_subscriber_data_channel(info: &SubscriberInfo) -> (DataSender, DataReceiver) {
    match info.sub_data_type {
        SubDataType::Frame => {
            let (sender, receiver) = frame_policy_for_subscriber(&info.sub_type).build();
            (
                DataSender::Frame { sender },
                DataReceiver {
                    frame_receiver: Some(receiver),
                    packet_receiver: None,
                },
            )
        }
        SubDataType::Packet => {
            let (sender, receiver) = mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
            (
                DataSender::Packet { sender },
                DataReceiver {
                    frame_receiver: None,
                    packet_receiver: Some(receiver),
                },
            )
        }
    }
}

pub(crate) fn build_publisher_data_channel(
    pub_data_type: &PubDataType,
) -> (
    Option<FrameDataSender>,
    Option<PacketDataSender>,
    DataReceiver,
) {
    match pub_data_type {
        PubDataType::Frame => {
            let (sender, receiver) = FrameChannelPolicy::Bounded.build();
            (
                Some(sender),
                None,
                DataReceiver {
                    frame_receiver: Some(receiver),
                    packet_receiver: None,
                },
            )
        }
        PubDataType::Packet => {
            let (sender, receiver) = mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
            (
                None,
                Some(sender),
                DataReceiver {
                    frame_receiver: None,
                    packet_receiver: Some(receiver),
                },
            )
        }
        PubDataType::Both => {
            let (frame_sender, frame_receiver) = FrameChannelPolicy::Bounded.build();
            let (packet_sender, packet_receiver) =
                mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
            (
                Some(frame_sender),
                Some(packet_sender),
                DataReceiver {
                    frame_receiver: Some(frame_receiver),
                    packet_receiver: Some(packet_receiver),
                },
            )
        }
    }
}
