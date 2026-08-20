use {
    crate::bytesio::bytes_errors::BytesReadError,
    crate::flv::errors::{FlvDemuxerError, Mpeg4AvcHevcError, MpegAacError},
    crate::h264::errors::H264Error,
    crate::rtmp::chunk::errors::PackError,
};

#[derive(Debug, thiserror::Error)]
pub enum CacheErrorValue {
    #[error("cache tag parse error")]
    DemuxerError(FlvDemuxerError),
    #[error("mpeg aac error")]
    MpegAacError(MpegAacError),
    #[error("mpeg avc error")]
    MpegAvcError(Mpeg4AvcHevcError),
    #[error("pack error")]
    PackError(PackError),
    #[error("read bytes error")]
    BytesReadError(BytesReadError),
    #[error("h264 error")]
    H264Error(H264Error),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct CacheError {
    pub value: CacheErrorValue,
}

impl From<FlvDemuxerError> for CacheError {
    fn from(error: FlvDemuxerError) -> Self {
        Self {
            value: CacheErrorValue::DemuxerError(error),
        }
    }
}

impl From<H264Error> for CacheError {
    fn from(error: H264Error) -> Self {
        Self {
            value: CacheErrorValue::H264Error(error),
        }
    }
}

impl From<MpegAacError> for CacheError {
    fn from(error: MpegAacError) -> Self {
        Self {
            value: CacheErrorValue::MpegAacError(error),
        }
    }
}

impl From<Mpeg4AvcHevcError> for CacheError {
    fn from(error: Mpeg4AvcHevcError) -> Self {
        Self {
            value: CacheErrorValue::MpegAvcError(error),
        }
    }
}

impl From<BytesReadError> for CacheError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: CacheErrorValue::BytesReadError(error),
        }
    }
}

impl From<PackError> for CacheError {
    fn from(error: PackError) -> Self {
        Self {
            value: CacheErrorValue::PackError(error),
        }
    }
}
