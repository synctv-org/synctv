use std::{io, net::AddrParseError};

#[derive(Debug, thiserror::Error)]
pub enum BytesIOErrorValue {
    #[error("not enough bytes")]
    NotEnoughBytes,
    #[error("empty stream")]
    EmptyStream,
    #[error("io error")]
    IOError(io::Error),
    #[error("invalid socket address {address}: {source}")]
    InvalidSocketAddress {
        address: String,
        #[source]
        source: AddrParseError,
    },
    #[error("operation timed out")]
    TimeoutError(tokio::time::error::Elapsed),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("no available UDP port pair")]
    NoAvailableUdpPortPair,
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct BytesIOError {
    pub value: BytesIOErrorValue,
}

impl From<BytesIOErrorValue> for BytesIOError {
    fn from(val: BytesIOErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<io::Error> for BytesIOError {
    fn from(error: io::Error) -> Self {
        Self {
            value: BytesIOErrorValue::IOError(error),
        }
    }
}
