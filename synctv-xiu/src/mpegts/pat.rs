use {
    super::{crc32, define::epat_pid, errors::MpegTsError, pmt},
    crate::bytesio::bytes_writer::BytesWriter,
    byteorder::{BigEndian, LittleEndian},
    bytes::BytesMut,
};

#[derive(Debug, Clone)]
pub struct Pat {
    transport_stream_id: u16,
    version_number: u8, //5bits
    //continuity_counter: u8, //s4 bits

    //pub pmt_count: usize,
    pub pmt: Vec<pmt::Pmt>,
}

impl Default for Pat {
    fn default() -> Self {
        Self::new()
    }
}

impl Pat {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transport_stream_id: 1,
            version_number: 0,
            //continuity_counter: 0,
            //pmt_count: 0,
            pmt: Vec::new(),
        }
    }
}
pub struct PatMuxer {
    pub bytes_writer: BytesWriter,
}

impl Default for PatMuxer {
    fn default() -> Self {
        Self::new()
    }
}
//ITU-T H.222.0
impl PatMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
        }
    }

    pub fn write(&mut self, pat: Pat) -> Result<BytesMut, MpegTsError> {
        /*table id*/
        self.bytes_writer.write_u8(epat_pid::PAT_TID_PAS as u8)?;

        /*section length*/
        let length = pat.pmt.len() as u16 * 4 + 5 + 4;
        self.bytes_writer.write_u16::<BigEndian>(0xb000 | length)?;
        /*transport_stream_id*/
        self.bytes_writer
            .write_u16::<BigEndian>(pat.transport_stream_id)?;
        /*version_number*/
        self.bytes_writer
            .write_u8(0xC1 | (pat.version_number << 1))?;

        /*section_number*/
        /*last_section_number*/
        self.bytes_writer.write_u16::<BigEndian>(0x00)?;

        for ele in &pat.pmt {
            /*program number*/
            self.bytes_writer
                .write_u16::<BigEndian>(ele.program_number)?;
            /*PID*/
            self.bytes_writer.write_u16::<BigEndian>(0xE000 | ele.pid)?;
        }

        /*crc32*/
        let crc32_value = crc32::gen_crc32(0xffffffff, self.bytes_writer.get_current_bytes());
        self.bytes_writer.write_u32::<LittleEndian>(crc32_value)?;

        Ok(self.bytes_writer.extract_current_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pat_new() {
        let pat = Pat::new();
        assert_eq!(pat.transport_stream_id, 1);
        assert_eq!(pat.version_number, 0);
        assert!(pat.pmt.is_empty());
    }

    #[test]
    fn test_pat_default() {
        let pat = Pat::default();
        assert_eq!(pat.transport_stream_id, 1);
        assert!(pat.pmt.is_empty());
    }

    #[test]
    fn test_pat_muxer_new() {
        let muxer = PatMuxer::new();
        assert!(muxer.bytes_writer.get_current_bytes().is_empty());
    }

    #[test]
    fn test_pat_muxer_default() {
        let muxer = PatMuxer::default();
        assert!(muxer.bytes_writer.get_current_bytes().is_empty());
    }

    #[test]
    fn test_pat_muxer_write_empty_pmt() {
        let mut muxer = PatMuxer::new();
        let pat = Pat::new();
        let result = muxer.write(pat);
        assert!(result.is_ok());
        let data = result.unwrap();
        // PAT header: table_id(1) + section_length(2) + transport_stream_id(2) + version(1) + section_nums(2) + crc32(4) = 12 bytes
        assert_eq!(data.len(), 12);
        // Check table_id
        assert_eq!(data[0], epat_pid::PAT_TID_PAS as u8);
    }

    #[test]
    fn test_pat_muxer_write_with_pmt() {
        use super::pmt::Pmt;

        let mut muxer = PatMuxer::new();
        let mut pat = Pat::new();
        pat.pmt.push(Pmt {
            program_number: 1,
            pid: 0x100,
            pcr_pid: 0x100,
            version_number: 0,
            continuity_counter: 0,
            program_info: bytes::BytesMut::new(),
            streams: Vec::new(),
        });

        let result = muxer.write(pat);
        assert!(result.is_ok());
        let data = result.unwrap();
        // PAT header(12) + PMT entry(4) = 16 bytes
        assert_eq!(data.len(), 16);
    }
}
