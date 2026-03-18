//! Comprehensive RTMP Handshake Module Unit Tests
//!
//! This test suite covers:
//! 1. Normal handshake flow (client and server)
//! 2. Invalid C0/C1 data handling
//! 3. Digest processor functionality
//! 4. Error handling scenarios
//! 5. Timeout tests (marked with #[ignore] as they require Docker/testcontainers)

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use std::sync::Arc;
use tokio::sync::Mutex;

use synctv_xiu::bytesio::bytesio::{NetType, TNetIO};
use synctv_xiu::bytesio::bytesio_errors::BytesIOError;
use synctv_xiu::rtmp::handshake::{
    define::{
        ClientHandshakeState, ServerHandshakeState, RTMP_CLIENT_KEY_FIRST_HALF, RTMP_DIGEST_LENGTH,
        RTMP_HANDSHAKE_SIZE, RTMP_SERVER_KEY_FIRST_HALF, RTMP_VERSION,
    },
    digest::DigestProcessor,
    errors::{DigestErrorValue, HandshakeError, HandshakeErrorValue},
    handshake_client::SimpleHandshakeClient,
    handshake_server::{ComplexHandshakeServer, HandshakeServer, SimpleHandshakeServer},
    handshake_trait::THandshakeServer,
    utils,
};

use async_trait::async_trait;
use bytes::Bytes;
use std::time::Duration;

// =============================================================================
// Mock IO Implementation
// =============================================================================

/// Mock `TNetIO` that captures writes and provides configurable reads
struct MockNetIO {
    read_data: Vec<u8>,
    read_pos: usize,
}

impl MockNetIO {
    const fn new() -> Self {
        Self {
            read_data: Vec::new(),
            read_pos: 0,
        }
    }
}

#[async_trait]
impl TNetIO for MockNetIO {
    async fn write(&mut self, _bytes: Bytes) -> Result<(), BytesIOError> {
        // Just discard writes in mock
        Ok(())
    }

    async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
        // Return data in chunks to simulate real network behavior
        let remaining = self.read_data.len() - self.read_pos;
        let chunk_size = std::cmp::min(remaining, 256);
        if chunk_size == 0 {
            // Return empty buffer when no more data
            return Ok(BytesMut::new());
        }
        let start = self.read_pos;
        let end = start + chunk_size;
        self.read_pos = end;
        Ok(BytesMut::from(&self.read_data[start..end]))
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

fn make_mock_io() -> Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> {
    Arc::new(Mutex::new(Box::new(MockNetIO::new())))
}

// =============================================================================
// Helper Functions for Building Handshake Data
// =============================================================================

/// Build a valid C0+C1 payload (1 byte C0 version + 1536 bytes C1)
fn build_c0c1() -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + RTMP_HANDSHAKE_SIZE);
    // C0: RTMP version byte
    data.push(RTMP_VERSION as u8);
    // C1: 4 bytes timestamp + 4 bytes zeros + 1528 bytes random
    let timestamp: u32 = 12345;
    data.extend_from_slice(&timestamp.to_be_bytes());
    data.extend_from_slice(&[0u8; 4]); // version zeros
                                       // Fill remaining 1528 bytes with pattern
    for i in 0..(RTMP_HANDSHAKE_SIZE - 8) {
        data.push((i % 256) as u8);
    }
    data
}

/// Build a valid S0+S1+S2 response (3073 bytes total)
fn build_s0s1s2(c1_echo: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + RTMP_HANDSHAKE_SIZE * 2);
    // S0: RTMP version byte
    data.push(RTMP_VERSION as u8);
    // S1: 4 bytes time + 4 bytes zero + 1528 bytes random
    data.extend_from_slice(&[0, 0, 0, 0]); // time
    data.extend_from_slice(&[0, 0, 0, 0]); // zero
    data.extend_from_slice(&[0u8; RTMP_HANDSHAKE_SIZE - 8]); // random
                                                             // S2: echo C1 back
    data.extend_from_slice(c1_echo);
    data
}

/// Build a valid C2 payload (1536 bytes echoing S1)
fn build_c2() -> Vec<u8> {
    vec![0xAA; RTMP_HANDSHAKE_SIZE]
}

// =============================================================================
// SimpleHandshakeClient Tests
// =============================================================================

mod simple_handshake_client_tests {
    use super::*;

    #[test]
    fn test_client_initial_state() {
        let io = make_mock_io();
        let client = SimpleHandshakeClient::new(io);
        assert_eq!(client.state, ClientHandshakeState::WriteC0C1);
    }

    #[tokio::test]
    async fn test_client_handshake_writes_c0c1() {
        let io = make_mock_io();
        let mut client = SimpleHandshakeClient::new(io);

        // First handshake call should write C0/C1 and transition state
        client.handshake().await.unwrap();
        assert_eq!(client.state, ClientHandshakeState::ReadS0S1S2);
    }

    #[tokio::test]
    async fn test_client_handshake_complete_flow() {
        let io = make_mock_io();
        let mut client = SimpleHandshakeClient::new(io);

        // Phase 1: Write C0/C1 (breaks after writing)
        client.handshake().await.unwrap();
        assert_eq!(client.state, ClientHandshakeState::ReadS0S1S2);

        // Manually feed S0/S1/S2 data to the client's reader
        let c0c1 = build_c0c1();
        let s0s1s2 = build_s0s1s2(&c0c1[1..]);
        client.extend_data(&s0s1s2).unwrap();

        // Phase 2 & 3: Read S0/S1/S2, write C2, and finish
        // The handshake loop continues until finish when there's data available
        client.handshake().await.unwrap();
        assert_eq!(client.state, ClientHandshakeState::Finish);
    }

    #[test]
    fn test_client_extend_data() {
        let io = make_mock_io();
        let mut client = SimpleHandshakeClient::new(io);
        let data = vec![1, 2, 3, 4, 5];
        let result = client.extend_data(&data);
        assert!(result.is_ok());
    }
}

// =============================================================================
// SimpleHandshakeServer Tests
// =============================================================================

mod simple_handshake_server_tests {
    use super::*;

    #[test]
    fn test_server_initial_state() {
        let io = make_mock_io();
        let server = SimpleHandshakeServer::new(io);
        assert!(matches!(server.state, ServerHandshakeState::ReadC0C1));
    }

    #[test]
    fn test_server_extend_data() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);
        let data = vec![1, 2, 3, 4, 5];
        server.extend_data(&data).unwrap();
    }

    #[test]
    fn test_server_read_c0_valid() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);
        server.extend_data(&[RTMP_VERSION as u8]).unwrap();
        let result = server.read_c0();
        assert!(result.is_ok());
    }

    #[test]
    fn test_server_read_c0_insufficient_data() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);
        // No data provided
        let result = server.read_c0();
        assert!(result.is_err());
    }

    #[test]
    fn test_server_read_c1_insufficient_data() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);
        server.extend_data(&[RTMP_VERSION as u8]).unwrap();
        server.read_c0().unwrap();
        // Only C0, no C1 data
        let result = server.read_c1();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_server_handshake_reads_c0c1_writes_s0s1s2() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Feed C0+C1
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Run handshake (should read C0C1, write S0S1S2, then wait for C2)
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::ReadC2));
    }

    #[tokio::test]
    async fn test_server_handshake_complete_flow() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Feed C0+C1
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Run handshake (should read C0C1, write S0S1S2)
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::ReadC2));

        // Feed C2
        let c2 = build_c2();
        server.extend_data(&c2).unwrap();

        // Run handshake (should read C2 and finish)
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::Finish));
    }
}

// =============================================================================
// ComplexHandshakeServer Tests
// =============================================================================

mod complex_handshake_server_tests {
    use super::*;

    #[test]
    fn test_complex_server_initial_state() {
        let io = make_mock_io();
        let server = ComplexHandshakeServer::new(io);
        assert!(matches!(server.state, ServerHandshakeState::ReadC0C1));
    }

    #[test]
    fn test_complex_server_read_c0_valid() {
        let io = make_mock_io();
        let mut server = ComplexHandshakeServer::new(io);
        server.extend_data(&[RTMP_VERSION as u8]).unwrap();
        let result = server.read_c0();
        assert!(result.is_ok());
    }

    #[test]
    fn test_complex_server_read_c0_insufficient_data() {
        let io = make_mock_io();
        let mut server = ComplexHandshakeServer::new(io);
        let result = server.read_c0();
        assert!(result.is_err());
    }
}

// =============================================================================
// DigestProcessor Tests
// =============================================================================

mod digest_processor_tests {
    use super::*;

    #[test]
    fn test_digest_processor_make_digest_basic() {
        let data = BytesMut::from(&[0u8; RTMP_HANDSHAKE_SIZE][..]);
        let key = BytesMut::from(RTMP_SERVER_KEY_FIRST_HALF.as_bytes());
        let mut processor = DigestProcessor::new(data, key);

        let message = vec![1, 2, 3, 4, 5];
        let result = processor.make_digest(&message);
        assert!(result.is_ok());
        let digest = result.unwrap();
        assert_eq!(digest.len(), RTMP_DIGEST_LENGTH);
    }

    #[test]
    fn test_digest_processor_make_digest_empty_message() {
        let data = BytesMut::new();
        let key = BytesMut::from(RTMP_SERVER_KEY_FIRST_HALF.as_bytes());
        let mut processor = DigestProcessor::new(data, key);

        let message: Vec<u8> = vec![];
        let result = processor.make_digest(&message);
        assert!(result.is_ok());
        let digest = result.unwrap();
        assert_eq!(digest.len(), RTMP_DIGEST_LENGTH);
    }

    #[test]
    fn test_digest_processor_consistent_results() {
        let data = BytesMut::from(&[0u8; RTMP_HANDSHAKE_SIZE][..]);
        let key = BytesMut::from(RTMP_SERVER_KEY_FIRST_HALF.as_bytes());

        let message = vec![42u8; 100];

        let mut processor1 = DigestProcessor::new(data.clone(), key.clone());
        let digest1 = processor1.make_digest(&message).unwrap();

        let mut processor2 = DigestProcessor::new(data, key);
        let digest2 = processor2.make_digest(&message).unwrap();

        assert_eq!(digest1, digest2, "Same input should produce same digest");
    }

    #[test]
    fn test_digest_processor_different_keys_different_results() {
        let data = BytesMut::from(&[0u8; RTMP_HANDSHAKE_SIZE][..]);
        let key1 = BytesMut::from(RTMP_SERVER_KEY_FIRST_HALF.as_bytes());
        let key2 = BytesMut::from(RTMP_CLIENT_KEY_FIRST_HALF.as_bytes());

        let message = vec![1, 2, 3, 4, 5];

        let mut processor1 = DigestProcessor::new(data.clone(), key1);
        let digest1 = processor1.make_digest(&message).unwrap();

        let mut processor2 = DigestProcessor::new(data, key2);
        let digest2 = processor2.make_digest(&message).unwrap();

        assert_ne!(
            digest1, digest2,
            "Different keys should produce different digests"
        );
    }

    #[test]
    fn test_digest_processor_generate_and_fill_digest() {
        // Create data that's large enough for digest offset calculation
        let mut data = BytesMut::with_capacity(RTMP_HANDSHAKE_SIZE);
        data.extend_from_slice(&[0u8; RTMP_HANDSHAKE_SIZE]);
        let key = BytesMut::from(RTMP_SERVER_KEY_FIRST_HALF.as_bytes());
        let mut processor = DigestProcessor::new(data, key);

        let result = processor.generate_and_fill_digest();
        assert!(result.is_ok());
        let filled = result.unwrap();
        assert_eq!(filled.len(), RTMP_HANDSHAKE_SIZE);
    }

    #[test]
    fn test_digest_processor_read_digest_simple_data() {
        // Simple data without valid digest should try both schemas
        let data = BytesMut::from(&[0u8; RTMP_HANDSHAKE_SIZE][..]);
        let key = BytesMut::from(RTMP_CLIENT_KEY_FIRST_HALF.as_bytes());
        let mut processor = DigestProcessor::new(data, key);

        // This should fail because the data doesn't contain a valid digest
        let result = processor.read_digest();
        assert!(
            result.is_err(),
            "Simple data without digest should fail validation"
        );
    }
}

// =============================================================================
// Error Handling Tests
// =============================================================================

mod error_handling_tests {
    use super::*;

    #[test]
    fn test_handshake_error_from_io_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test");
        let handshake_error: HandshakeError = io_error.into();
        assert!(matches!(
            handshake_error.value,
            HandshakeErrorValue::IOError(_)
        ));
    }

    #[test]
    fn test_handshake_error_display() {
        let error = HandshakeError {
            value: HandshakeErrorValue::S0VersionNotCorrect,
        };
        let message = error.to_string();
        assert!(message.contains("s0 version"));
    }

    #[test]
    fn test_digest_error_display() {
        let error = HandshakeError {
            value: HandshakeErrorValue::DigestError(
                synctv_xiu::rtmp::handshake::errors::DigestError {
                    value: DigestErrorValue::UnknowSchema,
                },
            ),
        };
        let message = error.to_string();
        assert!(message.contains("schema") || message.contains("digest"));
    }

    #[test]
    fn test_bytes_read_error_conversion() {
        use synctv_xiu::bytesio::bytes_errors::{BytesReadError, BytesReadErrorValue};

        let bytes_error = BytesReadError {
            value: BytesReadErrorValue::NotEnoughBytes,
        };
        let handshake_error: HandshakeError = bytes_error.into();
        assert!(matches!(
            handshake_error.value,
            HandshakeErrorValue::BytesReadError(_)
        ));
    }
}

// =============================================================================
// Constants and Define Tests
// =============================================================================

mod constants_tests {
    use super::*;

    #[test]
    fn test_rtmp_version_value() {
        assert_eq!(RTMP_VERSION, 3);
    }

    #[test]
    fn test_rtmp_handshake_size_value() {
        assert_eq!(RTMP_HANDSHAKE_SIZE, 1536);
    }

    #[test]
    fn test_rtmp_digest_length_value() {
        assert_eq!(RTMP_DIGEST_LENGTH, 32);
    }

    #[test]
    fn test_server_key_first_half_format() {
        assert!(RTMP_SERVER_KEY_FIRST_HALF.contains("Adobe"));
        assert!(RTMP_SERVER_KEY_FIRST_HALF.contains("Server"));
    }

    #[test]
    fn test_client_key_first_half_format() {
        assert!(RTMP_CLIENT_KEY_FIRST_HALF.contains("Adobe"));
        assert!(RTMP_CLIENT_KEY_FIRST_HALF.contains("Player"));
    }
}

// =============================================================================
// Utils Tests
// =============================================================================

mod utils_tests {
    use super::*;

    #[test]
    fn test_current_time_returns_value() {
        let time = utils::current_time();
        // current_time returns a u32 timestamp
        // Just verify it returns something (not testing exact value)
        let _ = time;
    }
}

// =============================================================================
// State Transition Tests
// =============================================================================

mod state_transition_tests {
    use super::*;

    #[test]
    fn test_client_state_equality() {
        assert_eq!(
            ClientHandshakeState::WriteC0C1,
            ClientHandshakeState::WriteC0C1
        );
        assert_ne!(
            ClientHandshakeState::WriteC0C1,
            ClientHandshakeState::Finish
        );
    }

    #[test]
    fn test_server_state_copy() {
        let state = ServerHandshakeState::ReadC0C1;
        let state_copy = state;
        assert!(matches!(state, ServerHandshakeState::ReadC0C1));
        assert!(matches!(state_copy, ServerHandshakeState::ReadC0C1));
    }

    #[test]
    fn test_client_state_debug() {
        let state = ClientHandshakeState::WriteC0C1;
        let debug_str = format!("{state:?}");
        assert!(debug_str.contains("WriteC0C1"));
    }

    #[test]
    fn test_client_state_sequence() {
        // Verify the expected state sequence for a client
        let states = [
            ClientHandshakeState::WriteC0C1,
            ClientHandshakeState::ReadS0S1S2,
            ClientHandshakeState::WriteC2,
            ClientHandshakeState::Finish,
        ];

        // Verify all states are distinct
        for (i, s1) in states.iter().enumerate() {
            for (j, s2) in states.iter().enumerate() {
                if i != j {
                    assert_ne!(s1, s2, "All client states should be distinct");
                }
            }
        }
    }
}

// =============================================================================
// Integration Tests
// =============================================================================

mod integration_tests {
    use super::*;

    /// Test full client handshake flow with mock server data
    #[tokio::test]
    async fn test_client_handshake_with_mock_server() {
        let io = make_mock_io();
        let mut client = SimpleHandshakeClient::new(io);

        // Phase 1: Write C0/C1
        client.handshake().await.unwrap();
        assert_eq!(client.state, ClientHandshakeState::ReadS0S1S2);

        // Manually feed S0/S1/S2 data to the client's reader
        let c0c1 = build_c0c1();
        let s0s1s2 = build_s0s1s2(&c0c1[1..]);
        client.extend_data(&s0s1s2).unwrap();

        // Phase 2 & 3: Read S0/S1/S2, write C2, and finish
        // The handshake loop continues until finish when there's data available
        client.handshake().await.unwrap();
        assert_eq!(client.state, ClientHandshakeState::Finish);
    }

    /// Test full server handshake flow with mock client data
    #[tokio::test]
    async fn test_server_handshake_with_mock_client() {
        let io = make_mock_io();
        let mut server = SimpleHandshakeServer::new(io);

        // Feed C0+C1
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Phase 1: Read C0C1, write S0S1S2
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::ReadC2));

        // Feed C2
        let c2 = build_c2();
        server.extend_data(&c2).unwrap();

        // Phase 2: Read C2, finish
        server.handshake().await.unwrap();
        assert!(matches!(server.state, ServerHandshakeState::Finish));
    }

    /// Test that HandshakeServer falls back to simple handshake
    #[tokio::test]
    async fn test_handshake_server_fallback_to_simple() {
        let io = make_mock_io();
        let mut server = HandshakeServer::new(io);

        // Feed simple C0+C1 (no valid digest for complex handshake)
        let c0c1 = build_c0c1();
        server.extend_data(&c0c1).unwrap();

        // Complex handshake should fail, fallback to simple
        let result = server.handshake().await;
        assert!(result.is_ok(), "Handshake should succeed after fallback");
    }
}
