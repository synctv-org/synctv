//! Global runtime settings store integration tests.

use synctv_core::service::global_settings::*;
use synctv_core_testing::{err, ok};

#[test]
fn test_room_password_policy_parse_and_display() {
    assert_eq!(
        ok(
            "optional".parse::<RoomPasswordPolicy>(),
            "optional policy should parse",
        ),
        RoomPasswordPolicy::Optional
    );
    assert_eq!(
        ok(
            "required".parse::<RoomPasswordPolicy>(),
            "required policy should parse",
        ),
        RoomPasswordPolicy::Required
    );
    assert_eq!(
        ok(
            "forbidden".parse::<RoomPasswordPolicy>(),
            "forbidden policy should parse",
        ),
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
    assert_eq!(list.0.len(), 2);

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

    let empty = IceServerList(vec![]);
    assert!(empty.0.is_empty());

    let json = custom.to_string();
    let parsed: IceServerList = ok(json.parse(), "ICE server list should parse");
    assert_eq!(parsed.0.len(), 3);
    assert_eq!(parsed.0[1].username.as_deref(), Some("turn-user"));
    assert_eq!(parsed.0[1].credential.as_deref(), Some("turn-secret"));
}

#[test]
fn test_cors_allowed_origins_updates() {
    let list = CorsAllowedOrigins::new();
    assert!(list.0.is_empty());

    let with_origins = CorsAllowedOrigins(vec![
        "https://example.com".to_string(),
        "https://app.example.com".to_string(),
        "https://*.example.com".to_string(),
    ]);
    assert_eq!(with_origins.0.len(), 3);

    let empty = CorsAllowedOrigins(vec![]);
    assert!(empty.0.is_empty());

    let json = with_origins.to_string();
    let parsed: CorsAllowedOrigins = ok(json.parse(), "CORS origins should parse");
    assert_eq!(parsed.0.len(), 3);
}

#[test]
fn test_public_settings_skips_empty_custom_publish_host() {
    let defaults = PublicSettings::defaults();
    let json = ok(
        serde_json::to_string(&defaults),
        "public settings should serialize",
    );

    assert!(!json.contains("custom_publish_host"));
    assert_eq!(defaults.max_pinned_chat_messages_per_room, 20);
    assert!(json.contains("max_pinned_chat_messages_per_room"));
}

#[test]
fn test_invalid_json_for_stun_server_list() {
    assert!("not json".parse::<IceServerList>().is_err());
    assert!("{invalid}".parse::<IceServerList>().is_err());
    assert!("[not-a-url]".parse::<IceServerList>().is_err());
}

#[test]
fn test_invalid_json_for_cors_origins() {
    assert!("not json".parse::<CorsAllowedOrigins>().is_err());
    assert!("{invalid}".parse::<CorsAllowedOrigins>().is_err());
    assert!("[not-a-url]".parse::<CorsAllowedOrigins>().is_err());
}

#[test]
fn test_custom_setting_value_roundtrip() {
    let stun_list = IceServerList(vec![
        ConfiguredIceServer::new(vec!["stun:example.com:19302".to_string()]),
        ConfiguredIceServer::new(vec!["turn:turn.example.com:3478".to_string()])
            .with_auth("alice", "secret"),
    ]);
    let stun_json = stun_list.to_string();
    let stun_parsed: IceServerList = ok(stun_json.parse(), "STUN list should parse");
    assert_eq!(stun_list, stun_parsed);

    let cors_origins = CorsAllowedOrigins(vec!["https://example.com".to_string()]);
    let cors_json = cors_origins.to_string();
    let cors_parsed: CorsAllowedOrigins = ok(cors_json.parse(), "CORS origins should parse");
    assert_eq!(cors_origins, cors_parsed);
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
    ok(
        service.initialize().await,
        "settings service should initialize",
    );
    let registry = RuntimeSettingsStore::new(service.clone());
    ok(
        registry.init(tokio_util::sync::CancellationToken::new()),
        "runtime settings store should init",
    );

    let mut settings = ok(
        registry.runtime_settings(),
        "runtime settings snapshot should load",
    );
    settings.email.enabled = true;

    let error = err(
        registry.persist_runtime_settings(&settings).await,
        "email.enabled=true must reject incomplete SMTP settings",
    );
    assert!(
        error
            .to_string()
            .contains(synctv_core::service::EmailSmtpHostSetting::KEY),
        "expected missing smtp_host validation error, got: {error:?}"
    );
    assert!(
        service
            .get(synctv_core::service::EmailEnabledSetting::KEY)
            .await
            .is_err(),
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
    ok(
        service.initialize().await,
        "settings service should initialize",
    );
    let registry = RuntimeSettingsStore::new(service.clone());
    ok(
        registry.init(tokio_util::sync::CancellationToken::new()),
        "runtime settings store should init",
    );

    let mut settings = ok(
        registry.runtime_settings(),
        "runtime settings snapshot should load",
    );
    settings.email.enabled = true;
    settings.email.smtp_host = "smtp.example.com".to_string();
    settings.email.smtp_port = 587;
    settings.email.from_email = "noreply@example.com".to_string();

    ok(
        registry.persist_runtime_settings(&settings).await,
        "complete enabled email settings should persist",
    );

    assert_eq!(
        ok(
            service
                .get(synctv_core::service::EmailEnabledSetting::KEY)
                .await,
            "email.enabled should load"
        )
        .value,
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
