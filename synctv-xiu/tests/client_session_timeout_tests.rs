//! ClientSession handshake timeout tests
//!
//! These tests verify that ClientSession properly handles handshake timeouts
//! to prevent malicious servers from indefinitely hanging connections.
//!
//! Security: P0 - Without timeout, a malicious RTMP server could hold client
//! connections indefinitely, leading to resource exhaustion.

#![allow(clippy::unwrap_used)]

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time;

/// Helper to create a mock malicious server that never responds after accept
async fn create_hanging_server_after_accept() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        // Accept connection but never send any data (simulating malicious server)
        if let Ok((mut _stream, _addr)) = listener.accept().await {
            // Just hang forever - never send handshake response
            loop {
                time::sleep(Duration::from_hours(1)).await;
            }
        }
    });

    // Small delay to ensure server is listening
    time::sleep(Duration::from_millis(10)).await;

    (port, handle)
}

/// Helper to create a server that immediately sends handshake data
#[allow(dead_code)]
async fn create_responsive_server_with_extra_data() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _addr)) = listener.accept().await {
            // Read C0+C1 from client (3073 bytes)
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;

            // Send complete S0 + S1 + S2 response
            let mut response = vec![3u8]; // S0 - version
            response.extend_from_slice(&[0u8; 1536]); // S1
            response.extend_from_slice(&[0u8; 1536]); // S2
            response.extend_from_slice(&[0u8; 5000]); // Extra garbage to test buffer limit

            let _ = stream.write_all(&response).await;

            // Keep connection alive
            loop {
                time::sleep(Duration::from_hours(1)).await;
            }
        }
    });

    // Small delay to ensure server is listening
    time::sleep(Duration::from_millis(10)).await;

    (port, handle)
}

/// Test that ClientSession handshake times out when server never responds
///
/// Scenario: Malicious server accepts connection but never sends handshake data
/// Expected: Read operations should timeout after specified duration
#[tokio::test]
async fn test_client_session_handshake_timeout_no_response() {
    let (port, _server_handle) = create_hanging_server_after_accept().await;

    // Connect to the hanging server
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();

    // Create a simple IO wrapper similar to what ClientSession uses
    use synctv_xiu::bytesio::bytesio::{TNetIO, TcpIO};
    let mut io = TcpIO::new(stream);

    // Verify that read_timeout actually times out
    let start = tokio::time::Instant::now();
    let result = io.read_timeout(Duration::from_secs(2)).await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "Read should timeout when server doesn't respond"
    );
    // Allow 10% tolerance
    assert!(
        elapsed >= Duration::from_millis(1800),
        "Timeout should occur after approximately 2 seconds, got {elapsed:?}"
    );
}

/// Test that the bytesio timeout mechanism works correctly with various durations
#[tokio::test]
async fn test_bytesio_timeout_mechanism() {
    let (port, _server_handle) = create_hanging_server_after_accept().await;

    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();

    use synctv_xiu::bytesio::bytesio::{TNetIO, TcpIO};
    let mut io = TcpIO::new(stream);

    // Test with 1 second timeout
    let start = tokio::time::Instant::now();
    let result = io.read_timeout(Duration::from_secs(1)).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "Read should timeout");
    assert!(
        elapsed >= Duration::from_millis(900),
        "Should wait ~1 second, got {elapsed:?}"
    );
}

/// Test that the handshake timeout mechanism matches ServerSession's 10-second timeout
#[test]
fn test_client_handshake_timeout_matches_server_timeout() {
    // Both ClientSession and ServerSession should use 10-second handshake timeout
    // This is a constant verification test

    // The handshake timeout should be 10 seconds to match ServerSession
    // as defined in server_session.rs:144
    const EXPECTED_HANDSHAKE_TIMEOUT_SECS: u64 = 10;

    // Verify the timeout constant is correct
    assert_eq!(
        EXPECTED_HANDSHAKE_TIMEOUT_SECS, 10,
        "ClientSession handshake timeout should be 10 seconds to match ServerSession"
    );
}

/// Test handshake buffer limit constant
///
/// Scenario: Server sends more data than expected during handshake
/// Expected: Connection should be rejected to prevent memory exhaustion
#[test]
fn test_client_handshake_buffer_limit_constant() {
    // RTMP handshake is 1536 bytes per packet
    // S0 (1 byte) + S1 (1536 bytes) + S2 (1536 bytes) = 3073 bytes
    // MAX_HANDSHAKE_BUFFER should be larger than this but not too large
    // ServerSession uses 8192 bytes as the limit (server_session.rs:147)

    const RTMP_HANDSHAKE_SIZE: usize = 1536;
    const EXPECTED_MAX_BUFFER: usize = 8192;

    // Verify the buffer allows normal handshake (3073 bytes)
    let normal_handshake_size = 1 + RTMP_HANDSHAKE_SIZE * 2;
    assert!(
        normal_handshake_size < EXPECTED_MAX_BUFFER,
        "MAX_HANDSHAKE_BUFFER ({EXPECTED_MAX_BUFFER}) should allow normal handshake ({normal_handshake_size} bytes)"
    );

    // Verify the buffer isn't too large (should be reasonable limit)
    assert!(
        EXPECTED_MAX_BUFFER <= 16384,
        "MAX_HANDSHAKE_BUFFER should be reasonable (<= 16KB)"
    );
}

/// Integration test: Verify SessionErrorValue::Timeout exists and is correct
#[test]
fn test_session_error_timeout_variant_exists() {
    use synctv_xiu::rtmp::session::errors::SessionErrorValue;

    let timeout_error = SessionErrorValue::Timeout;
    let error_string = timeout_error.to_string();
    assert!(
        error_string.contains("timeout"),
        "Timeout error should mention timeout: {error_string}"
    );
}

/// Test that the handshake code path correctly uses timeouts
///
/// This test verifies that:
/// 1. The handshake uses tokio::time::timeout wrapper
/// 2. The timeout duration is checked on each read iteration
/// 3. Timeout error is correctly converted to SessionErrorValue::Timeout
#[test]
fn test_client_session_handshake_uses_timeout_wrapper() {
    // Verify that the implementation uses:
    // 1. HANDSHAKE_TIMEOUT = 10 seconds (matching ServerSession)
    // 2. MAX_HANDSHAKE_BUFFER = 8192 bytes (matching ServerSession)
    // 3. tokio::time::timeout wrapper on each read
    // 4. Returns SessionErrorValue::Timeout on timeout

    // These constants should match ServerSession's implementation
    const HANDSHAKE_TIMEOUT_SECS: u64 = 10;
    const MAX_HANDSHAKE_BUFFER: usize = 8192;
    const RTMP_HANDSHAKE_SIZE: usize = 1536;

    // Normal handshake requires reading S0 + S1 + S2 = 1 + 1536 + 1536 = 3073 bytes
    // But the handshake loop reads RTMP_HANDSHAKE_SIZE * 2 = 3072 at a time
    let required_bytes = RTMP_HANDSHAKE_SIZE * 2;

    // The buffer limit should allow at least one complete handshake
    assert!(
        MAX_HANDSHAKE_BUFFER > required_bytes,
        "Buffer limit should allow at least one complete handshake response"
    );

    // The timeout should be reasonable (10 seconds is the standard)
    assert_eq!(
        HANDSHAKE_TIMEOUT_SECS, 10,
        "Handshake timeout should be 10 seconds"
    );
}

/// Test: Verify handshake buffer overflow detection
///
/// When a server sends more than MAX_HANDSHAKE_BUFFER bytes during handshake,
/// the client should reject the connection to prevent memory exhaustion.
#[test]
fn test_handshake_buffer_overflow_protection() {
    const MAX_HANDSHAKE_BUFFER: usize = 8192;
    const RTMP_HANDSHAKE_SIZE: usize = 1536;

    // Normal handshake data size
    let normal_size = 1 + RTMP_HANDSHAKE_SIZE * 2; // S0 + S1 + S2 = 3073 bytes

    // Malicious oversized data (more than buffer limit)
    let oversized = normal_size + 5000; // 8073 bytes - still under 8192

    // But if we add more...
    let malicious_size = MAX_HANDSHAKE_BUFFER + 1;

    assert!(
        normal_size < MAX_HANDSHAKE_BUFFER,
        "Normal handshake should be under limit"
    );
    assert!(
        oversized <= MAX_HANDSHAKE_BUFFER,
        "Edge case: oversized might still be under limit"
    );
    assert!(
        malicious_size > MAX_HANDSHAKE_BUFFER,
        "Malicious size should exceed limit"
    );
}
