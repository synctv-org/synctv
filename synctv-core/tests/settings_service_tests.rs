//! Settings service tests
//!
//! Tests settings cache behavior, default values, and model operations.
//!
//! Run with: cargo test --test `settings_service_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::models::settings::{get_default_settings, SettingsGroup};
use synctv_core::service::settings::get_default_settings_json;

#[test]
fn test_unknown_group_returns_none() {
    assert!(get_default_settings("nonexistent").is_none());
    assert!(get_default_settings_json("").is_none());
}

// ============================================================================
// SettingsGroup model tests
// ============================================================================

#[test]
fn test_settings_group_parse_json() {
    let group = SettingsGroup::new(
        "test".to_string(),
        serde_json::json!({"key": "value", "count": 42}).to_string(),
    );
    let parsed = group.parse_json().unwrap();
    assert_eq!(parsed["key"], "value");
    assert_eq!(parsed["count"], 42);
}

#[test]
fn test_settings_group_parse_invalid_json() {
    let group = SettingsGroup::new("test".to_string(), "not valid json".to_string());
    assert!(group.parse_json().is_err());
}

#[test]
fn test_settings_group_as_object() {
    let group = SettingsGroup::new(
        "test".to_string(),
        serde_json::json!({"a": 1, "b": "two"}).to_string(),
    );
    let obj = group.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert!(obj.contains_key("a"));
    assert!(obj.contains_key("b"));
}

#[test]
fn test_settings_group_as_object_not_object() {
    let group = SettingsGroup::new("test".to_string(), serde_json::json!([1, 2, 3]).to_string());
    assert!(group.as_object().is_err());
}

#[test]
fn test_settings_group_serialization_round_trip() {
    let group = SettingsGroup::new(
        "server".to_string(),
        serde_json::json!({"test": true}).to_string(),
    );

    let json = serde_json::to_string(&group).unwrap();
    let deserialized: SettingsGroup = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.group_name, group.group_name);
    assert_eq!(deserialized.key, group.key);
    assert_eq!(deserialized.value, group.value);
}
