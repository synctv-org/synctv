use {
    super::{define, errors::MpegTsError},
    crate::bytesio::bytes_writer::BytesWriter,
    bytes::BytesMut,
};

fn masked_timestamp_byte(value: i64, shift: u32) -> u8 {
    u8::try_from((value >> shift) & 0xFF).unwrap_or(0)
}

fn marked_timestamp_byte(value: i64, shift: u32, mask: i64) -> u8 {
    u8::try_from((value >> shift) & mask).unwrap_or(0) | 0x01
}

fn marked_timestamp_low_byte(value: i64) -> u8 {
    u8::try_from((value << 1) & 0xFE).unwrap_or(0) | 0x01
}

#[derive(Debug, Clone)]
pub struct Pes {
    pub program_number: u16,
    pub pid: u16,
    pub stream_id: u8,
    pub codec_id: u8,
    pub continuity_counter: u8,
    pub esinfo: BytesMut,
    pub esinfo_length: usize,

    pub data_alignment_indicator: u8,

    pub pts: i64,
    pub dts: i64,
}

impl Default for Pes {
    fn default() -> Self {
        Self::new()
    }
}

impl Pes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            program_number: 0,
            pid: 0,
            stream_id: 0,
            codec_id: 0,
            continuity_counter: 0,
            esinfo: BytesMut::new(),
            esinfo_length: 0,

            data_alignment_indicator: 0,

            pts: define::PTS_NO_VALUE,
            dts: define::PTS_NO_VALUE,
        }
    }
}

pub struct PesMuxer {
    pub bytes_writer: BytesWriter,
}

impl Default for PesMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl PesMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes_writer.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn write_pes_header(
        &mut self,
        payload_data_length: usize,
        stream_data: &Pes,
        h264_h265_with_aud: bool,
    ) -> Result<(), MpegTsError> {
        self.bytes_writer.write_u8(0x00)?;
        self.bytes_writer.write_u8(0x00)?;
        self.bytes_writer.write_u8(0x01)?;

        self.bytes_writer.write_u8(stream_data.stream_id)?;

        self.bytes_writer.write_u8(0x00)?;
        self.bytes_writer.write_u8(0x00)?;

        self.bytes_writer.write_u8(0x80)?;

        if stream_data.data_alignment_indicator > 0 {
            self.bytes_writer.or_u8_at(6, 0x04)?;
        }

        let mut flags: u8 = 0x00;
        let mut length: u8 = 0x00;
        if define::PTS_NO_VALUE != stream_data.pts {
            flags |= 0x80;
            length += 5;
        }

        if define::PTS_NO_VALUE != stream_data.dts && stream_data.dts != stream_data.pts {
            flags |= 0x40;
            length += 5;
        }

        self.bytes_writer.write_u8(flags)?;

        self.bytes_writer.write_u8(length)?;

        // PTS and DTS are each encoded as 5 marker-bit-protected bytes.
        if (flags & 0x80) > 0 {
            let pts_prefix = if (flags & 0x40) > 0 { 0x30 } else { 0x20 };
            let b9 = pts_prefix | marked_timestamp_byte(stream_data.pts, 29, 0x0E);
            self.bytes_writer.write_u8(b9)?;

            let b10 = masked_timestamp_byte(stream_data.pts, 22);
            self.bytes_writer.write_u8(b10)?;

            let b11 = marked_timestamp_byte(stream_data.pts, 14, 0xFE);
            self.bytes_writer.write_u8(b11)?;

            let b12 = masked_timestamp_byte(stream_data.pts, 7);
            self.bytes_writer.write_u8(b12)?;

            let b13 = marked_timestamp_low_byte(stream_data.pts);
            self.bytes_writer.write_u8(b13)?;
        }

        if (flags & 0x40) > 0 {
            let b14 = 0x10 | marked_timestamp_byte(stream_data.dts, 29, 0x0E);
            self.bytes_writer.write_u8(b14)?;

            let b15 = masked_timestamp_byte(stream_data.dts, 22);
            self.bytes_writer.write_u8(b15)?;

            let b16 = marked_timestamp_byte(stream_data.dts, 14, 0xFE);
            self.bytes_writer.write_u8(b16)?;

            let b17 = masked_timestamp_byte(stream_data.dts, 7);
            self.bytes_writer.write_u8(b17)?;

            let b18 = marked_timestamp_low_byte(stream_data.dts);
            self.bytes_writer.write_u8(b18)?;
        }

        if define::epsi_stream_type::PSI_STREAM_H264 == stream_data.codec_id && !h264_h265_with_aud
        {
            let header: [u8; 6] = [0x00, 0x00, 0x00, 0x01, 0x09, 0xF0];
            self.bytes_writer.write(&header)?;
        }

        let pes_payload_length = self
            .bytes_writer
            .len()
            .saturating_sub(define::PES_HEADER_LEN as usize)
            + payload_data_length;

        if pes_payload_length > 0xFFFF {
            // PES length 0 means unbounded payload length, used for large video frames.
            self.bytes_writer.write_u8_at(4, 0x00)?;
            self.bytes_writer.write_u8_at(5, 0x00)?;
        } else {
            self.bytes_writer.write_u8_at(
                4,
                u8::try_from(pes_payload_length >> 8).map_err(|_| {
                    std::io::Error::other("PES payload length high byte exceeds u8 range")
                })?,
            )?;
            self.bytes_writer.write_u8_at(
                5,
                u8::try_from(pes_payload_length & 0xFF).map_err(|_| {
                    std::io::Error::other("PES payload length low byte exceeds u8 range")
                })?,
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{define, Pes, PesMuxer};

    #[test]
    fn write_pes_header_accepts_large_valid_pts_values() {
        let mut pes = Pes::new();
        pes.stream_id = 0xE0;
        pes.pts = 1_627_702_096;

        let mut muxer = PesMuxer::new();
        muxer
            .write_pes_header(0, &pes, true)
            .expect("33-bit PTS should encode into PES header bytes");

        let bytes = muxer.bytes_writer.extract_current_bytes();
        assert_eq!(bytes[7], 0x80);
        assert_eq!(bytes[8], 5);
        assert_eq!(bytes[9], 35);
        assert_eq!(bytes[10], 132);
        assert_eq!(bytes[11], 19);
        assert_eq!(bytes[12], 134);
        assert_eq!(bytes[13], 161);
    }

    #[test]
    fn write_pes_header_omits_timestamps_by_default() {
        let mut pes = Pes::new();
        pes.stream_id = 0xE0;

        let mut muxer = PesMuxer::new();
        muxer
            .write_pes_header(0, &pes, true)
            .expect("PES header without timestamps should encode");

        let bytes = muxer.bytes_writer.extract_current_bytes();
        assert_eq!(bytes[7], 0);
        assert_eq!(bytes[8], 0);
        assert_eq!(bytes.len(), usize::from(define::PES_HEADER_LEN) + 3);
    }

    #[test]
    fn write_pes_header_encodes_pts_and_dts_prefixes() {
        let mut pes = Pes::new();
        pes.stream_id = 0xE0;
        pes.pts = 1_627_702_096;
        pes.dts = 1_627_701_000;

        let mut muxer = PesMuxer::new();
        muxer
            .write_pes_header(0, &pes, true)
            .expect("PES header with PTS/DTS should encode");

        let bytes = muxer.bytes_writer.extract_current_bytes();
        assert_eq!(bytes[7], 0xC0);
        assert_eq!(bytes[8], 10);
        assert_eq!(bytes[9], 51);
        assert_eq!(bytes[14], 19);
    }

    #[test]
    fn write_pes_header_encodes_payload_length_low_byte_with_masked_bits() {
        let mut pes = Pes::new();
        pes.stream_id = 0xE0;

        let payload_data_length = 0x1234;
        let mut muxer = PesMuxer::new();
        muxer
            .write_pes_header(payload_data_length, &pes, true)
            .expect("valid PES payload length should encode without range errors");

        let bytes = muxer.bytes_writer.extract_current_bytes();
        let expected_length =
            bytes.len() - usize::from(define::PES_HEADER_LEN) + payload_data_length;

        assert_eq!(bytes[4], ((expected_length >> 8) & 0xFF) as u8);
        assert_eq!(bytes[5], (expected_length & 0xFF) as u8);
    }
}
