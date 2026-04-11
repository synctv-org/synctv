//! Metadata is_metadata() tests.
//!
//! These tests verify that the MetaData::is_metadata() function correctly
//! identifies RTMP metadata packets according to the RTMP specification.

#![allow(clippy::unwrap_used)]
use bytes::BytesMut;
use synctv_xiu::flv::amf0::amf0_writer::Amf0Writer;
use synctv_xiu::flv::amf0::Amf0ValueType;
use synctv_xiu::rtmp::cache::metadata::MetaData;

/// Helper to create metadata bytes from AMF0 values
fn create_metadata_bytes(values: &[Amf0ValueType]) -> BytesMut {
    let mut writer = Amf0Writer::new();
    let values = values.to_vec();
    writer.write_anys(&values).unwrap();
    writer.extract_current_bytes()
}

#[test]
fn test_is_metadata_with_set_data_frame_and_on_metadata() {
    // Standard RTMP metadata format: @setDataFrame followed by onMetaData
    let values = vec![
        Amf0ValueType::UTF8String("@setDataFrame".to_string()),
        Amf0ValueType::UTF8String("onMetaData".to_string()),
    ];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        metadata.is_metadata(&bytes),
        "Should accept @setDataFrame + onMetaData"
    );
}

#[test]
fn test_is_metadata_with_only_on_metadata() {
    // Alternative format: just onMetaData
    let values = vec![Amf0ValueType::UTF8String("onMetaData".to_string())];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        metadata.is_metadata(&bytes),
        "Should accept onMetaData alone"
    );
}

#[test]
fn test_is_metadata_rejects_only_set_data_frame() {
    // @setDataFrame alone is NOT valid metadata (needs onMetaData after it)
    let values = vec![Amf0ValueType::UTF8String("@setDataFrame".to_string())];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        !metadata.is_metadata(&bytes),
        "Should reject @setDataFrame alone"
    );
}

#[test]
fn test_is_metadata_rejects_set_data_frame_with_wrong_second_value() {
    // @setDataFrame followed by something other than onMetaData is invalid
    let values = vec![
        Amf0ValueType::UTF8String("@setDataFrame".to_string()),
        Amf0ValueType::UTF8String("somethingElse".to_string()),
    ];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        !metadata.is_metadata(&bytes),
        "Should reject @setDataFrame + wrong second value"
    );
}

#[test]
fn test_is_metadata_rejects_random_string() {
    // Random strings should be rejected
    let values = vec![Amf0ValueType::UTF8String("randomString".to_string())];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(!metadata.is_metadata(&bytes), "Should reject random string");
}

#[test]
fn test_is_metadata_rejects_empty() {
    // Empty data should be rejected
    let bytes = BytesMut::new();
    let mut metadata = MetaData::new();
    assert!(!metadata.is_metadata(&bytes), "Should reject empty data");
}

#[test]
fn test_is_metadata_with_additional_data() {
    // Metadata with additional data after the header should still be accepted
    use indexmap::IndexMap;
    let mut props = IndexMap::new();
    props.insert("duration".to_string(), Amf0ValueType::Number(0.0));
    props.insert("width".to_string(), Amf0ValueType::Number(1920.0));
    props.insert("height".to_string(), Amf0ValueType::Number(1080.0));

    let values = vec![
        Amf0ValueType::UTF8String("onMetaData".to_string()),
        Amf0ValueType::Object(props),
    ];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        metadata.is_metadata(&bytes),
        "Should accept onMetaData with properties object"
    );
}

#[test]
fn test_is_metadata_with_set_data_frame_and_properties() {
    // @setDataFrame + onMetaData + properties should be accepted
    use indexmap::IndexMap;
    let mut props = IndexMap::new();
    props.insert("duration".to_string(), Amf0ValueType::Number(120.5));

    let values = vec![
        Amf0ValueType::UTF8String("@setDataFrame".to_string()),
        Amf0ValueType::UTF8String("onMetaData".to_string()),
        Amf0ValueType::Object(props),
    ];
    let bytes = create_metadata_bytes(&values);
    let mut metadata = MetaData::new();
    assert!(
        metadata.is_metadata(&bytes),
        "Should accept @setDataFrame + onMetaData + properties"
    );
}
