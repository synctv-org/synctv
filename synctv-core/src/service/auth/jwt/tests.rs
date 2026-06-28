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

    assert_eq!(claims.sub, user_id.to_string());
    assert!(claims.is_access_token());
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

    assert_eq!(access_claims.sid.as_deref(), Some(session_id.as_str()));
    assert_eq!(refresh_claims.sid.as_deref(), Some(session_id.as_str()));
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

    assert_eq!(ok(claims.room_id(), "guest room ID should parse"), room_id);
    assert!(claims.is_guest());
    assert_eq!(claims.typ, "guest");
    assert!(!claims.session_id().is_empty());
    assert!(claims.sub.starts_with("guest:"));
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
    assert_eq!(ok(claims.user_id(), "user ID claim should parse"), user_id);
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
        ok(
            claims.credential_binding(),
            "credential binding should parse"
        ),
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
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");
    assert_eq!(ok(claims.room_id(), "guest room ID should parse"), room_id);
}

#[test]
fn test_guest_claims_sub_format() {
    let jwt = create_jwt_service();
    let room_id = RoomId::new();
    let token = sign_guest_token(&jwt, &room_id);
    let claims = ok(jwt.verify_guest_token(&token), "guest token should verify");
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

    let past = Utc::now() - Duration::hours(2);
    let claims = Claims {
        sub: "expired_user".into(),
        typ: "access".into(),
        jti: "test-jti".into(),
        iat: (past - Duration::hours(3)).timestamp(),
        exp: past.timestamp(),
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
    let token = sign_access_token(&jwt, &user_id);
    let after = Utc::now().timestamp();

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
        exp: Utc::now().timestamp() + 3600,
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
        exp: Utc::now().timestamp() + 3600,
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
