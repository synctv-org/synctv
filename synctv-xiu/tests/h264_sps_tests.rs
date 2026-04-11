//! H264 SPS parser tests.
//!
//! Tests that truncated SPS data returns an error instead of panicking.

#![allow(clippy::unwrap_used)]
use bytes::BytesMut;
use synctv_xiu::bytesio::bytes_reader::BytesReader;
use synctv_xiu::h264::sps::SpsParser;

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

#[test]
fn test_sps_invalid_chroma_format_idc_returns_error() {
    // Construct SPS data with an invalid chroma_format_idc value (> 3)
    // profile_idc = 100 (0x64) triggers the high profile parsing path
    // We need to craft Exp-Golomb encoded value for chroma_format_idc = 4
    // Exp-Golomb encoding: codeNum 4 = 00101 (binary)
    // But first we need to get past: profile_idc(8) + flag(8) + level_idc(8) + seq_parameter_set_id(ue)
    // seq_parameter_set_id = 0 encoded as 1 (single bit)

    let mut data = BytesMut::new();
    // profile_idc = 100 (High Profile)
    data.extend_from_slice(&[0x64]);
    // flag = 0x00
    data.extend_from_slice(&[0x00]);
    // level_idc = 0x1F
    data.extend_from_slice(&[0x1F]);
    // seq_parameter_set_id = 0 (ue(v) = 1)
    // chroma_format_idc = 4 (ue(v) = 00101)
    // Combined bits: 1 00101 = 100101 = 0x25 as a byte
    data.extend_from_slice(&[0b1001_0100]); // 1 for sps_id=0, then 00101 for chroma_format_idc=4

    let reader = BytesReader::new(data);
    let mut parser = SpsParser::new(reader);

    let result = parser.parse();
    assert!(
        result.is_err(),
        "Invalid chroma_format_idc should return an error"
    );
}
