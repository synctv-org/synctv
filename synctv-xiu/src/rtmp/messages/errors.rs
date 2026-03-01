use {
    crate::bytesio::bytes_errors::BytesReadError,
    crate::flv::amf0::errors::Amf0ReadError,
    crate::rtmp::{
        protocol_control_messages::errors::ProtocolControlMessageReaderError,
        user_control_messages::errors::EventMessagesError,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum MessageErrorValue {
    #[error("bytes read error: {0}")]
    BytesReadError(BytesReadError),
    #[error("unknow read state")]
    UnknowReadState,
    #[error("amf0 read error: {0}")]
    Amf0ReadError(Amf0ReadError),
    #[error("unknown message type")]
    UnknowMessageType,
    #[error("protocol control message read error: {0}")]
    ProtocolControlMessageReaderError(ProtocolControlMessageReaderError),
    #[error("user control message read error: {0}")]
    EventMessagesError(EventMessagesError),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct MessageError {
    pub value: MessageErrorValue,
}

impl From<MessageErrorValue> for MessageError {
    fn from(val: MessageErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<BytesReadError> for MessageError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: MessageErrorValue::BytesReadError(error),
        }
    }
}

impl From<Amf0ReadError> for MessageError {
    fn from(error: Amf0ReadError) -> Self {
        Self {
            value: MessageErrorValue::Amf0ReadError(error),
        }
    }
}

impl From<ProtocolControlMessageReaderError> for MessageError {
    fn from(error: ProtocolControlMessageReaderError) -> Self {
        Self {
            value: MessageErrorValue::ProtocolControlMessageReaderError(error),
        }
    }
}

impl From<EventMessagesError> for MessageError {
    fn from(error: EventMessagesError) -> Self {
        Self {
            value: MessageErrorValue::EventMessagesError(error),
        }
    }
}
