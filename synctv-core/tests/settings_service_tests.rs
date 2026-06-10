//! Settings service tests
//!
//! Tests settings cache behavior, default values, and model operations.
//!

use synctv_core::models::settings::{get_default_settings, SettingsGroup};
use synctv_core_testing::ok;

#[test]
fn test_unknown_group_returns_none() {
    assert!(get_default_settings("nonexistent").is_none());
    assert!(get_default_settings("").is_none());
}

// SettingsGroup model tests

#[test]
fn test_settings_group_parse_json() {
    let group = SettingsGroup::new(
        "test".to_string(),
        serde_json::json!({"key": "value", "count": 42}).to_string(),
    );
    let parsed = ok(group.parse_json(), "settings group JSON should parse");
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
    let obj = ok(group.as_object(), "settings group should parse as object");
    assert_eq!(obj.len(), 2);
    assert!(obj.contains_key("a"));
    assert!(obj.contains_key("b"));
}

#[test]
fn test_settings_group_as_object_not_object() {
    let group = SettingsGroup::new("test".to_string(), serde_json::json!([1, 2, 3]).to_string());
    assert!(group.as_object().is_err());
}
