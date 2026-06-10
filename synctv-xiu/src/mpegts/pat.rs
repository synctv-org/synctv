use {
    super::{crc32, define::epat_pid, errors::MpegTsError, pmt},
    crate::bytesio::bytes_writer::BytesWriter,
    byteorder::{BigEndian, LittleEndian},
    bytes::BytesMut,
    std::io::{Error, ErrorKind},
};

fn invalid_data_error(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Clone)]
pub struct Pat {
    transport_stream_id: u16,
    version_number: u8,
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
impl PatMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
        }
    }

    pub fn write(&mut self, pat: &Pat) -> Result<BytesMut, MpegTsError> {
        let table_id = u8::try_from(epat_pid::PAT_TID_PAS)
            .map_err(|_| invalid_data_error("PAT table id exceeds u8"))?;
        self.bytes_writer.write_u8(table_id)?;

        let pmt_len = u16::try_from(pat.pmt.len())
            .map_err(|_| invalid_data_error("PAT PMT count exceeds u16"))?;
        let length = pmt_len.saturating_mul(4).saturating_add(9);
        self.bytes_writer.write_u16::<BigEndian>(0xb000 | length)?;
        self.bytes_writer
            .write_u16::<BigEndian>(pat.transport_stream_id)?;
        self.bytes_writer
            .write_u8(0xC1 | (pat.version_number << 1))?;

        self.bytes_writer.write_u16::<BigEndian>(0x00)?;

        for ele in &pat.pmt {
            self.bytes_writer
                .write_u16::<BigEndian>(ele.program_number)?;
            self.bytes_writer.write_u16::<BigEndian>(0xE000 | ele.pid)?;
        }

        let crc32_value = crc32::gen_crc32(0xffff_ffff, self.bytes_writer.get_current_bytes());
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
        let result = muxer.write(&pat);
        assert!(result.is_ok());
        let data = result.unwrap();
        // PAT header: table_id(1) + section_length(2) + transport_stream_id(2) + version(1) + section_nums(2) + crc32(4) = 12 bytes
        assert_eq!(data.len(), 12);
        // Check table_id
        assert_eq!(
            data[0],
            u8::try_from(epat_pid::PAT_TID_PAS).expect("PAT table id must fit in u8")
        );
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

        let result = muxer.write(&pat);
        assert!(result.is_ok());
        let data = result.unwrap();
        // PAT header(12) + PMT entry(4) = 16 bytes
        assert_eq!(data.len(), 16);
    }
}
