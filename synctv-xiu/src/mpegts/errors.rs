use {
    crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    std::io::Error,
};

#[derive(Debug, thiserror::Error)]
pub enum MpegTsErrorValue {
    #[error("bytes read error")]
    BytesReadError(BytesReadError),

    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),

    #[error("io error")]
    IOError(Error),

    #[error("program number exists")]
    ProgramNumberExists,

    #[error("pmt count execeed")]
    PmtCountExeceed,

    #[error("stream count execeed")]
    StreamCountExeceed,

    #[error("stream not found")]
    StreamNotFound,
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct MpegTsError {
    pub value: MpegTsErrorValue,
}

impl From<BytesReadError> for MpegTsError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: MpegTsErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for MpegTsError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: MpegTsErrorValue::BytesWriteError(error),
        }
    }
}

impl From<Error> for MpegTsError {
    fn from(error: Error) -> Self {
        Self {
            value: MpegTsErrorValue::IOError(error),
        }
    }
}
