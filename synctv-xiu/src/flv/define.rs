use bytes::BytesMut;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub enum SoundFormat {
    #[default]
    AAC = 10,
    OPUS = 13,
}

pub mod aac_packet_type {
    pub const AAC_SEQHDR: u8 = 0;
    pub const AAC_RAW: u8 = 1;
}

pub mod avc_packet_type {
    pub const AVC_SEQHDR: u8 = 0;
    pub const AVC_NALU: u8 = 1;
    pub const AVC_EOS: u8 = 2;
}

pub mod frame_type {
    /*
        1: keyframe (for AVC, a seekable frame)
        2: inter frame (for AVC, a non- seekable frame)
        3: disposable inter frame (H.263 only)
        4: generated keyframe (reserved for server use only)
        5: video info/command frame
    */
    pub const KEY_FRAME: u8 = 1;
    pub const INTER_FRAME: u8 = 2;
}

#[derive(Debug, Clone, Serialize, Default)]
pub enum AvcCodecId {
    #[default]
    UNKNOWN = 0,
    H264 = 7,
    HEVC = 12,
}

#[must_use]
pub const fn u8_2_avc_codec_id(codec_id: u8) -> AvcCodecId {
    match codec_id {
        7_u8 => AvcCodecId::H264,
        12_u8 => AvcCodecId::HEVC,
        _ => AvcCodecId::UNKNOWN,
    }
}

pub mod tag_type {
    pub const AUDIO: u8 = 8;
    pub const VIDEO: u8 = 9;
    pub const SCRIPT_DATA_AMF: u8 = 18;
}

pub mod h264_nal_type {
    pub const H264_NAL_IDR: u8 = 5;
    pub const H264_NAL_SPS: u8 = 7;
    pub const H264_NAL_PPS: u8 = 8;
    pub const H264_NAL_AUD: u8 = 9;
}
#[derive(Debug, Clone, Serialize, Default)]
pub enum AacProfile {
    // @see @see ISO_IEC_14496-3-AAC-2001.pdf, page 23
    #[default]
    UNKNOWN = -1,
    LC = 2,
    SSR = 3,
    // AAC HE = LC+SBR
    HE = 5,
    // AAC HEv2 = LC+SBR+PS
    HEV2 = 29,
}

#[must_use]
pub const fn u8_2_aac_profile(profile: u8) -> AacProfile {
    match profile {
        2_u8 => AacProfile::LC,
        3_u8 => AacProfile::SSR,
        5_u8 => AacProfile::HE,
        29_u8 => AacProfile::HEV2,
        _ => AacProfile::UNKNOWN,
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub enum AvcProfile {
    #[default]
    UNKNOWN = -1,
    // @see ffmpeg, libavcodec/avcodec.h:2713
    Baseline = 66,
    Main = 77,
    Extended = 88,
    High = 100,
}

#[must_use]
pub const fn u8_2_avc_profile(profile: u8) -> AvcProfile {
    match profile {
        66_u8 => AvcProfile::Baseline,
        77_u8 => AvcProfile::Main,
        88_u8 => AvcProfile::Extended,
        100_u8 => AvcProfile::High,
        _ => AvcProfile::UNKNOWN,
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub enum AvcLevel {
    #[default]
    UNKNOWN = -1,
    #[serde(rename = "1.0")]
    Level1 = 10,
    #[serde(rename = "1.1")]
    Level11 = 11,
    #[serde(rename = "1.2")]
    Level12 = 12,
    #[serde(rename = "1.3")]
    Level13 = 13,
    #[serde(rename = "2.0")]
    Level2 = 20,
    #[serde(rename = "2.1")]
    Level21 = 21,
    #[serde(rename = "2.2")]
    Level22 = 22,
    #[serde(rename = "3.0")]
    Level3 = 30,
    #[serde(rename = "3.1")]
    Level31 = 31,
    #[serde(rename = "3.2")]
    Level32 = 32,
    #[serde(rename = "4.0")]
    Level4 = 40,
    #[serde(rename = "4.1")]
    Level41 = 41,
    #[serde(rename = "5.0")]
    Level5 = 50,
    #[serde(rename = "5.1")]
    Level51 = 51,
}

#[must_use]
pub const fn u8_2_avc_level(profile: u8) -> AvcLevel {
    match profile {
        10_u8 => AvcLevel::Level1,
        11_u8 => AvcLevel::Level11,
        12_u8 => AvcLevel::Level12,
        13_u8 => AvcLevel::Level13,
        20_u8 => AvcLevel::Level2,
        21_u8 => AvcLevel::Level21,
        22_u8 => AvcLevel::Level22,
        30_u8 => AvcLevel::Level3,
        31_u8 => AvcLevel::Level31,
        32_u8 => AvcLevel::Level32,
        40_u8 => AvcLevel::Level4,
        41_u8 => AvcLevel::Level41,
        50_u8 => AvcLevel::Level5,
        51_u8 => AvcLevel::Level51,

        _ => AvcLevel::UNKNOWN,
    }
}

pub enum FlvData {
    Video { timestamp: u32, data: BytesMut },
    Audio { timestamp: u32, data: BytesMut },
    MetaData { timestamp: u32, data: BytesMut },
}
