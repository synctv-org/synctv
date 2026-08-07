mod client;
mod types;

pub use client::QnapClient;
pub use types::{
    QnapFile, QnapHardwareTranscode, QnapList, QnapLogin, QnapShare, QnapTranscodeEstimate,
    QnapTranscodeResolution,
};
