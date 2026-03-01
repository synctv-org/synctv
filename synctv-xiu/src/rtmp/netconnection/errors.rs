use {
    crate::flv::amf0::errors::{Amf0ReadError, Amf0WriteError},
    crate::rtmp::chunk::errors::PackError,
};

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct NetConnectionError {
    pub value: NetConnectionErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum NetConnectionErrorValue {
    #[error("amf0 write error: {0}")]
    Amf0WriteError(Amf0WriteError),
    #[error("amf0 read error: {0}")]
    Amf0ReadError(Amf0ReadError),
    #[error("pack error")]
    PackError(PackError),
}

impl From<Amf0WriteError> for NetConnectionError {
    fn from(error: Amf0WriteError) -> Self {
        Self {
            value: NetConnectionErrorValue::Amf0WriteError(error),
        }
    }
}

impl From<Amf0ReadError> for NetConnectionError {
    fn from(error: Amf0ReadError) -> Self {
        Self {
            value: NetConnectionErrorValue::Amf0ReadError(error),
        }
    }
}

impl From<PackError> for NetConnectionError {
    fn from(error: PackError) -> Self {
        Self {
            value: NetConnectionErrorValue::PackError(error),
        }
    }
}
