//! AMF0 serialization round-trip tests.
//!
//! These tests verify that AMF0 values can be written and read back correctly
//! through the Amf0Writer and Amf0Reader.

use synctv_xiu::flv::amf0::amf0_writer::Amf0Writer;
use synctv_xiu::flv::amf0::amf0_reader::Amf0Reader;
use synctv_xiu::flv::amf0::Amf0ValueType;
use synctv_xiu::bytesio::bytes_reader::BytesReader;

/// Helper: write a value with Amf0Writer, then read it back with Amf0Reader.
fn roundtrip(value: &Amf0ValueType) -> Amf0ValueType {
    let mut writer = Amf0Writer::new();
    writer.write_any(value).unwrap();
    let bytes = writer.extract_current_bytes();

    let reader = BytesReader::new(bytes);
    let mut amf_reader = Amf0Reader::new(reader);
    amf_reader.read_any().unwrap()
}

#[test]
fn test_amf0_number_roundtrip() {
    let original = Amf0ValueType::Number(42.5);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_number_zero() {
    let original = Amf0ValueType::Number(0.0);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_number_negative() {
    let original = Amf0ValueType::Number(-123.456);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_number_large() {
    let original = Amf0ValueType::Number(f64::MAX);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_string_roundtrip() {
    let original = Amf0ValueType::UTF8String("hello world".to_string());
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_string_empty() {
    let original = Amf0ValueType::UTF8String(String::new());
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_string_unicode() {
    let original = Amf0ValueType::UTF8String("hello ".to_string());
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_boolean_true_roundtrip() {
    let original = Amf0ValueType::Boolean(true);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_boolean_false_roundtrip() {
    let original = Amf0ValueType::Boolean(false);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_null_roundtrip() {
    let original = Amf0ValueType::Null;
    let result = roundtrip(&original);
    assert_eq!(result, original);
}

#[test]
fn test_amf0_multiple_values_roundtrip() {
    let values = vec![
        Amf0ValueType::UTF8String("connect".to_string()),
        Amf0ValueType::Number(1.0),
        Amf0ValueType::Null,
        Amf0ValueType::Boolean(true),
    ];

    let mut writer = Amf0Writer::new();
    writer.write_anys(&values).unwrap();
    let bytes = writer.extract_current_bytes();

    let reader = BytesReader::new(bytes);
    let mut amf_reader = Amf0Reader::new(reader);
    let result = amf_reader.read_all().unwrap();

    assert_eq!(result.len(), values.len());
    for (original, read_back) in values.iter().zip(result.iter()) {
        assert_eq!(original, read_back);
    }
}

#[test]
fn test_amf0_object_roundtrip() {
    use indexmap::IndexMap;

    let mut properties = IndexMap::new();
    properties.insert("app".to_string(), Amf0ValueType::UTF8String("live".to_string()));
    properties.insert("type".to_string(), Amf0ValueType::UTF8String("nonprivate".to_string()));
    properties.insert("fpad".to_string(), Amf0ValueType::Boolean(false));
    properties.insert("capabilities".to_string(), Amf0ValueType::Number(15.0));

    let original = Amf0ValueType::Object(properties);
    let result = roundtrip(&original);
    assert_eq!(result, original);
}
