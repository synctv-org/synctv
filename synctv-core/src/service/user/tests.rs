use super::{nonnegative_i64_to_u64, UserService};
use crate::{
    models::{UserAuthFactors, UserId},
    service::{BruteForceProtection, RateLimiter},
    validation::PasswordComplexityOptions,
    Error, Result,
};
use std::{collections::HashSet, sync::Arc};

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn validate_username(username: &str) -> Result<()> {
    crate::validation::UsernameValidator::new()
        .validate(username)
        .map_err(|e| Error::InvalidInput(e.to_string()))
}

fn validate_email(email: &str) -> Result<()> {
    let email = email.trim();
    if email.is_empty() {
        return Err(Error::InvalidInput("Email cannot be empty".to_string()));
    }
    crate::validation::EmailValidator::new()
        .validate(email)
        .map_err(|e| Error::InvalidInput(e.to_string()))
}

fn validate_password(password: &str) -> Result<()> {
    crate::validation::PasswordValidator::from_options(&PasswordComplexityOptions::default())
        .validate(password)
        .map_err(|e| Error::InvalidInput(e.to_string()))
}

#[test]
fn test_validate_username_cases() {
    let max_length = "a".repeat(50);
    let too_long = "a".repeat(51);
    let cases = [
        ("abc".to_string(), true),
        ("user123".to_string(), true),
        ("user_name".to_string(), true),
        ("user-name".to_string(), true),
        ("user_name-123".to_string(), true),
        ("User123".to_string(), true),
        ("a-b-c".to_string(), true),
        ("a_b_c".to_string(), true),
        (max_length, true),
        (String::new(), false),
        ("ab".to_string(), false),
        (too_long, false),
        ("_username".to_string(), false),
        ("-username".to_string(), false),
        ("user@name".to_string(), false),
        ("user name".to_string(), false),
        ("user.name".to_string(), false),
        ("user!name".to_string(), false),
    ];

    for (username, valid) in cases {
        assert_eq!(
            validate_username(&username).is_ok(),
            valid,
            "unexpected username validation result for {username:?}"
        );
    }
}

#[test]
fn test_validate_email_cases() {
    let cases = [
        ("user@example.com", true),
        ("user.name@example.co.uk", true),
        ("user+tag@example.com", true),
        ("  user@example.com  ", true),
        ("notanemail", false),
        ("@example.com", false),
        ("user@", false),
        ("user@example", false),
        ("", false),
        ("   ", false),
    ];

    for (email, valid) in cases {
        assert_eq!(
            validate_email(email).is_ok(),
            valid,
            "unexpected email validation result for {email:?}"
        );
    }
}

#[test]
fn test_validate_password_cases() {
    let max_length = "Ab1".repeat(42) + "Ab";
    let too_long = "Ab1".repeat(43);
    assert_eq!(max_length.len(), 128);
    assert_eq!(too_long.len(), 129);

    let cases = [
        ("Password123".to_string(), true),
        ("Pass123!".to_string(), true),
        ("Abcdefg1".to_string(), true),
        (max_length, true),
        (String::new(), false),
        ("short".to_string(), false),
        ("password123".to_string(), false),
        ("PASSWORD123".to_string(), false),
        ("Passworddd".to_string(), false),
        ("Abcdef1".to_string(), false),
        (too_long, false),
    ];

    for (password, valid) in cases {
        assert_eq!(
            validate_password(&password).is_ok(),
            valid,
            "unexpected password validation result for {password:?}"
        );
    }
}

#[test]
fn test_validation_failures_return_invalid_input_error() {
    let failures = [
        validate_username("ab"),
        validate_email("notanemail"),
        validate_password("short"),
    ];

    for result in failures {
        assert!(matches!(
            err(result, "validation should fail"),
            Error::InvalidInput(_)
        ));
    }
}

#[test]
fn nonnegative_i64_to_u64_clamps_negative_values() {
    assert_eq!(nonnegative_i64_to_u64(-1), 0);
    assert_eq!(nonnegative_i64_to_u64(0), 0);
    assert_eq!(nonnegative_i64_to_u64(42), 42);
    assert_eq!(nonnegative_i64_to_u64(i64::MAX), i64::MAX as u64);
}

#[tokio::test]
async fn test_refresh_token_uses_fail_closed_distributed_rate_limiter() {
    let refresh_rate_limiter =
        Arc::new(RateLimiter::local_only("test-refresh:".to_string()).with_strict_distributed());

    let result = refresh_rate_limiter
        .check_rate_limit_distributed("refresh:user-1", 1, 60)
        .await;
    assert!(
        result.is_err(),
        "distributed refresh limit should fail closed when Redis is unavailable"
    );
}

#[tokio::test]
async fn test_refresh_rate_limiter_non_strict_preserves_best_effort_behavior() {
    let refresh_rate_limiter = Arc::new(RateLimiter::local_only("refresh-nonstrict:".to_string()));

    let result = refresh_rate_limiter
        .check_rate_limit("refresh:user-1", 1, 60)
        .await;
    assert!(
        result.is_ok(),
        "non-strict mode should allow normal in-memory checks"
    );
}

#[test]
fn test_sign_in_method_count_includes_active_oauth2() {
    let factors = UserAuthFactors {
        password: false,
        webauthn: false,
        totp: false,
        totp_recovery_codes_remaining: 0,
        email: false,
    };
    assert_eq!(UserService::sign_in_method_count(&factors, 1), 1);

    let factors = UserAuthFactors {
        password: true,
        webauthn: true,
        totp: true,
        totp_recovery_codes_remaining: 10,
        email: true,
    };
    assert_eq!(UserService::sign_in_method_count(&factors, 2), 5);
}

#[test]
fn test_oauth2_username_candidates_normalize_external_display_name() {
    let (base, candidates) = ok(
        UserService::oauth2_username_candidates("provider_user_123", " User@Special.Name! "),
        "OAuth2 username candidates should build from display name",
    );

    assert_eq!(base, "userspecialname");
    assert_eq!(candidates.len(), 10);
    assert_eq!(candidates[0], base);
    assert!(candidates[1].starts_with("userspecialname_"));
}

#[test]
fn test_oauth2_username_candidates_fallback_to_provider_id() {
    let (base, candidates) = ok(
        UserService::oauth2_username_candidates("provider_user_id_longer_than_limit", "@@@!!!"),
        "OAuth2 username candidates should fall back to provider id",
    );

    assert_eq!(base, "user_provider_user_id_lon");
    assert_eq!(candidates[0], base);
}

#[test]
fn test_count_active_oauth2_identities_filters_missing_provider_instances() {
    use crate::models::oauth2_client::{OAuth2Provider, UserOAuthProviderMapping};

    let now = crate::SystemClock.now();
    let mappings = vec![
        UserOAuthProviderMapping {
            id: 1,
            provider: OAuth2Provider::GitHub,
            provider_instance_name: "github-main".to_string(),
            provider_issuer: None,
            provider_user_id: "github-a".to_string(),
            user_id: UserId::expect_positive(42),
            username: "github-a".to_string(),
            avatar_url: None,
            created_at: now,
            updated_at: now,
        },
        UserOAuthProviderMapping {
            id: 2,
            provider: OAuth2Provider::Google,
            provider_instance_name: "removed-google".to_string(),
            provider_issuer: None,
            provider_user_id: "google-a".to_string(),
            user_id: UserId::expect_positive(42),
            username: "google-a".to_string(),
            avatar_url: None,
            created_at: now,
            updated_at: now,
        },
    ];
    let active = HashSet::from([("github-main".to_string(), OAuth2Provider::GitHub)]);

    assert_eq!(
        UserService::count_active_oauth2_identities(&mappings, &active),
        1
    );
}

#[test]
fn test_email_domain_allowed_by_whitelist_normalizes_domains() {
    assert!(ok(
        UserService::email_domain_allowed_by_whitelist("alice@example.com", "@example.com"),
        "email whitelist should allow normalized matching domain",
    ));
    assert!(ok(
        UserService::email_domain_allowed_by_whitelist("alice@example.com", "Example.COM"),
        "email whitelist should normalize case",
    ));
    assert!(ok(
        UserService::email_domain_allowed_by_whitelist("alice@example.com", ""),
        "empty email whitelist should allow all domains",
    ));
    assert!(!ok(
        UserService::email_domain_allowed_by_whitelist("alice@example.com", "other.com"),
        "email whitelist should evaluate non-matching domain",
    ));
}

#[test]
fn test_email_domain_allowed_by_whitelist_rejects_missing_domain() {
    let error = UserService::email_domain_allowed_by_whitelist("alice", "example.com")
        .expect_err("email whitelist checks require an email domain");
    assert!(error
        .to_string()
        .contains("Email must include a domain for whitelist validation"));
}

#[tokio::test]
async fn test_password_login_uses_same_brute_force_key_for_check_and_record() {
    use crate::cache::KeyBuilder;

    let prefix = "test_password_login_key_consistency";
    let key_builder = KeyBuilder::new(prefix);
    let brute_force = BruteForceProtection::in_memory(prefix.to_string());
    let identifier = "user@example.com";
    let client_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    for _ in 0..5 {
        ok(
            brute_force.record_failure(identifier, client_ip).await,
            "brute-force failure should record",
        );
    }

    let prefixed_identifier_key = key_builder.login_attempts(identifier);
    let (attempts, _) = ok(
        brute_force
            .username_tracker()
            .get_attempts(&prefixed_identifier_key)
            .await,
        "brute-force attempts should be readable",
    );
    assert_eq!(attempts, 5, "identifier bucket should accumulate failures");

    let result = brute_force.check_allowed(identifier, client_ip).await;
    assert!(result.is_err(), "same identifier bucket should be checked");
}
