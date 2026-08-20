#[derive(Debug, thiserror::Error)]
#[error("The url is not valid")]
pub struct RtmpUrlParseError;
