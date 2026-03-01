use {crate::flv::amf0::errors::Amf0WriteError, crate::rtmp::chunk::errors::PackError};

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct NetStreamError {
    pub value: NetStreamErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum NetStreamErrorValue {
    #[error("amf0 write error: {0}")]
    Amf0WriteError(Amf0WriteError),
    #[error("invalid max chunk size")]
    InvalidMaxChunkSize { chunk_size: usize },
    #[error("pack error")]
    PackError(PackError),
}

impl From<Amf0WriteError> for NetStreamError {
    fn from(error: Amf0WriteError) -> Self {
        Self {
            value: NetStreamErrorValue::Amf0WriteError(error),
        }
    }
}

impl From<PackError> for NetStreamError {
    fn from(error: PackError) -> Self {
        Self {
            value: NetStreamErrorValue::PackError(error),
        }
    }
}
