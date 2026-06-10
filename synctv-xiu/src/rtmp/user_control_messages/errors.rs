use {
    crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    crate::flv::amf0::errors::Amf0WriteError,
};

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct EventMessagesError {
    pub value: EventMessagesErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum EventMessagesErrorValue {
    #[error("amf0 write error: {0}")]
    Amf0WriteError(Amf0WriteError),
    #[error("bytes write error: {0}")]
    BytesWriteError(BytesWriteError),
    #[error("bytes read error: {0}")]
    BytesReadError(BytesReadError),
    #[error("unknown user control event message type")]
    UnknownEventMessageType,
}

impl From<Amf0WriteError> for EventMessagesError {
    fn from(error: Amf0WriteError) -> Self {
        Self {
            value: EventMessagesErrorValue::Amf0WriteError(error),
        }
    }
}

impl From<BytesWriteError> for EventMessagesError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: EventMessagesErrorValue::BytesWriteError(error),
        }
    }
}

impl From<BytesReadError> for EventMessagesError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: EventMessagesErrorValue::BytesReadError(error),
        }
    }
}
