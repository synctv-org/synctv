//! Username reserved words validation tests
//!
//! Tests that reserved usernames (admin, system, root, etc.) are rejected
//! during username validation to prevent phishing/impersonation attacks.
//!
//! These tests validate the UsernameValidator from synctv-core which is used
//! by `set_username` in the API layer.

#![allow(clippy::unwrap_used)]

use synctv_core::validation::UsernameValidator;

/// Reserved words that should be rejected when setting a username.
/// Case-insensitive matching is required.
const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "system",
    "root",
    "moderator",
    "mod",
    "support",
    "help",
    "official",
    "staff",
    "owner",
    "bot",
    "service",
    "sysop",
    "operator",
    "dev",
    "developer",
    "security",
    "team",
];

#[test]
fn test_admin_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("admin");
    assert!(
        result.is_err(),
        "Expected 'admin' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_administrator_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("administrator");
    assert!(
        result.is_err(),
        "Expected 'administrator' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_system_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("system");
    assert!(
        result.is_err(),
        "Expected 'system' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_root_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("root");
    assert!(
        result.is_err(),
        "Expected 'root' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_moderator_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("moderator");
    assert!(
        result.is_err(),
        "Expected 'moderator' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_mod_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("mod");
    assert!(
        result.is_err(),
        "Expected 'mod' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_support_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("support");
    assert!(
        result.is_err(),
        "Expected 'support' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_official_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("official");
    assert!(
        result.is_err(),
        "Expected 'official' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_staff_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("staff");
    assert!(
        result.is_err(),
        "Expected 'staff' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_owner_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("owner");
    assert!(
        result.is_err(),
        "Expected 'owner' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_bot_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("bot");
    assert!(
        result.is_err(),
        "Expected 'bot' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_service_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("service");
    assert!(
        result.is_err(),
        "Expected 'service' to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_admin_uppercase_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("ADMIN");
    assert!(
        result.is_err(),
        "Expected 'ADMIN' (uppercase) to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_admin_mixed_case_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("AdMiN");
    assert!(
        result.is_err(),
        "Expected 'AdMiN' (mixed case) to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_root_uppercase_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("ROOT");
    assert!(
        result.is_err(),
        "Expected 'ROOT' (uppercase) to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_system_mixed_case_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("SyStEm");
    assert!(
        result.is_err(),
        "Expected 'SyStEm' (mixed case) to be rejected as reserved word"
    );
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("reserved"),
        "Error message should mention 'reserved', got: {err_msg}"
    );
}

#[test]
fn test_normal_username_john_passes() {
    let validator = UsernameValidator::new();
    let result = validator.validate("john");
    assert!(
        result.is_ok(),
        "Expected 'john' to pass validation as a normal username"
    );
}

#[test]
fn test_normal_username_alice_passes() {
    let validator = UsernameValidator::new();
    let result = validator.validate("alice");
    assert!(
        result.is_ok(),
        "Expected 'alice' to pass validation as a normal username"
    );
}

#[test]
fn test_normal_username_with_numbers_passes() {
    let validator = UsernameValidator::new();
    let result = validator.validate("user123");
    assert!(
        result.is_ok(),
        "Expected 'user123' to pass validation as a normal username"
    );
}

#[test]
fn test_normal_username_with_underscore_passes() {
    let validator = UsernameValidator::new();
    let result = validator.validate("john_doe");
    assert!(
        result.is_ok(),
        "Expected 'john_doe' to pass validation as a normal username"
    );
}

#[test]
fn test_normal_username_with_hyphen_passes() {
    let validator = UsernameValidator::new();
    let result = validator.validate("john-doe");
    assert!(
        result.is_ok(),
        "Expected 'john-doe' to pass validation as a normal username"
    );
}

#[test]
fn test_username_administratorx_passes() {
    // "administratorx" starts with "administrator" but is not exactly it
    let validator = UsernameValidator::new();
    let result = validator.validate("administratorx");
    assert!(
        result.is_ok(),
        "Expected 'administratorx' to pass validation (not a reserved word)"
    );
}

#[test]
fn test_username_myadmin_passes() {
    // "myadmin" contains "admin" but is not exactly "admin"
    let validator = UsernameValidator::new();
    let result = validator.validate("myadmin");
    assert!(
        result.is_ok(),
        "Expected 'myadmin' to pass validation (not a reserved word)"
    );
}

#[test]
fn test_username_root_user_passes() {
    // "root_user" contains "root" but is not exactly "root"
    let validator = UsernameValidator::new();
    let result = validator.validate("root_user");
    assert!(
        result.is_ok(),
        "Expected 'root_user' to pass validation (not a reserved word)"
    );
}

#[test]
fn test_all_reserved_words_are_rejected() {
    let validator = UsernameValidator::new();
    for reserved in RESERVED_USERNAMES {
        let result = validator.validate(reserved);
        assert!(
            result.is_err(),
            "Expected '{reserved}' to be rejected as reserved word"
        );

        // Also test uppercase version
        let upper = reserved.to_uppercase();
        let result = validator.validate(&upper);
        assert!(
            result.is_err(),
            "Expected '{upper}' (uppercase) to be rejected as reserved word"
        );
    }
}

#[test]
fn test_help_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("help");
    assert!(
        result.is_err(),
        "Expected 'help' to be rejected as reserved word"
    );
}

#[test]
fn test_sysop_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("sysop");
    assert!(
        result.is_err(),
        "Expected 'sysop' to be rejected as reserved word"
    );
}

#[test]
fn test_operator_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("operator");
    assert!(
        result.is_err(),
        "Expected 'operator' to be rejected as reserved word"
    );
}

#[test]
fn test_dev_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("dev");
    assert!(
        result.is_err(),
        "Expected 'dev' to be rejected as reserved word"
    );
}

#[test]
fn test_developer_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("developer");
    assert!(
        result.is_err(),
        "Expected 'developer' to be rejected as reserved word"
    );
}

#[test]
fn test_security_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("security");
    assert!(
        result.is_err(),
        "Expected 'security' to be rejected as reserved word"
    );
}

#[test]
fn test_team_is_rejected() {
    let validator = UsernameValidator::new();
    let result = validator.validate("team");
    assert!(
        result.is_err(),
        "Expected 'team' to be rejected as reserved word"
    );
}
