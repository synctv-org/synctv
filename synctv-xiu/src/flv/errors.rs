use {
    crate::bytesio::bits_errors::BitError,
    crate::bytesio::bytes_errors::{BytesReadError, BytesWriteError},
    crate::h264::errors::H264Error,
};

#[derive(Debug, thiserror::Error)]
pub enum TagParseErrorValue {
    #[error("bytes read error")]
    BytesReadError(BytesReadError),
    #[error("tag data length error")]
    TagDataLength,
    #[error("unknown tag type")]
    UnknownTagType,
}
#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct TagParseError {
    pub value: TagParseErrorValue,
}

impl From<BytesReadError> for TagParseError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: TagParseErrorValue::BytesReadError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct FlvMuxerError {
    pub value: MuxerErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum MuxerErrorValue {
    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),
}

impl From<BytesWriteError> for FlvMuxerError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: MuxerErrorValue::BytesWriteError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct FlvDemuxerError {
    pub value: DemuxerErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum DemuxerErrorValue {
    #[error("bytes write error:{0}")]
    BytesWriteError(#[source] BytesWriteError),
    #[error("bytes read error:{0}")]
    BytesReadError(#[source] BytesReadError),
    #[error("mpeg avc error:{0}")]
    MpegAvcError(#[source] Mpeg4AvcHevcError),
    #[error("mpeg aac error:{0}")]
    MpegAacError(#[source] MpegAacError),
}

impl From<BytesWriteError> for FlvDemuxerError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: DemuxerErrorValue::BytesWriteError(error),
        }
    }
}

impl From<BytesReadError> for FlvDemuxerError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: DemuxerErrorValue::BytesReadError(error),
        }
    }
}

impl From<Mpeg4AvcHevcError> for FlvDemuxerError {
    fn from(error: Mpeg4AvcHevcError) -> Self {
        Self {
            value: DemuxerErrorValue::MpegAvcError(error),
        }
    }
}

impl From<MpegAacError> for FlvDemuxerError {
    fn from(error: MpegAacError) -> Self {
        Self {
            value: DemuxerErrorValue::MpegAacError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MpegErrorValue {
    #[error("bytes read error:{0}")]
    BytesReadError(#[source] BytesReadError),
    #[error("bytes write error:{0}")]
    BytesWriteError(#[source] BytesWriteError),
    #[error("bits error:{0}")]
    BitError(#[source] BitError),
    #[error("h264 error:{0}")]
    H264Error(#[source] H264Error),
    #[error("there is not enough bits to read")]
    NotEnoughBitsToRead,
    #[error("integer value {value} exceeds {target} range")]
    IntegerRange { value: u128, target: &'static str },
    #[error("unsupported AAC epConfig value {0}")]
    UnsupportedAacEpConfig(u64),
    #[error("empty NAL unit")]
    EmptyNalu,
    #[error("invalid NAL unit length size {0}; expected 1..=4")]
    InvalidNaluLength(u8),
    #[error("invalid SPS NAL unit type")]
    SPSNalunitTypeNotCorrect,
    #[error("not supported sampling frequency")]
    NotSupportedSamplingFrequency,
    #[error("SPS/PPS count {count} exceeds maximum allowed {max}")]
    SpsPpsCountExceeded { count: u8, max: u8 },
    #[error("{kind} count {declared} exceeds available parameter sets {available}")]
    ParameterSetCountMismatch {
        kind: &'static str,
        declared: u8,
        available: usize,
    },
}
#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct Mpeg4AvcHevcError {
    pub value: MpegErrorValue,
}

impl From<BytesReadError> for Mpeg4AvcHevcError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: MpegErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for Mpeg4AvcHevcError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: MpegErrorValue::BytesWriteError(error),
        }
    }
}

impl From<H264Error> for Mpeg4AvcHevcError {
    fn from(error: H264Error) -> Self {
        Self {
            value: MpegErrorValue::H264Error(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct MpegAacError {
    pub value: MpegErrorValue,
}

impl From<BytesReadError> for MpegAacError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: MpegErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for MpegAacError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: MpegErrorValue::BytesWriteError(error),
        }
    }
}

impl From<BitError> for MpegAacError {
    fn from(error: BitError) -> Self {
        Self {
            value: MpegErrorValue::BitError(error),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BitVecErrorValue {
    #[error("not enough bits left")]
    NotEnoughBits,
}
#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct BitVecError {
    pub value: BitVecErrorValue,
}
