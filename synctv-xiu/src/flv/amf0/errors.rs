use {
    crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    std::{io, string},
};

#[derive(Debug, thiserror::Error)]
pub enum Amf0ReadErrorValue {
    #[error("Encountered unknown marker: {marker}")]
    UnknownMarker { marker: u8 },
    #[error("parser string error: {0}")]
    StringParseError(#[source] string::FromUtf8Error),
    #[error("bytes read error :{0}")]
    BytesReadError(BytesReadError),
    #[error("wrong type")]
    WrongType,
    #[error("string length {length} exceeds maximum {max}")]
    StringTooLong { length: usize, max: usize },
    #[error("object nesting depth {depth} exceeds maximum {max}")]
    DepthLimitExceeded { depth: usize, max: usize },
    #[error("object key count {count} exceeds maximum {max}")]
    KeyLimitExceeded { count: usize, max: usize },
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct Amf0ReadError {
    pub value: Amf0ReadErrorValue,
}

impl From<string::FromUtf8Error> for Amf0ReadError {
    fn from(error: string::FromUtf8Error) -> Self {
        Self {
            value: Amf0ReadErrorValue::StringParseError(error),
        }
    }
}

impl From<BytesReadError> for Amf0ReadError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: Amf0ReadErrorValue::BytesReadError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Amf0WriteErrorValue {
    #[error("normal string too long")]
    NormalStringTooLong,
    #[error("io error")]
    BufferWriteError(io::Error),
    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct Amf0WriteError {
    pub value: Amf0WriteErrorValue,
}

impl From<io::Error> for Amf0WriteError {
    fn from(error: io::Error) -> Self {
        Self {
            value: Amf0WriteErrorValue::BufferWriteError(error),
        }
    }
}

impl From<BytesWriteError> for Amf0WriteError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: Amf0WriteErrorValue::BytesWriteError(error),
        }
    }
}
