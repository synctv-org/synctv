use {
    crate::bytesio::{bytes_errors::BytesWriteError, bytesio_errors::BytesIOError},
    crate::flv::amf0::errors::Amf0WriteError,
    crate::rtmp::{
        cache::errors::CacheError,
        chunk::errors::{PackError, UnpackError},
        handshake::errors::HandshakeError,
        messages::errors::MessageError,
        netconnection::errors::NetConnectionError,
        netstream::errors::NetStreamError,
        user_control_messages::errors::EventMessagesError,
    },
    crate::streamhub::errors::StreamHubError,
    tokio::sync::oneshot::error::RecvError,
};

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct SessionError {
    pub value: SessionErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionErrorValue {
    #[error("amf0 write error: {0}")]
    Amf0WriteError(#[from] Amf0WriteError),
    #[error("bytes write error: {0}")]
    BytesWriteError(#[from] BytesWriteError),
    #[error("unpack error: {0}")]
    UnPackError(#[from] UnpackError),

    #[error("message error: {0}")]
    MessageError(#[from] MessageError),
    #[error("net connection error: {0}")]
    NetConnectionError(#[from] NetConnectionError),
    #[error("net stream error: {0}")]
    NetStreamError(#[from] NetStreamError),

    #[error("event messages error: {0}")]
    EventMessagesError(#[from] EventMessagesError),
    #[error("net io error: {0}")]
    BytesIOError(#[from] BytesIOError),
    #[error("pack error: {0}")]
    PackError(#[from] PackError),
    #[error("handshake error: {0}")]
    HandshakeError(#[from] HandshakeError),
    #[error("cache error name: {0}")]
    CacheError(#[from] CacheError),
    #[error("tokio: oneshot receiver err: {0}")]
    RecvError(#[from] RecvError),
    #[error("streamhub channel err: {0}")]
    ChannelError(#[from] StreamHubError),

    #[error("invalid AMF0 value count")]
    InvalidAmf0ValueCount,
    #[error("invalid AMF0 value type")]
    InvalidAmf0ValueType,
    #[error("invalid enhanced RTMP video data: {0}")]
    InvalidEnhancedVideoData(String),
    #[error("stream hub event send error")]
    StreamHubEventSendErr,
    #[error("none frame data sender error")]
    NoneFrameDataSender,
    #[error("none frame data receiver error")]
    NoneFrameDataReceiver,
    #[error("send frame data error")]
    SendFrameDataErr,
    #[error("subscribe count limit is reached.")]
    SubscribeCountLimitReach,

    #[error("no app name error")]
    NoAppName,
    #[error("no stream name error")]
    NoStreamName,
    #[error("no media data can be received now.")]
    NoMediaDataReceived,

    #[error("session is finished.")]
    Finish,
    #[error("auth failed: {0}")]
    AuthFailed(String),
    #[error("handshake timeout")]
    Timeout,
}

// Manual From impls for SessionError (wraps inner error type -> SessionErrorValue -> SessionError).
// The #[from] on SessionErrorValue handles InnerError -> SessionErrorValue automatically,
// but we still need these impls to convert InnerError -> SessionError directly.

impl From<Amf0WriteError> for SessionError {
    fn from(error: Amf0WriteError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<BytesWriteError> for SessionError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<UnpackError> for SessionError {
    fn from(error: UnpackError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<MessageError> for SessionError {
    fn from(error: MessageError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<NetConnectionError> for SessionError {
    fn from(error: NetConnectionError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<NetStreamError> for SessionError {
    fn from(error: NetStreamError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<EventMessagesError> for SessionError {
    fn from(error: EventMessagesError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<BytesIOError> for SessionError {
    fn from(error: BytesIOError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<PackError> for SessionError {
    fn from(error: PackError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<HandshakeError> for SessionError {
    fn from(error: HandshakeError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<CacheError> for SessionError {
    fn from(error: CacheError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<RecvError> for SessionError {
    fn from(error: RecvError) -> Self {
        Self {
            value: error.into(),
        }
    }
}

impl From<StreamHubError> for SessionError {
    fn from(error: StreamHubError) -> Self {
        Self {
            value: error.into(),
        }
    }
}
