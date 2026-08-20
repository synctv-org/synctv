use {crate::flv::amf0::define::Amf0ValueType, bytes::BytesMut};

#[derive(Debug)]
pub struct SetPeerBandwidthProperties {
    pub window_size: u32,
    pub limit_type: u8,
}

impl SetPeerBandwidthProperties {
    #[must_use]
    pub const fn new(window_size: u32, limit_type: u8) -> Self {
        Self {
            window_size,
            limit_type,
        }
    }
}

#[derive(Debug)]
pub struct Amf0CommandMessage {
    pub command_name: Amf0ValueType,
    pub transaction_id: Amf0ValueType,
    pub command_object: Amf0ValueType,
    pub others: Vec<Amf0ValueType>,
}

#[derive(Debug)]
pub enum RtmpMessageData {
    Amf0Command(Box<Amf0CommandMessage>),
    AmfData {
        raw_data: BytesMut,
        // values: Vec<Amf0ValueType>,
    },
    SetChunkSize {
        chunk_size: u32,
    },
    AbortMessage {
        chunk_stream_id: u32,
    },
    Acknowledgement {
        sequence_number: u32,
    },
    WindowAcknowledgementSize {
        size: u32,
    },
    SetPeerBandwidth {
        properties: SetPeerBandwidthProperties,
    },
    AudioData {
        data: BytesMut,
    },
    VideoData {
        data: BytesMut,
    },
    SetBufferLength {
        stream_id: u32,
        buffer_length: u32,
    },
    StreamBegin {
        stream_id: u32,
    },
    StreamIsRecorded {
        stream_id: u32,
    },
}

pub mod msg_type_id {
    pub const AUDIO: u8 = 8;
    pub const VIDEO: u8 = 9;

    pub const SET_CHUNK_SIZE: u8 = 1;
    pub const ABORT: u8 = 2;
    pub const ACKNOWLEDGEMENT: u8 = 3;
    pub const USER_CONTROL_EVENT: u8 = 4;
    pub const WIN_ACKNOWLEDGEMENT_SIZE: u8 = 5;
    pub const SET_PEER_BANDWIDTH: u8 = 6;

    pub const COMMAND_AMF3: u8 = 17;
    pub const COMMAND_AMF0: u8 = 20;

    pub const DATA_AMF3: u8 = 15;
    pub const DATA_AMF0: u8 = 18;
}
