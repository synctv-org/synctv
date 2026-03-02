use {
    super::{
        define::hevc_nal_type,
        errors::{Mpeg4AvcHevcError, MpegErrorValue},
    },
    crate::bytesio::{bytes_reader::BytesReader, bytes_writer::BytesWriter},
    byteorder::BigEndian,
    bytes::BytesMut,
};

const HEVC_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

#[derive(Clone, Default)]
pub struct HevcNal {
    pub data: BytesMut,
}

impl HevcNal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: BytesMut::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[allow(dead_code)]
#[derive(Default)]
pub struct Mpeg4Hevc {
    configuration_version: u8, // 1-only
    general_profile_space: u8, // 2bit,[0,3]
    general_tier_flag: u8,     // 1bit,[0,1]
    general_profile_idc: u8,   // 5bit,[0,31]
    general_profile_compatibility_flags: u32,
    general_constraint_indicator_flags: u64,
    general_level_idc: u8,
    min_spatial_segmentation_idc: u16,
    parallelism_type: u8,        // 2bit,[0,3]
    chroma_format: u8,           // 2bit,[0,3]
    bit_depth_luma_minus8: u8,   // 3bit,[0,7]
    bit_depth_chroma_minus8: u8, // 3bit,[0,7]
    avg_frame_rate: u16,
    constant_frame_rate: u8,   // 2bit,[0,3]
    num_temporal_layers: u8,   // 3bit,[0,7]
    temporal_id_nested: u8,    // 1bit,[0,1]
    length_size_minus_one: u8, // 2bit,[0,3]

    /// NAL unit length size in bytes (1-4)
    nalu_length: u8,

    /// VPS NAL units
    vps: Vec<HevcNal>,
    /// SPS NAL units
    sps: Vec<HevcNal>,
    /// PPS NAL units
    pps: Vec<HevcNal>,

    /// VPS data with Annex B start codes (for prepending to IDR frames)
    vps_annexb_data: BytesWriter,
    /// SPS data with Annex B start codes (for prepending to IDR frames)
    sps_annexb_data: BytesWriter,
    /// PPS data with Annex B start codes (for prepending to IDR frames)
    pps_annexb_data: BytesWriter,
}

#[derive(Default)]
pub struct Mpeg4HevcProcessor {
    pub mpeg4_hevc: Mpeg4Hevc,
}

impl Mpeg4HevcProcessor {
    /// Extracts the HEVC NAL unit type from the first byte of a NAL unit.
    /// In HEVC, the NAL type is in bits 1-6 of the first byte: (byte >> 1) & 0x3F
    fn get_hevc_nal_type(nal_byte: u8) -> u8 {
        (nal_byte >> 1) & 0x3F
    }

    fn clear_vps_data(&mut self) {
        self.mpeg4_hevc.vps.clear();
        self.mpeg4_hevc.vps_annexb_data.clear();
    }

    fn clear_sps_data(&mut self) {
        self.mpeg4_hevc.sps.clear();
        self.mpeg4_hevc.sps_annexb_data.clear();
    }

    fn clear_pps_data(&mut self) {
        self.mpeg4_hevc.pps.clear();
        self.mpeg4_hevc.pps_annexb_data.clear();
    }

    pub fn decoder_configuration_record_load(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<&mut Self, Mpeg4AvcHevcError> {
        self.mpeg4_hevc.configuration_version = bytes_reader.read_u8()?;
        let byte_1 = bytes_reader.read_u8()?;
        self.mpeg4_hevc.general_profile_space = (byte_1 >> 6) & 0x03;
        self.mpeg4_hevc.general_tier_flag = (byte_1 >> 5) & 0x01;
        self.mpeg4_hevc.general_profile_idc = byte_1 & 0x1F;
        self.mpeg4_hevc.general_profile_compatibility_flags =
            bytes_reader.read_u32::<BigEndian>()?;
        self.mpeg4_hevc.general_constraint_indicator_flags =
            bytes_reader.read_u48::<BigEndian>()?;
        self.mpeg4_hevc.general_level_idc = bytes_reader.read_u8()?;
        self.mpeg4_hevc.min_spatial_segmentation_idc =
            bytes_reader.read_u16::<BigEndian>()? & 0x0FFF;
        self.mpeg4_hevc.parallelism_type = bytes_reader.read_u8()? & 0x03;
        self.mpeg4_hevc.chroma_format = bytes_reader.read_u8()? & 0x03;
        self.mpeg4_hevc.bit_depth_luma_minus8 = bytes_reader.read_u8()? & 0x07;
        self.mpeg4_hevc.bit_depth_chroma_minus8 = bytes_reader.read_u8()? & 0x07;
        self.mpeg4_hevc.avg_frame_rate = bytes_reader.read_u16::<BigEndian>()?;

        let byte_cfg = bytes_reader.read_u8()?;
        self.mpeg4_hevc.constant_frame_rate = (byte_cfg >> 6) & 0x03;
        self.mpeg4_hevc.num_temporal_layers = (byte_cfg >> 3) & 0x07;
        self.mpeg4_hevc.temporal_id_nested = (byte_cfg >> 2) & 0x01;
        self.mpeg4_hevc.length_size_minus_one = byte_cfg & 0x03;
        self.mpeg4_hevc.nalu_length = self.mpeg4_hevc.length_size_minus_one + 1;

        // Clear existing data
        self.clear_vps_data();
        self.clear_sps_data();
        self.clear_pps_data();

        // Read arrays of NAL units (VPS, SPS, PPS, etc.)
        // The HVCC format has multiple arrays, each with:
        // - array_completeness(1) + reserved(1) + NAL_unit_type(6)
        // - numNalus(16)
        // - For each NALU: nalUnitLength(16) + nalUnit data
        let num_arrays = bytes_reader.read_u8()?;

        for _ in 0..num_arrays {
            let type_byte = bytes_reader.read_u8()?;
            let nal_type = type_byte & 0x3F;
            let num_nalus = bytes_reader.read_u16::<BigEndian>()?;

            for _ in 0..num_nalus {
                let nal_size = bytes_reader.read_u16::<BigEndian>()? as usize;
                let nal_data = bytes_reader.read_bytes(nal_size)?;

                match nal_type {
                    hevc_nal_type::HEVC_NAL_VPS => {
                        let nal = HevcNal { data: nal_data };
                        self.mpeg4_hevc.vps.push(nal);
                        // Store last VPS with Annex B start code
                        self.mpeg4_hevc.vps_annexb_data.clear();
                        self.mpeg4_hevc.vps_annexb_data.write(&HEVC_START_CODE)?;
                        self.mpeg4_hevc.vps_annexb_data.write(
                            &self
                                .mpeg4_hevc
                                .vps
                                .last()
                                .map(|v| v.data.as_ref())
                                .unwrap_or(&[]),
                        )?;
                    }
                    hevc_nal_type::HEVC_NAL_SPS => {
                        let nal = HevcNal { data: nal_data };
                        self.mpeg4_hevc.sps.push(nal);
                        // Store last SPS with Annex B start code
                        self.mpeg4_hevc.sps_annexb_data.clear();
                        self.mpeg4_hevc.sps_annexb_data.write(&HEVC_START_CODE)?;
                        self.mpeg4_hevc.sps_annexb_data.write(
                            &self
                                .mpeg4_hevc
                                .sps
                                .last()
                                .map(|s| s.data.as_ref())
                                .unwrap_or(&[]),
                        )?;
                    }
                    hevc_nal_type::HEVC_NAL_PPS => {
                        let nal = HevcNal { data: nal_data };
                        self.mpeg4_hevc.pps.push(nal);
                        // Store last PPS with Annex B start code
                        self.mpeg4_hevc.pps_annexb_data.clear();
                        self.mpeg4_hevc.pps_annexb_data.write(&HEVC_START_CODE)?;
                        self.mpeg4_hevc.pps_annexb_data.write(
                            &self
                                .mpeg4_hevc
                                .pps
                                .last()
                                .map(|p| p.data.as_ref())
                                .unwrap_or(&[]),
                        )?;
                    }
                    _ => {
                        // Ignore other NAL types in configuration record
                    }
                }
            }
        }

        Ok(self)
    }

    /// Read the NAL unit size from the MP4 container format.
    /// The size is stored in `nalu_length` bytes as a big-endian integer.
    fn read_nalu_size(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<u32, Mpeg4AvcHevcError> {
        // Default to 4 bytes if not initialized (e.g., in test scenarios)
        let nalu_length = if self.mpeg4_hevc.nalu_length == 0 {
            4
        } else {
            self.mpeg4_hevc.nalu_length
        };

        let mut size: u32 = 0;
        for _ in 0..nalu_length {
            size = u32::from(bytes_reader.read_u8()?) + (size << 8);
        }
        Ok(size)
    }

    /// Check if the NAL type is an IDR frame (random access point)
    fn is_idr_nal_type(nal_type: u8) -> bool {
        matches!(
            nal_type,
            hevc_nal_type::HEVC_NAL_IDR_W_RADL
                | hevc_nal_type::HEVC_NAL_IDR_N_LP
                | hevc_nal_type::HEVC_NAL_CRA
        )
    }

    /// Convert HEVC NAL units from MP4 container format to Annex B byte stream format.
    ///
    /// In MP4 container format, each NAL unit is prefixed with a size field (1-4 bytes).
    /// In Annex B byte stream format, each NAL unit is prefixed with a 4-byte start code
    /// `[0x00, 0x00, 0x00, 0x01]`.
    ///
    /// For IDR/CRA frames (random access points), VPS, SPS, and PPS are prepended to
    /// ensure the decoder has the necessary parameter sets to decode the frame.
    pub fn hevc_mp4toannexb(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<BytesMut, Mpeg4AvcHevcError> {
        let mut bytes_writer = BytesWriter::new();

        let mut vps_sps_pps_prepended = false;

        while !bytes_reader.is_empty() {
            let size = self.read_nalu_size(bytes_reader)?;

            // Peek at the first byte to determine NAL type
            // Note: advance_u8() does NOT consume the byte, it just peeks
            let nal_first_byte = bytes_reader.advance_u8()?;
            let nal_type = Self::get_hevc_nal_type(nal_first_byte);

            match nal_type {
                hevc_nal_type::HEVC_NAL_VPS
                | hevc_nal_type::HEVC_NAL_SPS
                | hevc_nal_type::HEVC_NAL_PPS => {
                    // Parameter sets are embedded in the stream - mark as prepended
                    vps_sps_pps_prepended = true;
                }
                _ if Self::is_idr_nal_type(nal_type) && !vps_sps_pps_prepended => {
                    // IDR/CRA frame without preceding VPS/SPS/PPS - prepend them
                    vps_sps_pps_prepended = true;

                    // Prepend in order: VPS, SPS, PPS
                    bytes_writer.prepend(&self.mpeg4_hevc.pps_annexb_data.get_current_bytes()[..])?;
                    bytes_writer.prepend(&self.mpeg4_hevc.sps_annexb_data.get_current_bytes()[..])?;
                    bytes_writer.prepend(&self.mpeg4_hevc.vps_annexb_data.get_current_bytes()[..])?;
                }
                _ => {}
            }

            // Write start code
            bytes_writer.write(&HEVC_START_CODE)?;

            // Read the entire NAL unit (including the first byte we peeked at)
            // The size field includes the first byte
            if size == 0 {
                return Err(Mpeg4AvcHevcError {
                    value: MpegErrorValue::ShouldNotComeHere,
                });
            }
            let data = bytes_reader.read_bytes(size as usize)?;
            bytes_writer.write(&data[..])?;
        }

        Ok(bytes_writer.extract_current_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hevc_nal_type_extraction() {
        // Test NAL type extraction: (byte >> 1) & 0x3F
        // VPS (type 32): byte should have (32 << 1) = 64 = 0x40, with nuh_layer_id=0 and nuh_temporal_id=1
        // So first byte = 0x40 | 0x01 = 0x41 for VPS
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x40), 32); // VPS
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x42), 33); // SPS (33 << 1 = 66 = 0x42)
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x44), 34); // PPS (34 << 1 = 68 = 0x44)
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x26), 19); // IDR_W_RADL
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x28), 20); // IDR_N_LP
        assert_eq!(Mpeg4HevcProcessor::get_hevc_nal_type(0x2A), 21); // CRA
    }

    #[test]
    fn test_is_idr_nal_type() {
        assert!(Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_IDR_W_RADL));
        assert!(Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_IDR_N_LP));
        assert!(Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_CRA));
        assert!(!Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_VPS));
        assert!(!Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_SPS));
        assert!(!Mpeg4HevcProcessor::is_idr_nal_type(hevc_nal_type::HEVC_NAL_PPS));
        assert!(!Mpeg4HevcProcessor::is_idr_nal_type(1)); // Non-IDR slice
    }

    #[test]
    fn test_hevc_mp4toannexb_single_nalu() {
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 4;

        // Create a simple NAL unit: size(4 bytes) + NAL data
        // NAL type 1 (non-IDR slice), size 4, data = [0x02, 0x03, 0x04, 0x05]
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]); // size = 4
        data.extend_from_slice(&[0x02, 0x03, 0x04, 0x05]); // NAL data

        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        if let Err(ref e) = result {
            eprintln!("Error: {:?}", e);
        }
        assert!(result.is_ok(), "hevc_mp4toannexb failed");
        let annexb = result.unwrap();

        // Should have start code + original data
        assert_eq!(&annexb[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb[4..], &[0x02, 0x03, 0x04, 0x05]);
    }

    #[test]
    fn test_hevc_mp4toannexb_multiple_nalus() {
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 4;

        // Create two NAL units
        let mut data = BytesMut::new();
        // First NAL: size=3, data=[0x02, 0xAA]
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        data.extend_from_slice(&[0x02, 0xAA]);
        // Second NAL: size=3, data=[0x02, 0xBB]
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        data.extend_from_slice(&[0x02, 0xBB]);

        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        assert!(result.is_ok());
        let annexb = result.unwrap();

        // Should have: start_code + nal1 + start_code + nal2
        let expected: Vec<u8> = vec![
            0x00, 0x00, 0x00, 0x01, // start code
            0x02, 0xAA, // first NAL
            0x00, 0x00, 0x00, 0x01, // start code
            0x02, 0xBB, // second NAL
        ];
        assert_eq!(&annexb[..], &expected[..]);
    }

    #[test]
    fn test_hevc_mp4toannexb_idr_with_parameter_sets() {
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 4;

        // Set up VPS, SPS, PPS data
        processor.mpeg4_hevc.vps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.vps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.vps_annexb_data.write(&[0x40, 0x01]).unwrap(); // VPS NAL

        processor.mpeg4_hevc.sps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.sps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.sps_annexb_data.write(&[0x42, 0x01]).unwrap(); // SPS NAL

        processor.mpeg4_hevc.pps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.pps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.pps_annexb_data.write(&[0x44, 0x01]).unwrap(); // PPS NAL

        // Create an IDR NAL unit (type 19 = IDR_W_RADL, first byte = 0x26)
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // size = 2
        data.extend_from_slice(&[0x26, 0xFF]); // IDR NAL data

        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        assert!(result.is_ok());
        let annexb = result.unwrap();

        // Should have: VPS + SPS + PPS + IDR
        // Check that VPS, SPS, PPS are prepended (they come before the IDR frame)
        let annexb_slice = &annexb[..];

        // Find VPS start code + data
        assert_eq!(&annexb_slice[0..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb_slice[4..6], &[0x40, 0x01]); // VPS

        // Find SPS
        assert_eq!(&annexb_slice[6..10], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb_slice[10..12], &[0x42, 0x01]); // SPS

        // Find PPS
        assert_eq!(&annexb_slice[12..16], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb_slice[16..18], &[0x44, 0x01]); // PPS

        // Find IDR (at the end)
        assert_eq!(&annexb_slice[18..22], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb_slice[22..24], &[0x26, 0xFF]); // IDR
    }

    #[test]
    fn test_hevc_mp4toannexb_empty_data() {
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 4;

        let data = BytesMut::new();
        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        assert!(result.is_ok());
        let annexb = result.unwrap();
        assert!(annexb.is_empty());
    }

    #[test]
    fn test_hevc_mp4toannexb_short_nalu_length() {
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 2; // 2-byte size field

        // Create a NAL unit with 2-byte size
        let mut data = BytesMut::new();
        data.extend_from_slice(&[0x00, 0x03]); // size = 3
        data.extend_from_slice(&[0x02, 0xAA, 0xBB]); // NAL data

        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        assert!(result.is_ok());
        let annexb = result.unwrap();

        // Should have start code + original data
        assert_eq!(&annexb[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb[4..], &[0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn test_hevc_mp4toannexb_preserves_embedded_parameter_sets() {
        // When VPS/SPS/PPS are already in the stream, don't prepend again
        let mut processor = Mpeg4HevcProcessor::default();
        processor.mpeg4_hevc.nalu_length = 4;

        // Set up parameter sets (but they shouldn't be prepended since SPS is in stream)
        processor.mpeg4_hevc.vps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.vps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.vps_annexb_data.write(&[0x40, 0x01]).unwrap();

        processor.mpeg4_hevc.sps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.sps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.sps_annexb_data.write(&[0x42, 0x01]).unwrap();

        processor.mpeg4_hevc.pps_annexb_data = BytesWriter::new();
        processor.mpeg4_hevc.pps_annexb_data.write(&HEVC_START_CODE).unwrap();
        processor.mpeg4_hevc.pps_annexb_data.write(&[0x44, 0x01]).unwrap();

        // Create stream with SPS first, then IDR (SPS in stream should prevent prepending)
        let mut data = BytesMut::new();
        // SPS NAL (type 33)
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // size = 2
        data.extend_from_slice(&[0x42, 0x01]); // SPS NAL data
        // IDR NAL (type 19)
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // size = 2
        data.extend_from_slice(&[0x26, 0xFF]); // IDR NAL data

        let mut reader = BytesReader::new(data);
        let result = processor.hevc_mp4toannexb(&mut reader);

        assert!(result.is_ok());
        let annexb = result.unwrap();

        // Should only have SPS + IDR (no prepended VPS/SPS/PPS)
        // Total: 2 start codes + 2 SPS bytes + 2 IDR bytes = 12 bytes
        assert_eq!(annexb.len(), 12);

        // Verify structure: start_code + SPS + start_code + IDR
        assert_eq!(&annexb[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb[4..6], &[0x42, 0x01]); // SPS
        assert_eq!(&annexb[6..10], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annexb[10..12], &[0x26, 0xFF]); // IDR
    }
}
