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

const NETSTREAM_MESSAGE_STREAM_ID: u32 = 1;

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

        self.write_chunk(NETSTREAM_MESSAGE_STREAM_ID).await
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

        self.write_chunk(NETSTREAM_MESSAGE_STREAM_ID).await
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

        self.write_chunk(NETSTREAM_MESSAGE_STREAM_ID).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytesio::bytesio::{NetType, TNetIO};
    use crate::bytesio::bytesio_errors::BytesIOError;
    use crate::flv::amf0::define::Amf0ValueType;
    use crate::rtmp::chunk::unpacketizer::{ChunkUnpacketizer, UnpackResult};
    use crate::rtmp::messages::{define::RtmpMessageData, parser::MessageParser};
    use async_trait::async_trait;
    use bytes::{Bytes, BytesMut};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;
    use tokio::sync::Mutex;

    struct CaptureNetIo {
        writes: Arc<StdMutex<Vec<BytesMut>>>,
        reads: VecDeque<BytesMut>,
    }

    #[async_trait]
    impl TNetIO for CaptureNetIo {
        async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError> {
            self.writes
                .lock()
                .expect("capture writes lock should succeed")
                .push(BytesMut::from(bytes.as_ref()));
            Ok(())
        }

        async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
            Ok(self.reads.pop_front().unwrap_or_default())
        }

        async fn read_timeout(&mut self, _duration: Duration) -> Result<BytesMut, BytesIOError> {
            self.read().await
        }

        async fn shutdown(&mut self) -> Result<(), BytesIOError> {
            Ok(())
        }

        fn get_net_type(&self) -> NetType {
            NetType::TCP
        }
    }

    #[tokio::test]
    async fn write_publish_serializes_stream_name_and_type() {
        let writes = Arc::new(StdMutex::new(Vec::new()));
        let io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> = Arc::new(Mutex::new(Box::new(
            CaptureNetIo {
                writes: Arc::clone(&writes),
                reads: VecDeque::new(),
            },
        )));

        let mut writer = NetStreamWriter::new(io);
        writer
            .write_publish(
                &3.0,
                &"room-1/media-2?key=secret".to_string(),
                &"live".to_string(),
            )
            .await
            .expect("publish should serialize");

        let payload = writes
            .lock()
            .expect("capture writes lock should succeed")
            .iter()
            .fold(BytesMut::new(), |mut acc, chunk| {
                acc.extend_from_slice(chunk);
                acc
            });
        let mut unpackizer = ChunkUnpacketizer::new();
        unpackizer
            .extend_data(&payload)
            .expect("serialized publish should be readable");

        let chunks = match unpackizer
            .read_chunks()
            .expect("serialized publish should unpack")
        {
            UnpackResult::Chunks(chunks) => chunks,
            _ => panic!("expected publish command chunks"),
        };
        assert_eq!(chunks.len(), 1, "publish should serialize into one RTMP message");

        let parsed = MessageParser::new(chunks.into_iter().next().expect("one chunk"))
            .parse()
            .expect("publish chunk should parse")
            .expect("publish chunk should contain a message");
        match parsed {
            RtmpMessageData::Amf0Command {
                command_name,
                transaction_id,
                others,
                ..
            } => {
                assert!(matches!(
                    command_name,
                    Amf0ValueType::UTF8String(ref value) if value == "publish"
                ));
                assert!(matches!(
                    transaction_id,
                    Amf0ValueType::Number(value) if (value - 3.0).abs() < f64::EPSILON
                ));
                assert_eq!(others.len(), 2, "publish should preserve stream name and type");
                assert!(matches!(
                    &others[0],
                    Amf0ValueType::UTF8String(value) if value == "room-1/media-2?key=secret"
                ));
                assert!(matches!(
                    &others[1],
                    Amf0ValueType::UTF8String(value) if value == "live"
                ));
            }
            _ => panic!("expected AMF0 publish command"),
        }
    }
}
