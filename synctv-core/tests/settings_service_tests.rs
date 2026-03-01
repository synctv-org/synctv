//! Settings service tests
//!
//! Tests settings cache behavior, default values, and model operations.
//!
//! Run with: cargo test --test settings_service_tests
#![allow(clippy::unwrap_used)]

use synctv_core::models::settings::{
    SettingsGroup, get_default_settings, default_server_settings,
    default_email_settings, default_oauth_settings,
};
use synctv_core::service::settings::get_default_settings_json;

// ============================================================================
// Settings cache behavior tests (using DashMap-based SettingsService)
// ============================================================================

// Note: SettingsService requires a PgPool and SettingsRepository. For unit tests
// we verify the cache behavior through the model layer and defaults.

#[test]
fn test_settings_get_cached_value() {
    // Simulate cache behavior: SettingsService uses DashMap for lock-free reads.
    // Here we verify the model correctly stores and retrieves values.
    let group = SettingsGroup::new(
        "server".to_string(),
        serde_json::json!({"allow_registration": true}).to_string(),
    );

    // Verify the value is correctly stored and accessible
    let parsed = group.parse_json().unwrap();
    assert_eq!(parsed["allow_registration"], true);

    // Access the same value multiple times (simulates cache hits)
    for _ in 0..10 {
        let parsed = group.parse_json().unwrap();
        assert_eq!(parsed["allow_registration"], true);
    }
}

#[test]
fn test_settings_set_updates_cache() {
    // When a setting is updated, the new value should be reflected
    use std::sync::Arc;
    use dashmap::DashMap;

    let cache: Arc<DashMap<String, SettingsGroup>> = Arc::new(DashMap::new());

    // Set initial value
    let initial = SettingsGroup::new(
        "server".to_string(),
        serde_json::json!({"max_rooms_per_user": 10}).to_string(),
    );
    cache.insert(initial.key.clone(), initial);

    // Read the initial value
    let value = cache.get("server.default").unwrap().value().clone();
    let parsed = value.parse_json().unwrap();
    assert_eq!(parsed["max_rooms_per_user"], 10);

    // Update the value (simulates SettingsService::update)
    let updated = SettingsGroup::new(
        "server".to_string(),
        serde_json::json!({"max_rooms_per_user": 20}).to_string(),
    );
    cache.insert(updated.key.clone(), updated);

    // Read the updated value
    let value = cache.get("server.default").unwrap().value().clone();
    let parsed = value.parse_json().unwrap();
    assert_eq!(parsed["max_rooms_per_user"], 20, "Cache should reflect updated value");
}

// ============================================================================
// Default settings tests
// ============================================================================

#[test]
fn test_server_defaults() {
    let settings = default_server_settings();
    assert_eq!(settings["allow_registration"], true);
    assert_eq!(settings["allow_room_creation"], true);
    assert_eq!(settings["max_rooms_per_user"], 10);
    assert_eq!(settings["max_members_per_room"], 100);
}

#[test]
fn test_email_defaults() {
    let settings = default_email_settings();
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["smtp_port"], 587);
    assert_eq!(settings["use_tls"], true);
}

#[test]
fn test_oauth_defaults() {
    let settings = default_oauth_settings();
    assert_eq!(settings["github_enabled"], false);
    assert_eq!(settings["google_enabled"], false);
}

#[test]
fn test_rate_limit_defaults() {
    let settings = get_default_settings("rate_limit").unwrap();
    assert_eq!(settings["enabled"], true);
    assert_eq!(settings["api_rate_limit"], 100);
}

#[test]
fn test_content_moderation_defaults() {
    let settings = get_default_settings("content_moderation").unwrap();
    assert_eq!(settings["enabled"], false);
}

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
    let group = SettingsGroup::new(
        "test".to_string(),
        serde_json::json!([1, 2, 3]).to_string(),
    );
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
