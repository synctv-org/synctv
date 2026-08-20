use {
    crate::bytesio::bytes_errors::BytesReadError, crate::flv::amf0::errors::Amf0ReadError,
    crate::rtmp::user_control_messages::errors::EventMessagesError,
};

#[derive(Debug, thiserror::Error)]
pub enum MessageErrorValue {
    #[error("bytes read error: {0}")]
    BytesReadError(BytesReadError),
    #[error("unknown read state")]
    UnknownReadState,
    #[error("amf0 read error: {0}")]
    Amf0ReadError(Amf0ReadError),
    #[error("unknown RTMP message type {0}")]
    UnknownMessageType(u8),
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

impl From<EventMessagesError> for MessageError {
    fn from(error: EventMessagesError) -> Self {
        Self {
            value: MessageErrorValue::EventMessagesError(error),
        }
    }
}
