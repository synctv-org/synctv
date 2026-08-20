use {
    super::{define, errors::EventMessagesError},
    crate::bytesio::bytes_writer::AsyncBytesWriter,
    crate::rtmp::messages::define::msg_type_id,
    byteorder::BigEndian,
};

pub struct EventMessagesWriter {
    writer: AsyncBytesWriter,
    // amf0_writer: Amf0Writer,
}

impl EventMessagesWriter {
    #[must_use]
    pub const fn new(writer: AsyncBytesWriter) -> Self {
        Self { writer }
    }
    fn write_control_message_header(&mut self, len: u32) -> Result<(), EventMessagesError> {
        //0 1 2 3 4 5 6 7
        //+-+-+-+-+-+-+-+-+
        //|fmt|  cs id  |
        //+-+-+-+-+-+-+-+-+
        // 0x0     0x02

        self.writer.write_u8(0x02)?; //fmt 0 and csid 2 //0x0 << 6 | 0x02
        self.writer.write_u24::<BigEndian>(0)?; //timestamp 3 bytes and value 0
        self.writer.write_u24::<BigEndian>(len)?; //msg length
        self.writer.write_u8(msg_type_id::USER_CONTROL_EVENT)?; //msg type id
        self.writer.write_u32::<BigEndian>(0)?; //msg stream ID 0

        Ok(())
    }

    pub async fn write_stream_begin(&mut self, stream_id: u32) -> Result<(), EventMessagesError> {
        self.write_control_message_header(6)?;
        self.writer
            .write_u16::<BigEndian>(define::RTMP_EVENT_STREAM_BEGIN)?;
        self.writer.write_u32::<BigEndian>(stream_id)?;

        self.writer.flush().await?;
        Ok(())
    }

    //this function may contain bugs.
    pub async fn write_set_buffer_length(
        &mut self,
        stream_id: u32,
        ms: u32,
    ) -> Result<(), EventMessagesError> {
        self.write_control_message_header(10)?;
        self.writer
            .write_u16::<BigEndian>(define::RTMP_EVENT_SET_BUFFER_LENGTH)?;
        self.writer.write_u32::<BigEndian>(stream_id)?;
        self.writer.write_u32::<BigEndian>(ms)?;

        self.writer.flush().await?;

        Ok(())
    }

    pub async fn write_stream_is_record(
        &mut self,
        stream_id: u32,
    ) -> Result<(), EventMessagesError> {
        self.write_control_message_header(6)?;
        self.writer
            .write_u16::<BigEndian>(define::RTMP_EVENT_STREAM_IS_RECORDED)?;
        self.writer.write_u32::<BigEndian>(stream_id)?;

        self.writer.flush().await?;

        Ok(())
    }
}
