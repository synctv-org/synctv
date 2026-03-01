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

#[derive(Clone, Default)]
pub struct Sps {
    // pub size: u16,
    pub data: BytesMut,
}

impl Sps {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // size: 0,
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
    // pub size: u16,
    pub data: BytesMut,
}

impl Pps {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // size: 0,
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

    pub sps_annexb_data: BytesWriter, // pice together all the sps data
    pub pps_annexb_data: BytesWriter, // pice together all the pps data

    //extension
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    // data: Vec<u8>, //[u8; 4 * 1024],
    // off: i32,
}

pub fn print(data: BytesMut) {
    tracing::trace!("==========={}", data.len());
    let mut idx = 0;
    let mut line = String::new();
    for i in data {
        line.push_str(&format!("{i:02X} "));
        idx += 1;
        if idx % 16 == 0 {
            tracing::trace!("{}", line);
            line.clear();
        }
    }
    if !line.is_empty() {
        tracing::trace!("{}", line);
    }
    tracing::trace!("===========");
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
        /*version */
        bytes_reader.read_u8()?;
        /*avc profile*/
        self.mpeg4_avc.profile = bytes_reader.read_u8()?;
        /*avc compatibility*/
        self.mpeg4_avc.compatibility = bytes_reader.read_u8()?;
        /*avc level*/
        self.mpeg4_avc.level = bytes_reader.read_u8()?;
        /*nalu length*/
        self.mpeg4_avc.nalu_length = (bytes_reader.read_u8()? & 0x03) + 1;

        /*number of SPS NALUs */
        self.mpeg4_avc.nb_sps = bytes_reader.read_u8()? & 0x1F;

        if self.mpeg4_avc.nb_sps > 0 {
            self.clear_sps_data();
        }

        for i in 0..self.mpeg4_avc.nb_sps as usize {
            /*SPS size*/
            let sps_data_size = bytes_reader.read_u16::<BigEndian>()?;
            let sps_data = Sps {
                // size: sps_data_size,
                /*SPS data*/
                data: bytes_reader.read_bytes(sps_data_size as usize)?,
            };

            let mut sps_reader = BytesReader::new(sps_data.clone().data);
            /*parse SPS data to get video resolution(widthxheight) */
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
        /*number of PPS NALUs*/
        self.mpeg4_avc.nb_pps = bytes_reader.read_u8()?;

        if self.mpeg4_avc.nb_pps > 0 {
            self.clear_pps_data();
        }

        for i in 0..self.mpeg4_avc.nb_pps as usize {
            let pps_data_size = bytes_reader.read_u16::<BigEndian>()?;
            let pps_data = Pps {
                // size: pps_data_size,
                data: bytes_reader.read_bytes(pps_data_size as usize)?,
            };

            self.mpeg4_avc.pps.push(pps_data);
            self.mpeg4_avc.pps_annexb_data.write(&H264_START_CODE)?;
            self.mpeg4_avc
                .pps_annexb_data
                .write(&self.mpeg4_avc.pps[i].data[..])?;
        }
        /*clear the left bytes*/
        bytes_reader.extract_remaining_bytes();

        Ok(self)
    }
    //https://stackoverflow.com/questions/28678615/efficiently-insert-or-replace-multiple-elements-in-the-middle-or-at-the-beginnin
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

                    bytes_writer
                        .prepend(&self.mpeg4_avc.pps_annexb_data.get_current_bytes()[..])?;
                    bytes_writer
                        .prepend(&self.mpeg4_avc.sps_annexb_data.get_current_bytes()[..])?;
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
        let mut size: u32 = 0;

        for _ in 0..self.mpeg4_avc.nalu_length {
            size = u32::from(bytes_reader.read_u8()?) + (size << 8);
        }
        Ok(size)
    }

    pub fn write_nalu_size(
        &mut self,
        writer: &mut BytesWriter,
        length: usize,
    ) -> Result<(), Mpeg4AvcHevcError> {
        let nalu_length = self.mpeg4_avc.nalu_length;
        for i in 0..nalu_length {
            let shift = (nalu_length - i - 1) * 8;
            let num = ((length >> shift) & 0xFF) as u8;
            writer.write_u8(num)?;
        }
        Ok(())
    }

    pub fn nalus_to_mpeg4avc(
        &mut self,
        nalus: Vec<BytesMut>,
    ) -> Result<BytesMut, Mpeg4AvcHevcError> {
        let mut bytes_writer = BytesWriter::new();

        for nalu in nalus {
            let length = nalu.len();
            self.write_nalu_size(&mut bytes_writer, length)?;
            bytes_writer.write(&nalu)?;
        }

        Ok(bytes_writer.extract_current_bytes())
    }

    pub fn decoder_configuration_record_save(&mut self) -> Result<BytesMut, Mpeg4AvcHevcError> {
        let mut bytes_writer = BytesWriter::new();

        bytes_writer.write_u8(1)?;
        bytes_writer.write_u8(self.mpeg4_avc.profile)?;
        bytes_writer.write_u8(self.mpeg4_avc.compatibility)?;
        bytes_writer.write_u8(self.mpeg4_avc.level)?;
        bytes_writer.write_u8((self.mpeg4_avc.nalu_length - 1) | 0xFC)?;

        //sps
        bytes_writer.write_u8(self.mpeg4_avc.nb_sps | 0xE0)?;
        for i in 0..self.mpeg4_avc.nb_sps as usize {
            bytes_writer.write_u16::<BigEndian>(self.mpeg4_avc.sps[i].len() as u16)?;
            bytes_writer.write(&self.mpeg4_avc.sps[i].data[..])?;
        }

        //pps
        bytes_writer.write_u8(self.mpeg4_avc.nb_pps)?;
        for i in 0..self.mpeg4_avc.nb_pps as usize {
            bytes_writer.write_u16::<BigEndian>(self.mpeg4_avc.pps[i].len() as u16)?;
            bytes_writer.write(&self.mpeg4_avc.pps[i].data[..])?;
        }

        match self.mpeg4_avc.profile {
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 => {
                bytes_writer.write_u8(0xFC | self.mpeg4_avc.chroma_format_idc)?;
                bytes_writer.write_u8(0xF8 | self.mpeg4_avc.bit_depth_luma_minus8)?;
                bytes_writer.write_u8(0xF8 | self.mpeg4_avc.bit_depth_chroma_minus8)?;
                bytes_writer.write_u8(0)?;
            }
            _ => {}
        }

        Ok(bytes_writer.extract_current_bytes())
    }
}

#[cfg(test)]
mod tests {
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
            let num = ((size >> shift) & 0xFF) as u8;
            bytes_writer.write_u8(num).unwrap();
        }
        assert_eq!(
            &bytes_writer.extract_current_bytes()[..],
            &[0, 0, 3, 232],
            "Expected 1000 to encode as big-endian [0, 0, 3, 232]"
        );
    }
}
