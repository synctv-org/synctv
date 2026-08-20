use {
    super::{define::h264_nal_type, errors::Mpeg4AvcHevcError},
    crate::bytesio::{bytes_reader::BytesReader, bytes_writer::BytesWriter},
    byteorder::BigEndian,
    bytes::BytesMut,
    std::vec::Vec,
};

use super::errors::MpegErrorValue;
use crate::h264::sps::SpsParser;

const H264_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];
const MAX_SPS_COUNT: u8 = 16;
const MAX_PPS_COUNT: u8 = 16;

#[derive(Clone, Default)]
pub struct Sps {
    pub data: BytesMut,
}

impl Sps {
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

#[derive(Clone, Default)]
pub struct Pps {
    pub data: BytesMut,
}

impl Pps {
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

#[derive(Default)]
pub struct Mpeg4Avc {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    pub nalu_length: u8,
    pub width: u32,
    pub height: u32,

    pub nb_sps: u8,
    pub nb_pps: u8,

    pub sps: Vec<Sps>,
    pub pps: Vec<Pps>,

    pub sps_annexb_data: BytesWriter,
    pub pps_annexb_data: BytesWriter,

    // AVCDecoderConfigurationRecord extension fields.
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
}

impl Mpeg4Avc {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profile: 0,
            compatibility: 0,
            level: 0,
            nalu_length: 0,
            width: 0,
            height: 0,

            nb_pps: 0,
            nb_sps: 0,

            sps: Vec::new(),
            pps: Vec::new(),

            sps_annexb_data: BytesWriter::new(),
            pps_annexb_data: BytesWriter::new(),

            chroma_format_idc: 0,
            bit_depth_chroma_minus8: 0,
            bit_depth_luma_minus8: 0,
        }
    }
}

#[derive(Default)]
pub struct Mpeg4AvcProcessor {
    pub mpeg4_avc: Mpeg4Avc,
}

impl Mpeg4AvcProcessor {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mpeg4_avc: Mpeg4Avc::new(),
        }
    }

    pub fn clear_sps_data(&mut self) {
        self.mpeg4_avc.sps.clear();
        self.mpeg4_avc.sps_annexb_data.clear();
    }

    pub fn clear_pps_data(&mut self) {
        self.mpeg4_avc.pps.clear();
        self.mpeg4_avc.pps_annexb_data.clear();
    }

    pub fn decoder_configuration_record_load(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<&mut Self, Mpeg4AvcHevcError> {
        bytes_reader.read_u8()?;
        self.mpeg4_avc.profile = bytes_reader.read_u8()?;
        self.mpeg4_avc.compatibility = bytes_reader.read_u8()?;
        self.mpeg4_avc.level = bytes_reader.read_u8()?;
        self.mpeg4_avc.nalu_length = (bytes_reader.read_u8()? & 0x03) + 1;

        self.mpeg4_avc.nb_sps = bytes_reader.read_u8()? & 0x1F;

        // Validate SPS count: H.264 spec allows up to 31, but typical streams use 1-4
        // Limiting to 16 prevents memory exhaustion while allowing reasonable flexibility
        if self.mpeg4_avc.nb_sps > MAX_SPS_COUNT {
            return Err(Mpeg4AvcHevcError {
                value: MpegErrorValue::SpsPpsCountExceeded {
                    count: self.mpeg4_avc.nb_sps,
                    max: MAX_SPS_COUNT,
                },
            });
        }

        if self.mpeg4_avc.nb_sps > 0 {
            self.clear_sps_data();
        }

        for i in 0..usize::from(self.mpeg4_avc.nb_sps) {
            let sps_data_size = bytes_reader.read_u16::<BigEndian>()?;
            let sps_data = Sps {
                data: bytes_reader.read_bytes(usize::from(sps_data_size))?,
            };

            let mut sps_reader = BytesReader::new(sps_data.clone().data);
            let nal_type = sps_reader.read_u8()?;
            if (nal_type & 0x1f) != h264_nal_type::H264_NAL_SPS {
                return Err(Mpeg4AvcHevcError {
                    value: MpegErrorValue::SPSNalunitTypeNotCorrect,
                });
            }
            let mut sps_parser = SpsParser::new(sps_reader);
            (self.mpeg4_avc.width, self.mpeg4_avc.height) = sps_parser.parse()?;

            tracing::info!("mpeg4 avc profile: {}", self.mpeg4_avc.profile);
            tracing::info!("mpeg4 avc compatibility: {}", self.mpeg4_avc.compatibility);
            tracing::info!("mpeg4 avc level: {}", self.mpeg4_avc.level);
            tracing::info!(
                "mpeg4 avc resolution: {}x{}",
                self.mpeg4_avc.width,
                self.mpeg4_avc.height
            );

            self.mpeg4_avc.sps.push(sps_data);
            self.mpeg4_avc.sps_annexb_data.write(&H264_START_CODE)?;
            self.mpeg4_avc
                .sps_annexb_data
                .write(&self.mpeg4_avc.sps[i].data[..])?;
        }
        self.mpeg4_avc.nb_pps = bytes_reader.read_u8()?;

        // Validate PPS count: similar to SPS, limit to prevent memory exhaustion
        if self.mpeg4_avc.nb_pps > MAX_PPS_COUNT {
            return Err(Mpeg4AvcHevcError {
                value: MpegErrorValue::SpsPpsCountExceeded {
                    count: self.mpeg4_avc.nb_pps,
                    max: MAX_PPS_COUNT,
                },
            });
        }

        if self.mpeg4_avc.nb_pps > 0 {
            self.clear_pps_data();
        }

        for i in 0..usize::from(self.mpeg4_avc.nb_pps) {
            let pps_data_size = bytes_reader.read_u16::<BigEndian>()?;
            let pps_data = Pps {
                data: bytes_reader.read_bytes(usize::from(pps_data_size))?,
            };

            self.mpeg4_avc.pps.push(pps_data);
            self.mpeg4_avc.pps_annexb_data.write(&H264_START_CODE)?;
            self.mpeg4_avc
                .pps_annexb_data
                .write(&self.mpeg4_avc.pps[i].data[..])?;
        }
        bytes_reader.extract_remaining_bytes();

        Ok(self)
    }
    pub fn h264_mp4toannexb(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<BytesMut, Mpeg4AvcHevcError> {
        let mut bytes_writer = BytesWriter::new();

        let mut sps_pps_flag = false;
        while !bytes_reader.is_empty() {
            let size = self.read_nalu_size(bytes_reader)?;
            let nalu_type = bytes_reader.advance_u8()? & 0x1f;

            match nalu_type {
                h264_nal_type::H264_NAL_PPS | h264_nal_type::H264_NAL_SPS => {
                    sps_pps_flag = true;
                }
                h264_nal_type::H264_NAL_IDR if !sps_pps_flag => {
                    sps_pps_flag = true;

                    bytes_writer.prepend(self.mpeg4_avc.pps_annexb_data.as_slice())?;
                    bytes_writer.prepend(self.mpeg4_avc.sps_annexb_data.as_slice())?;
                }
                _ => {}
            }

            bytes_writer.write(&H264_START_CODE)?;
            let data = bytes_reader.read_bytes(size as usize)?;
            bytes_writer.write(&data[..])?;
        }

        Ok(bytes_writer.extract_current_bytes())
    }

    pub fn read_nalu_size(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<u32, Mpeg4AvcHevcError> {
        if !(1..=4).contains(&self.mpeg4_avc.nalu_length) {
            return Err(Mpeg4AvcHevcError {
                value: MpegErrorValue::InvalidNaluLength(self.mpeg4_avc.nalu_length),
            });
        }
        let mut size: u32 = 0;

        for _ in 0..self.mpeg4_avc.nalu_length {
            size = u32::from(bytes_reader.read_u8()?) + (size << 8);
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::{Mpeg4AvcProcessor, MpegErrorValue};
    use crate::bytesio::{bytes_reader::BytesReader, bytes_writer::BytesWriter};
    use bytes::BytesMut;

    #[test]
    fn test_bytes_to_bigend() {
        let mut size: u32 = 0;
        let mut b = BytesMut::new();
        b.extend_from_slice(b"\0\0\x03\xe8");
        let mut bytes_reader = BytesReader::new(b);

        for _ in 0..4 {
            size = u32::from(bytes_reader.read_u8().unwrap()) + (size << 8);
        }
        assert_eq!(size, 1000, "Expected big-endian bytes to decode to 1000");
    }
    #[test]
    fn test_bigend_to_bytes() {
        let size = 1000;
        let length = 4;
        let mut bytes_writer = BytesWriter::new();

        for i in 0..length {
            let shift = (length - i - 1) * 8;
            let num = u8::try_from((size >> shift) & 0xFF).unwrap();
            bytes_writer.write_u8(num).unwrap();
        }
        assert_eq!(
            &bytes_writer.extract_current_bytes()[..],
            &[0, 0, 3, 232],
            "Expected 1000 to encode as big-endian [0, 0, 3, 232]"
        );
    }

    #[test]
    fn nalu_before_decoder_configuration_is_rejected() {
        let mut processor = Mpeg4AvcProcessor::new();
        let mut reader = BytesReader::new(BytesMut::from(&[0, 0, 0, 1, 0x65][..]));

        let err = processor
            .h264_mp4toannexb(&mut reader)
            .expect_err("NALU without an AVC decoder configuration must fail");

        assert!(matches!(err.value, MpegErrorValue::InvalidNaluLength(0)));
    }
}
