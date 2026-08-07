use std::fmt;

pub const WINDOW_ACKNOWLEDGEMENT_SIZE: u32 = 4096;
pub const PEER_BANDWIDTH: u32 = 4096;

pub mod peer_bandwidth_limit_type {
    pub const HARD: u8 = 0;
    pub const SOFT: u8 = 1;
    pub const DYNAMIC: u8 = 2;
}

pub const FMSVER: &str = "FMS/3,0,1,123";
pub const CAPABILITIES: f64 = 31.0;
pub const LEVEL: &str = "status";

pub const OBJENCODING_AMF0: f64 = 0.0;
pub const OBJENCODING_AMF3: f64 = 3.0;

pub const STREAM_ID: f64 = 1.0;

pub const TRANSACTION_ID_CONNECT: u8 = 1;
pub const TRANSACTION_ID_CREATE_STREAM: u8 = 2;

//pub mod
pub const RTMP_LEVEL_WARNING: &str = "warning";
pub const RTMP_LEVEL_STATUS: &str = "status";
pub const RTMP_LEVEL_ERROR: &str = "error";

pub enum SessionType {
    Client,
    Server,
}

impl fmt::Display for SessionType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let client_type = match self {
            Self::Client => String::from("client"),
            Self::Server => String::from("server"),
        };
        write!(f, "{client_type}")
    }
}
