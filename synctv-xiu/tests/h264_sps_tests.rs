//! H264 SPS parser tests.
//!
//! Tests that truncated SPS data returns an error instead of panicking.

#![allow(clippy::unwrap_used)]
use synctv_xiu::h264::sps::SpsParser;
use synctv_xiu::bytesio::bytes_reader::BytesReader;
use bytes::BytesMut;

#[test]
fn test_sps_truncated_data_returns_error() {
    // Create a BytesReader with only 1 byte (valid SPS requires more)
    let mut data = BytesMut::new();
    data.extend_from_slice(&[0x42]); // Just one byte

    let reader = BytesReader::new(data);
    let mut parser = SpsParser::new(reader);

    // Parsing truncated data should return an error, not panic
    let result = parser.parse();
    assert!(result.is_err(), "Truncated SPS data should return an error");
}

#[test]
fn test_sps_empty_data_returns_error() {
    let data = BytesMut::new();
    let reader = BytesReader::new(data);
    let mut parser = SpsParser::new(reader);

    let result = parser.parse();
    assert!(result.is_err(), "Empty SPS data should return an error");
}

#[test]
fn test_sps_two_bytes_returns_error() {
    let mut data = BytesMut::new();
    data.extend_from_slice(&[0x42, 0x00]); // profile_idc + flags, but no level_idc

    let reader = BytesReader::new(data);
    let mut parser = SpsParser::new(reader);

    let result = parser.parse();
    assert!(result.is_err(), "Two-byte SPS data should return an error");
}
