use crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError};

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct ControlMessagesError {
    pub value: ControlMessagesErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlMessagesErrorValue {
    //Amf0WriteError(Amf0WriteError),
    #[error("bytes write error: {0}")]
    BytesWriteError(BytesWriteError),
}

impl From<BytesWriteError> for ControlMessagesError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: ControlMessagesErrorValue::BytesWriteError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct ProtocolControlMessageReaderError {
    pub value: ProtocolControlMessageReaderErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolControlMessageReaderErrorValue {
    #[error("bytes read error: {0}")]
    BytesReadError(BytesReadError),
}

impl From<BytesReadError> for ProtocolControlMessageReaderError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: ProtocolControlMessageReaderErrorValue::BytesReadError(error),
        }
    }
}
