use {
    super::{
        define, define::ClientHandshakeState, errors::HandshakeError,
        handshake_trait::THandshakeClient, utils,
    },
    crate::bytesio::{bytes_reader::BytesReader, bytes_writer::AsyncBytesWriter, bytesio::TNetIO},
    byteorder::BigEndian,
    bytes::BytesMut,
    std::sync::Arc,
    tokio::sync::Mutex,
};

// use super::define;
// use super::utils;
// use super::{define::ClientHandshakeState, handshake_trait::THandshakeClient};

pub struct SimpleHandshakeClient {
    reader: BytesReader,
    writer: AsyncBytesWriter,
    s1_bytes: BytesMut,
    pub state: ClientHandshakeState,
}

impl SimpleHandshakeClient {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            reader: BytesReader::new(BytesMut::new()),
            writer: AsyncBytesWriter::new(io),
            s1_bytes: BytesMut::new(),
            state: ClientHandshakeState::WriteC0C1,
        }
    }

    pub fn extend_data(&mut self, data: &[u8]) -> Result<(), HandshakeError> {
        self.reader.extend_from_slice(data)?;
        Ok(())
    }
    pub async fn flush(&mut self) -> Result<(), HandshakeError> {
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn handshake(&mut self) -> Result<(), HandshakeError> {
        loop {
            match self.state {
                ClientHandshakeState::WriteC0C1 => {
                    self.write_c0()?;
                    self.write_c1()?;
                    self.flush().await?;
                    self.state = ClientHandshakeState::ReadS0S1S2;
                    break;
                }

                ClientHandshakeState::ReadS0S1S2 => {
                    self.read_s0()?;
                    self.read_s1()?;
                    self.read_s2()?;
                    self.state = ClientHandshakeState::WriteC2;
                }

                ClientHandshakeState::WriteC2 => {
                    self.write_c2()?;
                    self.flush().await?;
                    self.state = ClientHandshakeState::Finish;
                }

                ClientHandshakeState::Finish => {
                    break;
                }
            }
        }

        Ok(())
    }
}

impl THandshakeClient for SimpleHandshakeClient {
    fn write_c0(&mut self) -> Result<(), HandshakeError> {
        self.writer.write_u8(define::RTMP_VERSION as u8)?;
        Ok(())
    }
    fn write_c1(&mut self) -> Result<(), HandshakeError> {
        self.writer.write_u32::<BigEndian>(utils::current_time())?;
        self.writer.write_u32::<BigEndian>(0)?;

        self.writer
            .write_random_bytes((define::RTMP_HANDSHAKE_SIZE - 8) as u32)?;
        Ok(())
    }
    fn write_c2(&mut self) -> Result<(), HandshakeError> {
        self.writer.write(&self.s1_bytes[0..])?;
        Ok(())
    }

    fn read_s0(&mut self) -> Result<(), HandshakeError> {
        self.reader.read_u8()?;
        Ok(())
    }
    fn read_s1(&mut self) -> Result<(), HandshakeError> {
        self.s1_bytes = self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;
        Ok(())
    }
    fn read_s2(&mut self) -> Result<(), HandshakeError> {
        let _ = self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;
        Ok(())
    }
}
