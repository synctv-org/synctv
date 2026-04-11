use {
    super::{
        crc32,
        define::{epat_pid, epsi_stream_type},
        errors::MpegTsError,
        pes,
    },
    crate::bytesio::bytes_writer::BytesWriter,
    byteorder::{BigEndian, LittleEndian},
    bytes::BytesMut,
    std::io::{Error, ErrorKind},
};

fn invalid_data_error(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}
#[derive(Debug, Clone)]
pub struct Pmt {
    pub pid: u16,
    pub program_number: u16,
    pub version_number: u8,     //5 bits
    pub continuity_counter: u8, //4i bits
    pub pcr_pid: u16,           //13 bits
    pub program_info: BytesMut,
    pub streams: Vec<pes::Pes>,
}

impl Default for Pmt {
    fn default() -> Self {
        Self::new()
    }
}

impl Pmt {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pid: 0,
            program_number: 0,
            version_number: 0,     //5 bits
            continuity_counter: 0, //4i bits
            pcr_pid: 0,            //13 bit
            program_info: BytesMut::new(),
            streams: Vec::new(),
        }
    }
}

pub struct PmtMuxer {
    pub bytes_writer: BytesWriter,
}

impl Default for PmtMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl PmtMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
        }
    }

    pub fn write(&mut self, pmt: &Pmt) -> Result<BytesMut, MpegTsError> {
        /*table id*/
        let table_id = u8::try_from(epat_pid::PAT_TID_PMS)
            .map_err(|_| invalid_data_error("PMT table id exceeds u8"))?;
        self.bytes_writer.write_u8(table_id)?;

        let mut tmp_bytes_writer = BytesWriter::new();
        /*program_number*/
        tmp_bytes_writer.write_u16::<BigEndian>(pmt.program_number)?;
        /*version_number*/
        tmp_bytes_writer.write_u8(0xC1 | (pmt.version_number << 1))?;
        /*section_number*/
        tmp_bytes_writer.write_u8(0x00)?;
        /*last_section_number*/
        tmp_bytes_writer.write_u8(0x00)?;
        /*PCR_PID*/
        tmp_bytes_writer.write_u16::<BigEndian>(0xE000 | pmt.pcr_pid)?;
        /*program_info_length*/
        let program_info_length = u16::try_from(pmt.program_info.len())
            .map_err(|_| invalid_data_error("PMT program info length exceeds u16"))?;
        tmp_bytes_writer.write_u16::<BigEndian>(0xF000 | program_info_length)?;

        if program_info_length > 0 && program_info_length < 0x400 {
            tmp_bytes_writer.write(&pmt.program_info[..])?;
        }

        for stream in &pmt.streams {
            /*stream_type*/
            let stream_type = if stream.codec_id == epsi_stream_type::PSI_STREAM_AUDIO_OPUS {
                epsi_stream_type::PSI_STREAM_PRIVATE_DATA
            } else {
                stream.codec_id
            };
            tmp_bytes_writer.write_u8(stream_type)?;
            /*elementary_PID*/
            tmp_bytes_writer.write_u16::<BigEndian>(0xE000 | stream.pid)?;
            /*ES_info_length*/
            tmp_bytes_writer.write_u16::<BigEndian>(0xF000)?;
        }

        /*section_length*/
        let section_length = u16::try_from(tmp_bytes_writer.len())
            .map_err(|_| invalid_data_error("PMT section length exceeds u16"))?
            .saturating_add(4);
        self.bytes_writer
            .write_u16::<BigEndian>(0xB000 | section_length)?;

        self.bytes_writer
            .write(&tmp_bytes_writer.extract_current_bytes()[..])?;

        /*crc32*/
        let crc32_value = crc32::gen_crc32(0xffff_ffff, self.bytes_writer.get_current_bytes());
        self.bytes_writer.write_u32::<LittleEndian>(crc32_value)?;

        Ok(self.bytes_writer.extract_current_bytes())
    }

    pub const fn write_descriptor(&mut self) -> Result<(), MpegTsError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pmt_new() {
        let pmt = Pmt::new();
        assert_eq!(pmt.pid, 0);
        assert_eq!(pmt.program_number, 0);
        assert_eq!(pmt.version_number, 0);
        assert_eq!(pmt.continuity_counter, 0);
        assert_eq!(pmt.pcr_pid, 0);
        assert!(pmt.program_info.is_empty());
        assert!(pmt.streams.is_empty());
    }

    #[test]
    fn test_pmt_default() {
        let pmt = Pmt::default();
        assert_eq!(pmt.pid, 0);
        assert!(pmt.streams.is_empty());
    }

    #[test]
    fn test_pmt_muxer_new() {
        let muxer = PmtMuxer::new();
        assert!(muxer.bytes_writer.get_current_bytes().is_empty());
    }

    #[test]
    fn test_pmt_muxer_default() {
        let muxer = PmtMuxer::default();
        assert!(muxer.bytes_writer.get_current_bytes().is_empty());
    }

    #[test]
    fn test_pmt_muxer_write_empty_streams() {
        let mut muxer = PmtMuxer::new();
        let pmt = Pmt::new();
        let result = muxer.write(&pmt);
        assert!(result.is_ok());
        let data = result.unwrap();
        // Minimum PMT size: table_id(1) + section_length(2) + program_number(2) + version(1) +
        // section_nums(2) + pcr_pid(2) + program_info_len(2) + crc32(4) = 16 bytes
        assert!(!data.is_empty());
        // Check table_id
        assert_eq!(
            data[0],
            u8::try_from(epat_pid::PAT_TID_PMS).expect("PMT table id must fit in u8")
        );
    }

    #[test]
    fn test_pmt_muxer_write_with_pcr_pid() {
        let mut muxer = PmtMuxer::new();
        let mut pmt = Pmt::new();
        pmt.pcr_pid = 0x100;

        let result = muxer.write(&pmt);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
    }

    #[test]
    fn test_pmt_muxer_write_with_program_info() {
        let mut muxer = PmtMuxer::new();
        let mut pmt = Pmt::new();
        // Add some program info
        pmt.program_info = BytesMut::from(&[0x01, 0x02, 0x03][..]);

        let result = muxer.write(&pmt);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
    }

    #[test]
    fn test_pmt_muxer_write_descriptor() {
        let mut muxer = PmtMuxer::new();
        let result = muxer.write_descriptor();
        assert!(result.is_ok());
    }
}
