use super::bytesio_errors::BytesIOError;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum BytesReadErrorValue {
    #[error("not enough bytes to read")]
    NotEnoughBytes,
    #[error("empty stream")]
    EmptyStream,
    #[error("io error: {0}")]
    IO(#[source] io::Error),
    #[error("index out of range")]
    IndexOutofRange,
    #[error("bytesio read error: {0}")]
    BytesIOError(BytesIOError),
    #[error("buffer overflow: {current} + {additional} > {max} max")]
    BufferOverflow {
        current: usize,
        additional: usize,
        max: usize,
    },
    #[error("read timeout: {0}")]
    TimeoutError(#[source] tokio::time::error::Elapsed),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct BytesReadError {
    pub value: BytesReadErrorValue,
}

impl From<BytesReadErrorValue> for BytesReadError {
    fn from(val: BytesReadErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<io::Error> for BytesReadError {
    fn from(error: io::Error) -> Self {
        Self {
            value: BytesReadErrorValue::IO(error),
        }
    }
}

impl From<BytesIOError> for BytesReadError {
    fn from(error: BytesIOError) -> Self {
        Self {
            value: BytesReadErrorValue::BytesIOError(error),
        }
    }
}

impl From<tokio::time::error::Elapsed> for BytesReadError {
    fn from(error: tokio::time::error::Elapsed) -> Self {
        Self {
            value: BytesReadErrorValue::TimeoutError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct BytesWriteError {
    pub value: BytesWriteErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum BytesWriteErrorValue {
    #[error("io error")]
    IO(io::Error),
    #[error("bytes io error: {0}")]
    BytesIOError(BytesIOError),
    #[error("write time out")]
    Timeout,
    #[error("outof index")]
    OutofIndex,
}

impl From<io::Error> for BytesWriteError {
    fn from(error: io::Error) -> Self {
        Self {
            value: BytesWriteErrorValue::IO(error),
        }
    }
}

impl From<BytesIOError> for BytesWriteError {
    fn from(error: BytesIOError) -> Self {
        Self {
            value: BytesWriteErrorValue::BytesIOError(error),
        }
    }
}
