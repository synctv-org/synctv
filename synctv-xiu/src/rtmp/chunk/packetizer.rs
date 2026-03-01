use {
    super::{
        define::CHUNK_SIZE, errors::PackError, ChunkBasicHeader, ChunkHeader, ChunkInfo,
        ChunkMessageHeader, ExtendTimestampType,
    },
    crate::bytesio::{bytes_writer::AsyncBytesWriter, bytesio::TNetIO},
    byteorder::{BigEndian, LittleEndian},
    std::{num::NonZeroUsize, sync::Arc, time::Duration},
    tokio::sync::Mutex,
};

/// Write timeout for flushing RTMP chunks to TCP.
/// Prevents slow-write attacks where a subscriber accepts the connection but never reads.
const WRITE_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of chunk stream headers to cache in the packetizer.
/// Matches the unpacketizer's `MAX_CACHED_CHUNK_HEADERS` for consistency.
const MAX_CACHED_CHUNK_HEADERS: usize = 256;

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
    //save extended timestamp need to be write for chunk
    extended_timestamp: Option<u32>,
}

impl ChunkPacketizer {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            csid_2_chunk_header: lru::LruCache::new(
                NonZeroUsize::new(MAX_CACHED_CHUNK_HEADERS).expect("non-zero constant"),
            ),
            writer: AsyncBytesWriter::new(io),
            max_chunk_size: CHUNK_SIZE as usize,
            extended_timestamp: None,
        }
    }
    fn zip_chunk_header(&mut self, chunk_info: &mut ChunkInfo) -> Result<PackResult, PackError> {
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

        //update pre header
        self.csid_2_chunk_header.put(
            chunk_info.basic_header.chunk_stream_id,
            ChunkHeader {
                basic_header: chunk_info.basic_header.clone(),
                message_header: chunk_info.message_header.clone(),
            },
        );

        Ok(PackResult::Success)
    }

    fn write_basic_header(&mut self, fmt: u8, csid: u32) -> Result<(), PackError> {
        if csid >= 64 + 255 {
            self.writer.write_u8(fmt << 6 | 1)?;
            self.writer.write_u16::<BigEndian>((csid - 64) as u16)?;
        } else if csid >= 64 {
            self.writer.write_u8(fmt << 6)?;
            self.writer.write_u8((csid - 64) as u8)?;
        } else {
            self.writer.write_u8(fmt << 6 | csid as u8)?;
        }

        Ok(())
    }

    fn write_message_header(
        &mut self,
        basic_header: &ChunkBasicHeader,
        message_header: &mut ChunkMessageHeader,
    ) -> Result<(), PackError> {
        let message_header_timestamp: u32;
        (self.extended_timestamp, message_header_timestamp) = match basic_header.format {
            0 => {
                if message_header.timestamp >= 0xFFFFFF {
                    message_header.extended_timestamp_type = ExtendTimestampType::FORMAT0;
                    (Some(message_header.timestamp), 0xFFFFFF)
                } else {
                    (None, message_header.timestamp)
                }
            }
            1 | 2 => {
                if message_header.timestamp_delta >= 0xFFFFFF {
                    //if use the format1,2's extended timestamp, there may be a problem for
                    //av timestamp.
                    tracing::warn!(
                        "Now use extended timestamp for format {}, the value is: {}",
                        basic_header.format,
                        message_header.timestamp_delta
                    );
                    message_header.extended_timestamp_type = ExtendTimestampType::FORMAT12;
                    (Some(message_header.timestamp_delta), 0xFFFFFF)
                } else {
                    (None, message_header.timestamp_delta)
                }
            }
            _ => {
                //should not be here
                (None, 0)
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

        Ok(())
    }

    fn write_extened_timestamp(&mut self, timestamp: u32) -> Result<(), PackError> {
        self.writer.write_u32::<BigEndian>(timestamp)?;

        Ok(())
    }

    pub async fn write_chunk(&mut self, chunk_info: &mut ChunkInfo) -> Result<(), PackError> {
        self.zip_chunk_header(chunk_info)?;

        tracing::trace!(
            "write_chunk  current timestamp: {}",
            chunk_info.message_header.timestamp,
        );

        let mut whole_payload_size = chunk_info.payload.len();

        self.write_basic_header(
            chunk_info.basic_header.format,
            chunk_info.basic_header.chunk_stream_id,
        )?;

        self.write_message_header(&chunk_info.basic_header, &mut chunk_info.message_header)?;

        if let Some(extended_timestamp) = self.extended_timestamp {
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

                if let Some(extended_timestamp) = self.extended_timestamp {
                    self.write_extened_timestamp(extended_timestamp)?;
                }
            }
        }
        self.writer.flush_timeout(WRITE_FLUSH_TIMEOUT).await?;

        Ok(())
    }
}
