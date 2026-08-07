pub mod define;
pub mod errors;
pub mod packetizer;
pub mod unpacketizer;

// pub use chunk::{ChunkBasicHeader, ChunkHeader, ChunkInfo, ChunkMessageHeader};

use bytes::BytesMut;
use std::fmt;

//5.3.1.1
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct ChunkBasicHeader {
    pub format: u8,
    pub chunk_stream_id: u32,
}

impl ChunkBasicHeader {
    #[must_use]
    pub const fn new(fmt: u8, csid: u32) -> Self {
        Self {
            format: fmt,
            chunk_stream_id: csid,
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum ExtendTimestampType {
    //There is no extended timestamp
    NONE,
    //The extended timestamp field is read in format 0 chunk.
    FORMAT0,
    //The extended timestamp field is read in format 1 or 2 chunk.
    FORMAT12,
}

//5.3.1.2
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct ChunkMessageHeader {
    //save the absolute timestamp of chunk type 0.
    //or save the computed absolute timestamp of chunk type 1,2,3.
    pub timestamp: u32,
    pub msg_length: u32,
    pub msg_type_id: u8,
    pub msg_streamd_id: u32,
    // Save the timestamp delta of chunk type 1,2.
    // For chunk type 3, this field saves the timestamp
    // delta inherited from the previous chunk type 1 or 2.
    // NOTE: this value should be reset to 0 when the current chunk type is 0.
    pub timestamp_delta: u32,
    // This field will be set for type 0,1,2 .If the timestamp/timestamp delta >= 0xFFFFFF
    // then set this value to FORMAT0/FORMAT12 else set it to NONE.
    // Note that when the chunk format is 3, this value will be inherited from
    // the most recent chunk 0, 1, or 2 chunk.(5.3.1.3 Extended Timestamp).
    pub extended_timestamp_type: ExtendTimestampType,
}

impl ChunkMessageHeader {
    #[must_use]
    pub const fn new(timestamp: u32, msg_length: u32, msg_type_id: u8, msg_stream_id: u32) -> Self {
        Self {
            timestamp,
            msg_length,
            msg_type_id,
            msg_streamd_id: msg_stream_id,
            timestamp_delta: 0,
            extended_timestamp_type: ExtendTimestampType::NONE,
        }
    }
}

pub struct ChunkHeader {
    pub basic_header: ChunkBasicHeader,
    pub message_header: ChunkMessageHeader,
}

impl Default for ChunkHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkHeader {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            basic_header: ChunkBasicHeader::new(0, 0),
            message_header: ChunkMessageHeader::new(0, 0, 0, 0),
        }
    }
}

// pub struct Chunk {
//     basic_header: ChunkBasicHeader,
//     message_header: ChunkMessageHeader,
//     raw_data: BytesMut,
// }

#[derive(Eq, PartialEq, Clone)]
pub struct ChunkInfo {
    pub basic_header: ChunkBasicHeader,
    pub message_header: ChunkMessageHeader,
    pub payload: BytesMut,
}

impl fmt::Debug for ChunkInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex_payload = hex::encode(&self.payload);

        let formatted_payload = hex_payload
            .as_bytes()
            .chunks(2)
            .map(|chunk| format!("0x{}{}", chunk[0] as char, chunk[1] as char))
            .collect::<Vec<_>>()
            .join(", ");

        write!(
            f,
            "ChunkInfo {{ basic_header: {:?}, message_header: {:?}, payload: {} }}",
            self.basic_header, self.message_header, formatted_payload
        )
    }
}

impl Default for ChunkInfo {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0, 0, BytesMut::new())
    }
}

impl ChunkInfo {
    #[must_use]
    pub const fn new(
        csid: u32,
        format: u8,
        timestamp: u32,
        msg_length: u32,
        msg_type_id: u8,
        msg_stream_id: u32,
        payload: BytesMut,
    ) -> Self {
        Self {
            basic_header: ChunkBasicHeader::new(format, csid),
            message_header: ChunkMessageHeader::new(
                timestamp,
                msg_length,
                msg_type_id,
                msg_stream_id,
            ),
            payload,
        }
    }
}
