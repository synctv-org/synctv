use {
    super::{
        define,
        errors::{UnpackError, UnpackErrorValue},
        ChunkBasicHeader, ChunkInfo, ChunkMessageHeader, ExtendTimestampType,
    },
    crate::bytesio::bytes_reader::BytesReader,
    crate::rtmp::messages::define::msg_type_id,
    byteorder::{BigEndian, LittleEndian},
    bytes::{BufMut, BytesMut},
    std::{cmp::min, fmt, num::NonZeroUsize, vec::Vec},
};

const PARSE_ERROR_NUMBER: usize = 5;
/// Maximum number of chunk stream IDs to track before cleanup
/// RTMP spec allows up to 65599 stream IDs, but in practice most streams use far fewer
const MAX_CACHED_CHUNK_HEADERS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(255);
/// Maximum message size (10 MB) to prevent unbounded memory growth from malicious clients
const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

#[derive(Eq, PartialEq, Debug)]
pub enum UnpackResult {
    ChunkBasicHeaderResult(ChunkBasicHeader),
    ChunkMessageHeaderResult(ChunkMessageHeader),
    ChunkInfo(ChunkInfo),
    Chunks(Vec<ChunkInfo>),
    Success,
    NotEnoughBytes,
    Empty,
}

#[derive(Copy, Clone, Debug)]
enum ChunkReadState {
    ReadBasicHeader = 1,
    ReadMessageHeader = 2,
    ReadExtendedTimestamp = 3,
    ReadMessagePayload = 4,
    Finish = 5,
}

impl fmt::Display for ChunkReadState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::ReadBasicHeader => {
                write!(f, "ReadBasicHeader")
            }
            Self::ReadMessageHeader => {
                write!(f, "ReadMessageHeader")
            }
            Self::ReadExtendedTimestamp => {
                write!(f, "ReadExtendedTimestamp")
            }
            Self::ReadMessagePayload => {
                write!(f, "ReadMessagePayload")
            }
            Self::Finish => {
                write!(f, "Finish")
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum MessageHeaderReadState {
    ReadTimeStamp = 1,
    ReadMsgLength = 2,
    ReadMsgTypeID = 3,
    ReadMsgStreamID = 4,
}

pub struct ChunkUnpacketizer {
    pub reader: BytesReader,

    /// Chunk reconstruction state for the active message.
    /// RTMP chunks can inherit message header fields from previous chunks on
    /// the same chunk stream, and large messages are split across multiple
    /// chunks according to the negotiated max chunk size.
    pub current_chunk_info: ChunkInfo,
    /// LRU cache of chunk message headers per chunk stream ID.
    /// Bounded to `MAX_CACHED_CHUNK_HEADERS`; least-recently-used entries
    /// are automatically evicted, avoiding the random-eviction problem
    /// of the previous HashMap-based pruning approach.
    chunk_message_headers: lru::LruCache<u32, ChunkMessageHeader>,
    chunk_read_state: ChunkReadState,
    msg_header_read_state: MessageHeaderReadState,
    max_chunk_size: usize,
    chunk_index: u32,
    pub session_type: u8,
    parse_error_number: usize,
}

impl Default for ChunkUnpacketizer {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkUnpacketizer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            reader: BytesReader::new(BytesMut::new()),
            current_chunk_info: ChunkInfo::default(),
            chunk_message_headers: lru::LruCache::new(MAX_CACHED_CHUNK_HEADERS),
            chunk_read_state: ChunkReadState::ReadBasicHeader,
            msg_header_read_state: MessageHeaderReadState::ReadTimeStamp,
            max_chunk_size: define::INIT_CHUNK_SIZE as usize,
            chunk_index: 0,
            session_type: 0,
            parse_error_number: 0,
        }
    }

    /// Clear cached chunk headers to free memory
    /// Call this when the connection is closed or when memory usage is a concern
    pub fn clear_cached_headers(&mut self) {
        self.chunk_message_headers.clear();
    }

    pub fn extend_data(&mut self, data: &[u8]) -> Result<(), UnpackError> {
        self.reader.extend_from_slice(data)?;

        tracing::trace!(
            "extend_data length: {}: content:{:X?}",
            self.reader.len(),
            self.reader
                .get_remaining_bytes()
                .split_to(self.reader.len())
                .to_vec()
        );
        Ok(())
    }

    pub fn update_max_chunk_size(&mut self, chunk_size: usize) {
        tracing::trace!("update max chunk size: {chunk_size}");
        self.max_chunk_size = chunk_size;
    }

    pub fn read_chunks(&mut self) -> Result<UnpackResult, UnpackError> {
        let mut chunks: Vec<ChunkInfo> = Vec::new();

        loop {
            match self.read_chunk() {
                Ok(chunk) => {
                    if let UnpackResult::ChunkInfo(chunk_info) = chunk {
                        let msg_type_id = chunk_info.message_header.msg_type_id;
                        chunks.push(chunk_info);

                        // Chunk size changes apply to subsequent chunks.
                        if msg_type_id == msg_type_id::SET_CHUNK_SIZE {
                            break;
                        }
                    }
                }
                Err(err) => {
                    if matches!(err.value, UnpackErrorValue::CannotParse) {
                        return Err(err);
                    }
                    break;
                }
            }
        }

        if chunks.is_empty() {
            Err(UnpackError {
                value: UnpackErrorValue::EmptyChunks,
            })
        } else {
            Ok(UnpackResult::Chunks(chunks))
        }
    }

    /// Read one RTMP chunk: basic header, message header, optional extended
    /// timestamp, and payload fragment.
    pub fn read_chunk(&mut self) -> Result<UnpackResult, UnpackError> {
        let mut result: UnpackResult = UnpackResult::Empty;

        self.chunk_index += 1;

        loop {
            result = match self.chunk_read_state {
                ChunkReadState::ReadBasicHeader => self.read_basic_header()?,
                ChunkReadState::ReadMessageHeader => self.read_message_header()?,
                ChunkReadState::ReadExtendedTimestamp => self.read_extended_timestamp()?,
                ChunkReadState::ReadMessagePayload => self.read_message_payload()?,
                ChunkReadState::Finish => {
                    self.chunk_read_state = ChunkReadState::ReadBasicHeader;
                    break;
                }
            };
        }

        Ok(result)
    }

    fn print_current_basic_header(&mut self) {
        tracing::trace!(
            "print_current_basic_header, csid: {},format id: {}",
            self.current_chunk_info.basic_header.chunk_stream_id,
            self.current_chunk_info.basic_header.format
        );
    }

    /// Read the RTMP basic header and recover the chunk stream ID.
    /// IDs 2-63 use one byte, 64-319 use two bytes, and 64-65599 can use
    /// three bytes.
    pub fn read_basic_header(&mut self) -> Result<UnpackResult, UnpackError> {
        let byte = self.reader.read_u8()?;

        let format_id = (byte >> 6) & 0b0000_0011;
        let mut csid = u32::from(byte & 0b0011_1111);

        match csid {
            0 => {
                if self.reader.is_empty() {
                    return Ok(UnpackResult::NotEnoughBytes);
                }
                csid = 64;
                csid += u32::from(self.reader.read_u8()?);
            }
            1 => {
                if self.reader.is_empty() {
                    return Ok(UnpackResult::NotEnoughBytes);
                }
                csid = 64;
                csid += u32::from(self.reader.read_u8()?);
                csid += u32::from(self.reader.read_u8()?) * 256;
            }
            _ => {}
        }

        // Restore the cached message header when the chunk stream changes.
        // RTMP chunks may omit repeated header fields, so each chunk stream ID
        // keeps its own previous header context.
        if csid != self.current_chunk_info.basic_header.chunk_stream_id {
            tracing::trace!(
                "read_basic_header, chunk stream id update, new: {}, old:{}, byte: {}",
                csid,
                self.current_chunk_info.basic_header.chunk_stream_id,
                byte
            );
            match self.chunk_message_headers.get(&csid) {
                Some(header) => {
                    self.current_chunk_info.message_header = header.clone();
                    self.print_current_basic_header();
                }
                None => {
                    // A new chunk stream starts with a type 0 message header.
                    if format_id != 0 {
                        tracing::warn!(
                            "The chunk stream id: {csid}'s first chunk format is {format_id}."
                        );

                        if self.parse_error_number > PARSE_ERROR_NUMBER {
                            return Err(UnpackError {
                                value: UnpackErrorValue::CannotParse,
                            });
                        }
                        self.parse_error_number += 1;
                    } else {
                        self.parse_error_number = 0;
                    }
                }
            }
        }

        if format_id == 0 {
            self.current_message_header().timestamp_delta = 0;
        }
        self.current_chunk_info.basic_header.chunk_stream_id = csid;
        self.current_chunk_info.basic_header.format = format_id;
        self.print_current_basic_header();

        self.chunk_read_state = ChunkReadState::ReadMessageHeader;

        Ok(UnpackResult::ChunkBasicHeaderResult(ChunkBasicHeader::new(
            format_id, csid,
        )))
    }

    const fn current_message_header(&mut self) -> &mut ChunkMessageHeader {
        &mut self.current_chunk_info.message_header
    }

    fn print_current_message_header(&self, state: ChunkReadState) {
        tracing::trace!(
            "print_current_basic_header state {}, timestamp:{}, timestamp delta:{}, msg length: {},msg type id: {}, msg stream id:{}",
            state,
            self.current_chunk_info.message_header.timestamp,
            self.current_chunk_info.message_header.timestamp_delta,
            self.current_chunk_info.message_header.msg_length,
            self.current_chunk_info.message_header.msg_type_id,
            self.current_chunk_info.message_header.msg_streamd_id
        );
    }

    pub fn read_message_header(&mut self) -> Result<UnpackResult, UnpackError> {
        tracing::trace!(
            "read_message_header, data left in buffer: {}",
            self.reader.len(),
        );

        // Type 3 inherits the extended-timestamp flag from the previous
        // type 0, 1, or 2 chunk on the same chunk stream.
        if self.current_chunk_info.basic_header.format != 3 {
            self.current_message_header().extended_timestamp_type = ExtendTimestampType::NONE;
        }

        match self.current_chunk_info.basic_header.format {
            // Type 0: absolute timestamp, message length, message type ID,
            // and message stream ID.
            0 => {
                loop {
                    match self.msg_header_read_state {
                        MessageHeaderReadState::ReadTimeStamp => {
                            self.current_message_header().timestamp =
                                self.reader.read_u24::<BigEndian>()?;
                            self.msg_header_read_state = MessageHeaderReadState::ReadMsgLength;
                        }
                        MessageHeaderReadState::ReadMsgLength => {
                            self.current_message_header().msg_length =
                                self.reader.read_u24::<BigEndian>()?;

                            tracing::trace!(
                                "read_message_header format 0, msg_length: {}",
                                self.current_message_header().msg_length,
                            );
                            self.msg_header_read_state = MessageHeaderReadState::ReadMsgTypeID;
                        }
                        MessageHeaderReadState::ReadMsgTypeID => {
                            self.current_message_header().msg_type_id = self.reader.read_u8()?;

                            tracing::trace!(
                                "read_message_header format 0, msg_type_id: {}",
                                self.current_message_header().msg_type_id
                            );
                            self.msg_header_read_state = MessageHeaderReadState::ReadMsgStreamID;
                        }
                        MessageHeaderReadState::ReadMsgStreamID => {
                            self.current_message_header().msg_streamd_id =
                                self.reader.read_u32::<LittleEndian>()?;
                            self.msg_header_read_state = MessageHeaderReadState::ReadTimeStamp;
                            break;
                        }
                    }
                }

                if self.current_message_header().timestamp >= 0x00FF_FFFF {
                    self.current_message_header().extended_timestamp_type =
                        ExtendTimestampType::FORMAT0;
                }
            }
            // Type 1: timestamp delta, message length, and message type ID.
            1 => {
                loop {
                    match self.msg_header_read_state {
                        MessageHeaderReadState::ReadTimeStamp => {
                            self.current_message_header().timestamp_delta =
                                self.reader.read_u24::<BigEndian>()?;
                            self.msg_header_read_state = MessageHeaderReadState::ReadMsgLength;
                        }
                        MessageHeaderReadState::ReadMsgLength => {
                            self.current_message_header().msg_length =
                                self.reader.read_u24::<BigEndian>()?;

                            tracing::trace!(
                                "read_message_header format 1, msg_length: {}",
                                self.current_message_header().msg_length
                            );
                            self.msg_header_read_state = MessageHeaderReadState::ReadMsgTypeID;
                        }
                        MessageHeaderReadState::ReadMsgTypeID => {
                            self.current_message_header().msg_type_id = self.reader.read_u8()?;

                            tracing::trace!(
                                "read_message_header format 1, msg_type_id: {}",
                                self.current_message_header().msg_type_id
                            );
                            self.msg_header_read_state = MessageHeaderReadState::ReadTimeStamp;
                            break;
                        }
                        MessageHeaderReadState::ReadMsgStreamID => {
                            tracing::error!("unexpected message header read state");
                            break;
                        }
                    }
                }

                if self.current_message_header().timestamp_delta >= 0x00FF_FFFF {
                    self.current_message_header().extended_timestamp_type =
                        ExtendTimestampType::FORMAT12;
                }
            }
            // Type 2: timestamp delta only.
            2 => {
                tracing::trace!(
                    "read_message_header format 2, msg_type_id: {}",
                    self.current_message_header().msg_type_id
                );
                self.current_message_header().timestamp_delta =
                    self.reader.read_u24::<BigEndian>()?;

                if self.current_message_header().timestamp_delta >= 0x00FF_FFFF {
                    self.current_message_header().extended_timestamp_type =
                        ExtendTimestampType::FORMAT12;
                }
            }

            _ => {}
        }

        self.chunk_read_state = ChunkReadState::ReadExtendedTimestamp;
        self.print_current_message_header(ChunkReadState::ReadMessageHeader);

        Ok(UnpackResult::Success)
    }

    pub fn read_extended_timestamp(&mut self) -> Result<UnpackResult, UnpackError> {
        match self.current_message_header().extended_timestamp_type {
            ExtendTimestampType::FORMAT0 => {
                self.current_message_header().timestamp = self.reader.read_u32::<BigEndian>()?;
            }
            ExtendTimestampType::FORMAT12 => {
                self.current_message_header().timestamp_delta =
                    self.reader.read_u32::<BigEndian>()?;
            }
            ExtendTimestampType::NONE => {}
        }

        let cur_format_id = self.current_chunk_info.basic_header.format;
        if cur_format_id == 1
            || cur_format_id == 2
            || (cur_format_id == 3 && self.current_chunk_info.payload.is_empty())
        {
            let timestamp = self.current_message_header().timestamp;
            let timestamp_delta = self.current_message_header().timestamp_delta;

            let (cur_abs_timestamp, is_overflow) = timestamp.overflowing_add(timestamp_delta);
            if is_overflow {
                tracing::warn!(
                    "The current timestamp is overflow, current basic header: {:?}, current message header: {:?}, payload len: {}, abs timestamp: {}",
                    self.current_chunk_info.basic_header,
                    self.current_chunk_info.message_header,
                    self.current_chunk_info.payload.len(),
                    cur_abs_timestamp
                );
            }
            self.current_message_header().timestamp = cur_abs_timestamp;
        }

        self.chunk_read_state = ChunkReadState::ReadMessagePayload;
        self.print_current_message_header(ChunkReadState::ReadExtendedTimestamp);

        Ok(UnpackResult::Success)
    }

    pub fn read_message_payload(&mut self) -> Result<UnpackResult, UnpackError> {
        let whole_msg_length = self.current_message_header().msg_length as usize;

        if whole_msg_length > MAX_MESSAGE_SIZE {
            return Err(UnpackError {
                value: UnpackErrorValue::MessageTooLarge(whole_msg_length, MAX_MESSAGE_SIZE),
            });
        }

        let remaining_bytes = whole_msg_length - self.current_chunk_info.payload.len();

        tracing::trace!(
            "read_message_payload whole msg length: {whole_msg_length} and remaining bytes need to be read: {remaining_bytes}"
        );

        let need_read_length = if whole_msg_length > self.max_chunk_size {
            min(remaining_bytes, self.max_chunk_size)
        } else {
            remaining_bytes
        };

        let remaining_mut = self.current_chunk_info.payload.remaining_mut();
        if need_read_length > remaining_mut {
            let additional = need_read_length - remaining_mut;
            self.current_chunk_info.payload.reserve(additional);
        }

        tracing::trace!(
            "read_message_payload buffer len:{}, need_read_length: {}",
            self.reader.len(),
            need_read_length
        );

        let payload_data = self.reader.read_bytes(need_read_length)?;
        self.current_chunk_info
            .payload
            .extend_from_slice(&payload_data[..]);

        tracing::trace!(
            "read_message_payload current msg payload len:{}",
            self.current_chunk_info.payload.len()
        );

        if self.current_chunk_info.payload.len() == whole_msg_length {
            self.chunk_read_state = ChunkReadState::Finish;
            let chunk_info = self.current_chunk_info.clone();
            self.current_chunk_info.payload.clear();

            let csid = self.current_chunk_info.basic_header.chunk_stream_id;

            self.chunk_message_headers
                .put(csid, self.current_chunk_info.message_header.clone());

            return Ok(UnpackResult::ChunkInfo(chunk_info));
        }

        self.chunk_read_state = ChunkReadState::ReadBasicHeader;

        Ok(UnpackResult::Success)
    }
}

#[cfg(test)]
mod tests {

    use super::ChunkInfo;
    use super::ChunkUnpacketizer;
    use super::UnpackResult;
    use bytes::BytesMut;

    #[test]
    fn test_set_chunk_size() {
        let mut unpacker = ChunkUnpacketizer::new();

        let data: [u8; 16] = [
            2, 0, 0, 0, // format + csid, timestamp
            0, 0, 4, 1, // message length, message type ID
            0, 0, 0, 0, // message stream ID
            0, 0, 10, 0, // body
        ];

        unpacker.extend_data(&data[..]).unwrap();

        let rv = unpacker.read_chunk();

        let mut body = BytesMut::new();
        body.extend_from_slice(&[00, 00, 10, 00]);

        let expected = ChunkInfo::new(2, 0, 0, 4, 1, 0, body);

        assert_eq!(
            rv.unwrap(),
            UnpackResult::ChunkInfo(expected),
            "set chunk size packet should unpack into the expected RTMP chunk"
        );
    }
}
