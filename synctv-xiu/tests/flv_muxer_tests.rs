//! Tests for FLV muxer header and tag encoding.
//!
//! Verifies correct FLV header flag bytes for different audio/video combinations
//! and tag timestamp encoding.

use synctv_xiu::flv::muxer::FlvMuxer;

#[test]
fn test_flv_header_av() {
    let mut muxer = FlvMuxer::new();
    muxer.write_flv_header(true, true).unwrap();

    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 9);
    // Signature: "FLV"
    assert_eq!(bytes[0], 0x46); // 'F'
    assert_eq!(bytes[1], 0x4C); // 'L'
    assert_eq!(bytes[2], 0x56); // 'V'
    // Version
    assert_eq!(bytes[3], 0x01);
    // Flags: audio+video = 0x05 (bit 2 = audio, bit 0 = video)
    assert_eq!(bytes[4], 0x05);
    // Header size: 9 (big-endian u32)
    assert_eq!(&bytes[5..9], &[0x00, 0x00, 0x00, 0x09]);
}

#[test]
fn test_flv_header_audio_only() {
    let mut muxer = FlvMuxer::new();
    muxer.write_flv_header(true, false).unwrap();

    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 9);
    // Flags: audio only = 0x04
    assert_eq!(bytes[4], 0x04);
}

#[test]
fn test_flv_header_video_only() {
    let mut muxer = FlvMuxer::new();
    muxer.write_flv_header(false, true).unwrap();

    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 9);
    // Flags: video only = 0x01
    assert_eq!(bytes[4], 0x01);
}

#[test]
fn test_flv_header_no_av() {
    let mut muxer = FlvMuxer::new();
    muxer.write_flv_header(false, false).unwrap();

    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 9);
    // Flags: neither audio nor video = 0x00
    assert_eq!(bytes[4], 0x00);
}

#[test]
fn test_flv_tag_timestamp_encoding() {
    let mut muxer = FlvMuxer::new();

    // Write a tag header with a specific timestamp
    // Tag type 0x09 = video, data_size = 100, timestamp = 1000
    muxer.write_flv_tag_header(0x09, 100, 1000).unwrap();

    let bytes = muxer.writer.extract_current_bytes();
    // Tag header is 11 bytes:
    // [0] = tag_type
    // [1..4] = data_size (24-bit big-endian)
    // [4..7] = timestamp low 24 bits (big-endian)
    // [7] = timestamp extended (high 8 bits)
    // [8..11] = stream_id (always 0)
    assert_eq!(bytes.len(), 11);

    assert_eq!(bytes[0], 0x09); // tag type = video

    // data_size = 100 = 0x000064
    assert_eq!(bytes[1], 0x00);
    assert_eq!(bytes[2], 0x00);
    assert_eq!(bytes[3], 0x64);

    // timestamp = 1000 = 0x0003E8
    // Low 24 bits: 0x0003E8
    assert_eq!(bytes[4], 0x00);
    assert_eq!(bytes[5], 0x03);
    assert_eq!(bytes[6], 0xE8);
    // Timestamp extended (bits 24-31): 0
    assert_eq!(bytes[7], 0x00);

    // stream_id = 0
    assert_eq!(&bytes[8..11], &[0x00, 0x00, 0x00]);
}

#[test]
fn test_flv_tag_large_timestamp() {
    let mut muxer = FlvMuxer::new();

    // Timestamp > 24 bits to test extended timestamp
    // 0x01234567 = timestamp with extended byte = 0x01
    let timestamp: u32 = 0x01234567;
    muxer.write_flv_tag_header(0x08, 50, timestamp).unwrap();

    let bytes = muxer.writer.extract_current_bytes();

    // Low 24 bits of timestamp: 0x234567
    assert_eq!(bytes[4], 0x23);
    assert_eq!(bytes[5], 0x45);
    assert_eq!(bytes[6], 0x67);
    // Extended byte (bits 24-31): 0x01
    assert_eq!(bytes[7], 0x01);
}

#[test]
fn test_flv_previous_tag_size() {
    let mut muxer = FlvMuxer::new();

    // Write a previous tag size
    muxer.write_previous_tag_size(0).unwrap();
    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 4);
    assert_eq!(&bytes[..], &[0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn test_flv_previous_tag_size_nonzero() {
    let mut muxer = FlvMuxer::new();

    muxer.write_previous_tag_size(256).unwrap();
    let bytes = muxer.writer.extract_current_bytes();
    assert_eq!(bytes.len(), 4);
    // 256 = 0x00000100
    assert_eq!(&bytes[..], &[0x00, 0x00, 0x01, 0x00]);
}
