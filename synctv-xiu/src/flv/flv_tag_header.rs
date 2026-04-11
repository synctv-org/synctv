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
    //1010 11 1 1
    /*
        SoundFormat: UB[4]
        0 = Linear PCM, platform endian
        1 = ADPCM
        2 = MP3
        3 = Linear PCM, little endian
        4 = Nellymoser 16-kHz mono
        5 = Nellymoser 8-kHz mono
        6 = Nellymoser
        7 = G.711 A-law logarithmic PCM
        8 = G.711 mu-law logarithmic PCM
        9 = reserved
        10 = AAC
        11 = Speex
        14 = MP3 8-Khz
        15 = Device-specific sound
        Formats 7, 8, 14, and 15 are reserved for internal use
        AAC is supported in Flash Player 9,0,115,0 and higher.
        Speex is supported in Flash Player 10 and higher.
    */
    pub sound_format: u8,
    /*
        SoundRate: UB[2]
        Sampling rate
        0 = 5.5-kHz For AAC: always 3
        1 = 11-kHz
        2 = 22-kHz
        3 = 44-kHz
    */
    pub sound_rate: u8,
    /*
        SoundSize: UB[1]
        0 = snd8Bit
        1 = snd16Bit
        Size of each sample.
        This parameter only pertains to uncompressed formats.
        Compressed formats always decode to 16 bits internally
    */
    pub sound_size: u8,
    /*
        SoundType: UB[1]
        0 = sndMono
        1 = sndStereo
        Mono or stereo sound For Nellymoser: always 0
        For AAC: always 1
    */
    pub sound_type: u8,

    /*
        0: AAC sequence header
        1: AAC raw
    */
    pub aac_packet_type: u8,
}

impl AudioTagHeader {
    #[must_use]
    pub const fn defalut() -> Self {
        Self {
            sound_format: 0,
            sound_rate: 0,
            sound_size: 0,
            sound_type: 0,
            aac_packet_type: 0,
        }
    }
}

impl Unmarshal<&mut BytesReader, Result<Self, FlvDemuxerError>> for AudioTagHeader {
    fn unmarshal(reader: &mut BytesReader) -> Result<Self, FlvDemuxerError>
    where
        Self: Sized,
    {
        let mut tag_header = Self::defalut();

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
    /*
        1: keyframe (for AVC, a seekable frame)
        2: inter frame (for AVC, a non- seekable frame)
        3: disposable inter frame (H.263 only)
        4: generated keyframe (reserved for server use only)
        5: video info/command frame
    */
    pub frame_type: u8,
    /*
        1: JPEG (currently unused)
        2: Sorenson H.263
        3: Screen video
        4: On2 VP6
        5: On2 VP6 with alpha channel
        6: Screen video version 2
        7: AVC
        12: HEVC
    */
    pub codec_id: u8,
    /*
        0: AVC sequence header
        1: AVC NALU
        2: AVC end of sequence (lower level NALU sequence ender is not required or supported)
    */
    pub avc_packet_type: u8,
    pub composition_time: i32,
}

impl VideoTagHeader {
    #[must_use]
    pub const fn defalut() -> Self {
        Self {
            frame_type: 0,
            codec_id: 0,
            avc_packet_type: 0,
            composition_time: 0,
        }
    }
}

impl Unmarshal<&mut BytesReader, Result<Self, FlvDemuxerError>> for VideoTagHeader {
    fn unmarshal(reader: &mut BytesReader) -> Result<Self, FlvDemuxerError>
    where
        Self: Sized,
    {
        let mut tag_header = Self::defalut();

        let flags = reader.read_u8()?;
        tag_header.frame_type = flags >> 4;
        tag_header.codec_id = flags & 0x0f;

        if tag_header.codec_id == define::AvcCodecId::H264 as u8
            || tag_header.codec_id == define::AvcCodecId::HEVC as u8
        {
            tag_header.avc_packet_type = reader.read_u8()?;
            tag_header.composition_time = 0;

            //bigend 3bytes
            for _ in 0..3 {
                let time = reader.read_u8()?;
                //print!("==time0=={}\n", time);
                //print!("==time1=={}\n", self.tag.composition_time);
                tag_header.composition_time = (tag_header.composition_time << 8) + i32::from(time);
            }
            //transfer to signed i24
            if tag_header.composition_time & (1 << 23) != 0 {
                let sign_extend_mask = 0xff_ff << 23;
                // Sign extend the value
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
