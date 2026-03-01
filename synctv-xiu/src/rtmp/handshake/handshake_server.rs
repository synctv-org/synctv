use {
    super::{
        define, define::ServerHandshakeState, digest::DigestProcessor, errors::HandshakeError,
        handshake_trait::THandshakeServer, utils,
    },
    byteorder::BigEndian,
    bytes::BytesMut,
    crate::bytesio::{
        bytes_reader::BytesReader, bytes_writer::AsyncBytesWriter, bytes_writer::BytesWriter,
        bytesio::TNetIO,
    },
    std::sync::Arc,
    tokio::sync::Mutex,
};

pub struct SimpleHandshakeServer {
    pub reader: BytesReader,
    pub writer: AsyncBytesWriter,
    pub state: ServerHandshakeState,

    c1_bytes: BytesMut,
    c1_timestamp: u32,
}

pub struct ComplexHandshakeServer {
    pub reader: BytesReader,
    pub writer: AsyncBytesWriter,
    pub state: ServerHandshakeState,

    c1_digest: BytesMut,
    c1_timestamp: u32,
}

impl SimpleHandshakeServer {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            reader: BytesReader::new(BytesMut::new()),
            writer: AsyncBytesWriter::new(io),
            state: ServerHandshakeState::ReadC0C1,

            c1_bytes: BytesMut::new(),
            c1_timestamp: 0,
        }
    }
    pub fn extend_data(&mut self, data: &[u8]) -> Result<(), HandshakeError> {
        self.reader.extend_from_slice(data)?;
        Ok(())
    }

    pub async fn handshake(&mut self) -> Result<(), HandshakeError> {
        loop {
            match self.state {
                ServerHandshakeState::ReadC0C1 => {
                    tracing::info!("[ S<-C ] [simple handshake] read C0C1");
                    self.read_c0()?;
                    self.read_c1()?;
                    self.state = ServerHandshakeState::WriteS0S1S2;
                }

                ServerHandshakeState::WriteS0S1S2 => {
                    tracing::info!("[ S->C ] [simple handshake] write S0S1S2");
                    self.write_s0()?;
                    self.write_s1()?;
                    self.write_s2()?;
                    self.writer.flush().await?;
                    self.state = ServerHandshakeState::ReadC2;
                    break;
                }

                ServerHandshakeState::ReadC2 => {
                    tracing::info!("[ S<-C ] [simple handshake] read C2");
                    self.read_c2()?;
                    self.state = ServerHandshakeState::Finish;
                }

                ServerHandshakeState::Finish => {
                    tracing::info!("simple handshake successfully..");
                    break;
                }
            }
        }

        Ok(())
    }
}

impl ComplexHandshakeServer {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            reader: BytesReader::new(BytesMut::new()),
            writer: AsyncBytesWriter::new(io),
            state: ServerHandshakeState::ReadC0C1,

            c1_digest: BytesMut::new(),
            c1_timestamp: 0,
        }
    }

    pub fn extend_data(&mut self, data: &[u8]) -> Result<(), HandshakeError> {
        self.reader.extend_from_slice(data)?;
        Ok(())
    }

    pub async fn handshake(&mut self) -> Result<(), HandshakeError> {
        loop {
            match self.state {
                ServerHandshakeState::ReadC0C1 => {
                    tracing::info!("[ S<-C ] [complex handshake] read C0C1");
                    self.read_c0()?;
                    self.read_c1()?;
                    self.state = ServerHandshakeState::WriteS0S1S2;
                }

                ServerHandshakeState::WriteS0S1S2 => {
                    tracing::info!("[ S->C ] [complex handshake] write S0S1S2");
                    self.write_s0()?;
                    self.write_s1()?;
                    self.write_s2()?;
                    self.writer.flush().await?;
                    tracing::info!("[ S->C ] [complex handshake] write S0S1S2 finish");
                    self.state = ServerHandshakeState::ReadC2;
                    break;
                }

                ServerHandshakeState::ReadC2 => {
                    tracing::info!("[ S<-C ] [complex handshake] read C2");
                    self.read_c2()?;
                    self.state = ServerHandshakeState::Finish;
                }

                ServerHandshakeState::Finish => {
                    tracing::info!("complex handshake successfully..");
                    break;
                }
            }
        }

        Ok(())
    }
}

impl THandshakeServer for SimpleHandshakeServer {
    fn read_c0(&mut self) -> Result<(), HandshakeError> {
        self.reader.read_u8()?;
        Ok(())
    }

    fn read_c1(&mut self) -> Result<(), HandshakeError> {
        let c1_bytes = self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;
        self.c1_bytes = c1_bytes.clone();

        let mut reader = BytesReader::new(c1_bytes);
        self.c1_timestamp = reader.read_u32::<BigEndian>()?;

        Ok(())
    }

    fn read_c2(&mut self) -> Result<(), HandshakeError> {
        self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;
        Ok(())
    }

    fn write_s0(&mut self) -> Result<(), HandshakeError> {
        self.writer.write_u8(define::RTMP_VERSION as u8)?;
        Ok(())
    }

    fn write_s1(&mut self) -> Result<(), HandshakeError> {
        self.writer.write_u32::<BigEndian>(utils::current_time())?;

        let timestamp = self.c1_timestamp;
        self.writer.write_u32::<BigEndian>(timestamp)?;

        self.writer
            .write_random_bytes(define::RTMP_HANDSHAKE_SIZE as u32 - 8)?;
        Ok(())
    }

    fn write_s2(&mut self) -> Result<(), HandshakeError> {
        let data = self.c1_bytes.clone();
        self.writer.write(&data[..])?;
        Ok(())
    }
}

impl THandshakeServer for ComplexHandshakeServer {
    fn read_c0(&mut self) -> Result<(), HandshakeError> {
        self.reader.read_u8()?;
        Ok(())
    }

    fn read_c1(&mut self) -> Result<(), HandshakeError> {
        let c1_bytes = self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;

        /*read the timestamp*/
        self.c1_timestamp = BytesReader::new(c1_bytes.clone()).read_u32::<BigEndian>()?;

        /*read the digest and save*/
        let mut key = BytesMut::new();
        key.extend_from_slice(define::RTMP_CLIENT_KEY_FIRST_HALF.as_bytes());

        let mut digest_processor = DigestProcessor::new(c1_bytes, key);
        let (digest_content, _) = digest_processor.read_digest()?;

        self.c1_digest = digest_content;

        Ok(())
    }

    fn read_c2(&mut self) -> Result<(), HandshakeError> {
        self.reader.read_bytes(define::RTMP_HANDSHAKE_SIZE)?;
        Ok(())
    }

    fn write_s0(&mut self) -> Result<(), HandshakeError> {
        self.writer.write_u8(define::RTMP_VERSION as u8)?;
        Ok(())
    }

    fn write_s1(&mut self) -> Result<(), HandshakeError> {
        /*write the s1 data*/
        let mut writer = BytesWriter::new();

        writer.write_u32::<BigEndian>(utils::current_time())?;
        writer.write(&define::RTMP_SERVER_VERSION)?;
        writer.write_random_bytes(define::RTMP_HANDSHAKE_SIZE as u32 - 8)?;

        /*generate the digest*/
        let mut key = BytesMut::new();
        key.extend_from_slice(define::RTMP_SERVER_KEY_FIRST_HALF.as_bytes());

        let mut digest_processor = DigestProcessor::new(writer.extract_current_bytes(), key);
        let content = digest_processor.generate_and_fill_digest()?;

        /*write*/
        self.writer.write(&content[..])?;
        Ok(())
    }

    fn write_s2(&mut self) -> Result<(), HandshakeError> {
        /*write the s2 data*/
        let mut writer = BytesWriter::new();

        writer.write_u32::<BigEndian>(utils::current_time())?;
        writer.write_u32::<BigEndian>(self.c1_timestamp)?;
        writer.write_random_bytes(define::RTMP_HANDSHAKE_SIZE as u32 - 8)?;

        /*generate the key for s2*/
        let mut key = BytesMut::new();
        key.extend_from_slice(&define::RTMP_SERVER_KEY);

        let mut digest_processor = DigestProcessor::new(BytesMut::new(), key);
        let tmp_key = digest_processor.make_digest(Vec::from(&self.c1_digest[..]))?;

        /*generate the digest for s2 data*/
        let mut data: BytesMut = BytesMut::new();
        data.extend_from_slice(&writer.get_current_bytes()[..1504]);

        let mut digest_processor_2 = DigestProcessor::new(BytesMut::new(), tmp_key);
        let digtest = digest_processor_2.make_digest(Vec::from(&data[..]))?;

        let content = [data, digtest].concat();

        /*write*/
        self.writer.write(&content[..])?;

        Ok(())
    }
}

pub struct HandshakeServer {
    simple_handshaker: SimpleHandshakeServer,
    complex_handshaker: ComplexHandshakeServer,
    is_complex: bool,

    saved_data: BytesMut,
}

impl HandshakeServer {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            simple_handshaker: SimpleHandshakeServer::new(io.clone()),
            complex_handshaker: ComplexHandshakeServer::new(io),
            is_complex: true,

            saved_data: BytesMut::new(),
        }
    }

    #[cfg(test)]
    pub fn new_simple_only(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            simple_handshaker: SimpleHandshakeServer::new(io.clone()),
            complex_handshaker: ComplexHandshakeServer::new(io),
            is_complex: false,

            saved_data: BytesMut::new(),
        }
    }

    pub fn extend_data(&mut self, data: &[u8]) -> Result<(), HandshakeError> {
        if self.is_complex {
            self.complex_handshaker.extend_data(data)?;
            self.saved_data.extend_from_slice(data);
        } else {
            self.simple_handshaker.extend_data(data)?;
        }
        Ok(())
    }

    pub const fn state(&mut self) -> ServerHandshakeState {
        if self.is_complex {
            self.complex_handshaker.state
        } else {
            self.simple_handshaker.state
        }
    }

    pub fn get_remaining_bytes(&mut self) -> BytesMut {
        if self.is_complex { self.complex_handshaker.reader.get_remaining_bytes() } else { self.simple_handshaker.reader.get_remaining_bytes() }
    }
    pub async fn handshake(&mut self) -> Result<(), HandshakeError> {
        if self.is_complex {
            let result = self.complex_handshaker.handshake().await;
            match result {
                Ok(()) => {
                    //println!("Complex handshake is successfully!!")
                }
                Err(err) => {
                    tracing::warn!("complex handshake failed.. err:{err}");
                    self.is_complex = false;
                    let data = self.saved_data.clone();
                    self.extend_data(&data[..])?;
                    self.simple_handshaker.handshake().await?;
                }
            }
        } else {
            self.simple_handshaker.handshake().await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytesio::bytesio::{NetType, TNetIO};
    use crate::bytesio::bytesio_errors::BytesIOError;
    use async_trait::async_trait;
    use bytes::{Bytes, BytesMut};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    /// Mock `TNetIO` that captures writes and ignores reads
    struct MockNetIO {
        written: Vec<u8>,
    }

    impl MockNetIO {
        fn new() -> Self {
            Self {
                written: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl TNetIO for MockNetIO {
        async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError> {
            self.written.extend_from_slice(&bytes);
            Ok(())
        }
        async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
            Ok(BytesMut::new())
        }
        async fn read_timeout(&mut self, _duration: Duration) -> Result<BytesMut, BytesIOError> {
            Ok(BytesMut::new())
        }
        fn get_net_type(&self) -> NetType {
            NetType::TCP
        }
    }

    fn make_io() -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> {
        Arc::new(Mutex::new(Box::new(MockNetIO::new())))
    }

    /// Build a valid C0+C1 payload (1 byte C0 version + 1536 bytes C1)
    fn build_c0c1() -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + define::RTMP_HANDSHAKE_SIZE);
        // C0: RTMP version byte
        data.push(define::RTMP_VERSION as u8);
        // C1: 4 bytes timestamp + 4 bytes zeros + 1528 bytes random
        let timestamp: u32 = 12345;
        data.extend_from_slice(&timestamp.to_be_bytes());
        data.extend_from_slice(&[0u8; 4]); // version zeros
        // Fill remaining 1528 bytes with pattern
        for i in 0..(define::RTMP_HANDSHAKE_SIZE - 8) {
            data.push((i % 256) as u8);
        }
        data
    }

    /// Build a valid C2 payload (1536 bytes echoing S1)
    fn build_c2() -> Vec<u8> {
        vec![0xAA; define::RTMP_HANDSHAKE_SIZE]
    }

    // ==================== SimpleHandshakeServer Tests ====================

    #[test]
    fn test_simple_handshake_server_initial_state() {
        let io = make_io();
        let server = SimpleHandshakeServer::new(io);
        assert!(matches!(server.state, ServerHandshakeState::ReadC0C1));
    }

    #[test]
    fn test_simple_handshake_server_extend_data() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);
        let result = server.extend_data(&[1, 2, 3]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_handshake_read_c0c1() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);

        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Read C0
        assert!(server.read_c0().is_ok());
        // Read C1
        assert!(server.read_c1().is_ok());

        // c1_timestamp should be extracted
        assert_eq!(server.c1_timestamp, 12345);
        // c1_bytes should be 1536 bytes
        assert_eq!(server.c1_bytes.len(), define::RTMP_HANDSHAKE_SIZE);
    }

    #[test]
    fn test_simple_handshake_read_c0_insufficient_data() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);
        // Empty data -> should fail
        let result = server.read_c0();
        assert!(result.is_err());
    }

    #[test]
    fn test_simple_handshake_read_c1_insufficient_data() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);
        // Only provide C0 (1 byte), no C1 data
        server.extend_data(&[define::RTMP_VERSION as u8]).unwrap();
        server.read_c0().unwrap();
        // C1 requires 1536 bytes but none available
        let result = server.read_c1();
        assert!(result.is_err());
    }

    #[test]
    fn test_simple_handshake_write_s0() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);
        let result = server.write_s0();
        assert!(result.is_ok());
        // Should have written 1 byte (RTMP version)
        assert_eq!(server.writer.bytes_writer.bytes.len(), 1);
        assert_eq!(server.writer.bytes_writer.bytes[0], define::RTMP_VERSION as u8);
    }

    #[test]
    fn test_simple_handshake_write_s1() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);
        server.c1_timestamp = 42;
        let result = server.write_s1();
        assert!(result.is_ok());
        // S1 should be exactly 1536 bytes (4 timestamp + 4 echo timestamp + 1528 random)
        assert_eq!(
            server.writer.bytes_writer.bytes.len(),
            define::RTMP_HANDSHAKE_SIZE
        );
    }

    #[test]
    fn test_simple_handshake_write_s2_echoes_c1() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Set up C1 bytes
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();
        server.read_c0().unwrap();
        server.read_c1().unwrap();

        // Clear writer before S2
        server.writer.bytes_writer.bytes.clear();

        let result = server.write_s2();
        assert!(result.is_ok());
        // S2 should echo the C1 bytes exactly
        assert_eq!(
            server.writer.bytes_writer.bytes.len(),
            define::RTMP_HANDSHAKE_SIZE
        );
    }

    #[tokio::test]
    async fn test_simple_handshake_full_c0c1_to_write_s0s1s2() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Feed C0+C1 data
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Run handshake (should read C0C1 and write S0S1S2, then break waiting for C2)
        let result = server.handshake().await;
        assert!(result.is_ok());
        assert!(matches!(server.state, ServerHandshakeState::ReadC2));
    }

    #[tokio::test]
    async fn test_simple_handshake_complete_flow() {
        let io = make_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Phase 1: Feed C0+C1 and run handshake (reads C0C1, writes S0S1S2)
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::ReadC2));

        // Phase 2: Feed C2 and run handshake (reads C2, transitions to Finish)
        let c2 = build_c2();
        server.extend_data(&c2).unwrap();
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::Finish));
    }

    // ==================== ComplexHandshakeServer Tests ====================

    #[test]
    fn test_complex_handshake_server_initial_state() {
        let io = make_io();
        let server = ComplexHandshakeServer::new(io);
        assert!(matches!(server.state, ServerHandshakeState::ReadC0C1));
    }

    #[test]
    fn test_complex_handshake_read_c0() {
        let io = make_io();
        let mut server = ComplexHandshakeServer::new(io);
        server.extend_data(&[define::RTMP_VERSION as u8]).unwrap();
        assert!(server.read_c0().is_ok());
    }

    #[test]
    fn test_complex_handshake_read_c0_insufficient_data() {
        let io = make_io();
        let mut server = ComplexHandshakeServer::new(io);
        let result = server.read_c0();
        assert!(result.is_err());
    }

    // ==================== HandshakeServer (Composite) Tests ====================

    #[test]
    fn test_handshake_server_defaults_to_complex() {
        let io = make_io();
        let server = HandshakeServer::new(io);
        assert!(server.is_complex);
    }

    #[test]
    fn test_handshake_server_extend_data_complex_saves_data() {
        let io = make_io();
        let mut server = HandshakeServer::new(io);

        let data = vec![1, 2, 3, 4, 5];
        server.extend_data(&data).unwrap();

        // In complex mode, data should be saved for potential simple fallback
        assert_eq!(server.saved_data.len(), 5);
    }

    #[tokio::test]
    async fn test_handshake_server_falls_back_to_simple_on_complex_failure() {
        let io = make_io();
        let mut server = HandshakeServer::new(io);

        // Feed a simple C0+C1 (no valid digest for complex handshake)
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Complex handshake should fail, fallback to simple
        let result = server.handshake().await;
        assert!(result.is_ok());
        // After fallback, is_complex should be false
        assert!(!server.is_complex);
    }

    #[tokio::test]
    async fn test_handshake_server_simple_mode_directly() {
        let io = make_io();
        let mut server = HandshakeServer::new_simple_only(io);
        assert!(!server.is_complex);

        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        let result = server.handshake().await;
        assert!(result.is_ok());
    }

    // ==================== Define Constants ====================

    #[test]
    fn test_rtmp_version() {
        assert_eq!(define::RTMP_VERSION, 3);
    }

    #[test]
    fn test_rtmp_handshake_size() {
        assert_eq!(define::RTMP_HANDSHAKE_SIZE, 1536);
    }

    #[test]
    fn test_rtmp_server_key_length() {
        assert_eq!(define::RTMP_SERVER_KEY.len(), 68);
    }

    #[test]
    fn test_rtmp_server_key_starts_with_genuine() {
        // First 36 bytes should be "Genuine Adobe Flash Media Server 001"
        let prefix = &define::RTMP_SERVER_KEY[..36];
        assert_eq!(
            std::str::from_utf8(prefix).unwrap(),
            define::RTMP_SERVER_KEY_FIRST_HALF
        );
    }

    // ==================== Utils ====================

    #[test]
    fn test_current_time_returns_nonzero() {
        let t = utils::current_time();
        // Should be non-zero (unless UNIX epoch is now, which won't happen)
        assert_ne!(t, 0);
    }

    // ==================== ServerHandshakeState ====================

    #[test]
    fn test_server_handshake_state_is_copy() {
        let state = ServerHandshakeState::ReadC0C1;
        let state_copy = state;
        // Both should still be usable (Copy trait)
        assert!(matches!(state, ServerHandshakeState::ReadC0C1));
        assert!(matches!(state_copy, ServerHandshakeState::ReadC0C1));
    }
}
