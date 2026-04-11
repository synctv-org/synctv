use {
    super::errors::NetStreamError,
    crate::bytesio::bytesio::TNetIO,
    crate::flv::amf0::{amf0_writer::Amf0Writer, define::Amf0ValueType},
    crate::rtmp::{
        chunk::{define as chunk_define, packetizer::ChunkPacketizer, ChunkInfo},
        messages::define as messages_define,
    },
    indexmap::IndexMap,
    std::sync::Arc,
    tokio::sync::Mutex,
};

pub struct NetStreamWriter {
    amf0_writer: Amf0Writer,
    packetizer: ChunkPacketizer,
}

impl NetStreamWriter {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            amf0_writer: Amf0Writer::new(),
            packetizer: ChunkPacketizer::new(io),
        }
    }
    async fn write_chunk(&mut self, msg_stream_id: u32) -> Result<(), NetStreamError> {
        let data = self.amf0_writer.extract_current_bytes();
        let data_len = u32::try_from(data.len()).map_err(|_| NetStreamError {
            value: super::errors::NetStreamErrorValue::MessageTooLarge(data.len()),
        })?;

        let mut chunk_info = ChunkInfo::new(
            chunk_define::csid_type::COMMAND_AMF0_AMF3,
            chunk_define::chunk_type::TYPE_0,
            0,
            data_len,
            messages_define::msg_type_id::COMMAND_AMF0,
            msg_stream_id,
            data,
        );

        self.packetizer.write_chunk(&mut chunk_info).await?;
        Ok(())
    }
    pub async fn write_play(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
        start: &f64,
        duration: &f64,
        reset: &bool,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer.write_string(&String::from("play"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_string(stream_name)?;
        self.amf0_writer.write_number(start)?;
        self.amf0_writer.write_number(duration)?;
        self.amf0_writer.write_bool(reset)?;

        self.write_chunk(0).await
    }
    pub async fn write_delete_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer
            .write_string(&String::from("deleteStream"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_number(stream_id)?;

        self.write_chunk(0).await
    }

    pub async fn write_close_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer
            .write_string(&String::from("closeStream"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_number(stream_id)?;

        self.write_chunk(0).await
    }

    pub async fn write_release_stream(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer
            .write_string(&String::from("releaseStream"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_string(stream_name)?;

        self.write_chunk(0).await
    }

    pub async fn write_fcpublish(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer.write_string(&String::from("FCPublish"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_string(stream_name)?;

        self.write_chunk(0).await
    }

    pub async fn write_publish(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
        stream_type: &String,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer.write_string(&String::from("publish"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_string(stream_name)?;
        self.amf0_writer.write_string(stream_type)?;

        self.write_chunk(0).await
    }
    pub async fn write_on_status(
        &mut self,
        transaction_id: &f64,
        level: &str,
        code: &str,
        description: &str,
    ) -> Result<(), NetStreamError> {
        self.amf0_writer.write_string(&String::from("onStatus"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;

        let mut properties_map = IndexMap::new();

        properties_map.insert(
            String::from("level"),
            Amf0ValueType::UTF8String(level.to_owned()),
        );
        properties_map.insert(
            String::from("code"),
            Amf0ValueType::UTF8String(code.to_owned()),
        );
        properties_map.insert(
            String::from("description"),
            Amf0ValueType::UTF8String(description.to_owned()),
        );

        self.amf0_writer.write_object(&properties_map)?;

        self.write_chunk(1).await
    }
}
