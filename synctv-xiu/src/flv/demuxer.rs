use super::{
    flv_tag_header::{AudioTagHeader, VideoTagHeader},
    Unmarshal,
};

use {
    super::{
        define::{aac_packet_type, avc_packet_type, tag_type, AvcCodecId, FlvData, SoundFormat},
        errors::FlvDemuxerError,
        mpeg4_aac::Mpeg4AacProcessor,
        mpeg4_avc::Mpeg4AvcProcessor,
        mpeg4_hevc::Mpeg4HevcProcessor,
    },
    crate::bytesio::bytes_reader::BytesReader,
    byteorder::BigEndian,
    bytes::BytesMut,
};

const FLV_HEADER_LEN: usize = 9;

struct FlvTagHeader {
    tag_type: u8,
    data_size: u32,
    timestamp: u32,
}

impl FlvTagHeader {
    fn read_from(reader: &mut BytesReader) -> Result<Self, FlvDemuxerError> {
        reader.read_u32::<BigEndian>()?;

        let tag_type = reader.read_u8()?;
        let data_size = reader.read_u24::<BigEndian>()?;
        let timestamp_lower = reader.read_u24::<BigEndian>()?;
        let timestamp_extended = reader.read_u8()?;
        reader.read_u24::<BigEndian>()?;

        Ok(Self {
            tag_type,
            data_size,
            timestamp: timestamp_lower | (u32::from(timestamp_extended) << 24),
        })
    }
}

#[derive(Default)]
pub struct FlvDemuxerAudioData {
    pub has_data: bool,
    pub sound_format: u8,
    pub dts: i64,
    pub pts: i64,
    pub data: BytesMut,
}

impl FlvDemuxerAudioData {
    #[must_use]
    pub fn new() -> Self {
        Self {
            has_data: false,
            sound_format: 0,
            dts: 0,
            pts: 0,
            data: BytesMut::new(),
        }
    }
}
#[derive(Debug, Default)]
pub struct FlvDemuxerVideoData {
    pub frame_type: u8,
    pub codec_id: u8,
    pub dts: i64,
    pub pts: i64,
    pub data: BytesMut,
}

impl FlvDemuxerVideoData {
    #[must_use]
    pub fn new() -> Self {
        Self {
            codec_id: 0,
            dts: 0,
            pts: 0,
            frame_type: 0,
            data: BytesMut::new(),
        }
    }
}

#[derive(Default)]
pub struct FlvVideoTagDemuxer {
    avc_processor: Mpeg4AvcProcessor,
    hevc_processor: Mpeg4HevcProcessor,
}

impl FlvVideoTagDemuxer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            avc_processor: Mpeg4AvcProcessor::new(),
            hevc_processor: Mpeg4HevcProcessor::default(),
        }
    }

    pub fn demux(
        &mut self,
        timestamp: u32,
        data: BytesMut,
    ) -> Result<Option<FlvDemuxerVideoData>, FlvDemuxerError> {
        let mut reader = BytesReader::new(data);

        let tag_header = VideoTagHeader::unmarshal(&mut reader)?;
        if tag_header.codec_id == AvcCodecId::H264 as u8 {
            match tag_header.avc_packet_type {
                avc_packet_type::AVC_SEQHDR => {
                    self.avc_processor
                        .decoder_configuration_record_load(&mut reader)?;

                    return Ok(None);
                }
                avc_packet_type::AVC_NALU => {
                    let data = self.avc_processor.h264_mp4toannexb(&mut reader)?;

                    let video_data = FlvDemuxerVideoData {
                        codec_id: AvcCodecId::H264 as u8,
                        pts: i64::from(timestamp) + i64::from(tag_header.composition_time),
                        dts: i64::from(timestamp),
                        frame_type: tag_header.frame_type,
                        data,
                    };
                    return Ok(Some(video_data));
                }
                _ => {}
            }
        } else if tag_header.codec_id == AvcCodecId::HEVC as u8 {
            match tag_header.avc_packet_type {
                avc_packet_type::AVC_SEQHDR => {
                    // Load HEVC decoder configuration record (VPS/SPS/PPS)
                    self.hevc_processor
                        .decoder_configuration_record_load(&mut reader)?;
                    return Ok(None);
                }
                avc_packet_type::AVC_NALU => {
                    // Convert HEVC NAL units from MP4 format to Annex B byte stream format
                    let data = self.hevc_processor.hevc_mp4toannexb(&mut reader)?;

                    let video_data = FlvDemuxerVideoData {
                        codec_id: AvcCodecId::HEVC as u8,
                        pts: i64::from(timestamp) + i64::from(tag_header.composition_time),
                        dts: i64::from(timestamp),
                        frame_type: tag_header.frame_type,
                        data,
                    };
                    return Ok(Some(video_data));
                }
                _ => {}
            }
        } else {
            tracing::warn!(
                codec_id = tag_header.codec_id,
                "Unsupported video codec; only H.264 and H.265/HEVC are supported, dropping frame"
            );
        }

        Ok(None)
    }
}

#[derive(Default)]
pub struct FlvAudioTagDemuxer {
    aac_processor: Mpeg4AacProcessor,
}

impl FlvAudioTagDemuxer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            aac_processor: Mpeg4AacProcessor::new(),
        }
    }

    pub fn demux(
        &mut self,
        timestamp: u32,
        data: BytesMut,
    ) -> Result<FlvDemuxerAudioData, FlvDemuxerError> {
        let mut reader = BytesReader::new(data);

        let tag_header = AudioTagHeader::unmarshal(&mut reader)?;
        self.aac_processor
            .extend_data(&reader.extract_remaining_bytes())?;

        if tag_header.sound_format == SoundFormat::AAC as u8 {
            match tag_header.aac_packet_type {
                aac_packet_type::AAC_SEQHDR => {
                    if self.aac_processor.bytes_reader.len() >= 2 {
                        self.aac_processor.audio_specific_config_load()?;
                    }

                    return Ok(FlvDemuxerAudioData::new());
                }
                aac_packet_type::AAC_RAW => {
                    self.aac_processor.adts_save()?;

                    let audio_data = FlvDemuxerAudioData {
                        has_data: true,
                        sound_format: tag_header.sound_format,
                        pts: i64::from(timestamp),
                        dts: i64::from(timestamp),
                        data: self.aac_processor.bytes_writer.extract_current_bytes(),
                    };
                    return Ok(audio_data);
                }
                _ => {}
            }
        } else {
            tracing::warn!(
                sound_format = tag_header.sound_format,
                "Unsupported audio codec; only AAC is supported, dropping frame"
            );
        }

        Ok(FlvDemuxerAudioData::new())
    }
}

pub struct FlvDemuxer {
    bytes_reader: BytesReader,
}

impl FlvDemuxer {
    #[must_use]
    pub const fn new(data: BytesMut) -> Self {
        Self {
            bytes_reader: BytesReader::new(data),
        }
    }

    pub fn read_flv_header(&mut self) -> Result<(), FlvDemuxerError> {
        self.bytes_reader.read_bytes(FLV_HEADER_LEN)?;
        Ok(())
    }

    pub fn read_flv_tag(&mut self) -> Result<Option<FlvData>, FlvDemuxerError> {
        let header = FlvTagHeader::read_from(&mut self.bytes_reader)?;
        let body = self.bytes_reader.read_bytes(header.data_size as usize)?;

        match header.tag_type {
            tag_type::VIDEO => {
                return Ok(Some(FlvData::Video {
                    timestamp: header.timestamp,
                    data: body,
                }));
            }
            tag_type::AUDIO => {
                return Ok(Some(FlvData::Audio {
                    timestamp: header.timestamp,
                    data: body,
                }));
            }

            _ => {}
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_video_tag(tag: Option<FlvData>) -> (u32, BytesMut) {
        match tag {
            Some(FlvData::Video { timestamp, data }) => (timestamp, data),
            other => panic!("expected video tag, got {other:?}"),
        }
    }

    fn expect_audio_tag(tag: Option<FlvData>) -> (u32, BytesMut) {
        match tag {
            Some(FlvData::Audio { timestamp, data }) => (timestamp, data),
            other => panic!("expected audio tag, got {other:?}"),
        }
    }

    #[test]
    fn test_flv_demuxer_audio_data_new() {
        let data = FlvDemuxerAudioData::new();
        assert!(!data.has_data);
        assert_eq!(data.sound_format, 0);
        assert_eq!(data.dts, 0);
        assert_eq!(data.pts, 0);
        assert!(data.data.is_empty());
    }

    #[test]
    fn test_flv_demuxer_video_data_new() {
        let data = FlvDemuxerVideoData::new();
        assert_eq!(data.codec_id, 0);
        assert_eq!(data.dts, 0);
        assert_eq!(data.pts, 0);
        assert_eq!(data.frame_type, 0);
        assert!(data.data.is_empty());
    }

    #[test]
    fn test_flv_video_tag_demuxer_new() {
        let demuxer = FlvVideoTagDemuxer::new();
        assert!(demuxer.avc_processor.mpeg4_avc.sps.is_empty());
    }

    #[test]
    fn test_flv_audio_tag_demuxer_new() {
        let demuxer = FlvAudioTagDemuxer::new();
        assert_eq!(demuxer.aac_processor.mpeg4_aac.channels, 0);
    }

    #[test]
    fn test_flv_demuxer_new() {
        let data = BytesMut::new();
        let demuxer = FlvDemuxer::new(data);
        assert!(demuxer.bytes_reader.is_empty());
    }

    #[test]
    fn test_flv_demuxer_read_header_insufficient_data() {
        let data = BytesMut::from(&[0x46, 0x4C, 0x56][..]); // "FLV" signature (3 bytes)
        let mut demuxer = FlvDemuxer::new(data);
        // Should fail because we need 9 bytes but only have 3
        let result = demuxer.read_flv_header();
        assert!(result.is_err());
    }

    #[test]
    fn test_flv_demuxer_read_header_valid() {
        // Create a minimal valid FLV header: signature(3) + version(1) + flags(1) + header_size(4)
        let header = [
            0x46, 0x4C, 0x56, // 'F' 'L' 'V' signature
            0x01, // version 1
            0x05, // audio + video flags
            0x00, 0x00, 0x00, 0x09,
        ]; // header size = 9
        let data = BytesMut::from(&header[..]);
        let mut demuxer = FlvDemuxer::new(data);
        let result = demuxer.read_flv_header();
        assert!(result.is_ok());
    }

    #[test]
    fn test_flv_demuxer_read_tag_insufficient_data() {
        let data = BytesMut::new();
        let mut demuxer = FlvDemuxer::new(data);
        // Should fail because there's no data
        let result = demuxer.read_flv_tag();
        assert!(result.is_err());
    }

    #[test]
    fn test_flv_demuxer_read_video_tag() {
        // Create a minimal video tag:
        // PreviousTagSize(4) + TagType(1) + DataSize(3) + Timestamp(3) + TimestampExt(1) + StreamID(3) + Data
        let mut tag_data = vec![
            0x00, 0x00, 0x00, 0x00, // PreviousTagSize = 0
            0x09, // TagType = video (9)
            0x00, 0x00, 0x05, // DataSize = 5
            0x00, 0x00, 0x00, // Timestamp = 0
            0x00, // TimestampExtended = 0
            0x00, 0x00, 0x00, // StreamID = 0
        ];
        // Add 5 bytes of video data
        tag_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);

        let data = BytesMut::from(&tag_data[..]);
        let mut demuxer = FlvDemuxer::new(data);
        let result = demuxer.read_flv_tag();

        assert!(result.is_ok());
        let tag = result.unwrap();
        assert!(tag.is_some());
        let (timestamp, video_data) = expect_video_tag(tag);
        assert_eq!(timestamp, 0);
        assert_eq!(video_data.len(), 5);
    }

    #[test]
    fn test_flv_demuxer_read_audio_tag() {
        // Create a minimal audio tag
        let mut tag_data = vec![
            0x00, 0x00, 0x00, 0x00, // PreviousTagSize = 0
            0x08, // TagType = audio (8)
            0x00, 0x00, 0x04, // DataSize = 4
            0x00, 0x00, 0x00, // Timestamp = 0
            0x00, // TimestampExtended = 0
            0x00, 0x00, 0x00, // StreamID = 0
        ];
        // Add 4 bytes of audio data
        tag_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let data = BytesMut::from(&tag_data[..]);
        let mut demuxer = FlvDemuxer::new(data);
        let result = demuxer.read_flv_tag();

        assert!(result.is_ok());
        let tag = result.unwrap();
        assert!(tag.is_some());
        let (timestamp, audio_data) = expect_audio_tag(tag);
        assert_eq!(timestamp, 0);
        assert_eq!(audio_data.len(), 4);
    }

    #[test]
    fn test_flv_demuxer_read_script_data_tag() {
        // Create a script data tag (tag type 18)
        let mut tag_data = vec![
            0x00, 0x00, 0x00, 0x00, // PreviousTagSize = 0
            0x12, // TagType = script data (18)
            0x00, 0x00, 0x02, // DataSize = 2
            0x00, 0x00, 0x00, // Timestamp = 0
            0x00, // TimestampExtended = 0
            0x00, 0x00, 0x00, // StreamID = 0
        ];
        // Add 2 bytes of data
        tag_data.extend_from_slice(&[0x00, 0x00]);

        let data = BytesMut::from(&tag_data[..]);
        let mut demuxer = FlvDemuxer::new(data);
        let result = demuxer.read_flv_tag();

        // Script data tags are ignored, should return Ok(None)
        assert!(result.is_ok());
        let tag = result.unwrap();
        assert!(tag.is_none());
    }

    #[test]
    fn test_flv_demuxer_timestamp_calculation() {
        // Test timestamp with extended bits
        // Timestamp = 0x123456, Extended = 0x78
        // Full timestamp = (0x123456 & 0xffffff) | (0x78 << 24) = 0x78123456
        let tag_data = vec![
            0x00, 0x00, 0x00, 0x00, // PreviousTagSize = 0
            0x09, // TagType = video
            0x00, 0x00, 0x01, // DataSize = 1
            0x12, 0x34, 0x56, // Timestamp = 0x123456
            0x78, // TimestampExtended = 0x78
            0x00, 0x00, 0x00, // StreamID = 0
            0x00, // 1 byte of data
        ];

        let data = BytesMut::from(&tag_data[..]);
        let mut demuxer = FlvDemuxer::new(data);
        let result = demuxer.read_flv_tag();

        assert!(result.is_ok());
        let tag = result.unwrap();
        let (timestamp, _) = expect_video_tag(tag);
        // Expected: (0x123456 & 0xffffff) | (0x78 << 24) = 0x78123456 = 2014734422
        assert_eq!(timestamp, 0x7812_3456);
    }

    #[test]
    fn test_flv_video_tag_demuxer_empty_data() {
        let mut demuxer = FlvVideoTagDemuxer::new();
        let data = BytesMut::new();
        // Empty data should fail when trying to read video tag header
        let result = demuxer.demux(0, data);
        assert!(result.is_err());
    }

    #[test]
    fn test_flv_audio_tag_demuxer_empty_data() {
        let mut demuxer = FlvAudioTagDemuxer::new();
        let data = BytesMut::new();
        // Empty data should fail when trying to read audio tag header
        let result = demuxer.demux(0, data);
        assert!(result.is_err());
    }

    #[test]
    fn test_flv_audio_tag_demuxer_non_aac() {
        let mut demuxer = FlvAudioTagDemuxer::new();
        // SoundFormat = 0 (Linear PCM, platform endian), not AAC
        // Format: SoundFormat(4) | SoundRate(2) | SoundSize(1) | SoundType(1)
        let data = BytesMut::from(&[0x00_u8, 0x00_u8][..]); // Non-AAC audio
        let result = demuxer.demux(1000, data);
        // Should return empty audio data since it's not AAC
        assert!(result.is_ok());
        let audio_data = result.unwrap();
        assert!(!audio_data.has_data);
    }

    #[test]
    fn test_flv_video_tag_demuxer_non_h264() {
        let mut demuxer = FlvVideoTagDemuxer::new();
        // CodecID = 2 (Sorenson H.263), not H264
        // Format: FrameType(4) | CodecID(4)
        let data = BytesMut::from(&[0x20_u8, 0x00_u8, 0x00_u8, 0x00_u8, 0x00_u8][..]); // H.263 codec
        let result = demuxer.demux(1000, data);
        // Should return None since it's not H264
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_flv_video_tag_demuxer_hevc_nalu() {
        let mut demuxer = FlvVideoTagDemuxer::new();
        load_minimal_hevc_sequence_header(&mut demuxer);

        // Create a minimal HEVC NALU data:
        // VideoTagHeader: FrameType(4) | CodecID(4) | AVCPacketType(1) | CompositionTime(3)
        // FrameType = 1 (keyframe), CodecID = 12 (HEVC)
        // AVCPacketType = 1 (NALU), CompositionTime = 0
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x1C]); // FrameType(1) + CodecID(12) = 0x1C
        data.extend_from_slice(&[0x01]); // AVCPacketType = AVC_NALU
        data.extend_from_slice(&[0x00, 0x00, 0x00]); // CompositionTime = 0

        // Add HEVC NAL data in MP4 format (size prefix + NAL data)
        // NAL size (4 bytes) + NAL type byte + payload
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]); // size = 3
        data.extend_from_slice(&[0x02, 0xAA, 0xBB]); // NAL data (non-IDR slice)

        let result = demuxer.demux(1000, data);

        assert!(result.is_ok());
        let video_data_opt = result.unwrap();
        assert!(video_data_opt.is_some());

        let video_data = video_data_opt.unwrap();
        assert_eq!(video_data.codec_id, AvcCodecId::HEVC as u8);
        assert_eq!(video_data.dts, 1000);
        assert_eq!(video_data.pts, 1000);
        assert_eq!(video_data.frame_type, 1); // keyframe

        // Verify Annex B format: start code + NAL data
        assert_eq!(&video_data.data[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&video_data.data[4..], &[0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn test_flv_video_tag_demuxer_hevc_seqhdr() {
        let mut demuxer = FlvVideoTagDemuxer::new();

        // Create HEVC sequence header (decoder configuration record)
        // VideoTagHeader: FrameType(4) | CodecID(4) | AVCPacketType(1) | CompositionTime(3)
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x1C]); // FrameType(1) + CodecID(12) = 0x1C
        data.extend_from_slice(&[0x00]); // AVCPacketType = AVC_SEQHDR
        data.extend_from_slice(&[0x00, 0x00, 0x00]); // CompositionTime = 0

        // Minimal HEVCDecoderConfigurationRecord
        data.extend_from_slice(&[0x01]); // configurationVersion = 1
        data.extend_from_slice(&[0x01]); // general_profile_space=0, tier=0, profile_idc=1
        data.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]); // general_profile_compatibility_flags
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // constraint_indicator_flags
        data.extend_from_slice(&[0x5D]); // general_level_idc = 93 (level 3.1)
        data.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc
        data.extend_from_slice(&[0xFC]); // parallelismType
        data.extend_from_slice(&[0xFD]); // chroma_format_idc = 1
        data.extend_from_slice(&[0xFA]); // bit_depth_luma_minus8 = 2
        data.extend_from_slice(&[0xFB]); // bit_depth_chroma_minus8 = 3
        data.extend_from_slice(&[0x00, 0x00]); // avgFrameRate
        data.extend_from_slice(&[0x0F]); // constantFrameRate=0, numTemporalLayers=0, temporalIdNested=0, lengthSizeMinusOne=3
        data.extend_from_slice(&[0x00]); // numOfArrays = 0 (no NAL arrays for simplicity)

        let result = demuxer.demux(1000, data);

        // Sequence header should return None (no video data to output)
        assert!(
            result.is_ok(),
            "demux should succeed: {:?}",
            result.as_ref().err()
        );
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_flv_video_tag_demuxer_hevc_multiple_nalus() {
        let mut demuxer = FlvVideoTagDemuxer::new();
        load_minimal_hevc_sequence_header(&mut demuxer);

        // Create HEVC data with multiple NAL units
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x1C]); // FrameType(1) + CodecID(12) = 0x1C
        data.extend_from_slice(&[0x01]); // AVCPacketType = AVC_NALU
        data.extend_from_slice(&[0x00, 0x00, 0x00]); // CompositionTime = 0

        // First NAL: slice type
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // size = 2
        data.extend_from_slice(&[0x02, 0xAA]); // NAL data

        // Second NAL: another slice
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // size = 2
        data.extend_from_slice(&[0x02, 0xBB]); // NAL data

        let result = demuxer.demux(2000, data);

        assert!(result.is_ok());
        let video_data = result.unwrap().unwrap();

        // Verify both NAL units are present with start codes
        // Expected: start_code + nal1 + start_code + nal2
        assert_eq!(video_data.data.len(), 12); // 4 + 2 + 4 + 2
        assert_eq!(&video_data.data[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&video_data.data[4..6], &[0x02, 0xAA]);
        assert_eq!(&video_data.data[6..10], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&video_data.data[10..12], &[0x02, 0xBB]);
    }

    fn load_minimal_hevc_sequence_header(demuxer: &mut FlvVideoTagDemuxer) {
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x1C]);
        data.extend_from_slice(&[0x00]);
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data.extend_from_slice(&[0x01]);
        data.extend_from_slice(&[0x01]);
        data.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&[0x5D]);
        data.extend_from_slice(&[0xF0, 0x00]);
        data.extend_from_slice(&[0xFC]);
        data.extend_from_slice(&[0xFD]);
        data.extend_from_slice(&[0xFA]);
        data.extend_from_slice(&[0xFB]);
        data.extend_from_slice(&[0x00, 0x00]);
        data.extend_from_slice(&[0x0F]);
        data.extend_from_slice(&[0x00]);

        match demuxer.demux(0, data) {
            Ok(None) => {}
            Ok(Some(video_data)) => {
                panic!("HEVC sequence header emitted video data: {video_data:?}")
            }
            Err(err) => panic!("HEVC sequence header should initialize demuxer: {err}"),
        }
    }
}
