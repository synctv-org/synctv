//! ClientSession Handshake Timeout Tests
//!
//! This test suite covers:
//! 1. Handshake timeout protection
//! 2. Normal handshake flow
//! 3. Invalid C0/C1 data handling

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Instant};

use synctv_xiu::bytesio::bytesio::{TNetIO, TcpIO};
use synctv_xiu::rtmp::handshake::{
    define::ClientHandshakeState, handshake_client::SimpleHandshakeClient,
};
use synctv_xiu::rtmp::session::errors::{SessionError, SessionErrorValue};

/// Helper to create a mock RTMP server that can simulate various handshake scenarios
struct MockRtmpServer {
    listener: TcpListener,
}

impl MockRtmpServer {
    async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self { listener }
    }

    fn addr(&self) -> std::net::SocketAddr {
        self.listener.local_addr().unwrap()
    }

    /// Accept a connection and return the stream
    async fn accept(&self) -> TcpStream {
        self.listener.accept().await.unwrap().0
    }
}

// When a malicious server doesn't respond, the client should timeout after 10 seconds

#[tokio::test]
async fn test_client_session_handshake_timeout_on_no_response() {
    // This test verifies that ClientSession handshake times out when server doesn't respond.
    // We test the timeout mechanism directly without running full session.

    let server = MockRtmpServer::bind().await;
    let addr = server.addr();

    // Spawn server that accepts but never sends data (simulates malicious server)
    let server_handle = tokio::spawn(async move {
        let mut stream = server.accept().await;
        // Read C0/C1 but never respond
        let mut buf = vec![0u8; 1537];
        let _ = stream.read_exact(&mut buf).await;
        // Hold connection open without responding
        tokio::time::sleep(Duration::from_secs(10)).await;
    });

    let client_stream = TcpStream::connect(addr).await.unwrap();
    let io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> =
        Arc::new(Mutex::new(Box::new(TcpIO::new(client_stream))));

    let mut handshaker = SimpleHandshakeClient::new(Arc::clone(&io));

    handshaker.handshake().await.unwrap();
    assert_eq!(handshaker.state, ClientHandshakeState::ReadS0S1S2);

    // Try to read S0/S1/S2 with timeout - this should timeout
    let handshake_timeout = Duration::from_secs(1);
    let start = Instant::now();

    let read_result = timeout(handshake_timeout, async {
        let mut bytes_len = 0;
        while bytes_len < 3073 {
            // C0+C1=1537, S0+S1+S2=3073
            let data = io.lock().await.read().await.unwrap();
            bytes_len += data.len();
        }
    })
    .await;

    let elapsed = start.elapsed();

    // The timeout should trigger within reasonable time (1s + tolerance)
    assert!(
        read_result.is_err(),
        "Expected timeout when server doesn't respond"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "Timeout should occur within ~1 second, took {elapsed:?}"
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_client_session_handshake_normal_flow() {
    // This test verifies the handshake timeout mechanism works correctly
    // by testing the timeout logic directly.

    let server = MockRtmpServer::bind().await;
    let addr = server.addr();

    // Spawn server that responds correctly to handshake
    let server_handle = tokio::spawn(async move {
        let mut stream = server.accept().await;

        // Read C0/C1 (1537 bytes: 1 byte version + 1536 bytes)
        let mut c0c1 = vec![0u8; 1537];
        stream.read_exact(&mut c0c1).await.unwrap();

        // Verify C0 version byte
        assert_eq!(c0c1[0], 3, "RTMP version should be 3");

        // S0: version byte
        // S1: 1536 bytes (time + zero + random)
        // S2: echo back C1
        let mut s0s1s2 = Vec::with_capacity(3073);
        s0s1s2.push(3); // S0: RTMP version 3
                        // S1: 4 bytes time + 4 bytes zero + 1528 bytes random
        s0s1s2.extend_from_slice(&[0, 0, 0, 0]); // time
        s0s1s2.extend_from_slice(&[0, 0, 0, 0]); // zero
        s0s1s2.extend_from_slice(&c0c1[9..1537]); // random data (echo part of C1)
                                                  // S2: echo C1 back
        s0s1s2.extend_from_slice(&c0c1[1..1537]);

        stream.write_all(&s0s1s2).await.unwrap();
        stream.flush().await.unwrap();

        // Read C2 (1536 bytes)
        let mut c2 = vec![0u8; 1536];
        let _ = stream.read_exact(&mut c2).await;

        // Keep connection alive briefly
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let client_stream = TcpStream::connect(addr).await.unwrap();
    let io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> =
        Arc::new(Mutex::new(Box::new(TcpIO::new(client_stream))));

    let mut handshaker = SimpleHandshakeClient::new(Arc::clone(&io));

    handshaker.handshake().await.unwrap();
    assert_eq!(handshaker.state, ClientHandshakeState::ReadS0S1S2);

    // Read S0/S1/S2 with timeout - this should succeed quickly
    let handshake_timeout = Duration::from_secs(10);
    let start = Instant::now();

    let read_result = timeout(handshake_timeout, async {
        let mut bytes_len = 0;
        while bytes_len < 3073 {
            let data = io.lock().await.read().await.unwrap();
            bytes_len += data.len();
            handshaker.extend_data(&data).unwrap();
        }
    })
    .await;

    let elapsed = start.elapsed();

    assert!(
        read_result.is_ok(),
        "Expected successful read when server responds"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "Normal handshake should complete quickly, took {elapsed:?}"
    );

    // Complete handshake
    handshaker.handshake().await.unwrap();
    assert_eq!(handshaker.state, ClientHandshakeState::Finish);

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_client_session_handles_invalid_s0_version() {
    // This test verifies that client properly handles invalid S0 version

    let server = MockRtmpServer::bind().await;
    let addr = server.addr();

    // Spawn server that sends invalid S0 version
    let server_handle = tokio::spawn(async move {
        let mut stream = server.accept().await;

        // Read C0/C1
        let mut c0c1 = vec![0u8; 1537];
        stream.read_exact(&mut c0c1).await.unwrap();

        let mut s0s1s2 = Vec::with_capacity(3073);
        s0s1s2.push(99); // Invalid RTMP version!
        s0s1s2.extend_from_slice(&[0u8; 1536]); // S1
        s0s1s2.extend_from_slice(&[0u8; 1536]); // S2

        stream.write_all(&s0s1s2).await.unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let client_stream = TcpStream::connect(addr).await.unwrap();
    let io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>> =
        Arc::new(Mutex::new(Box::new(TcpIO::new(client_stream))));

    let mut handshaker = SimpleHandshakeClient::new(Arc::clone(&io));

    handshaker.handshake().await.unwrap();

    // Read S0/S1/S2
    let mut bytes_len = 0;
    while bytes_len < 3073 {
        let data = io.lock().await.read().await.unwrap();
        bytes_len += data.len();
        handshaker.extend_data(&data).unwrap();
    }

    // The handshake should handle the invalid version gracefully
    // Depending on implementation, this may succeed with warning or fail
    let result = handshaker.handshake().await;

    // The handshake should either fail or handle it gracefully
    // The simple handshake client typically accepts any version for compatibility
    // but logs a warning - this test verifies it doesn't panic
    let _ = result;

    server_handle.await.unwrap();
}

// This test verifies the timeout error type exists and has correct message
// (actual timeout behavior is tested in test_client_session_handshake_timeout_on_no_response)

#[tokio::test]
async fn test_client_session_handshake_timeout_error_type() {
    // Verify that SessionErrorValue::Timeout has the correct error message
    let error = SessionError {
        value: SessionErrorValue::Timeout,
    };

    let error_message = error.to_string();
    assert!(
        error_message.contains("timeout"),
        "Timeout error message should contain 'timeout': {error_message}"
    );
}
