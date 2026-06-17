//! System settings model for runtime configuration
//!
//! Settings are organized by groups (e.g., "server", "email", "oauth")
//! Each group contains JSON settings that can be updated at runtime

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const SERVER_GROUP: &str = "server";
const EMAIL_GROUP: &str = "email";
const OAUTH_GROUP: &str = "oauth";
const RATE_LIMIT_GROUP: &str = "rate_limit";
const CONTENT_MODERATION_GROUP: &str = "content_moderation";
const CHAT_GROUP: &str = "chat";

/// System settings group
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SettingsGroup {
    pub key: String,
    /// Settings group name (Rust field is `group_name` to match the database column; serialized as `group` in JSON)
    #[serde(rename = "group")]
    pub group_name: String,
    pub value: String,
    /// Version for optimistic locking.
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SettingsGroup {
    /// Create a new settings group
    #[must_use]
    pub fn new(group_name: String, value: String) -> Self {
        Self {
            key: format!("{group_name}.default"),
            group_name,
            value,
            version: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Parse value as JSON value
    pub fn parse_json(&self) -> anyhow::Result<JsonValue> {
        serde_json::from_str(&self.value)
            .map_err(|e| anyhow::anyhow!("Failed to parse settings value: {e}"))
    }

    /// Get value as JSON object
    pub fn as_object(&self) -> anyhow::Result<serde_json::Map<String, JsonValue>> {
        self.parse_json()?
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("settings value is not an object"))
    }
}

/// Default settings for server group
#[must_use]
fn default_server_settings() -> JsonValue {
    serde_json::json!({
        "allow_room_creation": true,
        "max_rooms_per_user": 10,
        "max_members_per_room": 100,
        "default_room_settings": {
            "allow_guest": true
        }
    })
}

/// Default settings for email group
#[must_use]
fn default_email_settings() -> JsonValue {
    serde_json::json!({
        "enabled": false,
        "smtp_host": "",
        "smtp_port": 587,
        "smtp_username": "",
        "smtp_password": "",
        "use_tls": true,
        "from_email": "",
        "from_name": "SyncTV"
    })
}

/// Default settings for OAuth group
#[must_use]
fn default_oauth_settings() -> JsonValue {
    serde_json::json!({
        "github_enabled": false,
        "google_enabled": false,
        "microsoft_enabled": false,
        "discord_enabled": false
    })
}

/// Get default settings for a group
#[must_use]
pub fn get_default_settings(group_name: &str) -> Option<JsonValue> {
    match group_name {
        SERVER_GROUP => Some(default_server_settings()),
        EMAIL_GROUP => Some(default_email_settings()),
        OAUTH_GROUP => Some(default_oauth_settings()),
        RATE_LIMIT_GROUP => Some(serde_json::json!({
            "enabled": true,
            "api_rate_limit": 100,
            "api_rate_window": 60,
            "ws_rate_limit": 50,
            "ws_rate_window": 60
        })),
        CONTENT_MODERATION_GROUP => Some(serde_json::json!({
            "enabled": false,
            "filter_profanity": false,
            "max_message_length": 1000,
            "link_filter_enabled": false
        })),
        CHAT_GROUP => Some(serde_json::json!({
            "max_messages_per_room": 500,
            "max_pinned_messages_per_room": 20,
            "message_retention_days": 90
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn err<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => std::panic::panic_any(context.to_string()),
            Err(error) => error,
        }
    }

    fn json_field<'a>(json: &'a JsonValue, key: &str) -> &'a JsonValue {
        match json.get(key) {
            Some(value) => value,
            None => std::panic::panic_any(format!("JSON should contain '{key}' field")),
        }
    }

    #[test]
    fn test_parse_json() {
        let settings = SettingsGroup::new(
            "server".to_string(),
            serde_json::json!({"test": true}).to_string(),
        );

        let parsed = ok(settings.parse_json(), "settings JSON should parse");
        assert_eq!(json_field(&parsed, "test"), &JsonValue::Bool(true));
    }

    #[test]
    fn test_as_object() {
        let settings = SettingsGroup::new(
            "server".to_string(),
            serde_json::json!({"key1": "value1", "key2": 123}).to_string(),
        );

        let obj = ok(settings.as_object(), "settings should be an object");
        assert_eq!(
            obj.get("key1").cloned(),
            Some(JsonValue::String("value1".to_string()))
        );
        assert_eq!(
            obj.get("key2").cloned(),
            Some(JsonValue::Number(123.into()))
        );
    }

    #[test]
    fn test_settings_group_new_auto_key() {
        let sg = SettingsGroup::new("email".to_string(), "{}".to_string());
        assert_eq!(sg.key, "email.default");
        assert_eq!(sg.group_name, "email");
        assert_eq!(sg.value, "{}");
    }

    #[test]
    fn test_settings_group_json_field_name_is_group() {
        let sg = SettingsGroup::new("email".to_string(), r#"{"enabled":true}"#.to_string());
        let json = ok(serde_json::to_value(&sg), "settings group should serialize");

        assert!(
            json.get("group").is_some(),
            "JSON should contain 'group' field"
        );
        assert_eq!(json_field(&json, "group"), "email");
        assert!(
            json.get("group_name").is_none(),
            "JSON should not contain 'group_name' field"
        );
    }

    #[test]
    fn test_settings_group_deserialize_from_group_field() {
        let json = serde_json::json!({
            "key": "server.default",
            "group": "server",
            "value": "{\"test\": true}",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "version": 0
        });

        let sg: SettingsGroup = ok(
            serde_json::from_value(json),
            "settings group should deserialize",
        );
        assert_eq!(sg.key, "server.default");
        assert_eq!(sg.group_name, "server");
        assert_eq!(sg.value, "{\"test\": true}");
    }

    #[test]
    fn test_parse_json_invalid() {
        let sg = SettingsGroup::new("test".to_string(), "not valid json".to_string());
        let error = err(sg.parse_json(), "invalid JSON should fail");
        assert!(error.to_string().contains("parse"));
    }

    #[test]
    fn test_as_object_non_object() {
        let sg = SettingsGroup::new("test".to_string(), "42".to_string());
        let error = err(sg.as_object(), "non-object settings should fail");
        assert!(error.to_string().contains("not an object"));
    }
}
