use {
    super::{define, errors::MpegTsError},
    crate::bytesio::bytes_writer::BytesWriter,
    bytes::BytesMut,
};

fn i64_to_u8(value: i64) -> Result<u8, MpegTsError> {
    u8::try_from(value)
        .map_err(|_| std::io::Error::other("PES timestamp field exceeds u8 range").into())
}

fn timestamp_byte(value: i64, shift: u32) -> Result<u8, MpegTsError> {
    i64_to_u8((value >> shift) & 0xFF)
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Pes {
    pub program_number: u16,
    pub pid: u16,
    pub stream_id: u8,
    pub codec_id: u8,
    pub continuity_counter: u8,
    pub esinfo: BytesMut,
    pub esinfo_length: usize,

    pub data_alignment_indicator: u8, //1

    pub pts: i64,
    pub dts: i64,
    escr_base: u64,
    escr_extension: u32,
    es_rate: u32,
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

            data_alignment_indicator: 0, //1

            pts: 0,
            dts: 0,
            escr_base: 0,
            escr_extension: 0,
            es_rate: 0,
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

    //http://dvdnav.mplayerhq.hu/dvdinfo/pes-hdr.html
    pub fn write_pes_header(
        &mut self,
        payload_data_length: usize,
        stream_data: &Pes,
        h264_h265_with_aud: bool,
    ) -> Result<(), MpegTsError> {
        /*pes start code 3 bytes*/
        self.bytes_writer.write_u8(0x00)?; //0
        self.bytes_writer.write_u8(0x00)?; //1
        self.bytes_writer.write_u8(0x01)?; //2

        /*stream id 1 byte*/
        self.bytes_writer.write_u8(stream_data.stream_id)?; //3

        /*pes packet length 2 bytes*/
        self.bytes_writer.write_u8(0x00)?; //4
        self.bytes_writer.write_u8(0x00)?; //5

        /*first flag 1 byte*/
        self.bytes_writer.write_u8(0x80)?; //6

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

        /*second flag 1 byte*/
        self.bytes_writer.write_u8(flags)?; //7

        /*pes header data length*/
        self.bytes_writer.write_u8(length)?; //8

        //http://dvdnav.mplayerhq.hu/dvdinfo/pes-hdr.html
        /*The flags has 0x80 means that it has pts -- 5 bytes*/
        if (flags & 0x80) > 0 {
            let b9 = ((flags >> 2) & 0x30)/* 0011/0010 */
                | i64_to_u8(((stream_data.pts >> 30) & 0x07) << 1)? /* PTS 30-32 */
                | 0x01 /* marker_bit */;
            self.bytes_writer.write_u8(b9)?; //9

            let b10 = timestamp_byte(stream_data.pts, 22)?; /* PTS 22-29 */
            self.bytes_writer.write_u8(b10)?; //10

            let b11 = i64_to_u8((stream_data.pts >> 14) & 0xFE)? /* PTS 15-21 */ | 0x01; /* marker_bit */
            self.bytes_writer.write_u8(b11)?; //11

            let b12 = timestamp_byte(stream_data.pts, 7)?; /* PTS 7-14 */
            self.bytes_writer.write_u8(b12)?; //12

            let b13 = i64_to_u8((stream_data.pts << 1) & 0xFE)? /* PTS 0-6 */ | 0x01; /* marker_bit */
            self.bytes_writer.write_u8(b13)?; //13
        }

        /*The flags has 0x40 means that it has dts -- 5 bytes*/
        if (flags & 0x40) > 0 {
            let b14 = 0x10 /* 0001 */
                | i64_to_u8(((stream_data.dts >> 30) & 0x07) << 1)? /* DTS 30-32 */
                | 0x01 /* marker_bit */;
            self.bytes_writer.write_u8(b14)?;

            let b15 = timestamp_byte(stream_data.dts, 22)?; /* DTS 22-29 */
            self.bytes_writer.write_u8(b15)?;

            let b16 = i64_to_u8((stream_data.dts >> 14) & 0xFE)? /* DTS 15-21 */ | 0x01 /* marker_bit */;
            self.bytes_writer.write_u8(b16)?;

            let b17 = timestamp_byte(stream_data.dts, 7)?; /* DTS 7-14 */
            self.bytes_writer.write_u8(b17)?;

            let b18 = i64_to_u8((stream_data.dts << 1) & 0xFE)? /* DTS 0-6 */ | 0x01 /* marker_bit */;
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

        /*pes header -- update pes packet length*/
        if pes_payload_length > 0xFFFF {
            //only video data can exceed the 0xFFFF length,0 represet unlimited length
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
        assert_eq!(bytes[9], 51);
        assert_eq!(bytes[10], 132);
        assert_eq!(bytes[11], 19);
        assert_eq!(bytes[12], 134);
        assert_eq!(bytes[13], 161);
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
