use super::define::{
    self, DataReceiver, DataSender, FrameDataReceiver, FrameDataSender, PacketDataSender,
    PubDataType, SubDataType, SubscriberInfo,
};
use tokio::sync::mpsc;
pub(crate) fn build_frame_data_channel() -> (FrameDataSender, FrameDataReceiver) {
    FrameDataSender::budgeted(
        define::FRAME_DATA_CHANNEL_CAPACITY,
        define::FRAME_DATA_CHANNEL_MAX_BYTES,
    )
}

pub(crate) fn build_subscriber_data_channel(info: &SubscriberInfo) -> (DataSender, DataReceiver) {
    match info.sub_data_type {
        SubDataType::Frame => {
            let (sender, receiver) = build_frame_data_channel();
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
            let (sender, receiver) = build_frame_data_channel();
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
            let (frame_sender, frame_receiver) = build_frame_data_channel();
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
