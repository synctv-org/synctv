use crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError};

#[derive(Debug, thiserror::Error)]
pub enum UnpackErrorValue {
    #[error("bytes read error: {0}")]
    BytesReadError(BytesReadError),
    #[error("unknow read state")]
    UnknowReadState,
    #[error("empty chunks")]
    EmptyChunks,
    //IO(io::Error),
    #[error("cannot parse")]
    CannotParse,
    #[error("message size {0} exceeds maximum {1}")]
    MessageTooLarge(usize, usize),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct UnpackError {
    pub value: UnpackErrorValue,
}

impl From<UnpackErrorValue> for UnpackError {
    fn from(val: UnpackErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<BytesReadError> for UnpackError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: UnpackErrorValue::BytesReadError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackErrorValue {
    #[error("not exist header")]
    NotExistHeader,
    #[error("unknow read state")]
    UnknowReadState,
    #[error("invalid chunk stream id {0}")]
    InvalidChunkStreamId(u32),
    #[error("bytes writer error: {0}")]
    BytesWriteError(BytesWriteError),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct PackError {
    pub value: PackErrorValue,
}

impl From<PackErrorValue> for PackError {
    fn from(val: PackErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<BytesWriteError> for PackError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: PackErrorValue::BytesWriteError(error),
        }
    }
}
