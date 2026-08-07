use crate::bytesio::bytes_writer::BytesWriter;

use {
    super::{
        define,
        errors::{FlvDemuxerError, FlvMuxerError},
    },
    super::{Marshal, Unmarshal},
    crate::bytesio::bytes_reader::BytesReader,
    bytes::BytesMut,
};

#[derive(Clone, Debug)]
pub struct AudioTagHeader {
    /// FLV SoundFormat field.
    pub sound_format: u8,
    /// FLV SoundRate field.
    pub sound_rate: u8,
    /// FLV SoundSize field.
    pub sound_size: u8,
    /// FLV SoundType field.
    pub sound_type: u8,
    /// AACPacketType for AAC tags.
    pub aac_packet_type: u8,
}

impl AudioTagHeader {
    #[must_use]
    pub const fn default_header() -> Self {
        Self {
            sound_format: 0,
            sound_rate: 0,
            sound_size: 0,
            sound_type: 0,
            aac_packet_type: 0,
        }
    }
}

impl Default for AudioTagHeader {
    fn default() -> Self {
        Self::default_header()
    }
}

impl Unmarshal<&mut BytesReader, Result<Self, FlvDemuxerError>> for AudioTagHeader {
    fn unmarshal(reader: &mut BytesReader) -> Result<Self, FlvDemuxerError>
    where
        Self: Sized,
    {
        let mut tag_header = Self::default_header();

        let flags = reader.read_u8()?;
        tag_header.sound_format = flags >> 4;
        tag_header.sound_rate = (flags >> 2) & 0x03;
        tag_header.sound_size = (flags >> 1) & 0x01;
        tag_header.sound_type = flags & 0x01;

        if tag_header.sound_format == define::SoundFormat::AAC as u8 {
            tag_header.aac_packet_type = reader.read_u8()?;
        }

        Ok(tag_header)
    }
}

impl Marshal<Result<BytesMut, FlvMuxerError>> for AudioTagHeader {
    fn marshal(&self) -> Result<BytesMut, FlvMuxerError> {
        let mut writer = BytesWriter::default();

        let byte_1st =
            self.sound_format << 4 | self.sound_rate << 2 | self.sound_size << 1 | self.sound_type;
        writer.write_u8(byte_1st)?;

        if self.sound_format == define::SoundFormat::AAC as u8 {
            writer.write_u8(self.aac_packet_type)?;
        }

        Ok(writer.extract_current_bytes())
    }
}

#[derive(Clone)]
pub struct VideoTagHeader {
    /// FLV FrameType field.
    pub frame_type: u8,
    /// FLV CodecID field.
    pub codec_id: u8,
    /// AVCPacketType/HEVCPacketType for AVC and HEVC tags.
    pub avc_packet_type: u8,
    /// Signed 24-bit composition time offset.
    pub composition_time: i32,
}

impl VideoTagHeader {
    #[must_use]
    pub const fn default_header() -> Self {
        Self {
            frame_type: 0,
            codec_id: 0,
            avc_packet_type: 0,
            composition_time: 0,
        }
    }
}

impl Default for VideoTagHeader {
    fn default() -> Self {
        Self::default_header()
    }
}

impl Unmarshal<&mut BytesReader, Result<Self, FlvDemuxerError>> for VideoTagHeader {
    fn unmarshal(reader: &mut BytesReader) -> Result<Self, FlvDemuxerError>
    where
        Self: Sized,
    {
        let mut tag_header = Self::default_header();

        let flags = reader.read_u8()?;
        tag_header.frame_type = flags >> 4;
        tag_header.codec_id = flags & 0x0f;

        if tag_header.codec_id == define::AvcCodecId::H264 as u8
            || tag_header.codec_id == define::AvcCodecId::HEVC as u8
        {
            tag_header.avc_packet_type = reader.read_u8()?;
            tag_header.composition_time = 0;

            for _ in 0..3 {
                let time = reader.read_u8()?;
                tag_header.composition_time = (tag_header.composition_time << 8) + i32::from(time);
            }
            if tag_header.composition_time & (1 << 23) != 0 {
                let sign_extend_mask = 0xff_ff << 23;
                tag_header.composition_time |= sign_extend_mask;
            }
        }

        Ok(tag_header)
    }
}

impl Marshal<Result<BytesMut, FlvMuxerError>> for VideoTagHeader {
    fn marshal(&self) -> Result<BytesMut, FlvMuxerError> {
        let mut writer = BytesWriter::default();

        let byte_1st = self.frame_type << 4 | self.codec_id;
        writer.write_u8(byte_1st)?;

        if self.codec_id == define::AvcCodecId::H264 as u8
            || self.codec_id == define::AvcCodecId::HEVC as u8
        {
            writer.write_u8(self.avc_packet_type)?;

            let cts = self.composition_time;
            let cts_bytes = cts.to_be_bytes();
            writer.write_u8(cts_bytes[1])?;
            writer.write_u8(cts_bytes[2])?;
            writer.write_u8(cts_bytes[3])?;
        }

        Ok(writer.extract_current_bytes())
    }
}
