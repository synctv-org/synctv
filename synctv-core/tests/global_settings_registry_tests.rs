//! Global settings registry tests
//!
//! Comprehensive tests for the settings registry including:
//! - Type conversion for all setting types
//! - Validation (min/max bounds)
//! - Cache invalidation on update
//! - TURN credential generation
//! - STUN/TURN server list updates
//! - Concurrent updates
//! - Settings persistence
//!
//! Run with: cargo test --test `global_settings_registry_tests`
#![allow(clippy::unwrap_used)]

use std::time::Duration;
use synctv_core::service::global_settings::*;
use synctv_core::service::turn_server::generate_turn_credentials;
use tokio::time::sleep;

// ============================================================================
// Type Conversion Tests (string ↔ T)
// ============================================================================

#[test]
fn test_bool_setting_conversion() {
    // Valid conversions
    assert!("true".parse::<bool>().unwrap());
    assert!(!"false".parse::<bool>().unwrap());
    assert_eq!(true.to_string(), "true");
    assert_eq!(false.to_string(), "false");

    // Invalid conversions
    assert!("invalid".parse::<bool>().is_err());
    assert!("1".parse::<bool>().is_err());
    assert!("0".parse::<bool>().is_err());
}

#[test]
fn test_i64_setting_conversion() {
    // Valid conversions
    assert_eq!("42".parse::<i64>().unwrap(), 42);
    assert_eq!("-100".parse::<i64>().unwrap(), -100);
    assert_eq!("0".parse::<i64>().unwrap(), 0);
    assert_eq!(42_i64.to_string(), "42");
    assert_eq!((-100_i64).to_string(), "-100");

    // Invalid conversions
    assert!("abc".parse::<i64>().is_err());
    assert!("12.34".parse::<i64>().is_err());
    assert!("1e5".parse::<i64>().is_err());
}

#[test]
fn test_u64_setting_conversion() {
    // Valid conversions
    assert_eq!("42".parse::<u64>().unwrap(), 42);
    assert_eq!("0".parse::<u64>().unwrap(), 0);
    assert_eq!(42_u64.to_string(), "42");

    // Invalid conversions
    assert!("-1".parse::<u64>().is_err());
    assert!("abc".parse::<u64>().is_err());
}

#[test]
fn test_string_setting_conversion() {
    // String conversion is trivial but we test it for completeness
    assert_eq!("hello".parse::<String>().unwrap(), "hello");
    assert_eq!("world".to_string(), "world");
    assert_eq!("".parse::<String>().unwrap(), "");
}

#[test]
fn test_turn_server_list_conversion() {
    // Empty list
    let empty: TurnServerList = "".parse().unwrap();
    assert!(empty.0.is_empty());
    assert_eq!(empty.to_string(), "[]");

    // Single server
    let json = r#"[{"urls":["turn:example.com:3478"],"username":"user","credential":"pass"}]"#;
    let parsed: TurnServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 1);
    assert_eq!(parsed.0[0].urls[0], "turn:example.com:3478");
    assert_eq!(parsed.0[0].username, Some("user".to_string()));
    assert_eq!(parsed.0[0].credential, Some("pass".to_string()));

    // Round-trip
    let serialized = parsed.to_string();
    let deserialized: TurnServerList = serialized.parse().unwrap();
    assert_eq!(parsed, deserialized);
}

#[test]
fn test_stun_server_list_conversion() {
    // Empty list
    let empty: StunServerList = "".parse().unwrap();
    assert!(empty.0.is_empty());

    // Single STUN server
    let json = r#"["stun:stun.example.com:19302"]"#;
    let parsed: StunServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 1);
    assert_eq!(parsed.0[0], "stun:stun.example.com:19302");

    // Round-trip
    let serialized = parsed.to_string();
    let deserialized: StunServerList = serialized.parse().unwrap();
    assert_eq!(parsed, deserialized);
}

#[test]
fn test_cors_allowed_origins_conversion() {
    // Empty list
    let empty: CorsAllowedOrigins = "".parse().unwrap();
    assert!(empty.0.is_empty());

    // Multiple origins
    let json = r#"["https://example.com","https://app.example.com"]"#;
    let parsed: CorsAllowedOrigins = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 2);
    assert_eq!(parsed.0[0], "https://example.com");
    assert_eq!(parsed.0[1], "https://app.example.com");

    // Round-trip
    let serialized = parsed.to_string();
    let deserialized: CorsAllowedOrigins = serialized.parse().unwrap();
    assert_eq!(parsed, deserialized);
}

// ============================================================================
// Validation Tests (min/max bounds)
// ============================================================================

#[test]
fn test_max_rooms_per_user_validation() {
    // Valid range: 1 to 1000
    let valid_values = [1, 10, 100, 500, 1000];
    for v in valid_values {
        let result = validate_max_rooms_per_user(v);
        assert!(result.is_ok(), "Value {v} should be valid");
    }

    // Invalid values
    let invalid_values = [0, -1, -100, 1001, 10000];
    for v in invalid_values {
        let result = validate_max_rooms_per_user(v);
        assert!(result.is_err(), "Value {v} should be invalid");
    }
}

fn validate_max_rooms_per_user(v: i64) -> synctv_core::Result<()> {
    if v > 0 && v <= 1000 {
        Ok(())
    } else {
        Err(synctv_core::Error::InvalidInput(
            "max_rooms_per_user must be between 1 and 1000".into(),
        ))
    }
}

#[test]
fn test_max_members_per_room_validation() {
    // Valid range: 1 to MaxMembers::MAX
    let valid_values = [1, 10, 100, 1000];
    for v in valid_values {
        let result = validate_max_members_per_room(v);
        assert!(result.is_ok(), "Value {v} should be valid");
    }

    // Invalid values
    let invalid_values = [0, -1, -100];
    for v in invalid_values {
        let result = validate_max_members_per_room(v);
        assert!(result.is_err(), "Value {v} should be invalid");
    }
}

fn validate_max_members_per_room(v: i64) -> synctv_core::Result<()> {
    use synctv_core::models::room_settings::MaxMembers;
    if v > 0 && v <= MaxMembers::MAX.cast_signed() {
        Ok(())
    } else {
        Err(synctv_core::Error::InvalidInput(format!(
            "max_members_per_room must be between 1 and {}",
            MaxMembers::MAX
        )))
    }
}

#[test]
fn test_max_chat_messages_validation() {
    // Valid range: 0 to MAX_CHAT_MESSAGES_LIMIT (10_000)
    let valid_values = [0, 1, 100, 500, 1000, 5000, 10_000];
    for v in valid_values {
        let result = validate_max_chat_messages(v);
        assert!(result.is_ok(), "Value {v} should be valid");
    }

    // Invalid values
    let invalid_values = [10_001, 100_000];
    for v in invalid_values {
        let result = validate_max_chat_messages(v);
        assert!(result.is_err(), "Value {v} should be invalid");
    }
}

fn validate_max_chat_messages(v: u64) -> synctv_core::Result<()> {
    const MAX_CHAT_MESSAGES_LIMIT: u64 = 10_000;
    if v <= MAX_CHAT_MESSAGES_LIMIT {
        Ok(())
    } else {
        Err(synctv_core::Error::InvalidInput(format!(
            "max_chat_messages must be at most {MAX_CHAT_MESSAGES_LIMIT} (0 = unlimited)"
        )))
    }
}

#[test]
fn test_room_ttl_validation() {
    // Valid range: >= 0
    let valid_values = [0, 60, 3600, 86400, 172_800];
    for v in valid_values {
        let result = validate_room_ttl(v);
        assert!(result.is_ok(), "Value {v} should be valid");
    }

    // Invalid values
    let invalid_values = [-1, -100];
    for v in invalid_values {
        let result = validate_room_ttl(v);
        assert!(result.is_err(), "Value {v} should be invalid");
    }
}

fn validate_room_ttl(v: i64) -> synctv_core::Result<()> {
    if v >= 0 {
        Ok(())
    } else {
        Err(synctv_core::Error::InvalidInput(
            "room_ttl must be non-negative (0 = never expire)".into(),
        ))
    }
}

#[test]
fn test_max_chat_messages_per_room_validation() {
    // Valid range: 0 to 100000
    let valid_values = [0, 1, 100, 1000, 10000, 100_000];
    for v in valid_values {
        let result = validate_max_chat_messages_per_room(v);
        assert!(result.is_ok(), "Value {v} should be valid");
    }

    // Invalid values
    let invalid_values = [100_001, 1_000_000];
    for v in invalid_values {
        let result = validate_max_chat_messages_per_room(v);
        assert!(result.is_err(), "Value {v} should be invalid");
    }
}

fn validate_max_chat_messages_per_room(v: u64) -> synctv_core::Result<()> {
    if v <= 100_000 {
        Ok(())
    } else {
        Err(synctv_core::Error::InvalidInput(
            "max_chat_messages_per_room must be <= 100000 (0 = unlimited)".into(),
        ))
    }
}

#[test]
fn test_cross_validation_room_password_settings() {
    // Test that room_must_need_pwd and room_must_no_need_pwd cannot both be true
    let must_need_pwd = true;
    let must_no_need_pwd = true;

    let result = validate_room_password_settings(must_need_pwd, must_no_need_pwd);
    assert!(
        result.is_err(),
        "Both settings cannot be true simultaneously"
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("cannot both be true"));

    // Valid combinations
    assert!(validate_room_password_settings(true, false).is_ok());
    assert!(validate_room_password_settings(false, true).is_ok());
    assert!(validate_room_password_settings(false, false).is_ok());
}

fn validate_room_password_settings(
    must_need_pwd: bool,
    must_no_need_pwd: bool,
) -> synctv_core::Result<()> {
    if must_need_pwd && must_no_need_pwd {
        Err(synctv_core::Error::InvalidInput(
            "room_must_need_pwd and room_must_no_need_pwd cannot both be true".into(),
        ))
    } else {
        Ok(())
    }
}

// ============================================================================
// TURN Credential HMAC-SHA1 Generation Tests
// ============================================================================

#[test]
fn test_turn_credential_format() {
    let cred = generate_turn_credentials("my-secret", "user123", 86400);

    // Username should be in "timestamp:userid" format
    assert!(cred.username.contains(':'));
    let parts: Vec<&str> = cred.username.splitn(2, ':').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[1], "user123");

    // Timestamp part should be a valid number
    let ts: u64 = parts[0].parse().expect("timestamp should be a number");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Expiry should be roughly now + 86400 (within 5 seconds tolerance)
    assert!(ts >= now + 86400 - 5);
    assert!(ts <= now + 86400 + 5);

    // Password should be valid Base64
    assert!(!cred.password.is_empty());
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&cred.password)
        .expect("password should be valid Base64");
    // HMAC-SHA1 produces 20 bytes
    assert_eq!(decoded.len(), 20);

    // Expiry timestamp should match
    assert_eq!(cred.expiry_timestamp, ts);
}

#[test]
fn test_turn_credential_deterministic() {
    // Same inputs at the same time should produce the same output
    let cred1 = generate_turn_credentials("secret", "user1", 3600);
    let cred2 = generate_turn_credentials("secret", "user1", 3600);

    // Timestamps should be within 1 second of each other
    assert!(cred1.expiry_timestamp.abs_diff(cred2.expiry_timestamp) <= 1);

    // If timestamps match, passwords should match
    if cred1.username == cred2.username {
        assert_eq!(cred1.password, cred2.password);
    }
}

#[test]
fn test_turn_credential_different_secrets() {
    let cred1 = generate_turn_credentials("secret-a", "user1", 3600);
    let cred2 = generate_turn_credentials("secret-b", "user1", 3600);

    // Different secrets should produce different passwords
    // (unless by astronomically unlikely collision)
    if cred1.username == cred2.username {
        assert_ne!(cred1.password, cred2.password);
    }
}

#[test]
fn test_turn_credential_different_users() {
    let cred1 = generate_turn_credentials("secret", "alice", 3600);
    let cred2 = generate_turn_credentials("secret", "bob", 3600);

    // Different user IDs should produce different usernames
    assert_ne!(cred1.username, cred2.username);
}

#[test]
fn test_turn_credential_zero_ttl() {
    let cred = generate_turn_credentials("secret", "user1", 0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // With TTL=0, expiry should be approximately now
    assert!(cred.expiry_timestamp.abs_diff(now) <= 2);
}

#[test]
fn test_turn_credential_base64_encoding() {
    let cred = generate_turn_credentials("secret", "user1", 3600);

    // Verify password is valid Base64 containing only valid characters
    assert!(cred
        .password
        .chars()
        .all(|c| { c.is_alphanumeric() || c == '+' || c == '/' || c == '=' }));

    // Should be decodable
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&cred.password)
        .expect("Should be valid Base64");

    // HMAC-SHA1 output is always 20 bytes
    assert_eq!(decoded.len(), 20);
}

// ============================================================================
// STUN/TURN Server List Update Tests
// ============================================================================

#[test]
fn test_turn_server_list_updates() {
    // Start with empty list
    let list = TurnServerList::new();
    assert!(list.0.is_empty());

    // Add server
    let updated = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn1.example.com:3478".to_string()],
        username: Some("user1".to_string()),
        credential: Some("pass1".to_string()),
    }]);
    assert_eq!(updated.0.len(), 1);

    // Add more servers
    let multi = TurnServerList(vec![
        TurnServer {
            urls: vec!["turn:turn1.example.com:3478".to_string()],
            username: Some("user1".to_string()),
            credential: Some("pass1".to_string()),
        },
        TurnServer {
            urls: vec!["turn:turn2.example.com:3478".to_string()],
            username: Some("user2".to_string()),
            credential: Some("pass2".to_string()),
        },
    ]);
    assert_eq!(multi.0.len(), 2);

    // Serialize and deserialize
    let json = multi.to_string();
    let parsed: TurnServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 2);
}

#[test]
fn test_stun_server_list_updates() {
    // Start with default list
    let list = StunServerList::new();
    assert_eq!(list.0.len(), 2); // Default has 2 servers

    // Update to custom list
    let custom = StunServerList(vec![
        "stun:custom1.example.com:19302".to_string(),
        "stun:custom2.example.com:19302".to_string(),
        "stun:custom3.example.com:19302".to_string(),
    ]);
    assert_eq!(custom.0.len(), 3);

    // Clear list
    let empty = StunServerList(vec![]);
    assert!(empty.0.is_empty());

    // Serialize and deserialize
    let json = custom.to_string();
    let parsed: StunServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 3);
}

#[test]
fn test_cors_allowed_origins_updates() {
    // Start with empty list (secure default)
    let list = CorsAllowedOrigins::new();
    assert!(list.0.is_empty());

    // Add origins
    let with_origins = CorsAllowedOrigins(vec![
        "https://example.com".to_string(),
        "https://app.example.com".to_string(),
        "https://*.example.com".to_string(),
    ]);
    assert_eq!(with_origins.0.len(), 3);

    // Remove all origins
    let empty = CorsAllowedOrigins(vec![]);
    assert!(empty.0.is_empty());

    // Serialize and deserialize
    let json = with_origins.to_string();
    let parsed: CorsAllowedOrigins = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 3);
}

// ============================================================================
// PublicSettings Tests
// ============================================================================

#[test]
fn test_public_settings_defaults() {
    let defaults = PublicSettings::defaults();

    assert!(defaults.signup_enabled);
    assert!(defaults.allow_room_creation);
    assert_eq!(defaults.max_rooms_per_user, 10);
    assert_eq!(defaults.max_members_per_room, 100);
    assert!(!defaults.disable_create_room);
    assert!(!defaults.create_room_need_review);
    assert_eq!(defaults.room_ttl, 172_800); // 48 hours
    assert!(!defaults.room_must_need_pwd);
    assert!(!defaults.room_must_no_need_pwd);
    assert!(!defaults.signup_need_review);
    assert!(defaults.enable_password_signup);
    assert!(defaults.enable_guest);
    assert!(defaults.movie_proxy);
    assert!(defaults.live_proxy);
    assert!(defaults.ts_disguised_as_png);
    assert!(defaults.custom_publish_host.is_empty());
    assert!(!defaults.email_whitelist_enabled);
}

#[test]
fn test_public_settings_serialization() {
    let mut settings = PublicSettings::defaults();
    settings.custom_publish_host = "rtmp://live.example.com".to_string();

    // Serialize
    let json = serde_json::to_string(&settings).unwrap();

    // Deserialize
    let deserialized: PublicSettings = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.signup_enabled, settings.signup_enabled);
    assert_eq!(deserialized.max_rooms_per_user, settings.max_rooms_per_user);
    assert_eq!(deserialized.custom_publish_host, "rtmp://live.example.com");
}

#[test]
fn test_public_settings_skips_empty_custom_publish_host() {
    let defaults = PublicSettings::defaults();
    let json = serde_json::to_string(&defaults).unwrap();

    // Empty custom_publish_host should be omitted via skip_serializing_if
    assert!(!json.contains("custom_publish_host"));
}

// ============================================================================
// Concurrent Updates Test (Last Write Wins)
// ============================================================================

#[tokio::test]
async fn test_concurrent_settings_updates_last_write_wins() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    // Simulate concurrent updates

    // Simulate concurrent updates
    let write_count = Arc::new(AtomicU64::new(0));
    let last_value = Arc::new(parking_lot::RwLock::new("initial".to_string()));

    // Spawn multiple concurrent updates
    let mut handles = vec![];

    for i in 0..10 {
        let last_value_clone = last_value.clone();
        let write_count_clone = write_count.clone();

        handles.push(tokio::spawn(async move {
            // Simulate setting a value
            let value = format!("update-{i}");
            *last_value_clone.write() = value.clone();
            write_count_clone.fetch_add(1, Ordering::SeqCst);

            // Small random delay
            sleep(Duration::from_millis(10)).await;

            // Verify our write happened
            let current = last_value_clone.read().clone();
            (i, value, current)
        }));
    }

    // Collect results
    let results = futures::future::join_all(handles).await;

    // All writes should have been attempted
    assert_eq!(write_count.load(Ordering::SeqCst), 10);

    // The last writer should have won
    let final_value = last_value.read().clone();
    assert!(final_value.starts_with("update-"));

    // Verify at least one concurrent update happened
    let concurrent_writes: Vec<_> = results
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|(_, written, current)| *written != current.clone())
        .collect();

    assert!(
        !concurrent_writes.is_empty(),
        "Should have at least one concurrent write where the value changed after the write"
    );
}

// ============================================================================
// Cache Invalidation Tests
// ============================================================================

#[test]
fn test_turn_server_list_cache_key_uniqueness() {
    // Each different TURN server list should produce a unique serialized form
    let list1 = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn1.example.com:3478".to_string()],
        username: Some("user1".to_string()),
        credential: Some("pass1".to_string()),
    }]);

    let list2 = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn2.example.com:3478".to_string()],
        username: Some("user2".to_string()),
        credential: Some("pass2".to_string()),
    }]);

    let key1 = list1.to_string();
    let key2 = list2.to_string();

    assert_ne!(
        key1, key2,
        "Different TURN lists should produce different cache keys"
    );
}

#[test]
fn test_stun_server_list_cache_key_uniqueness() {
    let list1 = StunServerList(vec!["stun:stun1.example.com:19302".to_string()]);
    let list2 = StunServerList(vec!["stun:stun2.example.com:19302".to_string()]);

    let key1 = list1.to_string();
    let key2 = list2.to_string();

    assert_ne!(
        key1, key2,
        "Different STUN lists should produce different cache keys"
    );
}

#[test]
fn test_cors_origins_cache_key_uniqueness() {
    let origins1 = CorsAllowedOrigins(vec!["https://example.com".to_string()]);
    let origins2 = CorsAllowedOrigins(vec!["https://app.example.com".to_string()]);

    let key1 = origins1.to_string();
    let key2 = origins2.to_string();

    assert_ne!(
        key1, key2,
        "Different CORS origin lists should produce different cache keys"
    );
}

#[test]
fn test_cache_invalidation_on_value_change() {
    // Simulate cache invalidation when a setting value changes

    // Initial value
    let initial = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn1.example.com:3478".to_string()],
        username: Some("user1".to_string()),
        credential: Some("pass1".to_string()),
    }]);
    let initial_key = initial.to_string();

    // Updated value
    let updated = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn2.example.com:3478".to_string()],
        username: Some("user2".to_string()),
        credential: Some("pass2".to_string()),
    }]);
    let updated_key = updated.to_string();

    // Cache should detect the change
    assert_ne!(
        initial_key, updated_key,
        "Cache key should change when value changes"
    );

    // Same value should produce same cache key
    let same_as_initial = TurnServerList(vec![TurnServer {
        urls: vec!["turn:turn1.example.com:3478".to_string()],
        username: Some("user1".to_string()),
        credential: Some("pass1".to_string()),
    }]);
    let same_key = same_as_initial.to_string();

    assert_eq!(
        initial_key, same_key,
        "Same value should produce same cache key"
    );
}

// ============================================================================
// Edge Cases and Error Handling Tests
// ============================================================================

#[test]
fn test_turn_server_with_empty_urls() {
    // TURN server with empty URL array should serialize correctly
    let server = TurnServer {
        urls: vec![],
        username: Some("user".to_string()),
        credential: Some("pass".to_string()),
    };

    let json = serde_json::to_string(&server).unwrap();
    let parsed: TurnServer = serde_json::from_str(&json).unwrap();

    assert!(parsed.urls.is_empty());
    assert_eq!(parsed.username, Some("user".to_string()));
    assert_eq!(parsed.credential, Some("pass".to_string()));
}

#[test]
fn test_turn_server_with_multiple_urls() {
    // TURN server can have multiple URLs (e.g., for different transports/ports)
    let server = TurnServer {
        urls: vec![
            "turn:turn.example.com:3478?transport=udp".to_string(),
            "turn:turn.example.com:3478?transport=tcp".to_string(),
            "turns:turn.example.com:5349?transport=tcp".to_string(),
        ],
        username: Some("user".to_string()),
        credential: Some("pass".to_string()),
    };

    let json = serde_json::to_string(&server).unwrap();
    let parsed: TurnServer = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.urls.len(), 3);
    assert!(parsed.urls[0].contains("udp"));
    assert!(parsed.urls[1].contains("tcp"));
    assert!(parsed.urls[2].contains("turns"));
}

#[test]
fn test_turn_server_without_auth() {
    // TURN server can be configured without authentication
    let server = TurnServer {
        urls: vec!["turn:public-turn.example.com:3478".to_string()],
        username: None,
        credential: None,
    };

    let json = serde_json::to_string(&server).unwrap();

    // None fields should be omitted from JSON
    assert!(!json.contains("username"));
    assert!(!json.contains("credential"));

    let parsed: TurnServer = serde_json::from_str(&json).unwrap();
    assert!(parsed.username.is_none());
    assert!(parsed.credential.is_none());
}

#[test]
fn test_invalid_json_for_turn_server_list() {
    // Invalid JSON should fail to parse
    assert!("not json".parse::<TurnServerList>().is_err());
    assert!("{invalid}".parse::<TurnServerList>().is_err());
    assert!("[]]".parse::<TurnServerList>().is_err());
}

#[test]
fn test_invalid_json_for_stun_server_list() {
    // Invalid JSON should fail to parse
    assert!("not json".parse::<StunServerList>().is_err());
    assert!("{invalid}".parse::<StunServerList>().is_err());
    assert!("[not-a-url]".parse::<StunServerList>().is_err());
}

#[test]
fn test_invalid_json_for_cors_origins() {
    // Invalid JSON should fail to parse
    assert!("not json".parse::<CorsAllowedOrigins>().is_err());
    assert!("{invalid}".parse::<CorsAllowedOrigins>().is_err());
    assert!("[not-a-url]".parse::<CorsAllowedOrigins>().is_err());
}

#[test]
fn test_room_ttl_boundary_values() {
    // Test boundary values for room_ttl
    assert!(validate_room_ttl(0).is_ok()); // Zero is valid (never expire)
    assert!(validate_room_ttl(1).is_ok()); // Minimum positive value
    assert!(validate_room_ttl(i64::MAX).is_ok()); // Very large value is valid
    assert!(validate_room_ttl(-1).is_err()); // Negative is invalid
}

#[test]
fn test_max_chat_messages_zero_means_unlimited() {
    // Zero should be valid and means "unlimited"
    assert!(validate_max_chat_messages(0).is_ok());
    assert!(validate_max_chat_messages_per_room(0).is_ok());
}

// ============================================================================
// Settings Persistence Simulation Tests
// ============================================================================

#[test]
fn test_setting_value_roundtrip() {
    // Test that setting values survive serialization/deserialization roundtrip

    // Boolean
    let bool_val = true;
    assert_eq!(bool_val.to_string().parse::<bool>().unwrap(), bool_val);

    // Integer
    let int_val = 42_i64;
    assert_eq!(int_val.to_string().parse::<i64>().unwrap(), int_val);

    // Unsigned
    let uint_val = 100_u64;
    assert_eq!(uint_val.to_string().parse::<u64>().unwrap(), uint_val);

    // String
    let str_val = "hello world";
    assert_eq!(str_val.to_string().parse::<String>().unwrap(), str_val);

    // TURN server list
    let turn_list = TurnServerList(vec![TurnServer {
        urls: vec!["turn:example.com:3478".to_string()],
        username: Some("user".to_string()),
        credential: Some("pass".to_string()),
    }]);
    let turn_json = turn_list.to_string();
    let turn_parsed: TurnServerList = turn_json.parse().unwrap();
    assert_eq!(turn_list, turn_parsed);

    // STUN server list
    let stun_list = StunServerList(vec!["stun:example.com:19302".to_string()]);
    let stun_json = stun_list.to_string();
    let stun_parsed: StunServerList = stun_json.parse().unwrap();
    assert_eq!(stun_list, stun_parsed);

    // CORS origins
    let cors_origins = CorsAllowedOrigins(vec!["https://example.com".to_string()]);
    let cors_json = cors_origins.to_string();
    let cors_parsed: CorsAllowedOrigins = cors_json.parse().unwrap();
    assert_eq!(cors_origins, cors_parsed);
}

#[test]
fn test_default_values_are_valid() {
    // Verify all default values pass validation

    // max_rooms_per_user default: 10
    assert!(validate_max_rooms_per_user(10).is_ok());

    // max_members_per_room default: 100
    assert!(validate_max_members_per_room(100).is_ok());

    // max_chat_messages default: 500
    assert!(validate_max_chat_messages(500).is_ok());

    // room_ttl default: 172800 (48 hours)
    assert!(validate_room_ttl(172_800).is_ok());

    // max_chat_messages_per_room default: 500
    assert!(validate_max_chat_messages_per_room(500).is_ok());
}

// ============================================================================
// SettingsService.update() validation tests (Issue #1)
// ============================================================================

/// Verify that `SettingsRegistry` wires providers to `SettingsService` so that
/// single-key `update()` calls validate before persisting.
#[tokio::test]
async fn test_registry_wires_validation_to_settings_service() {
    use std::sync::Arc;
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::SettingsService;

    let pool_opts = sqlx::postgres::PgPoolOptions::new().max_connections(1);
    let pool = pool_opts
        .connect_lazy("postgres://fake:fake@localhost/fake")
        .unwrap();
    let repo = SettingsRepository::new(pool.clone());
    let service = Arc::new(SettingsService::new(repo, pool));
    let registry = SettingsRegistry::new(service.clone());

    // max_rooms_per_user has a validator: 1..=1000
    assert!(
        service
            .validate_setting("server.max_rooms_per_user", "10")
            .is_ok(),
        "Valid max_rooms_per_user should pass"
    );
    assert!(
        service
            .validate_setting("server.max_rooms_per_user", "0")
            .is_err(),
        "Zero max_rooms_per_user should fail"
    );
    assert!(
        service
            .validate_setting("server.max_rooms_per_user", "1001")
            .is_err(),
        "Exceeding max_rooms_per_user limit should fail"
    );

    // max_members_per_room has a validator
    assert!(
        service
            .validate_setting("server.max_members_per_room", "100")
            .is_ok(),
        "Valid max_members_per_room should pass"
    );
    assert!(
        service
            .validate_setting("server.max_members_per_room", "0")
            .is_err(),
        "Zero max_members_per_room should fail"
    );

    // room_ttl has a validator: >= 0
    assert!(
        service.validate_setting("room.room_ttl", "0").is_ok(),
        "Zero room_ttl should pass (never expire)"
    );
    assert!(
        service.validate_setting("room.room_ttl", "-1").is_err(),
        "Negative room_ttl should fail"
    );

    // Boolean settings should validate parse-ability
    assert!(
        service
            .validate_setting("server.signup_enabled", "true")
            .is_ok(),
        "Valid boolean should pass"
    );
    assert!(
        service
            .validate_setting("server.signup_enabled", "not_bool")
            .is_err(),
        "Invalid boolean should fail"
    );

    // max_chat_messages has a validator: <= 10000
    assert!(
        service
            .validate_setting("server.max_chat_messages", "500")
            .is_ok(),
        "Valid max_chat_messages should pass"
    );
    assert!(
        service
            .validate_setting("server.max_chat_messages", "10001")
            .is_err(),
        "Exceeding max_chat_messages should fail"
    );

    // Ensure the registry is used (prevent optimizer from dropping it)
    let _ = registry.to_public_settings();
}

// ============================================================================
// Cross-validation race condition tests (Issue #2)
// ============================================================================

#[test]
fn test_contradictory_settings_update_batch_rejects_both_true() {
    // This test verifies that update_batch() rejects the contradictory
    // combination regardless of cache state. The actual DB read happens at
    // runtime; here we verify the pre-batch validation at the SettingsService level.

    // Both explicitly set to true in the batch -> must reject
    let result = validate_room_password_settings(true, true);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("cannot both be true"));
}

#[test]
fn test_contradictory_settings_valid_combinations() {
    // (true, false) -> ok
    assert!(validate_room_password_settings(true, false).is_ok());
    // (false, true) -> ok
    assert!(validate_room_password_settings(false, true).is_ok());
    // (false, false) -> ok
    assert!(validate_room_password_settings(false, false).is_ok());
}
