use chrono::{Duration, Utc};
use jsonwebtoken::{
    decode, encode, Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{models::{UserId, RoomId}, Error, Result, InternalExt};

/// JWT token type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Access,  // default: 1 hour (configurable)
    Refresh, // default: 30 days (configurable)
    Guest,   // default: 4 hours (configurable)
}

/// JWT claims structure
///
/// Note: Does NOT contain role/permissions - these must be fetched from database in real-time
/// to ensure current permissions are enforced (roles can change after token issuance)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// User ID
    pub sub: String,
    /// Token type (access or refresh)
    pub typ: String,
    /// JWT ID (unique token identifier for efficient blacklisting)
    #[serde(default)]
    pub jti: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expiration time (Unix timestamp)
    pub exp: i64,
}

impl Claims {
    #[must_use]
    pub fn user_id(&self) -> UserId {
        UserId::from_string(self.sub.clone())
    }

    #[must_use]
    pub fn is_access_token(&self) -> bool {
        self.typ == "access"
    }

    #[must_use]
    pub fn is_refresh_token(&self) -> bool {
        self.typ == "refresh"
    }

    #[must_use]
    pub fn is_guest_token(&self) -> bool {
        self.typ == "guest"
    }
}

/// Guest token claims structure (stateless guest authentication)
///
/// Guest tokens contain the room ID and a random session ID instead of a user ID.
/// Format: `guest:{room_id}:{session_id}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestClaims {
    /// Guest subject (format: "`guest:{room_id}:{session_id`}")
    pub sub: String,
    /// Room ID
    pub room_id: String,
    /// Random session ID for this guest
    pub session_id: String,
    /// Token type (always "guest")
    pub typ: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expiration time (Unix timestamp)
    pub exp: i64,
}

impl GuestClaims {
    /// Parse room ID from claims
    #[must_use]
    pub fn room_id(&self) -> RoomId {
        RoomId::from_string(self.room_id.clone())
    }

    /// Get session ID
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Check if this is a guest token
    #[must_use]
    pub fn is_guest(&self) -> bool {
        self.sub.starts_with("guest:")
    }
}

/// JWT service for signing and verifying tokens
#[derive(Clone)]
pub struct JwtService {
    encoding_key: Arc<EncodingKey>,
    decoding_key: Arc<DecodingKey>,
    algorithm: Algorithm,
    access_token_duration_hours: u64,
    refresh_token_duration_days: u64,
    guest_token_duration_hours: u64,
    clock_skew_leeway_secs: u64,
}

impl std::fmt::Debug for JwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtService")
            .field("algorithm", &self.algorithm)
            .finish()
    }
}

/// Minimum entropy bits required for JWT secret (256 bits = 32 bytes)
const MIN_JWT_SECRET_ENTROPY_BITS: usize = 256;

impl JwtService {
    /// Create a new JWT service with HS256 secret and configurable token durations
    ///
    /// # Arguments
    /// * `secret` - Secret string for HMAC signing
    /// * `access_token_duration_hours` - Access token lifetime in hours (default: 1)
    /// * `refresh_token_duration_days` - Refresh token lifetime in days (default: 30)
    ///
    /// # Security
    /// The secret must have sufficient entropy (at least 256 bits / 32 characters).
    /// Weak secrets will be rejected with an error.
    pub fn new(secret: &str) -> Result<Self> {
        Self::with_durations(secret, 1, 30, 4, 60)
    }

    /// Create a new JWT service with custom token durations
    pub fn with_durations(
        secret: &str,
        access_token_duration_hours: u64,
        refresh_token_duration_days: u64,
        guest_token_duration_hours: u64,
        clock_skew_leeway_secs: u64,
    ) -> Result<Self> {
        if secret.is_empty() {
            return Err(Error::Internal("JWT secret cannot be empty".to_string()));
        }

        // Validate secret entropy in production builds
        #[cfg(not(debug_assertions))]
        Self::validate_secret_entropy(secret)?;

        // In debug builds, warn but allow (for development/testing convenience)
        #[cfg(debug_assertions)]
        if let Err(e) = Self::validate_secret_entropy(secret) {
            tracing::warn!("JWT secret validation: {}", e);
        }

        let encoding_key = EncodingKey::from_secret(secret.as_bytes());
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());

        Ok(Self {
            encoding_key: Arc::new(encoding_key),
            decoding_key: Arc::new(decoding_key),
            algorithm: Algorithm::HS256,
            access_token_duration_hours,
            refresh_token_duration_days,
            guest_token_duration_hours,
            clock_skew_leeway_secs,
        })
    }

    /// Validate that the secret has sufficient entropy
    fn validate_secret_entropy(secret: &str) -> Result<()> {
        // Calculate entropy estimate based on character variety
        let mut has_lowercase = false;
        let mut has_uppercase = false;
        let mut has_digit = false;
        let mut has_special = false;

        for c in secret.chars() {
            if c.is_ascii_lowercase() {
                has_lowercase = true;
            } else if c.is_ascii_uppercase() {
                has_uppercase = true;
            } else if c.is_ascii_digit() {
                has_digit = true;
            } else {
                has_special = true;
            }
        }

        // Estimate charset size based on character variety
        let charset_size = {
            let mut size = 0usize;
            if has_lowercase { size += 26; }
            if has_uppercase { size += 26; }
            if has_digit { size += 10; }
            if has_special { size += 32; } // Common special chars estimate
            size.max(10) // Minimum assumed charset
        };

        // Entropy = length * log2(charset_size)
        let entropy_bits = (secret.len() as f64) * (charset_size as f64).log2();

        if entropy_bits < MIN_JWT_SECRET_ENTROPY_BITS as f64 {
            return Err(Error::Internal(format!(
                "JWT secret has insufficient entropy ({entropy_bits:.0} bits, need at least {MIN_JWT_SECRET_ENTROPY_BITS} bits). \
                 Use a longer secret with mixed case, numbers, and special characters."
            )));
        }

        Ok(())
    }

    /// Sign a token
    ///
    /// # Arguments
    /// * `user_id` - User ID
    /// * `token_type` - Access or refresh token
    ///
    /// Note: Role is NOT included in token - it must be fetched from database on each request
    pub fn sign_token(
        &self,
        user_id: &UserId,
        token_type: TokenType,
    ) -> Result<String> {
        let now = Utc::now();
        let duration = match token_type {
            TokenType::Access => Duration::hours(self.access_token_duration_hours as i64),
            TokenType::Refresh => Duration::days(self.refresh_token_duration_days as i64),
            TokenType::Guest => Duration::hours(self.guest_token_duration_hours as i64),
        };

        let claims = Claims {
            sub: user_id.as_str().to_string(),
            typ: match token_type {
                TokenType::Access => "access".to_string(),
                TokenType::Refresh => "refresh".to_string(),
                TokenType::Guest => "guest".to_string(),
            },
            jti: nanoid::nanoid!(16),
            iat: now.timestamp(),
            exp: (now + duration).timestamp(),
        };

        let header = Header::new(self.algorithm);
        encode(&header, &claims, &self.encoding_key)
            .internal_with_err("Failed to sign token")
    }

    /// Verify a token and extract claims
    ///
    /// # Arguments
    /// * `token` - JWT token string
    pub fn verify_token(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        let token_data: TokenData<Claims> = decode(token, &self.decoding_key, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    Error::Authentication("Token expired".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidToken => {
                    Error::Authentication("Invalid token".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                    Error::Authentication("Invalid token signature".to_string())
                }
                _ => Error::Authentication(format!("Token verification failed: {e}")),
            })?;

        Ok(token_data.claims)
    }

    /// Verify an access token (convenience method)
    pub fn verify_access_token(&self, token: &str) -> Result<Claims> {
        let claims = self.verify_token(token)?;
        if !claims.is_access_token() {
            return Err(Error::Authentication("Not an access token".to_string()));
        }
        Ok(claims)
    }

    /// Verify a refresh token (convenience method)
    pub fn verify_refresh_token(&self, token: &str) -> Result<Claims> {
        let claims = self.verify_token(token)?;
        if !claims.is_refresh_token() {
            return Err(Error::Authentication("Not a refresh token".to_string()));
        }
        Ok(claims)
    }

    /// Sign a guest token for stateless guest authentication
    ///
    /// Guest tokens do NOT store user information in the database.
    /// Instead, they contain the room ID and a random session ID.
    ///
    /// # Arguments
    /// * `room_id` - Room ID the guest is joining
    ///
    /// # Returns
    /// * Guest JWT token string
    pub fn sign_guest_token(&self, room_id: &RoomId) -> Result<String> {
        let now = Utc::now();
        let duration = Duration::hours(self.guest_token_duration_hours as i64);
        let session_id = nanoid::nanoid!(16); // Generate random session ID

        let guest_claims = GuestClaims {
            sub: format!("guest:{}:{}", room_id.as_str(), session_id),
            room_id: room_id.as_str().to_string(),
            session_id,
            typ: "guest".to_string(),
            iat: now.timestamp(),
            exp: (now + duration).timestamp(),
        };

        let header = Header::new(self.algorithm);
        encode(&header, &guest_claims, &self.encoding_key)
            .internal_with_err("Failed to sign guest token")
    }

    /// Verify a guest token and extract guest claims
    ///
    /// # Arguments
    /// * `token` - Guest JWT token string
    ///
    /// # Returns
    /// * Guest claims with room ID and session ID
    pub fn verify_guest_token(&self, token: &str) -> Result<GuestClaims> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        let token_data: TokenData<GuestClaims> = decode(token, &self.decoding_key, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    Error::Authentication("Guest token expired".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidToken => {
                    Error::Authentication("Invalid guest token".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                    Error::Authentication("Invalid guest token signature".to_string())
                }
                _ => Error::Authentication(format!("Guest token verification failed: {e}")),
            })?;

        let claims = token_data.claims;

        // Verify it's actually a guest token
        if !claims.is_guest() {
            return Err(Error::Authentication("Not a guest token".to_string()));
        }

        Ok(claims)
    }

    /// Check if a token string is a guest token (by attempting to parse it)
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Returns
    /// * `true` if token is a valid guest token, `false` otherwise
    #[must_use] 
    pub fn is_guest_token(&self, token: &str) -> bool {
        self.verify_guest_token(token).is_ok()
    }

    /// Sign a custom JSON value as JWT
    ///
    /// This allows signing arbitrary claims (not just the standard Claims struct).
    /// Useful for RTMP publish keys and other custom tokens.
    ///
    /// # Arguments
    /// * `claims` - JSON value containing the claims
    ///
    /// # Returns
    /// Signed JWT token string
    pub async fn sign_custom(&self, claims: &serde_json::Value) -> Result<String> {
        let now = Utc::now();

        // Add standard JWT claims if not present
        let mut claims_with_standard = claims.clone();
        if let Some(obj) = claims_with_standard.as_object_mut() {
            obj.entry("jti".to_string())
                .or_insert_with(|| serde_json::Value::String(nanoid::nanoid!(16)));

            obj.entry("iat".to_string())
                .or_insert_with(|| serde_json::Value::Number(now.timestamp().into()));

            if !obj.contains_key("exp") {
                obj.entry("exp".to_string())
                    .or_insert_with(|| serde_json::Value::Number((now.timestamp() + 86400).into())); // Default 24h
            }
        }

        let header = Header::new(self.algorithm);
        encode(&header, &claims_with_standard, &self.encoding_key)
            .internal_with_err("Failed to sign custom token")
    }

    /// Verify a custom JWT token
    ///
    /// This allows verifying tokens with arbitrary claims.
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Returns
    /// JSON value containing the claims
    pub async fn verify_custom(&self, token: &str) -> Result<serde_json::Value> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        let token_data = decode(token, &self.decoding_key, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    Error::Authentication("Token expired".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidToken => {
                    Error::Authentication("Invalid token".to_string())
                }
                jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                    Error::Authentication("Invalid token signature".to_string())
                }
                _ => Error::Authentication(format!("Token verification failed: {e}")),
            })?;

        Ok(token_data.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-jwt").unwrap()
    }

    #[test]
    fn test_sign_and_verify_access_token() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = jwt.verify_access_token(&token).unwrap();

        assert_eq!(claims.sub, user_id.as_str());
        assert!(claims.is_access_token());
    }

    #[test]
    fn test_sign_and_verify_refresh_token() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let token = jwt.sign_token(&user_id, TokenType::Refresh).unwrap();
        let claims = jwt.verify_refresh_token(&token).unwrap();

        assert_eq!(claims.sub, user_id.as_str());
        assert!(claims.is_refresh_token());
    }

    #[test]
    fn test_verify_wrong_token_type() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let result = jwt.verify_refresh_token(&access_token);
        assert!(result.is_err());

        let refresh_token = jwt.sign_token(&user_id, TokenType::Refresh).unwrap();
        let result = jwt.verify_access_token(&refresh_token);
        assert!(result.is_err());
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

        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
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

        assert_eq!(claims.room_id(), room_id);
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
        let access_token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        assert!(!jwt.is_guest_token(&access_token));
    }

    #[test]
    fn test_verify_regular_token_as_guest_fails() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let result = jwt.verify_guest_token(&access_token);
        assert!(result.is_err());
    }

    // ========== Token Type Enforcement ==========

    #[test]
    fn test_access_token_rejected_as_refresh() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let result = jwt.verify_refresh_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_refresh_token_rejected_as_access() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Refresh).unwrap();
        let result = jwt.verify_access_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_guest_type_token_rejected_as_access() {
        // A token signed with TokenType::Guest via sign_token has typ="guest"
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Guest).unwrap();
        let result = jwt.verify_access_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_guest_type_token_rejected_as_refresh() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Guest).unwrap();
        let result = jwt.verify_refresh_token(&token);
        assert!(result.is_err());
    }

    // ========== Claims Inspection ==========

    #[test]
    fn test_claims_user_id_extraction() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = jwt.verify_token(&token).unwrap();
        assert_eq!(claims.user_id(), user_id);
    }

    #[test]
    fn test_claims_type_predicates() {
        let access = Claims { sub: "u1".into(), typ: "access".into(), jti: String::new(), iat: 0, exp: 0 };
        assert!(access.is_access_token());
        assert!(!access.is_refresh_token());
        assert!(!access.is_guest_token());

        let refresh = Claims { sub: "u1".into(), typ: "refresh".into(), jti: String::new(), iat: 0, exp: 0 };
        assert!(!refresh.is_access_token());
        assert!(refresh.is_refresh_token());
        assert!(!refresh.is_guest_token());

        let guest = Claims { sub: "u1".into(), typ: "guest".into(), jti: String::new(), iat: 0, exp: 0 };
        assert!(!guest.is_access_token());
        assert!(!guest.is_refresh_token());
        assert!(guest.is_guest_token());
    }

    // ========== Guest Token Edge Cases ==========

    #[test]
    fn test_guest_claims_room_id_extraction() {
        let jwt = create_jwt_service();
        let room_id = RoomId::new();
        let token = jwt.sign_guest_token(&room_id).unwrap();
        let claims = jwt.verify_guest_token(&token).unwrap();
        assert_eq!(claims.room_id(), room_id);
    }

    #[test]
    fn test_guest_claims_sub_format() {
        let jwt = create_jwt_service();
        let room_id = RoomId::new();
        let token = jwt.sign_guest_token(&room_id).unwrap();
        let claims = jwt.verify_guest_token(&token).unwrap();
        assert!(claims.sub.starts_with("guest:"));
        assert!(claims.sub.contains(room_id.as_str()));
    }

    #[test]
    fn test_guest_claims_is_guest_false_for_non_guest_sub() {
        let claims = GuestClaims {
            sub: "user:some_id".into(),
            room_id: "room1".into(),
            session_id: "sess1".into(),
            typ: "guest".into(),
            iat: 0,
            exp: 0,
        };
        assert!(!claims.is_guest());
    }

    // ========== Different Secrets ==========

    #[test]
    fn test_token_from_different_secret_is_rejected() {
        let jwt1 = JwtService::new("secret-key-one-long-enough-1234567890").unwrap();
        let jwt2 = JwtService::new("secret-key-two-long-enough-1234567890").unwrap();
        let user_id = UserId::new();

        let token = jwt1.sign_token(&user_id, TokenType::Access).unwrap();
        let result = jwt2.verify_token(&token);
        assert!(result.is_err());
    }

    // ========== Custom Durations ==========

    #[test]
    fn test_custom_token_durations() {
        let jwt = JwtService::with_durations(
            "custom-secret-key-long-enough-1234567890",
            2,  // 2 hour access
            7,  // 7 day refresh
            1,  // 1 hour guest
            30, // 30 second leeway
        ).unwrap();

        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = jwt.verify_token(&token).unwrap();

        // Verify token has exp roughly 2 hours from iat
        let duration = claims.exp - claims.iat;
        assert_eq!(duration, 7200); // 2 hours in seconds
    }

    #[test]
    fn test_refresh_token_duration() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Refresh).unwrap();
        let claims = jwt.verify_token(&token).unwrap();
        let duration = claims.exp - claims.iat;
        assert_eq!(duration, 30 * 86400); // 30 days in seconds
    }

    #[test]
    fn test_expired_token_is_rejected() {
        let secret = "expired-token-test-secret-1234567890";
        let jwt = JwtService::with_durations(secret, 1, 1, 1, 0).unwrap();

        // Manually craft a token with exp in the past
        let past = Utc::now() - Duration::hours(2);
        let claims = Claims {
            sub: "expired_user".into(),
            typ: "access".into(),
            jti: "test-jti".into(),
            iat: (past - Duration::hours(3)).timestamp(),
            exp: past.timestamp(), // expired 2 hours ago
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        ).unwrap();

        let result = jwt.verify_token(&token);
        assert!(result.is_err(), "Expired token should be rejected");
    }

    #[test]
    fn test_jti_is_unique_per_token() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let token1 = jwt.sign_token(&user_id, TokenType::Access).unwrap();
        let token2 = jwt.sign_token(&user_id, TokenType::Access).unwrap();

        let claims1 = jwt.verify_token(&token1).unwrap();
        let claims2 = jwt.verify_token(&token2).unwrap();

        assert_ne!(claims1.jti, claims2.jti, "Each token should have a unique jti");
        assert!(!claims1.jti.is_empty());
        assert!(!claims2.jti.is_empty());
    }

    #[test]
    fn test_token_iat_is_recent() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let before = Utc::now().timestamp();
        let token = jwt.sign_token(&user_id, TokenType::Access).unwrap();
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

    #[tokio::test]
    async fn test_sign_and_verify_custom_token() {
        let jwt = create_jwt_service();
        let claims = serde_json::json!({
            "sub": "custom_subject",
            "custom_field": "custom_value",
        });

        let token = jwt.sign_custom(&claims).await.unwrap();
        let verified: serde_json::Value = jwt.verify_custom(&token).await.unwrap();

        assert_eq!(verified["sub"], "custom_subject");
        assert_eq!(verified["custom_field"], "custom_value");
        assert!(verified.get("jti").is_some());
        assert!(verified.get("iat").is_some());
        assert!(verified.get("exp").is_some());
    }

    #[tokio::test]
    async fn test_custom_token_wrong_secret_rejected() {
        let jwt1 = JwtService::new("custom-secret-one-long-enough-1234567890").unwrap();
        let jwt2 = JwtService::new("custom-secret-two-long-enough-1234567890").unwrap();

        let claims = serde_json::json!({"sub": "test"});
        let token = jwt1.sign_custom(&claims).await.unwrap();
        let result = jwt2.verify_custom(&token).await;
        assert!(result.is_err());
    }
}
