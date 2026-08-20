pub mod epat_pid {
    pub const PAT_TID_PAS: u16 = 0x00;
    pub const PAT_TID_PMS: u16 = 0x02; // TS_program_map_section
}

pub mod ts {
    pub const TS_PACKET_SIZE: u8 = 188;
}

pub mod epsi_stream_type {
    pub const PSI_STREAM_MP3: u8 = 0x04; // ISO/IEC 13818-3 Audio
    pub const PSI_STREAM_PRIVATE_DATA: u8 = 0x06;
    pub const PSI_STREAM_H264: u8 = 0x1b; // H.264 (27)
    pub const PSI_STREAM_HEVC: u8 = 0x24; // H.265/HEVC (36)
    pub const PSI_STREAM_AAC: u8 = 0x0f;
    pub const PSI_STREAM_MPEG4_AAC: u8 = 0x1c;
    pub const PSI_STREAM_AUDIO_OPUS: u8 = 0x9c;
}

pub mod epes_stream_id {

    pub const PES_SID_AUDIO: u8 = 0xC0; // ISO/IEC 13818-3/11172-3/13818-7/14496-3 audio stream '110x xxxx'
    pub const PES_SID_VIDEO: u8 = 0xE0; // H.262 | H.264 | H.265 | ISO/IEC 13818-2/11172-2/14496-2/14496-10 video stream '1110 xxxx'
    pub const PES_SID_PRIVATE_1: u8 = 0xBD; // private_stream_1
}

pub const AF_FLAG_PCR: u8 = 0x10;
pub const AF_FLAG_RANDOM_ACCESS_INDICATOR: u8 = 0x40;
pub const PTS_NO_VALUE: i64 = i64::MIN; //(int64_t)0x8000000000000000L

pub const PES_HEADER_LEN: u8 = 6; // 3-bytes packet_start_code_prefix + 1-byte stream_id + 2-bytes PES_packet_length

pub const TS_PAYLOAD_UNIT_START_INDICATOR: u8 = 0x40;

pub const TS_PACKET_SIZE: usize = 188;

pub const MPEG_FLAG_IDR_FRAME: u16 = 0x0001;
pub const MPEG_FLAG_H264_H265_WITH_AUD: u16 = 0x8000;

pub const PAT_PERIOD: i64 = 400 * 90;
