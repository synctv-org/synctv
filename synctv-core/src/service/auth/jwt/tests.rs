use super::*;

const TEST_JWT_SECRET: &str = "test-secret-key-for-jwt-that-is-long-enough-1234567890";

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn create_jwt_service() -> JwtService {
    ok(JwtService::new(TEST_JWT_SECRET), "JWT service should build")
}

fn sign_test_refresh_token(jwt: &JwtService, user_id: &UserId) -> String {
    ok(
        jwt.sign_refresh_token_with_session(
            user_id,
            0,
            None,
            "test-refresh-session",
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "refresh token should sign",
    )
}

fn sign_access_token(jwt: &JwtService, user_id: &UserId) -> String {
    ok(
        jwt.sign_access_token(user_id, 0),
        "access token should sign",
    )
}

fn sign_guest_token(jwt: &JwtService, room_id: &RoomId) -> String {
    ok(jwt.sign_guest_token(room_id), "guest token should sign")
}

fn jwt_with_claims(secret: &str, issuer: Option<&str>, audience: Option<&str>) -> JwtService {
    ok(
        JwtService::with_durations_and_claims(
            secret,
            1,
            30,
            4,
            60,
            issuer.map(str::to_string),
            audience.map(str::to_string),
        ),
        "JWT service with issuer settings should build",
    )
}

#[test]
fn test_sign_and_verify_access_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let token = sign_access_token(&jwt, &user_id);
    let claims = ok(
        jwt.verify_access_token(&token),
        "access token should verify",
    );

    assert_eq!(claims.user_id(), user_id);
    assert_eq!(claims.token_type(), TokenType::Access);
}

#[test]
fn test_sign_and_verify_refresh_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let session_id = synctv_common::snanoid!(32);

    let token = ok(
        jwt.sign_refresh_token_with_session(
            &user_id,
            0,
            None,
            &session_id,
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "refresh token should sign",
    );
    let claims = ok(
        jwt.verify_refresh_token(&token),
        "refresh token should verify",
    );

    assert_eq!(claims.user_id(), user_id);
    assert_eq!(claims.token_type(), TokenType::Refresh);
    assert_eq!(claims.session_id(), Some(session_id.as_str()));
}

#[test]
fn test_refresh_token_without_session_id_is_rejected() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let result = jwt.sign_refresh_token_with_session(
        &user_id,
        0,
        None,
        "",
        &TokenCredentialBinding::Password { version: 0 },
    );

    assert!(matches!(result, Err(Error::InvalidInput(message)) if message.contains("session id")));
}

#[test]
fn test_empty_token_identity_fields_are_rejected_during_decode() {
    let jwt = create_jwt_service();
    let now = crate::SystemClock.now();
    let user_id = UserId::new().to_string();
    let cases = [
        serde_json::json!({
            "sub": user_id,
            "typ": "access",
            "jti": "",
            "iat": now.timestamp(),
            "exp": (now + Duration::hours(1)).timestamp(),
            "pv": 0,
            "cbm": "password"
        }),
        serde_json::json!({
            "sub": user_id,
            "typ": "refresh",
            "sid": "",
            "jti": "test-jti",
            "iat": now.timestamp(),
            "exp": (now + Duration::hours(1)).timestamp(),
            "pv": 0,
            "cbm": "password"
        }),
        serde_json::json!({
            "sub": user_id,
            "typ": "access",
            "sid": "  ",
            "jti": "test-jti",
            "iat": now.timestamp(),
            "exp": (now + Duration::hours(1)).timestamp(),
            "pv": 0,
            "cbm": "password"
        }),
    ];

    for claims in cases {
        let token = ok(
            encode(
                &Header::new(Algorithm::HS256),
                &claims,
                &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
            ),
            "malformed token should encode",
        );
        assert!(matches!(
            jwt.verify_token(&token),
            Err(Error::Authentication(_))
        ));
    }
}

#[test]
fn test_token_pair_can_share_session_id() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let session_id = synctv_common::snanoid!(32);

    let access_token = ok(
        jwt.sign_access_token_with_auth_context_and_session(
            &user_id,
            0,
            None,
            Some(&session_id),
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "access token with session should sign",
    );
    let refresh_token = ok(
        jwt.sign_refresh_token_with_session(
            &user_id,
            0,
            None,
            &session_id,
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "refresh token with session should sign",
    );

    let access_claims = ok(
        jwt.verify_access_token(&access_token),
        "access token should verify",
    );
    let refresh_claims = ok(
        jwt.verify_refresh_token(&refresh_token),
        "refresh token should verify",
    );

    assert_eq!(access_claims.session_id(), Some(session_id.as_str()));
    assert_eq!(refresh_claims.session_id(), Some(session_id.as_str()));
}

#[test]
fn test_verify_wrong_token_type() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let access_token = sign_access_token(&jwt, &user_id);
    let result = jwt.verify_refresh_token(&access_token);
    assert!(result.is_err());

    let refresh_token = sign_test_refresh_token(&jwt, &user_id);
    let result = jwt.verify_access_token(&refresh_token);
    assert!(result.is_err());
}

#[test]
fn test_access_token_rejects_invalid_user_id_claim() {
    let jwt = create_jwt_service();
    let now = crate::SystemClock.now();
    let claims = serde_json::json!({
        "sub": "not-a-user-id",
        "typ": "access",
        "jti": "test-jti",
        "iat": now.timestamp(),
        "exp": (now + Duration::hours(1)).timestamp(),
        "pv": 0,
        "cbm": "password"
    });
    let token = ok(
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        ),
        "manually encoded access token should sign",
    );

    let result = jwt.verify_access_token(&token);

    assert!(matches!(result, Err(Error::Authentication(_))));
}

#[test]
fn test_guest_token_rejects_invalid_room_id_claim() {
    let jwt = create_jwt_service();
    let now = crate::SystemClock.now();
    let claims = serde_json::json!({
        "sub": "guest:not-a-room-id:session",
        "typ": "guest",
        "room_id": "not-a-room-id",
        "session_id": "session",
        "jti": "test-jti",
        "iat": now.timestamp(),
        "exp": (now + Duration::hours(1)).timestamp(),
        "gv": 0
    });
    let token = ok(
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        ),
        "manually encoded guest token should sign",
    );

    let result = jwt.verify_guest_token(&token);

    assert!(matches!(result, Err(Error::Authentication(_))));
}

#[test]
fn test_invalid_token() {
    let jwt = create_jwt_service();
    let result = jwt.verify_token("invalid.token.here");
    assert!(result.is_err());
}

#[test]
fn test_tampered_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let token = sign_access_token(&jwt, &user_id);
    let mut parts: Vec<&str> = token.split('.').collect();
    parts[1] = "tampered_payload";
    let tampered_token = parts.join(".");

    let result = jwt.verify_token(&tampered_token);
    assert!(result.is_err());
}

#[test]
fn test_empty_secret() {
    let result = JwtService::new("");
    assert!(result.is_err());
}

#[test]
fn test_sign_and_verify_guest_token() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();

    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");

    assert_eq!(claims.room_id(), room_id);
    assert!(!claims.session_id().is_empty());
}

#[test]
fn test_guest_token_contains_session_id() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();

    let token1 = sign_guest_token(&jwt, &room_id);
    let token2 = sign_guest_token(&jwt, &room_id);

    let claims1 = ok(
        jwt.verify_guest_token(&token1),
        "first guest token should verify",
    );
    let claims2 = ok(
        jwt.verify_guest_token(&token2),
        "second guest token should verify",
    );

    assert_ne!(claims1.session_id(), claims2.session_id());
}

#[test]
fn test_is_guest_token() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();

    let guest_token = sign_guest_token(&jwt, &room_id);
    assert!(jwt.is_guest_token(&guest_token));

    let user_id = UserId::new();
    let access_token = sign_access_token(&jwt, &user_id);
    assert!(!jwt.is_guest_token(&access_token));
}

#[test]
fn test_verify_regular_token_as_guest_fails() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let access_token = sign_access_token(&jwt, &user_id);
    let result = jwt.verify_guest_token(&access_token);
    assert!(result.is_err());
}

#[test]
fn test_access_token_rejected_as_refresh() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = sign_access_token(&jwt, &user_id);
    let result = jwt.verify_refresh_token(&token);
    assert!(result.is_err());
}

#[test]
fn test_refresh_token_rejected_as_access() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = sign_test_refresh_token(&jwt, &user_id);
    let result = jwt.verify_access_token(&token);
    assert!(result.is_err());
}

#[test]
fn test_claims_user_id_extraction() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = sign_access_token(&jwt, &user_id);
    let claims = ok(jwt.verify_token(&token), "token should verify");
    assert_eq!(claims.user_id(), user_id);
}

#[test]
fn test_claims_credential_binding_round_trips_typed_variants() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let bindings = [
        TokenCredentialBinding::Password { version: 7 },
        TokenCredentialBinding::OAuth2 {
            provider_instance_name: "github".to_string(),
            provider_user_id: "provider-user".to_string(),
        },
        TokenCredentialBinding::WebAuthn {
            credential_id: vec![0, 1, 2, 253, 254, 255],
        },
        TokenCredentialBinding::Email {
            email: "user@example.com".to_string(),
        },
    ];

    for binding in bindings {
        let token = ok(
            jwt.sign_refresh_token_with_session(&user_id, 7, None, "session", &binding),
            "refresh token should sign",
        );
        let claims = ok(
            jwt.verify_refresh_token(&token),
            "refresh token should verify",
        );
        assert_eq!(claims.credential_binding(), binding);
    }
}

#[test]
fn test_claims_credential_binding_rejects_malformed_binding() {
    let jwt = create_jwt_service();
    let now = crate::SystemClock.now();
    let base = serde_json::json!({
        "sub": UserId::new().to_string(),
        "typ": "access",
        "jti": "test-jti",
        "iat": now.timestamp(),
        "exp": (now + Duration::hours(1)).timestamp(),
        "pv": 0
    });

    for binding in [
        serde_json::json!({"cbm": "oauth2", "opi": "github"}),
        serde_json::json!({"cbm": "unknown"}),
    ] {
        let mut claims = base.clone();
        claims
            .as_object_mut()
            .expect("test claims should be an object")
            .extend(
                binding
                    .as_object()
                    .expect("binding should be an object")
                    .clone(),
            );
        let token = ok(
            encode(
                &Header::new(Algorithm::HS256),
                &claims,
                &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
            ),
            "malformed token should encode",
        );

        assert!(matches!(
            jwt.verify_access_token(&token),
            Err(Error::Authentication(_))
        ));
    }

    let mut invalid_webauthn = base;
    invalid_webauthn
        .as_object_mut()
        .expect("test claims should be an object")
        .extend(
            serde_json::json!({"cbm": "webauthn", "wcid": "***not-base64url***"})
                .as_object()
                .expect("binding should be an object")
                .clone(),
        );
    let token = ok(
        encode(
            &Header::new(Algorithm::HS256),
            &invalid_webauthn,
            &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        ),
        "malformed token should encode",
    );
    assert!(matches!(
        jwt.verify_access_token(&token),
        Err(Error::Authentication(_))
    ));
}

#[test]
fn test_guest_claims_room_id_extraction() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");
    assert_eq!(claims.room_id(), room_id);
}

#[test]
fn test_guest_claims_sub_format() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");
    let serialized = ok(
        serde_json::to_value(claims),
        "guest claims should serialize",
    );
    assert!(serialized["sub"]
        .as_str()
        .is_some_and(|subject| subject.starts_with(&format!("guest:{room_id}:"))));
}

#[test]
fn test_token_from_different_secret_is_rejected() {
    let jwt1 = ok(
        JwtService::new("secret-KEY-One-LONG-ENOUGH-1234567890!@#$"),
        "first JWT service should build",
    );
    let jwt2 = ok(
        JwtService::new("secret-KEY-Two-LONG-ENOUGH-0987654321!@#$"),
        "second JWT service should build",
    );
    let user_id = UserId::new();

    let token = sign_access_token(&jwt1, &user_id);
    let result = jwt2.verify_token(&token);
    assert!(result.is_err());
}

#[test]
fn test_custom_token_durations() {
    let jwt = ok(
        JwtService::with_durations(
            "custom-secret-KEY-Long-ENOUGH-1234567890!@#$%^&*()",
            2,
            7,
            1,
            30,
        ),
        "JWT service with custom durations should build",
    );

    let user_id = UserId::new();
    let token = sign_access_token(&jwt, &user_id);
    let claims = ok(jwt.verify_token(&token), "token should verify");

    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 7200);
}

#[test]
fn test_refresh_token_duration() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = sign_test_refresh_token(&jwt, &user_id);
    let claims = ok(jwt.verify_token(&token), "refresh token should verify");
    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 30 * 86400);
}

#[test]
fn test_expired_token_is_rejected() {
    let secret = "expired-TOKEN-Test-SECRET-1234567890!@#$%^&*()";
    let jwt = ok(
        JwtService::with_durations(secret, 1, 1, 1, 0),
        "JWT service with no leeway should build",
    );

    let past = crate::SystemClock.now() - Duration::hours(2);
    let claims = serde_json::json!({
        "sub": UserId::new().to_string(),
        "typ": "access",
        "jti": "test-jti",
        "iat": (past - Duration::hours(3)).timestamp(),
        "exp": past.timestamp(),
        "pv": 0,
        "cbm": "password"
    });
    let token = ok(
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        ),
        "expired token should encode",
    );

    let result = jwt.verify_token(&token);
    assert!(result.is_err(), "Expired token should be rejected");
}

#[test]
fn test_map_jwt_error_hides_unexpected_verification_details_for_tokens() {
    let error = map_jwt_error(
        &jsonwebtoken::errors::Error::from(jsonwebtoken::errors::ErrorKind::InvalidIssuer),
        "Token",
    );

    assert!(matches!(
        error,
        Error::Authentication(ref message) if message == "Invalid or expired token"
    ));
}

#[test]
fn test_map_jwt_error_hides_unexpected_verification_details_for_guest_tokens() {
    let error = map_jwt_error(
        &jsonwebtoken::errors::Error::from(jsonwebtoken::errors::ErrorKind::InvalidAudience),
        "Guest token",
    );

    assert!(matches!(
        error,
        Error::Authentication(ref message) if message == "Invalid or expired guest token"
    ));
}

#[test]
fn test_jti_is_unique_per_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let token1 = sign_access_token(&jwt, &user_id);
    let token2 = sign_access_token(&jwt, &user_id);

    let claims1 = ok(jwt.verify_token(&token1), "first token should verify");
    let claims2 = ok(jwt.verify_token(&token2), "second token should verify");

    assert_ne!(
        claims1.token_id(),
        claims2.token_id(),
        "Each token should have a unique jti"
    );
    assert!(!claims1.token_id().is_empty());
    assert!(!claims2.token_id().is_empty());
}

#[test]
fn test_token_iat_is_recent() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let before = crate::SystemClock.now().timestamp();
    let token = sign_access_token(&jwt, &user_id);
    let after = crate::SystemClock.now().timestamp();

    let claims = ok(jwt.verify_token(&token), "token should verify");
    assert!(claims.iat >= before && claims.iat <= after);
}

#[test]
fn test_guest_token_duration() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");
    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 4 * 3600);
}

#[test]
fn test_sign_and_verify_custom_token() {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct CustomClaims {
        sub: String,
        custom_field: String,
        exp: i64,
    }

    let jwt = create_jwt_service();
    let claims = CustomClaims {
        sub: "custom_subject".to_string(),
        custom_field: "custom_value".to_string(),
        exp: crate::SystemClock.now().timestamp() + 3600,
    };

    let token = ok(jwt.sign_custom(&claims), "custom token should sign");
    let verified: CustomClaims = ok(jwt.verify_custom(&token), "custom token should verify");

    assert_eq!(verified.sub, "custom_subject");
    assert_eq!(verified.custom_field, "custom_value");
    assert_eq!(verified.exp, claims.exp);
}

#[test]
fn test_custom_token_wrong_secret_rejected() {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct CustomClaims {
        sub: String,
        exp: i64,
    }

    let jwt1 = ok(
        JwtService::new("custom-SECRET-One-LONG-ENOUGH-1234567890!@#$"),
        "first custom JWT service should build",
    );
    let jwt2 = ok(
        JwtService::new("custom-SECRET-Two-LONG-ENOUGH-0987654321!@#$"),
        "second custom JWT service should build",
    );

    let claims = CustomClaims {
        sub: "test".to_string(),
        exp: crate::SystemClock.now().timestamp() + 3600,
    };
    let token = ok(jwt1.sign_custom(&claims), "custom token should sign");
    let result = jwt2.verify_custom::<CustomClaims>(&token);
    assert!(result.is_err());
}

#[test]
fn test_token_with_issuer_and_audience() {
    let jwt = jwt_with_claims(
        "secret-with-issuer-aud-LONG-ENOUGH-1234567890!@#$%",
        Some("synctv"),
        Some("synctv-api"),
    );

    let user_id = UserId::new();
    let token = sign_access_token(&jwt, &user_id);
    let claims = ok(jwt.verify_token(&token), "token should verify");

    assert_eq!(claims.iss.as_deref(), Some("synctv"));
    assert_eq!(claims.aud.as_deref(), Some("synctv-api"));
}

#[test]
fn test_token_without_issuer_accepted_when_no_issuer_expected() {
    let jwt = ok(
        JwtService::new("secret-no-issuer-validation-LONG-ENOUGH-1234567890"),
        "JWT service without issuer validation should build",
    );
    let user_id = UserId::new();
    let token = sign_access_token(&jwt, &user_id);
    let result = jwt.verify_token(&token);
    assert!(result.is_ok());
}

#[test]
fn test_token_with_wrong_issuer_rejected() {
    let jwt_expected = jwt_with_claims(
        "secret-issuer-check-LONG-ENOUGH-1234567890!@#$%",
        Some("synctv"),
        None,
    );

    let jwt_other = jwt_with_claims(
        "secret-issuer-check-LONG-ENOUGH-1234567890!@#$%",
        Some("other-service"),
        None,
    );

    let user_id = UserId::new();
    let token = sign_access_token(&jwt_other, &user_id);
    let result = jwt_expected.verify_token(&token);

    assert!(
        result.is_err(),
        "Token with wrong issuer should be rejected"
    );
}

#[test]
fn test_token_with_wrong_audience_rejected() {
    let jwt_expected = jwt_with_claims(
        "secret-aud-check-LONG-ENOUGH-1234567890!@#$%",
        None,
        Some("synctv-api"),
    );

    let jwt_other = jwt_with_claims(
        "secret-aud-check-LONG-ENOUGH-1234567890!@#$%",
        None,
        Some("other-audience"),
    );

    let user_id = UserId::new();
    let token = sign_access_token(&jwt_other, &user_id);
    let result = jwt_expected.verify_token(&token);

    assert!(
        result.is_err(),
        "Token with wrong audience should be rejected"
    );
}

#[test]
fn test_guest_token_with_issuer_and_audience() {
    let jwt = jwt_with_claims(
        "guest-issuer-aud-secret-LONG-ENOUGH-1234567890!@#$%",
        Some("synctv"),
        Some("synctv-guest"),
    );

    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");

    assert_eq!(claims.iss.as_deref(), Some("synctv"));
    assert_eq!(claims.aud.as_deref(), Some("synctv-guest"));
}

#[test]
fn test_guest_token_without_issuer_accepted_when_no_issuer_expected() {
    let jwt = ok(
        JwtService::new("guest-no-issuer-secret-LONG-ENOUGH-1234567890!@#"),
        "guest JWT service without issuer validation should build",
    );
    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let result = jwt.verify_guest_token(&token);
    assert!(result.is_ok());
}

#[test]
fn test_weak_secret_too_short_rejected() {
    // Less than 32 characters
    let result = JwtService::new("short-secret-123");
    assert!(result.is_err(), "Short secret should be rejected");
}

#[test]
fn test_weak_secret_all_same_character_rejected() {
    // All same character - no entropy
    let result = JwtService::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(
        result.is_err(),
        "All-same-character secret should be rejected"
    );
}

#[test]
fn test_weak_secret_repeated_pattern_allowed_with_warning() {
    let result = JwtService::new("abc1abc1abc1abc1abc1abc1abc1abc1");
    assert!(
        result.is_ok(),
        "Repeated pattern secret should be allowed: {result:?}"
    );
}

#[test]
fn test_weak_secret_keyboard_walk_allowed_with_warning() {
    // Keyboard walk pattern with insufficient variety
    // "qwerty" repeated - this is a simple repeating pattern
    let result = JwtService::new("qwertyqwertyqwertyqwertyqwerty12");
    assert!(
        result.is_ok(),
        "Repeated keyboard walk should be allowed: {result:?}"
    );
}

#[test]
fn test_weak_secret_sequential_allowed_with_warning() {
    // Sequential characters - fully sequential alphabet
    let result = JwtService::new("abcdefghijklmnopqrstuvwxyz123456");
    assert!(
        result.is_ok(),
        "Sequential character secret should be allowed: {result:?}"
    );
}

#[test]
fn test_weak_secret_numeric_only_rejected() {
    // Numeric only - even if long enough
    let result = JwtService::new("1234567890123456789012345678901234567890");
    assert!(result.is_err(), "Numeric-only secret should be rejected");
}

#[test]
fn test_weak_secret_repeated_word_allowed_with_warning() {
    let result = JwtService::new("passpasspasspasspasspasspasspass12");
    assert!(
        result.is_ok(),
        "Repeated low-variety secret should be allowed: {result:?}"
    );
}

#[test]
fn test_strong_secret_base64_accepted() {
    // Strong base64-like secret (32+ chars, high entropy)
    let result = JwtService::new("kL9mN2pQ5rT8vW1xY4zA7bC0dE3fG6hJ9");
    assert!(result.is_ok(), "Strong base64 secret should be accepted");
}

#[test]
fn test_strong_secret_mixed_accepted() {
    // Strong mixed character secret
    let result = JwtService::new("My-Super-Secret-Key-2024!@#$%^&*()XYZ");
    assert!(
        result.is_ok(),
        "Strong mixed character secret should be accepted"
    );
}

#[test]
fn test_strong_secret_random_hex_accepted() {
    // Strong random hex-like string (64 hex chars = 256 bits)
    let result =
        JwtService::new("a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456");
    assert!(result.is_ok(), "Strong hex secret should be accepted");
}

#[test]
fn test_strong_secret_with_spaces_accepted() {
    // Strong secret with spaces (phrase-like but long enough)
    let result = JwtService::new("This is a very secure JWT secret key 2024!");
    assert!(result.is_ok(), "Long phrase secret should be accepted");
}

#[test]
fn test_weak_secret_low_unique_chars_allowed_with_warning() {
    let result = JwtService::new("aabbccdd11aabbccdd11aabbccdd11aabbccdd11");
    assert!(
        result.is_ok(),
        "Low unique character secret should be allowed: {result:?}"
    );
}

#[test]
fn test_low_unique_character_ratio_is_non_blocking() {
    let secret = concat!(
        "aA0!bB1?cC2#",
        "A0!bB1?cC2#a",
        "0!bB1?cC2#aA",
        "!bB1?cC2#aA0",
        "bB1?cC2#aA0!",
        "B1?cC2#aA0!b",
        "1?cC2#aA0!bB",
        "?cC2#aA0!bB1"
    );

    let result = JwtService::new(secret);

    assert!(
        result.is_ok(),
        "Low unique character ratio should emit a warning without rejecting the secret: {result:?}"
    );
}

#[test]
fn test_non_ascii_secret_is_supported() {
    let result = JwtService::new(concat!(
        "\u{4e2d}a\u{6587}b\u{5bc6}c\u{94a5}d\u{6d4b}e\u{8bd5}f",
        "\u{4e2d}a\u{6587}b\u{5bc6}c\u{94a5}d\u{6d4b}e\u{8bd5}f",
        "\u{4e2d}a\u{6587}b\u{5bc6}c\u{94a5}d\u{6d4b}e\u{8bd5}f"
    ));

    assert!(
        result.is_ok(),
        "Non-ASCII secret should not panic or be rejected: {result:?}"
    );
}

#[test]
fn test_shannon_entropy_counts_characters_consistently() {
    let entropy = ok(
        JwtService::calculate_shannon_entropy("a\u{4e2d}"),
        "entropy should be calculated",
    );

    assert!((entropy - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_strong_secret_minimum_length_accepted() {
    // Exactly 32 characters with good variety
    let result = JwtService::new("Ab3Cd4Ef5Gh6Ij7Kl8Mn9Op0Qr1St2Uv");
    assert!(
        result.is_ok(),
        "Minimum length with variety should be accepted"
    );
}
