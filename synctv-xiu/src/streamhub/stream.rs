use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum StreamIdentifier {
    #[serde(rename = "rtmp")]
    Rtmp {
        app_name: String,
        stream_name: String,
    },
}
impl fmt::Display for StreamIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Rtmp {
                app_name,
                stream_name,
            } => {
                write!(f, "RTMP - app_name: {app_name}, stream_name: {stream_name}")
            }
        }
    }
}
