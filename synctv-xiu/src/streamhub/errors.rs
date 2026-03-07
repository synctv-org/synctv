use crate::bytesio::bytes_errors::BytesReadError;
use crate::bytesio::bytes_errors::BytesWriteError;
use serde_json::error::Error;
use tokio::sync::oneshot::error::RecvError;

#[derive(Debug, thiserror::Error)]
pub enum StreamHubErrorValue {
    #[error("no app name")]
    NoAppName,
    #[error("no stream name")]
    NoStreamName,
    #[error("no app or stream name")]
    NoAppOrStreamName,
    #[error("exists")]
    Exists,
    #[error("send error")]
    SendError,
    #[error("send video error")]
    SendVideoError,
    #[error("send audio error")]
    SendAudioError,
    #[error("bytes read error")]
    BytesReadError(BytesReadError),
    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),
    #[error("not correct data sender type")]
    NotCorrectDataSenderType,
    #[error("subscriber channel closed")]
    SubscriberClosed,
    #[error("Tokio oneshot recv error")]
    RecvError(RecvError),
    #[error("Serde json error")]
    SerdeError(Error),
    #[error("client session error: {0}")]
    ClientSessionError(String),
    #[error("internal streamhub task failed: {0}")]
    InternalTaskError(String),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct StreamHubError {
    pub value: StreamHubErrorValue,
}

impl From<BytesReadError> for StreamHubError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: StreamHubErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for StreamHubError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: StreamHubErrorValue::BytesWriteError(error),
        }
    }
}

impl From<RecvError> for StreamHubError {
    fn from(error: RecvError) -> Self {
        Self {
            value: StreamHubErrorValue::RecvError(error),
        }
    }
}

impl From<Error> for StreamHubError {
    fn from(error: Error) -> Self {
        Self {
            value: StreamHubErrorValue::SerdeError(error),
        }
    }
}

impl From<String> for StreamHubError {
    fn from(error: String) -> Self {
        Self {
            value: StreamHubErrorValue::ClientSessionError(error),
        }
    }
}
