use super::*;

const TEST_JWT_SECRET: &str = "test-secret-key-for-jwt-that-is-long-enough-1234567890";

fn create_jwt_service() -> JwtService {
    // Use a sufficiently long secret to pass entropy validation
    JwtService::new(TEST_JWT_SECRET).unwrap()
}

fn sign_test_refresh_token(jwt: &JwtService, user_id: &UserId) -> String {
    jwt.sign_refresh_token_with_session(
        user_id,
        0,
        None,
        "test-refresh-session",
        &TokenCredentialBinding::Password { version: 0 },
    )
    .unwrap()
}

#[test]
fn test_sign_and_verify_access_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let claims = jwt.verify_access_token(&token).unwrap();

    assert_eq!(claims.sub, user_id.to_string());
    assert!(claims.is_access_token());
}

#[test]
fn test_sign_and_verify_refresh_token() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let session_id = synctv_common::snanoid!(32);

    let token = jwt
        .sign_refresh_token_with_session(
            &user_id,
            0,
            None,
            &session_id,
            &TokenCredentialBinding::Password { version: 0 },
        )
        .unwrap();
    let claims = jwt.verify_refresh_token(&token).unwrap();

    assert_eq!(claims.sub, user_id.to_string());
    assert!(claims.is_refresh_token());
    assert_eq!(claims.sid.as_deref(), Some(session_id.as_str()));
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
fn test_token_pair_can_share_session_id() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let session_id = synctv_common::snanoid!(32);

    let access_token = jwt
        .sign_access_token_with_auth_context_and_session(
            &user_id,
            0,
            None,
            Some(&session_id),
            &TokenCredentialBinding::Password { version: 0 },
        )
        .unwrap();
    let refresh_token = jwt
        .sign_refresh_token_with_session(
            &user_id,
            0,
            None,
            &session_id,
            &TokenCredentialBinding::Password { version: 0 },
        )
        .unwrap();

    let access_claims = jwt.verify_access_token(&access_token).unwrap();
    let refresh_claims = jwt.verify_refresh_token(&refresh_token).unwrap();

    assert_eq!(access_claims.sid.as_deref(), Some(session_id.as_str()));
    assert_eq!(refresh_claims.sid.as_deref(), Some(session_id.as_str()));
}

#[test]
fn test_verify_wrong_token_type() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt.sign_access_token(&user_id, 0).unwrap();
    let result = jwt.verify_refresh_token(&access_token);
    assert!(result.is_err());

    let refresh_token = sign_test_refresh_token(&jwt, &user_id);
    let result = jwt.verify_access_token(&refresh_token);
    assert!(result.is_err());
}

#[test]
fn test_access_token_rejects_invalid_user_id_claim() {
    let jwt = create_jwt_service();
    let now = Utc::now();
    let claims = Claims {
        sub: "not-a-user-id".to_string(),
        typ: "access".to_string(),
        jti: "test-jti".to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(1)).timestamp(),
        pv: 0,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap();

    let result = jwt.verify_access_token(&token);

    assert!(matches!(result, Err(Error::Authentication(_))));
}

#[test]
fn test_guest_token_rejects_invalid_room_id_claim() {
    let jwt = create_jwt_service();
    let now = Utc::now();
    let claims = GuestClaims {
        sub: "guest:not-a-room-id:session".to_string(),
        room_id: "not-a-room-id".to_string(),
        session_id: "session".to_string(),
        jti: "test-jti".to_string(),
        typ: "guest".to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(1)).timestamp(),
        gv: 0,
        iss: None,
        aud: None,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap();

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

    let token = jwt.sign_access_token(&user_id, 0).unwrap();
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

    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();

    assert_eq!(claims.room_id().unwrap(), room_id);
    assert!(claims.is_guest());
    assert_eq!(claims.typ, "guest");
    assert!(!claims.session_id().is_empty());
    assert!(claims.sub.starts_with("guest:"));
}

#[test]
fn test_guest_token_contains_session_id() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();

    let token1 = jwt.sign_guest_token(&room_id).unwrap();
    let token2 = jwt.sign_guest_token(&room_id).unwrap();

    let claims1 = jwt.verify_guest_token(&token1).unwrap();
    let claims2 = jwt.verify_guest_token(&token2).unwrap();

    // Each guest token should have a unique session ID
    assert_ne!(claims1.session_id(), claims2.session_id());
}

#[test]
fn test_is_guest_token() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();

    let guest_token = jwt.sign_guest_token(&room_id).unwrap();
    assert!(jwt.is_guest_token(&guest_token));

    let user_id = UserId::new();
    let access_token = jwt.sign_access_token(&user_id, 0).unwrap();
    assert!(!jwt.is_guest_token(&access_token));
}

#[test]
fn test_verify_regular_token_as_guest_fails() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt.sign_access_token(&user_id, 0).unwrap();
    let result = jwt.verify_guest_token(&access_token);
    assert!(result.is_err());
}

#[test]
fn test_access_token_rejected_as_refresh() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
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
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let claims = jwt.verify_token(&token).unwrap();
    assert_eq!(claims.user_id().unwrap(), user_id);
}

#[test]
fn test_claims_type_predicates() {
    let access = Claims {
        sub: "u1".into(),
        typ: "access".into(),
        jti: String::new(),
        iat: 0,
        exp: 0,
        pv: 0,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    };
    assert!(access.is_access_token());
    assert!(!access.is_refresh_token());
    assert!(!access.is_guest_token());

    let refresh = Claims {
        sub: "u1".into(),
        typ: "refresh".into(),
        jti: String::new(),
        iat: 0,
        exp: 0,
        pv: 0,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    };
    assert!(!refresh.is_access_token());
    assert!(refresh.is_refresh_token());
    assert!(!refresh.is_guest_token());

    let guest = Claims {
        sub: "u1".into(),
        typ: "guest".into(),
        jti: String::new(),
        iat: 0,
        exp: 0,
        pv: 0,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    };
    assert!(!guest.is_access_token());
    assert!(!guest.is_refresh_token());
    assert!(guest.is_guest_token());
}

fn base_test_claims() -> Claims {
    Claims {
        sub: "1".into(),
        typ: "refresh".into(),
        jti: "test-jti".into(),
        iat: 0,
        exp: 3600,
        pv: 7,
        sid: Some("session".into()),
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    }
}

#[test]
fn test_claims_credential_binding_parses_password_binding() {
    let mut claims = base_test_claims();
    claims.cbm = Some("password".to_string());

    assert!(matches!(
        claims.credential_binding().unwrap(),
        TokenCredentialBinding::Password { version: 7 }
    ));
}

#[test]
fn test_claims_credential_binding_rejects_malformed_binding() {
    let mut missing_oauth_field = base_test_claims();
    missing_oauth_field.cbm = Some("oauth2".to_string());
    missing_oauth_field.opi = Some("github".to_string());
    assert!(matches!(
        missing_oauth_field.credential_binding(),
        Err(Error::Authentication(_))
    ));

    let mut invalid_webauthn = base_test_claims();
    invalid_webauthn.cbm = Some("webauthn".to_string());
    invalid_webauthn.wcid = Some("***not-base64url***".to_string());
    assert!(matches!(
        invalid_webauthn.credential_binding(),
        Err(Error::Authentication(_))
    ));

    let mut unknown = base_test_claims();
    unknown.cbm = Some("unknown".to_string());
    assert!(matches!(
        unknown.credential_binding(),
        Err(Error::Authentication(_))
    ));
}

#[test]
fn test_guest_claims_room_id_extraction() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();
    assert_eq!(claims.room_id().unwrap(), room_id);
}

#[test]
fn test_guest_claims_sub_format() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();
    assert!(claims.sub.starts_with("guest:"));
    assert!(claims.sub.contains(&room_id.to_string()));
}

#[test]
fn test_guest_claims_is_guest_false_for_non_guest_sub() {
    let claims = GuestClaims {
        sub: "user:some_id".into(),
        room_id: "room1".into(),
        session_id: "sess1".into(),
        jti: "test-jti".into(),
        typ: "guest".into(),
        iat: 0,
        exp: 0,
        gv: 0,
        iss: None,
        aud: None,
    };
    assert!(!claims.is_guest());
}

#[test]
fn test_token_from_different_secret_is_rejected() {
    let jwt1 = JwtService::new("secret-KEY-One-LONG-ENOUGH-1234567890!@#$").unwrap();
    let jwt2 = JwtService::new("secret-KEY-Two-LONG-ENOUGH-0987654321!@#$").unwrap();
    let user_id = UserId::new();

    let token = jwt1.sign_access_token(&user_id, 0).unwrap();
    let result = jwt2.verify_token(&token);
    assert!(result.is_err());
}

#[test]
fn test_custom_token_durations() {
    let jwt = JwtService::with_durations(
        "custom-secret-KEY-Long-ENOUGH-1234567890!@#$%^&*()",
        2,  // 2 hour access
        7,  // 7 day refresh
        1,  // 1 hour guest
        30, // 30 second leeway
    )
    .unwrap();

    let user_id = UserId::new();
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let claims = jwt.verify_token(&token).unwrap();

    // Verify token has exp roughly 2 hours from iat
    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 7200); // 2 hours in seconds
}

#[test]
fn test_refresh_token_duration() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();
    let token = sign_test_refresh_token(&jwt, &user_id);
    let claims = jwt.verify_token(&token).unwrap();
    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 30 * 86400); // 30 days in seconds
}

#[test]
fn test_expired_token_is_rejected() {
    let secret = "expired-TOKEN-Test-SECRET-1234567890!@#$%^&*()";
    let jwt = JwtService::with_durations(secret, 1, 1, 1, 0).unwrap();

    // Manually craft a token with exp in the past
    let past = Utc::now() - Duration::hours(2);
    let claims = Claims {
        sub: "expired_user".into(),
        typ: "access".into(),
        jti: "test-jti".into(),
        iat: (past - Duration::hours(3)).timestamp(),
        exp: past.timestamp(), // expired 2 hours ago
        pv: 0,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap();

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

    let token1 = jwt.sign_access_token(&user_id, 0).unwrap();
    let token2 = jwt.sign_access_token(&user_id, 0).unwrap();

    let claims1 = jwt.verify_token(&token1).unwrap();
    let claims2 = jwt.verify_token(&token2).unwrap();

    assert_ne!(
        claims1.jti, claims2.jti,
        "Each token should have a unique jti"
    );
    assert!(!claims1.jti.is_empty());
    assert!(!claims2.jti.is_empty());
}

#[test]
fn test_token_iat_is_recent() {
    let jwt = create_jwt_service();
    let user_id = UserId::new();

    let before = Utc::now().timestamp();
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let after = Utc::now().timestamp();

    let claims = jwt.verify_token(&token).unwrap();
    assert!(claims.iat >= before && claims.iat <= after);
}

#[test]
fn test_guest_token_duration() {
    // Default guest token duration is 4 hours
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();
    let duration = claims.exp - claims.iat;
    assert_eq!(duration, 4 * 3600); // 4 hours in seconds
}

#[test]
fn test_sign_and_verify_custom_token() {
    let jwt = create_jwt_service();
    let claims = serde_json::json!({
        "sub": "custom_subject",
        "custom_field": "custom_value",
    });

    let token = jwt.sign_custom(&claims).unwrap();
    let verified: serde_json::Value = jwt.verify_custom(&token).unwrap();

    assert_eq!(verified["sub"], "custom_subject");
    assert_eq!(verified["custom_field"], "custom_value");
    assert!(verified.get("jti").is_some());
    assert!(verified.get("iat").is_some());
    assert!(verified.get("exp").is_some());
}

#[test]
fn test_custom_token_wrong_secret_rejected() {
    let jwt1 = JwtService::new("custom-SECRET-One-LONG-ENOUGH-1234567890!@#$").unwrap();
    let jwt2 = JwtService::new("custom-SECRET-Two-LONG-ENOUGH-0987654321!@#$").unwrap();

    let claims = serde_json::json!({"sub": "test"});
    let token = jwt1.sign_custom(&claims).unwrap();
    let result = jwt2.verify_custom(&token);
    assert!(result.is_err());
}

#[test]
fn test_token_with_issuer_and_audience() {
    let jwt = JwtService::with_durations_and_claims(
        "secret-with-issuer-aud-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        Some("synctv".to_string()),
        Some("synctv-api".to_string()),
    )
    .unwrap();

    let user_id = UserId::new();
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let claims = jwt.verify_token(&token).unwrap();

    assert_eq!(claims.iss.as_deref(), Some("synctv"));
    assert_eq!(claims.aud.as_deref(), Some("synctv-api"));
}

#[test]
fn test_token_without_issuer_accepted_when_no_issuer_expected() {
    // Service without issuer validation
    let jwt = JwtService::new("secret-no-issuer-validation-LONG-ENOUGH-1234567890").unwrap();
    let user_id = UserId::new();
    let token = jwt.sign_access_token(&user_id, 0).unwrap();
    let result = jwt.verify_token(&token);
    assert!(result.is_ok());
}

#[test]
fn test_token_with_wrong_issuer_rejected() {
    // Service that expects "synctv" as issuer
    let jwt_expected = JwtService::with_durations_and_claims(
        "secret-issuer-check-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        Some("synctv".to_string()),
        None,
    )
    .unwrap();

    // Service that signs tokens with different issuer
    let jwt_other = JwtService::with_durations_and_claims(
        "secret-issuer-check-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        Some("other-service".to_string()),
        None,
    )
    .unwrap();

    let user_id = UserId::new();
    let token = jwt_other.sign_access_token(&user_id, 0).unwrap();
    let result = jwt_expected.verify_token(&token);

    assert!(
        result.is_err(),
        "Token with wrong issuer should be rejected"
    );
}

#[test]
fn test_token_with_wrong_audience_rejected() {
    // Service that expects "synctv-api" as audience
    let jwt_expected = JwtService::with_durations_and_claims(
        "secret-aud-check-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        None,
        Some("synctv-api".to_string()),
    )
    .unwrap();

    // Service that signs tokens with different audience
    let jwt_other = JwtService::with_durations_and_claims(
        "secret-aud-check-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        None,
        Some("other-audience".to_string()),
    )
    .unwrap();

    let user_id = UserId::new();
    let token = jwt_other.sign_access_token(&user_id, 0).unwrap();
    let result = jwt_expected.verify_token(&token);

    assert!(
        result.is_err(),
        "Token with wrong audience should be rejected"
    );
}

#[test]
fn test_guest_token_with_issuer_and_audience() {
    let jwt = JwtService::with_durations_and_claims(
        "guest-issuer-aud-secret-LONG-ENOUGH-1234567890!@#$%",
        1,
        30,
        4,
        60,
        Some("synctv".to_string()),
        Some("synctv-guest".to_string()),
    )
    .unwrap();

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();

    assert_eq!(claims.iss.as_deref(), Some("synctv"));
    assert_eq!(claims.aud.as_deref(), Some("synctv-guest"));
}

#[test]
fn test_guest_token_without_issuer_accepted_when_no_issuer_expected() {
    let jwt = JwtService::new("guest-no-issuer-secret-LONG-ENOUGH-1234567890!@#").unwrap();
    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();
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
fn test_weak_secret_repeated_pattern_rejected() {
    // Repeated pattern "abcabcabcabcabcabcabcabcabcabcab"
    let result = JwtService::new("abcabcabcabcabcabcabcabcabcabcab");
    assert!(
        result.is_err(),
        "Repeated pattern secret should be rejected"
    );
}

#[test]
fn test_weak_secret_keyboard_walk_rejected() {
    // Keyboard walk pattern with insufficient variety
    // "qwerty" repeated - this is a simple repeating pattern
    let result = JwtService::new("qwertyqwertyqwertyqwertyqwerty12");
    assert!(result.is_err(), "Repeated keyboard walk should be rejected");
}

#[test]
fn test_weak_secret_sequential_rejected() {
    // Sequential characters - fully sequential alphabet
    let result = JwtService::new("abcdefghijklmnopqrstuvwxyz123456");
    assert!(
        result.is_err(),
        "Sequential character secret should be rejected"
    );
}

#[test]
fn test_weak_secret_numeric_only_rejected() {
    // Numeric only - even if long enough
    let result = JwtService::new("1234567890123456789012345678901234567890");
    assert!(result.is_err(), "Numeric-only secret should be rejected");
}

#[test]
fn test_weak_secret_repeated_word_rejected() {
    // Repeated low-variety word pattern with padding.
    let result = JwtService::new("passpasspasspasspasspasspass12");
    assert!(
        result.is_err(),
        "Repeated low-variety secret should be rejected"
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
fn test_weak_secret_low_unique_chars_rejected() {
    // Low unique character count (mostly repeated chars)
    let result = JwtService::new("aabbccddaabbccddaabbccddaabbccdd");
    assert!(
        result.is_err(),
        "Low unique character secret should be rejected"
    );
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
