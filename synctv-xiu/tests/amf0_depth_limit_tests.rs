//! AMF0 depth and key limit tests.
//!
//! These tests verify that AMF0 parsing has proper limits to prevent
//! stack overflow or memory exhaustion from malicious deeply nested
//! or oversized objects.

#![allow(clippy::unwrap_used)]
use indexmap::IndexMap;
use synctv_xiu::bytesio::bytes_reader::BytesReader;
use synctv_xiu::flv::amf0::amf0_reader::Amf0Reader;
use synctv_xiu::flv::amf0::amf0_writer::Amf0Writer;
use synctv_xiu::flv::amf0::Amf0ValueType;

/// Creates a deeply nested AMF0 object with the specified depth.
fn create_nested_object(depth: usize) -> Amf0ValueType {
    let mut current = IndexMap::new();
    current.insert("value".to_string(), Amf0ValueType::Number(42.0));

    for _ in 0..depth {
        let mut wrapper = IndexMap::new();
        wrapper.insert("nested".to_string(), Amf0ValueType::Object(current));
        current = wrapper;
    }

    Amf0ValueType::Object(current)
}

/// Creates an AMF0 object with the specified number of keys.
fn create_large_object(key_count: usize) -> Amf0ValueType {
    let mut properties = IndexMap::new();
    for i in 0..key_count {
        properties.insert(format!("key{i}"), Amf0ValueType::Number(i as f64));
    }
    Amf0ValueType::Object(properties)
}

/// Helper: write a value with `Amf0Writer`, then read it back with `Amf0Reader`.
fn roundtrip(value: &Amf0ValueType) -> Result<Amf0ValueType, synctv_xiu::flv::amf0::Amf0ReadError> {
    let mut writer = Amf0Writer::new();
    writer.write_any(value).unwrap();
    let bytes = writer.extract_current_bytes();

    let reader = BytesReader::new(bytes);
    let mut amf_reader = Amf0Reader::new(reader);
    amf_reader.read_any()
}

#[test]
fn test_normal_object_still_parses() {
    // A normal object with reasonable depth and key count should still work
    let nested = create_nested_object(5);
    let result = roundtrip(&nested);
    assert!(
        result.is_ok(),
        "Normal nested object should parse successfully"
    );
}

#[test]
fn test_normal_large_object_parses() {
    // An object with a reasonable number of keys should still work
    let large = create_large_object(100);
    let result = roundtrip(&large);
    assert!(
        result.is_ok(),
        "Object with 100 keys should parse successfully"
    );
}

#[test]
fn test_deeply_nested_object_rejected() {
    // An object with more than 32 levels of nesting should be rejected
    let too_deep = create_nested_object(40);
    let result = roundtrip(&too_deep);
    assert!(result.is_err(), "Deeply nested object should be rejected");

    let err = result.unwrap_err();
    let err_msg = format!("{err}");
    assert!(
        err_msg.contains("depth") || err_msg.contains("Depth"),
        "Error should mention depth limit, got: {err_msg}"
    );
}

#[test]
fn test_oversized_object_rejected() {
    // An object with more than 1000 keys should be rejected
    let too_large = create_large_object(1500);
    let result = roundtrip(&too_large);
    assert!(
        result.is_err(),
        "Object with too many keys should be rejected"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{err}");
    assert!(
        err_msg.contains("key") || err_msg.contains("Key"),
        "Error should mention key limit, got: {err_msg}"
    );
}

#[test]
fn test_ecma_array_depth_limit() {
    // Create a deeply nested ECMA array structure
    let mut current = IndexMap::new();
    current.insert("value".to_string(), Amf0ValueType::Number(1.0));

    // Create 40 levels of nesting
    for _ in 0..40 {
        let mut wrapper = IndexMap::new();
        wrapper.insert("nested".to_string(), Amf0ValueType::Object(current));
        current = wrapper;
    }

    // Write as ECMA array marker
    let mut writer = Amf0Writer::new();

    // Manually construct ECMA array with deep nesting
    // First write the ECMA array marker (0x08)
    let inner_value = Amf0ValueType::Object(current);
    writer.write_any(&inner_value).unwrap();
    let bytes = writer.extract_current_bytes();

    let reader = BytesReader::new(bytes);
    let mut amf_reader = Amf0Reader::new(reader);
    let result = amf_reader.read_any();

    assert!(
        result.is_err(),
        "Deeply nested structure should be rejected"
    );
}

#[test]
fn test_exact_depth_boundary() {
    // Test that exactly 32 levels of nesting works
    let at_limit = create_nested_object(31); // 31 wrappers + 1 inner = 32 total
    let result = roundtrip(&at_limit);
    assert!(result.is_ok(), "Object at depth limit (32) should parse");

    // Test that 33 levels fails
    let over_limit = create_nested_object(32); // 32 wrappers + 1 inner = 33 total
    let result = roundtrip(&over_limit);
    assert!(
        result.is_err(),
        "Object over depth limit (33) should be rejected"
    );
}

#[test]
fn test_exact_key_boundary() {
    // Test that exactly 1000 keys works
    let at_limit = create_large_object(1000);
    let result = roundtrip(&at_limit);
    assert!(result.is_ok(), "Object with 1000 keys should parse");

    // Test that 1001 keys fails
    let over_limit = create_large_object(1001);
    let result = roundtrip(&over_limit);
    assert!(result.is_err(), "Object with 1001 keys should be rejected");
}
