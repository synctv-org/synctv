//! Runtime settings persistence model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One flat runtime setting row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSetting {
    pub key: String,
    /// Rust field matches the database column; serialized output keeps the shorter `group` name.
    #[serde(rename = "group")]
    pub group_name: String,
    pub value: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RuntimeSetting {
    /// Create a new runtime setting
    #[must_use]
    pub fn new(group_name: String, value: String) -> Self {
        Self {
            key: format!("{group_name}.default"),
            group_name,
            value,
            version: 0,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        }
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

    fn json_field<'a>(json: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
        match json.get(key) {
            Some(value) => value,
            None => std::panic::panic_any(format!("JSON should contain '{key}' field")),
        }
    }

    #[test]
    fn test_runtime_setting_json_field_name_is_group() {
        let sg = RuntimeSetting::new("email".to_string(), r#"{"enabled":true}"#.to_string());
        let json = ok(
            serde_json::to_value(&sg),
            "runtime setting should serialize",
        );

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
    fn test_runtime_setting_deserialize_from_group_field() {
        let json = serde_json::json!({
            "key": "server.default",
            "group": "server",
            "value": "{\"test\": true}",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "version": 0
        });

        let sg: RuntimeSetting = ok(
            serde_json::from_value(json),
            "runtime setting should deserialize",
        );
        assert_eq!(sg.key, "server.default");
        assert_eq!(sg.group_name, "server");
        assert_eq!(sg.value, "{\"test\": true}");
    }
}
