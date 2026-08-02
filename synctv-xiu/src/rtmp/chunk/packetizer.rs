use {
    super::{
        define::CHUNK_SIZE,
        errors::{PackError, PackErrorValue},
        ChunkBasicHeader, ChunkHeader, ChunkInfo, ChunkMessageHeader, ExtendTimestampType,
    },
    crate::bytesio::{bytes_writer::AsyncBytesWriter, net_io::TNetIO},
    byteorder::{BigEndian, LittleEndian},
    std::{num::NonZeroUsize, sync::Arc, time::Duration},
    tokio::sync::Mutex,
};

/// Write timeout for flushing RTMP chunks to TCP.
/// Prevents slow-write attacks where a subscriber accepts the connection but never reads.
const WRITE_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of chunk stream headers to cache in the packetizer.
/// Matches the unpacketizer's `MAX_CACHED_CHUNK_HEADERS` for consistency.
const MAX_CACHED_CHUNK_HEADERS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(255);

#[derive(Eq, PartialEq, Debug)]
pub enum PackResult {
    Success,
    NotEnoughBytes,
}

pub struct ChunkPacketizer {
    /// LRU cache of chunk headers per chunk stream ID.
    /// Bounded to `MAX_CACHED_CHUNK_HEADERS` to prevent unbounded memory growth.
    csid_2_chunk_header: lru::LruCache<u32, ChunkHeader>,
    max_chunk_size: usize,
    writer: AsyncBytesWriter,
}

impl ChunkPacketizer {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            csid_2_chunk_header: lru::LruCache::new(MAX_CACHED_CHUNK_HEADERS),
            writer: AsyncBytesWriter::new(io),
            max_chunk_size: CHUNK_SIZE as usize,
        }
    }
    fn zip_chunk_header(&mut self, chunk_info: &mut ChunkInfo) -> PackResult {
        chunk_info.basic_header.format = 0;

        if let Some(pre_header) = self
            .csid_2_chunk_header
            .peek(&chunk_info.basic_header.chunk_stream_id)
        {
            let cur_msg_header = &mut chunk_info.message_header;
            let pre_msg_header = &pre_header.message_header;

            if cur_msg_header.timestamp < pre_msg_header.timestamp {
                tracing::warn!(
                    "Chunk stream id: {}, the current timestamp:{}  is smaller than pre chunk timestamp: {}",
                    chunk_info.basic_header.chunk_stream_id,
                    cur_msg_header.timestamp,
                    pre_msg_header.timestamp
                );
            } else if cur_msg_header.msg_streamd_id == pre_msg_header.msg_streamd_id {
                chunk_info.basic_header.format = 1;
                cur_msg_header.timestamp_delta =
                    cur_msg_header.timestamp - pre_msg_header.timestamp;

                if cur_msg_header.msg_type_id == pre_msg_header.msg_type_id
                    && cur_msg_header.msg_length == pre_msg_header.msg_length
                {
                    chunk_info.basic_header.format = 2;
                    if cur_msg_header.timestamp_delta == pre_msg_header.timestamp_delta {
                        chunk_info.basic_header.format = 3;
                        cur_msg_header.extended_timestamp_type =
                            pre_msg_header.extended_timestamp_type.clone();
                    }
                }
            }
        } else if chunk_info.message_header.timestamp_delta != 0 {
            tracing::warn!(
                "First chunk for csid {} has non-zero timestamp_delta: {}, resetting to 0",
                chunk_info.basic_header.chunk_stream_id,
                chunk_info.message_header.timestamp_delta
            );
            chunk_info.message_header.timestamp_delta = 0;
        }

        PackResult::Success
    }

    fn write_basic_header(&mut self, fmt: u8, csid: u32) -> Result<(), PackError> {
        if csid >= 64 + 255 {
            let extended_csid =
                u16::try_from(csid - 64).map_err(|_| PackErrorValue::InvalidChunkStreamId(csid))?;
            self.writer.write_u8(fmt << 6 | 1)?;
            // RTMP encodes the 16-bit extended CSID least-significant byte first.
            self.writer.write_u16::<LittleEndian>(extended_csid)?;
        } else if csid >= 64 {
            let extended_csid =
                u8::try_from(csid - 64).map_err(|_| PackErrorValue::InvalidChunkStreamId(csid))?;
            self.writer.write_u8(fmt << 6)?;
            self.writer.write_u8(extended_csid)?;
        } else {
            let basic_csid =
                u8::try_from(csid).map_err(|_| PackErrorValue::InvalidChunkStreamId(csid))?;
            self.writer.write_u8(fmt << 6 | basic_csid)?;
        }

        Ok(())
    }

    fn write_message_header(
        &mut self,
        basic_header: &ChunkBasicHeader,
        message_header: &mut ChunkMessageHeader,
    ) -> Result<Option<u32>, PackError> {
        let message_header_timestamp: u32;
        let extended_timestamp;
        (extended_timestamp, message_header_timestamp) = match basic_header.format {
            0 => {
                message_header.extended_timestamp_type = ExtendTimestampType::NONE;
                if message_header.timestamp >= 0x00FF_FFFF {
                    message_header.extended_timestamp_type = ExtendTimestampType::FORMAT0;
                    (Some(message_header.timestamp), 0x00FF_FFFF)
                } else {
                    (None, message_header.timestamp)
                }
            }
            1 | 2 => {
                message_header.extended_timestamp_type = ExtendTimestampType::NONE;
                if message_header.timestamp_delta >= 0x00FF_FFFF {
                    //if use the format1,2's extended timestamp, there may be a problem for
                    //av timestamp.
                    tracing::warn!(
                        "Now use extended timestamp for format {}, the value is: {}",
                        basic_header.format,
                        message_header.timestamp_delta
                    );
                    message_header.extended_timestamp_type = ExtendTimestampType::FORMAT12;
                    (Some(message_header.timestamp_delta), 0x00FF_FFFF)
                } else {
                    (None, message_header.timestamp_delta)
                }
            }
            3 => (
                match message_header.extended_timestamp_type {
                    ExtendTimestampType::FORMAT0 => Some(message_header.timestamp),
                    ExtendTimestampType::FORMAT12 => Some(message_header.timestamp_delta),
                    ExtendTimestampType::NONE => None,
                },
                0,
            ),
            _ => {
                return Err(PackErrorValue::InvalidChunkHeaderFormat(basic_header.format).into());
            }
        };

        match basic_header.format {
            0 => {
                self.writer
                    .write_u24::<BigEndian>(message_header_timestamp)?;
                self.writer
                    .write_u24::<BigEndian>(message_header.msg_length)?;
                self.writer.write_u8(message_header.msg_type_id)?;
                self.writer
                    .write_u32::<LittleEndian>(message_header.msg_streamd_id)?;
            }
            1 => {
                self.writer
                    .write_u24::<BigEndian>(message_header_timestamp)?;
                self.writer
                    .write_u24::<BigEndian>(message_header.msg_length)?;
                self.writer.write_u8(message_header.msg_type_id)?;
            }
            2 => {
                self.writer
                    .write_u24::<BigEndian>(message_header_timestamp)?;
            }
            _ => {}
        }

        Ok(extended_timestamp)
    }

    fn write_extened_timestamp(&mut self, timestamp: u32) -> Result<(), PackError> {
        self.writer.write_u32::<BigEndian>(timestamp)?;

        Ok(())
    }

    pub async fn write_chunk(&mut self, chunk_info: &mut ChunkInfo) -> Result<(), PackError> {
        self.zip_chunk_header(chunk_info);

        tracing::trace!(
            "write_chunk  current timestamp: {}",
            chunk_info.message_header.timestamp,
        );

        let mut whole_payload_size = chunk_info.payload.len();

        self.write_basic_header(
            chunk_info.basic_header.format,
            chunk_info.basic_header.chunk_stream_id,
        )?;

        let extended_timestamp =
            self.write_message_header(&chunk_info.basic_header, &mut chunk_info.message_header)?;

        // Header compression for the next message must observe the finalized
        // extended-timestamp type selected by `write_message_header`.
        self.csid_2_chunk_header.put(
            chunk_info.basic_header.chunk_stream_id,
            ChunkHeader {
                basic_header: chunk_info.basic_header.clone(),
                message_header: chunk_info.message_header.clone(),
            },
        );

        if let Some(extended_timestamp) = extended_timestamp {
            self.write_extened_timestamp(extended_timestamp)?;
        }

        let mut cur_payload_size: usize;
        while whole_payload_size > 0 {
            cur_payload_size = if whole_payload_size > self.max_chunk_size {
                self.max_chunk_size
            } else {
                whole_payload_size
            };

            let payload_bytes = chunk_info.payload.split_to(cur_payload_size);
            self.writer.write(&payload_bytes[0..])?;

            whole_payload_size -= cur_payload_size;

            if whole_payload_size > 0 {
                self.write_basic_header(3, chunk_info.basic_header.chunk_stream_id)?;

                if let Some(extended_timestamp) = extended_timestamp {
                    self.write_extened_timestamp(extended_timestamp)?;
                }
            }
        }
        self.writer.flush_timeout(WRITE_FLUSH_TIMEOUT).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytesio::{
        bytesio_errors::BytesIOError,
        net_io::{NetType, TNetIO},
    };
    use crate::rtmp::chunk::unpacketizer::{ChunkUnpacketizer, UnpackResult};
    use async_trait::async_trait;
    use bytes::{Bytes, BytesMut};
    use parking_lot::Mutex as ParkingMutex;

    struct CaptureIo {
        bytes: Arc<ParkingMutex<Vec<u8>>>,
    }

    #[async_trait]
    impl TNetIO for CaptureIo {
        async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError> {
            self.bytes.lock().extend_from_slice(&bytes);
            Ok(())
        }

        async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
            panic!("capture IO is write-only")
        }

        async fn read_timeout(&mut self, _duration: Duration) -> Result<BytesMut, BytesIOError> {
            panic!("capture IO is write-only")
        }

        async fn shutdown(&mut self) -> Result<(), BytesIOError> {
            Ok(())
        }

        fn get_net_type(&self) -> NetType {
            NetType::TCP
        }
    }

    #[tokio::test]
    async fn three_byte_csid_encoding_round_trips_through_unpacketizer() {
        let captured = Arc::new(ParkingMutex::new(Vec::new()));
        let io: Arc<tokio::sync::Mutex<Box<dyn TNetIO + Send + Sync>>> =
            Arc::new(tokio::sync::Mutex::new(Box::new(CaptureIo {
                bytes: Arc::clone(&captured),
            })));
        let mut packetizer = ChunkPacketizer::new(io);
        let mut chunk = ChunkInfo::new(320, 0, 7, 1, 9, 1, BytesMut::from(&[0xaa][..]));

        packetizer.write_chunk(&mut chunk).await.unwrap();
        let wire = captured.lock().clone();
        assert_eq!(&wire[..3], &[0x01, 0x00, 0x01]);

        let mut unpacker = ChunkUnpacketizer::new();
        unpacker.extend_data(&wire).unwrap();
        let UnpackResult::Chunks(chunks) = unpacker.read_chunks().unwrap() else {
            panic!("packetized message should unpack")
        };
        assert_eq!(chunks[0].basic_header.chunk_stream_id, 320);
        assert_eq!(chunks[0].payload.as_ref(), &[0xaa]);
    }

    #[tokio::test]
    async fn format_three_inherits_format_zero_extended_timestamp() {
        let captured = Arc::new(ParkingMutex::new(Vec::new()));
        let io: Arc<tokio::sync::Mutex<Box<dyn TNetIO + Send + Sync>>> =
            Arc::new(tokio::sync::Mutex::new(Box::new(CaptureIo {
                bytes: Arc::clone(&captured),
            })));
        let mut packetizer = ChunkPacketizer::new(io);
        let timestamp = 0x0100_0000;

        for payload in [0xaa, 0xbb] {
            let mut chunk =
                ChunkInfo::new(7, 0, timestamp, 1, 9, 1, BytesMut::from(&[payload][..]));
            packetizer.write_chunk(&mut chunk).await.unwrap();
        }

        let wire = captured.lock().clone();
        let mut unpacker = ChunkUnpacketizer::new();
        unpacker.extend_data(&wire).unwrap();
        let UnpackResult::Chunks(chunks) = unpacker.read_chunks().unwrap() else {
            panic!("packetized messages should unpack")
        };
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].message_header.timestamp, timestamp);
        assert_eq!(chunks[1].message_header.timestamp, timestamp);
        assert_eq!(chunks[0].payload.as_ref(), &[0xaa]);
        assert_eq!(chunks[1].payload.as_ref(), &[0xbb]);
    }
}
