#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::manual_let_else,
        clippy::unwrap_used
    )
)]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod bytesio;
pub mod flv;
pub mod h264;
pub mod hls;
pub mod httpflv;
pub mod mpegts;
pub mod rtmp;
pub mod storage;
pub mod streamhub;
