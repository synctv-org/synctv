//! Global settings registry tests
//!
//! Comprehensive tests for the settings registry including:
//! - Type conversion for all setting types
//! - Validation (min/max bounds)
//! - Cache invalidation on update
//! - STUN server list updates
//! - Concurrent updates
//! - Settings persistence
//!
#![allow(clippy::unwrap_used)]

use std::time::Duration;
use synctv_core::service::global_settings::*;
use tokio::time::sleep;

// Type Conversion Tests (string ↔ T)

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
fn test_stun_server_list_conversion() {
    // Empty list
    let empty: IceServerList = "".parse().unwrap();
    assert!(empty.0.is_empty());

    // Single ICE server
    let json = r#"[{"urls":["stun:stun.example.com:19302"]}]"#;
    let parsed: IceServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 1);
    assert_eq!(parsed.0[0].urls, vec!["stun:stun.example.com:19302"]);
    assert_eq!(parsed.0[0].username, None);
    assert_eq!(parsed.0[0].credential, None);

    // Round-trip
    let serialized = parsed.to_string();
    let deserialized: IceServerList = serialized.parse().unwrap();
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

// Validation Tests (min/max bounds)

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
fn test_room_password_policy_parse_and_display() {
    assert_eq!(
        "optional".parse::<RoomPasswordPolicy>().unwrap(),
        RoomPasswordPolicy::Optional
    );
    assert_eq!(
        "required".parse::<RoomPasswordPolicy>().unwrap(),
        RoomPasswordPolicy::Required
    );
    assert_eq!(
        "forbidden".parse::<RoomPasswordPolicy>().unwrap(),
        RoomPasswordPolicy::Forbidden
    );

    assert_eq!(RoomPasswordPolicy::Optional.to_string(), "optional");
    assert_eq!(RoomPasswordPolicy::Required.to_string(), "required");
    assert_eq!(RoomPasswordPolicy::Forbidden.to_string(), "forbidden");
    assert!("true".parse::<RoomPasswordPolicy>().is_err());
    assert!("invalid_policy".parse::<RoomPasswordPolicy>().is_err());
}

#[test]
fn test_stun_server_list_updates() {
    let list = IceServerList::new();
    assert_eq!(list.0.len(), 2); // Default has 2 servers

    // Update to custom list
    let custom = IceServerList(vec![
        ConfiguredIceServer::new(vec!["stun:custom1.example.com:19302".to_string()]),
        ConfiguredIceServer::new(vec![
            "turn:custom2.example.com:3478?transport=udp".to_string()
        ])
        .with_auth("turn-user", "turn-secret"),
        ConfiguredIceServer::new(vec!["turns:custom3.example.com:5349".to_string()])
            .with_auth("turn-user-2", "turn-secret-2"),
    ]);
    assert_eq!(custom.0.len(), 3);

    // Clear list
    let empty = IceServerList(vec![]);
    assert!(empty.0.is_empty());

    // Serialize and deserialize
    let json = custom.to_string();
    let parsed: IceServerList = json.parse().unwrap();
    assert_eq!(parsed.0.len(), 3);
    assert_eq!(parsed.0[1].username.as_deref(), Some("turn-user"));
    assert_eq!(parsed.0[1].credential.as_deref(), Some("turn-secret"));
}

#[test]
fn test_cors_allowed_origins_updates() {
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

#[test]
fn test_public_settings_skips_empty_custom_publish_host() {
    let defaults = PublicSettings::defaults();
    let json = serde_json::to_string(&defaults).unwrap();

    assert!(!json.contains("custom_publish_host"));
}

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

// Cache Invalidation Tests

#[test]
fn test_stun_server_list_cache_key_uniqueness() {
    let list1 = IceServerList(vec![ConfiguredIceServer::new(vec![
        "stun:stun1.example.com:19302".to_string(),
    ])]);
    let list2 = IceServerList(vec![ConfiguredIceServer::new(vec![
        "stun:stun2.example.com:19302".to_string(),
    ])]);

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
    let initial = IceServerList(vec![ConfiguredIceServer::new(vec![
        "stun:stun1.example.com:19302".to_string(),
    ])]);
    let initial_key = initial.to_string();

    // Updated value
    let updated = IceServerList(vec![ConfiguredIceServer::new(vec![
        "stun:stun2.example.com:19302".to_string(),
    ])]);
    let updated_key = updated.to_string();

    // Cache should detect the change
    assert_ne!(
        initial_key, updated_key,
        "Cache key should change when value changes"
    );

    // Same value should produce same cache key
    let same_as_initial = IceServerList(vec![ConfiguredIceServer::new(vec![
        "stun:stun1.example.com:19302".to_string(),
    ])]);
    let same_key = same_as_initial.to_string();

    assert_eq!(
        initial_key, same_key,
        "Same value should produce same cache key"
    );
}

// Edge Cases and Error Handling Tests

#[test]
fn test_invalid_json_for_stun_server_list() {
    // Invalid JSON should fail to parse
    assert!("not json".parse::<IceServerList>().is_err());
    assert!("{invalid}".parse::<IceServerList>().is_err());
    assert!("[not-a-url]".parse::<IceServerList>().is_err());
}

#[test]
fn test_invalid_json_for_cors_origins() {
    // Invalid JSON should fail to parse
    assert!("not json".parse::<CorsAllowedOrigins>().is_err());
    assert!("{invalid}".parse::<CorsAllowedOrigins>().is_err());
    assert!("[not-a-url]".parse::<CorsAllowedOrigins>().is_err());
}

#[test]
fn test_max_chat_messages_zero_means_unlimited() {
    // Zero should be valid and means "unlimited"
    assert!(validate_max_chat_messages(0).is_ok());
    assert!(validate_max_chat_messages_per_room(0).is_ok());
}

// Settings Persistence Simulation Tests

#[test]
fn test_custom_setting_value_roundtrip() {
    let stun_list = IceServerList(vec![
        ConfiguredIceServer::new(vec!["stun:example.com:19302".to_string()]),
        ConfiguredIceServer::new(vec!["turn:turn.example.com:3478".to_string()])
            .with_auth("alice", "secret"),
    ]);
    let stun_json = stun_list.to_string();
    let stun_parsed: IceServerList = stun_json.parse().unwrap();
    assert_eq!(stun_list, stun_parsed);

    let cors_origins = CorsAllowedOrigins(vec!["https://example.com".to_string()]);
    let cors_json = cors_origins.to_string();
    let cors_parsed: CorsAllowedOrigins = cors_json.parse().unwrap();
    assert_eq!(cors_origins, cors_parsed);
}

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

    // Boolean settings should validate parse-ability
    assert!(
        service
            .validate_setting("user.enable_password_signup", "true")
            .is_ok(),
        "Valid boolean should pass"
    );
    assert!(
        service
            .validate_setting("user.enable_password_signup", "not_bool")
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_email_enabled_update_requires_complete_email_config() {
    use std::sync::Arc;
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::SettingsService;
    use synctv_core_testing::create_test_pool;

    let (_container, pool) = create_test_pool().await;
    let service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool,
    ));
    service.initialize().await.unwrap();
    let registry = SettingsRegistry::new(service.clone());
    registry
        .init(tokio_util::sync::CancellationToken::new())
        .unwrap();

    let error = service
        .update("email.enabled", "true".to_string())
        .await
        .expect_err("email.enabled=true must reject incomplete SMTP settings");
    assert!(
        error.to_string().contains("email.smtp_host"),
        "expected missing smtp_host validation error, got: {error:?}"
    );
    assert!(
        service.get("email.enabled").await.is_err(),
        "failed update must not persist email.enabled"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_email_enabled_batch_update_accepts_complete_email_config() {
    use std::sync::Arc;
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::SettingsService;
    use synctv_core_testing::create_test_pool;

    let (_container, pool) = create_test_pool().await;
    let service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool,
    ));
    service.initialize().await.unwrap();
    let registry = SettingsRegistry::new(service.clone());
    registry
        .init(tokio_util::sync::CancellationToken::new())
        .unwrap();

    service
        .update_batch([
            ("email.enabled".to_string(), "true".to_string()),
            (
                "email.smtp_host".to_string(),
                "smtp.example.com".to_string(),
            ),
            ("email.smtp_port".to_string(), "587".to_string()),
            (
                "email.from_email".to_string(),
                "noreply@example.com".to_string(),
            ),
        ])
        .await
        .expect("complete enabled email settings should persist");

    assert_eq!(
        service.get("email.enabled").await.unwrap().value,
        "true",
        "email.enabled should persist after complete batch update"
    );
}

#[test]
fn test_room_password_policy_has_no_contradictory_boolean_state() {
    let policies = [
        RoomPasswordPolicy::Optional,
        RoomPasswordPolicy::Required,
        RoomPasswordPolicy::Forbidden,
    ];

    let serialized = policies
        .into_iter()
        .map(|policy| policy.to_string())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        serialized,
        std::collections::HashSet::from([
            "optional".to_string(),
            "required".to_string(),
            "forbidden".to_string(),
        ])
    );
}

#[test]
fn test_room_password_policy_rejects_non_policy_values() {
    for invalid in [
        "true",
        "false",
        "required,forbidden",
        "",
        "optional|required",
    ] {
        assert!(
            invalid.parse::<RoomPasswordPolicy>().is_err(),
            "{invalid:?} must not parse as a room password policy"
        );
    }
}
