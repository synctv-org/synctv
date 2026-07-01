//! Settings service tests
//!
//! Tests settings row model operations.
//!

use synctv_core::models::settings::RuntimeSetting;
use synctv_core_testing::ok;

#[test]
fn test_runtime_setting_new_uses_group_default_key() {
    let group = RuntimeSetting::new("server".to_string(), "true".to_string());

    assert_eq!(group.key, "server.default");
    assert_eq!(group.group_name, "server");
    assert_eq!(group.value, "true");
    assert_eq!(group.version, 0);
}

#[test]
fn test_runtime_setting_json_field_name_is_group() {
    let group = RuntimeSetting::new("email".to_string(), "enabled".to_string());
    let json = ok(
        serde_json::to_value(&group),
        "runtime setting should serialize",
    );

    assert_eq!(json["group"], "email");
    assert!(json.get("group_name").is_none());
}
