use {
    super::errors::FlvMuxerError, crate::bytesio::bytes_writer::BytesWriter, byteorder::BigEndian,
};

const FLV_HEADER_AV: [u8; 9] = [
    0x46, // 'F'
    0x4c, //'L'
    0x56, //'V'
    0x01, //version
    0x05, //00000101  audio tag  and video tag
    0x00, 0x00, 0x00, 0x09, //flv header size
]; // 9
const FLV_HEADER_JUST_AUDIO: [u8; 9] = [
    0x46, // 'F'
    0x4c, //'L'
    0x56, //'V'
    0x01, //version
    0x04, //00000101  audio tag  and video tag
    0x00, 0x00, 0x00, 0x09, //flv header size
]; // 9
const FLV_HEADER_JUST_VIDEO: [u8; 9] = [
    0x46, // 'F'
    0x4c, //'L'
    0x56, //'V'
    0x01, //version
    0x01, //00000101  audio tag  and video tag
    0x00, 0x00, 0x00, 0x09, //flv header size
]; // 9
const FLV_HEADER_EMPTY_AV: [u8; 9] = [
    0x46, // 'F'
    0x4c, //'L'
    0x56, //'V'
    0x01, //version
    0x00, //00000101  audio tag  and video tag
    0x00, 0x00, 0x00, 0x09, //flv header size
]; // 9
pub const HEADER_LENGTH: u32 = 11;
pub struct FlvMuxer {
    pub writer: BytesWriter,
}

impl Default for FlvMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl FlvMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            writer: BytesWriter::new(),
        }
    }

    pub fn write_flv_header(
        &mut self,
        has_audio: bool,
        has_video: bool,
    ) -> Result<(), FlvMuxerError> {
        if has_audio && has_video {
            self.writer.write(&FLV_HEADER_AV)?;
        } else if has_audio {
            self.writer.write(&FLV_HEADER_JUST_AUDIO)?;
        } else if has_video {
            self.writer.write(&FLV_HEADER_JUST_VIDEO)?;
        } else {
            self.writer.write(&FLV_HEADER_EMPTY_AV)?;
        }
        Ok(())
    }

    pub fn write_flv_tag_header(
        &mut self,
        tag_type: u8,
        data_size: u32,
        timestamp: u32,
    ) -> Result<(), FlvMuxerError> {
        //tag type
        self.writer.write_u8(tag_type)?;
        //data size
        self.writer.write_u24::<BigEndian>(data_size)?;
        //timestamp
        self.writer
            .write_u24::<BigEndian>(timestamp & 0x00ff_ffff)?;
        //timestamp extended.
        let timestamp_ext = (timestamp >> 24 & 0xff) as u8;
        self.writer.write_u8(timestamp_ext)?;
        //stream id
        self.writer.write_u24::<BigEndian>(0)?;

        Ok(())
    }

    pub fn write_flv_tag_body(&mut self, body: &[u8]) -> Result<(), FlvMuxerError> {
        self.writer.write(body)?;
        Ok(())
    }

    pub fn write_previous_tag_size(&mut self, size: u32) -> Result<(), FlvMuxerError> {
        self.writer.write_u32::<BigEndian>(size)?;
        Ok(())
    }
}
