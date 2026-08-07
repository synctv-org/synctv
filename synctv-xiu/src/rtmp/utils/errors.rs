#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct RtmpUrlParseError {
    pub value: RtmpUrlParseErrorValue,
}

#[derive(Debug, thiserror::Error)]
pub enum RtmpUrlParseErrorValue {
    #[error("The url is not valid")]
    Notvalid,
}
