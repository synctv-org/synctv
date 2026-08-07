use {
    super::{
        define::{msg_type_id, Amf0CommandMessage, RtmpMessageData},
        errors::{MessageError, MessageErrorValue},
    },
    crate::bytesio::bytes_reader::BytesReader,
    crate::flv::amf0::{amf0_markers, amf0_reader::Amf0Reader},
    crate::rtmp::{
        chunk::ChunkInfo, protocol_control_messages::reader::ProtocolControlMessageReader,
        user_control_messages::reader::EventMessagesReader,
    },
};

pub struct MessageParser {
    chunk_info: ChunkInfo,
}

impl MessageParser {
    #[must_use]
    pub const fn new(chunk_info: ChunkInfo) -> Self {
        Self { chunk_info }
    }
    pub fn parse(self) -> Result<Option<RtmpMessageData>, MessageError> {
        let mut reader = BytesReader::new(self.chunk_info.payload);

        match self.chunk_info.message_header.msg_type_id {
            msg_type_id::COMMAND_AMF0 | msg_type_id::COMMAND_AMF3 => {
                if self.chunk_info.message_header.msg_type_id == msg_type_id::COMMAND_AMF3 {
                    reader.read_u8()?;
                }
                let mut amf_reader = Amf0Reader::new(reader);

                let command_name = amf_reader.read_with_type(amf0_markers::STRING)?;
                let transaction_id = amf_reader.read_with_type(amf0_markers::NUMBER)?;

                let command_obj_raw = amf_reader.read_with_type(amf0_markers::OBJECT);
                let command_obj = match command_obj_raw {
                    Ok(val) => val,
                    Err(_) => amf_reader.read_with_type(amf0_markers::NULL)?,
                };

                let others = amf_reader.read_all()?;

                Ok(Some(RtmpMessageData::Amf0Command(Box::new(
                    Amf0CommandMessage {
                        command_name,
                        transaction_id,
                        command_object: command_obj,
                        others,
                    },
                ))))
            }

            msg_type_id::AUDIO => {
                tracing::trace!(
                    "receive audio msg , msg length is{}\n",
                    self.chunk_info.message_header.msg_length
                );

                Ok(Some(RtmpMessageData::AudioData {
                    data: reader.extract_remaining_bytes(),
                }))
            }
            msg_type_id::VIDEO => {
                tracing::trace!(
                    "receive video msg , msg length is{}\n",
                    self.chunk_info.message_header.msg_length
                );
                Ok(Some(RtmpMessageData::VideoData {
                    data: reader.extract_remaining_bytes(),
                }))
            }
            msg_type_id::USER_CONTROL_EVENT => {
                tracing::trace!(
                    "receive user control event msg , msg length is{}\n",
                    self.chunk_info.message_header.msg_length
                );
                let data = EventMessagesReader::new(reader).parse_event()?;
                Ok(Some(data))
            }
            msg_type_id::SET_CHUNK_SIZE => {
                let chunk_size = ProtocolControlMessageReader::new(reader).read_set_chunk_size()?;
                Ok(Some(RtmpMessageData::SetChunkSize { chunk_size }))
            }
            msg_type_id::ABORT => {
                let chunk_stream_id =
                    ProtocolControlMessageReader::new(reader).read_abort_message()?;
                Ok(Some(RtmpMessageData::AbortMessage { chunk_stream_id }))
            }
            msg_type_id::ACKNOWLEDGEMENT => {
                let sequence_number =
                    ProtocolControlMessageReader::new(reader).read_acknowledgement()?;
                Ok(Some(RtmpMessageData::Acknowledgement { sequence_number }))
            }
            msg_type_id::WIN_ACKNOWLEDGEMENT_SIZE => {
                let size =
                    ProtocolControlMessageReader::new(reader).read_window_acknowledgement_size()?;
                Ok(Some(RtmpMessageData::WindowAcknowledgementSize { size }))
            }
            msg_type_id::SET_PEER_BANDWIDTH => {
                let properties =
                    ProtocolControlMessageReader::new(reader).read_set_peer_bandwidth()?;
                Ok(Some(RtmpMessageData::SetPeerBandwidth { properties }))
            }
            msg_type_id::DATA_AMF0 | msg_type_id::DATA_AMF3 => Ok(Some(RtmpMessageData::AmfData {
                raw_data: reader.extract_remaining_bytes(),
            })),

            message_type => Err(MessageError {
                value: MessageErrorValue::UnknownMessageType(message_type),
            }),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::MessageParser;
    use crate::flv::amf0::{amf0_writer::Amf0Writer, define::Amf0ValueType};
    use crate::rtmp::chunk::unpacketizer::ChunkUnpacketizer;
    use crate::rtmp::chunk::unpacketizer::UnpackResult;
    use crate::rtmp::chunk::ChunkInfo;
    use crate::rtmp::messages::define::{msg_type_id, RtmpMessageData};
    use crate::rtmp::messages::errors::MessageErrorValue;
    use bytes::BytesMut;
    use indexmap::IndexMap;

    fn expect_unknown_message_type(
        result: Result<Option<RtmpMessageData>, crate::rtmp::messages::errors::MessageError>,
    ) -> u8 {
        let err = result.expect_err("unknown RTMP message type should fail");
        match err.value {
            MessageErrorValue::UnknownMessageType(message_type) => message_type,
            other => panic!("expected unknown message type error, got {other:?}"),
        }
    }

    fn expect_set_chunk_size(message: &RtmpMessageData) -> u32 {
        let RtmpMessageData::SetChunkSize { chunk_size } = message else {
            panic!("expected set chunk size message, got {message:?}");
        };
        *chunk_size
    }

    fn expect_amf0_command(
        message: RtmpMessageData,
    ) -> Box<crate::rtmp::messages::define::Amf0CommandMessage> {
        let RtmpMessageData::Amf0Command(command) = message else {
            panic!("expected AMF0 command message, got {message:?}");
        };
        command
    }

    fn expect_amf0_object(value: Amf0ValueType) -> IndexMap<String, Amf0ValueType> {
        let Amf0ValueType::Object(properties) = value else {
            panic!("expected AMF0 object, got {value:?}");
        };
        properties
    }

    #[test]
    fn unknown_message_type_returns_typed_error() {
        let result =
            MessageParser::new(ChunkInfo::new(3, 0, 0, 0, 0x7F, 0, BytesMut::new())).parse();

        assert_eq!(expect_unknown_message_type(result), 0x7F);
    }

    #[test]
    fn parses_set_chunk_size_from_chunk_stream() {
        let mut unpacker = ChunkUnpacketizer::new();

        let data: [u8; 16] = [
            2, // basic header: format 0, csid 2
            0,
            0,
            0, // timestamp
            0,
            0,
            4, // message length
            msg_type_id::SET_CHUNK_SIZE,
            0,
            0,
            0,
            0, // message stream id
            0,
            0,
            16,
            0, // chunk size 4096
        ];

        unpacker.extend_data(&data[..]).unwrap();

        let mut parsed_messages = Vec::new();

        loop {
            let result = unpacker.read_chunk();

            let Ok(rv) = result else {
                break;
            };

            if let UnpackResult::ChunkInfo(chunk_info) = rv {
                assert_eq!(chunk_info.message_header.timestamp, 0);
                assert_eq!(chunk_info.message_header.msg_streamd_id, 0);

                let message_parser = MessageParser::new(chunk_info);
                if let Some(message) = message_parser.parse().expect("message should parse") {
                    parsed_messages.push(message);
                }
            }
        }

        assert_eq!(parsed_messages.len(), 1);

        assert_eq!(expect_set_chunk_size(&parsed_messages[0]), 4096);
    }

    #[test]
    fn parses_amf0_connect_command() {
        let mut properties = IndexMap::new();
        properties.insert(
            "app".to_string(),
            Amf0ValueType::UTF8String("harlan".to_string()),
        );
        properties.insert(
            "tcUrl".to_string(),
            Amf0ValueType::UTF8String("rtmp://localhost:1935/harlan".to_string()),
        );

        let mut writer = Amf0Writer::new();
        writer
            .write_string(&"connect".to_string())
            .expect("command name should encode");
        writer
            .write_number(&1.0)
            .expect("transaction id should encode");
        writer
            .write_object(&properties)
            .expect("connect object should encode");
        let payload = writer.extract_current_bytes();

        let message = MessageParser::new(ChunkInfo::new(
            3,
            0,
            0,
            payload.len() as u32,
            msg_type_id::COMMAND_AMF0,
            0,
            payload,
        ))
        .parse()
        .expect("connect message should parse")
        .expect("connect message should produce data");

        let command = expect_amf0_command(message);
        assert_eq!(
            command.command_name,
            Amf0ValueType::UTF8String("connect".to_string())
        );
        assert_eq!(command.transaction_id, Amf0ValueType::Number(1.0));
        let properties = expect_amf0_object(command.command_object);
        assert_eq!(
            properties.get("app"),
            Some(&Amf0ValueType::UTF8String("harlan".to_string()))
        );
    }
}
