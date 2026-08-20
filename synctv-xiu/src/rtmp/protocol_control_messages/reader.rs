use {
    crate::bytesio::{bytes_errors::BytesReadError, bytes_reader::BytesReader},
    crate::rtmp::messages::define::SetPeerBandwidthProperties,
    byteorder::BigEndian,
};

pub struct ProtocolControlMessageReader {
    pub reader: BytesReader,
}

impl ProtocolControlMessageReader {
    #[must_use]
    pub const fn new(reader: BytesReader) -> Self {
        Self { reader }
    }
    pub fn read_set_chunk_size(&mut self) -> Result<u32, BytesReadError> {
        let chunk_size = self.reader.read_u32::<BigEndian>()?;
        Ok(chunk_size)
    }

    pub fn read_abort_message(&mut self) -> Result<u32, BytesReadError> {
        let chunk_stream_id = self.reader.read_u32::<BigEndian>()?;
        Ok(chunk_stream_id)
    }

    pub fn read_acknowledgement(&mut self) -> Result<u32, BytesReadError> {
        let sequence_number = self.reader.read_u32::<BigEndian>()?;
        Ok(sequence_number)
    }

    pub fn read_window_acknowledgement_size(&mut self) -> Result<u32, BytesReadError> {
        let window_acknowledgement_size = self.reader.read_u32::<BigEndian>()?;
        Ok(window_acknowledgement_size)
    }

    pub fn read_set_peer_bandwidth(
        &mut self,
    ) -> Result<SetPeerBandwidthProperties, BytesReadError> {
        let window_size = self.reader.read_u32::<BigEndian>()?;
        let limit_type = self.reader.read_u8()?;

        Ok(SetPeerBandwidthProperties::new(window_size, limit_type))
    }
}
