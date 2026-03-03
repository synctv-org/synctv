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

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
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

/// HEVC (H.265) NAL unit types according to ITU-T H.265
/// NAL type is extracted from the first byte: (nal_byte >> 1) & 0x3F
pub mod hevc_nal_type {
    /// Coded slice of a CRA (Clean Random Access) picture
    pub const HEVC_NAL_CRA: u8 = 21;
    /// Coded slice of an IDR (Instantaneous Decoding Refresh) picture
    pub const HEVC_NAL_IDR_W_RADL: u8 = 19;
    /// Coded slice of an IDR_N_LP picture
    pub const HEVC_NAL_IDR_N_LP: u8 = 20;
    /// Video Parameter Set (VPS)
    pub const HEVC_NAL_VPS: u8 = 32;
    /// Sequence Parameter Set (SPS)
    pub const HEVC_NAL_SPS: u8 = 33;
    /// Picture Parameter Set (PPS)
    pub const HEVC_NAL_PPS: u8 = 34;
    /// Access Unit Delimiter (AUD)
    pub const HEVC_NAL_AUD: u8 = 35;
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

/// HEVC (H.265) profiles according to ITU-T H.265 Table A.2
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub enum HevcProfile {
    #[default]
    UNKNOWN = -1,
    /// Main profile (profile_idc = 1)
    Main = 1,
    /// Main 10 profile (profile_idc = 2)
    Main10 = 2,
    /// Main Still Picture profile (profile_idc = 3)
    MainStillPicture = 3,
    /// Rext format range extensions (profile_idc = 4)
    Rext = 4,
    /// High Throughput (profile_idc = 5)
    HighThroughput = 5,
}

#[must_use]
pub const fn u8_2_hevc_profile(profile_idc: u8) -> HevcProfile {
    match profile_idc {
        1_u8 => HevcProfile::Main,
        2_u8 => HevcProfile::Main10,
        3_u8 => HevcProfile::MainStillPicture,
        4_u8 => HevcProfile::Rext,
        5_u8 => HevcProfile::HighThroughput,
        _ => HevcProfile::UNKNOWN,
    }
}

/// HEVC (H.265) levels according to ITU-T H.265 Table A.1
/// Level values are expressed as general_level_idc * 3 (e.g., Level 4.0 = 120)
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub enum HevcLevel {
    #[default]
    UNKNOWN = -1,
    #[serde(rename = "1.0")]
    Level1 = 30,
    #[serde(rename = "2.0")]
    Level2 = 60,
    #[serde(rename = "2.1")]
    Level21 = 63,
    #[serde(rename = "3.0")]
    Level3 = 90,
    #[serde(rename = "3.1")]
    Level31 = 93,
    #[serde(rename = "4.0")]
    Level4 = 120,
    #[serde(rename = "4.1")]
    Level41 = 123,
    #[serde(rename = "5.0")]
    Level5 = 150,
    #[serde(rename = "5.1")]
    Level51 = 153,
    #[serde(rename = "5.2")]
    Level52 = 156,
    #[serde(rename = "6.0")]
    Level6 = 180,
    #[serde(rename = "6.1")]
    Level61 = 183,
    #[serde(rename = "6.2")]
    Level62 = 186,
}

#[must_use]
pub const fn u8_2_hevc_level(level_idc: u8) -> HevcLevel {
    match level_idc {
        30_u8 => HevcLevel::Level1,
        60_u8 => HevcLevel::Level2,
        63_u8 => HevcLevel::Level21,
        90_u8 => HevcLevel::Level3,
        93_u8 => HevcLevel::Level31,
        120_u8 => HevcLevel::Level4,
        123_u8 => HevcLevel::Level41,
        150_u8 => HevcLevel::Level5,
        153_u8 => HevcLevel::Level51,
        156_u8 => HevcLevel::Level52,
        180_u8 => HevcLevel::Level6,
        183_u8 => HevcLevel::Level61,
        186_u8 => HevcLevel::Level62,
        _ => HevcLevel::UNKNOWN,
    }
}

pub enum FlvData {
    Video { timestamp: u32, data: BytesMut },
    Audio { timestamp: u32, data: BytesMut },
    MetaData { timestamp: u32, data: BytesMut },
}
