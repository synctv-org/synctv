use chrono::{Duration, Utc};
use jsonwebtoken::{
    decode, encode, Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    models::{RoomId, UserId},
    Error, InternalExt, Result,
};

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn u64_to_i64(value: u64) -> i64 {
    value.cast_signed()
}

const MIN_JWT_SECRET_ENTROPY_BITS_F64: f64 = 128.0;
const MIN_SHANNON_ENTROPY: f64 = 3.5;

/// JWT token type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Access,  // default: 1 hour (configurable)
    Refresh, // default: 30 days (configurable)
    Guest,   // default: 4 hours (configurable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAuthContext {
    LocalTwoFactor,
    OAuth2,
}

impl TokenAuthContext {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalTwoFactor => "local_2fa",
            Self::OAuth2 => "oauth2",
        }
    }
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
    pub jti: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expiration time (Unix timestamp)
    pub exp: i64,
    /// Password version at time of token issuance.
    /// Tokens with a `pv` lower than the user's current `password_version` are rejected.
    pub pv: i32,
    /// Authentication context used when the token was issued.
    ///
    /// Omitted for ordinary local single-factor sessions. 2FA-enabled refresh
    /// token rotation only accepts `local_2fa` or `oauth2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amr: Option<String>,
    /// Issuer - identifies the service that issued the token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    /// Audience - identifies the intended recipients of the token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl Claims {
    #[must_use]
    pub fn user_id(&self) -> UserId {
        self.sub.parse().expect("valid numeric user id claim")
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

    #[must_use]
    pub fn satisfies_two_factor_requirement(&self) -> bool {
        matches!(self.amr.as_deref(), Some("local_2fa" | "oauth2"))
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
    /// JWT ID (unique token identifier for individual token revocation/blacklisting)
    pub jti: String,
    /// Token type (always "guest")
    pub typ: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expiration time (Unix timestamp)
    pub exp: i64,
    /// Room guest version at time of token issuance.
    /// Tokens with a `gv` lower than the room's current `guest_version` are rejected.
    /// This allows revoking all guest tokens for a room by incrementing the version.
    #[serde(default)]
    pub gv: i64,
    /// Issuer - identifies the service that issued the token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    /// Audience - identifies the intended recipients of the token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl GuestClaims {
    /// Parse room ID from claims
    #[must_use]
    pub fn room_id(&self) -> RoomId {
        self.room_id.parse().expect("valid numeric room id claim")
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
    /// Expected issuer for token validation. If set, tokens must have matching `iss` claim.
    issuer: Option<String>,
    /// Expected audience for token validation. If set, tokens must have matching `aud` claim.
    audience: Option<String>,
}

impl std::fmt::Debug for JwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtService")
            .field("algorithm", &self.algorithm)
            .finish()
    }
}

/// Minimum entropy bits required for JWT secret (128 bits minimum, 256 recommended)
/// HMAC-SHA256 requires at least 128 bits for security, but 256 bits is preferred.
const MIN_JWT_SECRET_ENTROPY_BITS: usize = 128;

/// Map a `jsonwebtoken` error to our domain `Error::Authentication`, using
/// the given `context` string to prefix the messages (e.g. "Token" or "Guest token").
fn map_jwt_error(e: &jsonwebtoken::errors::Error, context: &str) -> Error {
    match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
            Error::Authentication(format!("{context} expired"))
        }
        jsonwebtoken::errors::ErrorKind::InvalidToken => {
            Error::Authentication(format!("Invalid {}", context.to_lowercase()))
        }
        jsonwebtoken::errors::ErrorKind::InvalidSignature => {
            Error::Authentication(format!("Invalid {} signature", context.to_lowercase()))
        }
        _ => {
            tracing::warn!(context, error = %e, "JWT verification failed");
            let context = context.to_lowercase();
            Error::Authentication(format!("Invalid or expired {context}"))
        }
    }
}

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
        Self::with_durations_and_claims(
            secret,
            access_token_duration_hours,
            refresh_token_duration_days,
            guest_token_duration_hours,
            clock_skew_leeway_secs,
            None,
            None,
        )
    }

    /// Create a new JWT service with custom token durations and issuer/audience
    ///
    /// # Arguments
    /// * `secret` - Secret string for HMAC signing
    /// * `access_token_duration_hours` - Access token lifetime in hours
    /// * `refresh_token_duration_days` - Refresh token lifetime in days
    /// * `guest_token_duration_hours` - Guest token lifetime in hours
    /// * `clock_skew_leeway_secs` - Allowed clock skew in seconds
    /// * `issuer` - Optional issuer identifier (e.g., "synctv")
    /// * `audience` - Optional audience identifier (e.g., "synctv-api")
    pub fn with_durations_and_claims(
        secret: &str,
        access_token_duration_hours: u64,
        refresh_token_duration_days: u64,
        guest_token_duration_hours: u64,
        clock_skew_leeway_secs: u64,
        issuer: Option<String>,
        audience: Option<String>,
    ) -> Result<Self> {
        if secret.is_empty() {
            return Err(Error::Internal("JWT secret cannot be empty".to_string()));
        }

        // Always validate secret entropy
        Self::validate_secret_entropy(secret)?;

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
            issuer,
            audience,
        })
    }

    /// Validate that the secret has sufficient entropy
    ///
    /// Uses multiple checks to ensure the secret is cryptographically strong:
    /// 1. Minimum length of 32 characters (256 bits for HMAC-SHA256)
    /// 2. Shannon entropy calculation (not just charset-based estimation)
    /// 3. Pattern detection (repeating characters, sequences, keyboard walks)
    /// 4. Unique character ratio (prevents "aaaa..." patterns)
    fn validate_secret_entropy(secret: &str) -> Result<()> {
        // Requirement 1: Minimum length (32 characters = 256 bits minimum)
        const MIN_SECRET_LENGTH: usize = 32;
        if secret.len() < MIN_SECRET_LENGTH {
            return Err(Error::Internal(format!(
                "JWT secret is too short ({} characters, need at least {}). \
                 Use a secret with at least 32 characters for HMAC-SHA256.",
                secret.len(),
                MIN_SECRET_LENGTH
            )));
        }

        // Requirement 2: Check character variety
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
            } else if !c.is_whitespace() {
                has_special = true;
            }
        }

        // Count character classes present
        let char_classes = [has_lowercase, has_uppercase, has_digit, has_special]
            .iter()
            .filter(|&&x| x)
            .count();

        // Require at least 2 character classes (e.g., lowercase + digits)
        if char_classes < 2 {
            return Err(Error::Internal(
                "JWT secret uses only one character class. \
                 Use a secret with at least 2 character types (uppercase, lowercase, digits, or special characters)."
                    .to_string(),
            ));
        }

        // Requirement 3: Unique character ratio
        // Prevents "aaaa..." or "abcabcabc..." patterns
        let unique_chars: std::collections::HashSet<char> = secret.chars().collect();
        let unique_ratio = usize_to_f64(unique_chars.len()) / usize_to_f64(secret.len());

        // Require at least 25% unique characters
        if unique_ratio < 0.25 {
            return Err(Error::Internal(format!(
                "JWT secret has low character diversity ({:.0}% unique characters). \
                 Use a secret with more varied characters.",
                unique_ratio * 100.0
            )));
        }

        // Requirement 4: Reject obvious patterns

        // Check for all same character
        if unique_chars.len() == 1 {
            return Err(Error::Internal(
                "JWT secret consists of a single repeated character. \
                 Use a secret with varied characters."
                    .to_string(),
            ));
        }

        // Check for numeric-only secrets (even if long)
        if secret.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Internal(
                "JWT secret contains only digits. \
                 Use a secret with mixed character types for better security."
                    .to_string(),
            ));
        }

        // Check for simple sequential patterns
        if Self::is_sequential_pattern(secret) {
            return Err(Error::Internal(
                "JWT secret appears to be a simple sequential pattern. \
                 Use a randomly generated secret."
                    .to_string(),
            ));
        }

        // Check for repeated short patterns (e.g., "abcabcabc...")
        if Self::has_repeating_pattern(secret) {
            return Err(Error::Internal(
                "JWT secret contains repeating patterns. \
                 Use a randomly generated secret without patterns."
                    .to_string(),
            ));
        }

        // Requirement 5: Shannon entropy calculation
        // More accurate than simple charset-based estimation
        let entropy = Self::calculate_shannon_entropy(secret);
        if entropy < MIN_SHANNON_ENTROPY {
            return Err(Error::Internal(format!(
                "JWT secret has low entropy ({entropy:.1} bits/char, need at least {MIN_SHANNON_ENTROPY:.1}). \
                 Use a more random secret with varied characters."
            )));
        }

        // Requirement 6: Estimate total entropy bits
        // Entropy = length * entropy_per_char
        let estimated_entropy_bits = usize_to_f64(secret.len()) * entropy;

        if estimated_entropy_bits < MIN_JWT_SECRET_ENTROPY_BITS_F64 {
            return Err(Error::Internal(format!(
                "JWT secret has insufficient total entropy ({estimated_entropy_bits:.0} bits, need at least {MIN_JWT_SECRET_ENTROPY_BITS} bits). \
                 Use a longer secret or one with more character variety."
            )));
        }

        Ok(())
    }

    /// Calculate Shannon entropy in bits per character
    ///
    /// Higher values indicate more randomness. Maximum for ASCII is ~4.7 bits/char.
    fn calculate_shannon_entropy(s: &str) -> f64 {
        use std::collections::HashMap;

        if s.is_empty() {
            return 0.0;
        }

        let mut freq: HashMap<char, usize> = HashMap::new();
        for c in s.chars() {
            *freq.entry(c).or_insert(0) += 1;
        }

        let len = usize_to_f64(s.len());
        let mut entropy = 0.0;

        for &count in freq.values() {
            let p = usize_to_f64(count) / len;
            if p > 0.0 {
                entropy = p.mul_add(-p.log2(), entropy);
            }
        }

        entropy
    }

    /// Check if the string is a simple sequential pattern
    fn is_sequential_pattern(s: &str) -> bool {
        let s_lower = s.to_lowercase();
        let chars: Vec<char> = s_lower.chars().collect();

        if chars.len() < 8 {
            return false;
        }

        // Check for ascending sequence (abc...xyz)
        let mut ascending_count = 0;
        let mut descending_count = 0;

        for i in 1..chars.len() {
            if let (Some(prev), Some(curr)) = (chars[i - 1].to_digit(36), chars[i].to_digit(36)) {
                if curr == prev + 1 {
                    ascending_count += 1;
                } else if curr + 1 == prev {
                    descending_count += 1;
                }
            }
        }

        // If more than 70% of adjacent chars are sequential, reject
        let threshold = usize_to_f64(chars.len() - 1) * 0.7;
        f64::from(ascending_count) > threshold || f64::from(descending_count) > threshold
    }

    /// Check if the string has a repeating pattern
    fn has_repeating_pattern(s: &str) -> bool {
        if s.len() < 12 {
            return false;
        }

        // Check for patterns of length 2-8 that repeat
        for pattern_len in 2..=8.min(s.len() / 3) {
            let pattern = &s[..pattern_len];
            let rest = &s[pattern_len..];

            // Check if rest is mostly repetitions of pattern
            let repetitions = rest.len() / pattern_len;
            if repetitions >= 2 {
                let mut matches = 0;
                for i in 0..repetitions {
                    let start = i * pattern_len;
                    let end = start + pattern_len;
                    if end <= rest.len() && &rest[start..end] == pattern {
                        matches += 1;
                    }
                }
                // If more than 80% match, it's a repeating pattern
                if f64::from(matches) / usize_to_f64(repetitions) > 0.8 {
                    return true;
                }
            }
        }

        false
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
        password_version: i32,
    ) -> Result<String> {
        self.sign_token_with_auth_context(user_id, token_type, password_version, None)
    }

    pub fn sign_token_with_auth_context(
        &self,
        user_id: &UserId,
        token_type: TokenType,
        password_version: i32,
        auth_context: Option<TokenAuthContext>,
    ) -> Result<String> {
        let now = Utc::now();
        let duration = match token_type {
            TokenType::Access => Duration::hours(u64_to_i64(self.access_token_duration_hours)),
            TokenType::Refresh => Duration::days(u64_to_i64(self.refresh_token_duration_days)),
            TokenType::Guest => Duration::hours(u64_to_i64(self.guest_token_duration_hours)),
        };

        let claims = Claims {
            sub: user_id.to_string(),
            typ: match token_type {
                TokenType::Access => "access".to_string(),
                TokenType::Refresh => "refresh".to_string(),
                TokenType::Guest => "guest".to_string(),
            },
            jti: synctv_common::snanoid!(16),
            iat: now.timestamp(),
            exp: (now + duration).timestamp(),
            pv: password_version,
            amr: auth_context.map(|value| value.as_str().to_string()),
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
        };

        let header = Header::new(self.algorithm);
        encode(&header, &claims, &self.encoding_key).internal_with_err("Failed to sign token")
    }

    /// Verify a token and extract claims
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Validation
    /// - Signature verification
    /// - Expiration time check
    /// - Issuer validation (if configured)
    /// - Audience validation (if configured)
    pub fn verify_token(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        // Configure issuer validation if expected issuer is set
        if let Some(ref expected_iss) = self.issuer {
            validation.set_issuer(&[expected_iss]);
        }

        // Configure audience validation if expected audience is set
        if let Some(ref expected_aud) = self.audience {
            validation.set_audience(&[expected_aud]);
        }

        let token_data: TokenData<Claims> = decode(token, &self.decoding_key, &validation)
            .map_err(|e| map_jwt_error(&e, "Token"))?;

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
        // Reject tokens with empty jti: refresh token rotation relies on jti for
        // blacklisting. A missing/empty jti would bypass the blacklist check entirely.
        if claims.jti.is_empty() {
            return Err(Error::Authentication(
                "Refresh token missing jti".to_string(),
            ));
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
        self.sign_guest_token_with_version(room_id, 0)
    }

    /// Sign a guest token with a specific room guest version
    ///
    /// This allows embedding the room's current guest version in the token,
    /// enabling room-level revocation by incrementing the room's guest version.
    ///
    /// # Arguments
    /// * `room_id` - Room ID the guest is joining
    /// * `room_guest_version` - Current room guest version for revocation support
    ///
    /// # Returns
    /// * Guest JWT token string
    pub fn sign_guest_token_with_version(
        &self,
        room_id: &RoomId,
        room_guest_version: i64,
    ) -> Result<String> {
        let now = Utc::now();
        let duration = Duration::hours(u64_to_i64(self.guest_token_duration_hours));
        let session_id = synctv_common::snanoid!(16); // Generate random session ID

        let guest_claims = GuestClaims {
            sub: format!("guest:{room_id}:{session_id}"),
            room_id: room_id.to_string(),
            session_id,
            jti: synctv_common::snanoid!(16), // Unique JWT ID for individual token revocation
            typ: "guest".to_string(),
            iat: now.timestamp(),
            exp: (now + duration).timestamp(),
            gv: room_guest_version,
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
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
    ///
    /// # Validation
    /// - Signature verification
    /// - Expiration time check
    /// - Issuer validation (if configured)
    /// - Audience validation (if configured)
    pub fn verify_guest_token(&self, token: &str) -> Result<GuestClaims> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        // Configure issuer validation if expected issuer is set
        if let Some(ref expected_iss) = self.issuer {
            validation.set_issuer(&[expected_iss]);
        }

        // Configure audience validation if expected audience is set
        if let Some(ref expected_aud) = self.audience {
            validation.set_audience(&[expected_aud]);
        }

        let token_data: TokenData<GuestClaims> = decode(token, &self.decoding_key, &validation)
            .map_err(|e| map_jwt_error(&e, "Guest token"))?;

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

    /// Get access token duration in seconds
    ///
    /// Used by `OAuth2` token response to report the correct `expires_in` value.
    #[must_use]
    pub fn access_token_duration_seconds(&self) -> i64 {
        u64_to_i64(self.access_token_duration_hours) * 3600
    }

    /// Get refresh token duration in seconds
    ///
    /// Used by token family revocation to ensure the TTL covers the full refresh
    /// token lifetime, preventing attackers from outlasting a short TTL.
    #[must_use]
    pub const fn refresh_token_duration_seconds(&self) -> u64 {
        self.refresh_token_duration_days * 86400
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
    pub fn sign_custom(&self, claims: &serde_json::Value) -> Result<String> {
        let now = Utc::now();

        // Add standard JWT claims if not present
        let mut claims_with_standard = claims.clone();
        if let Some(obj) = claims_with_standard.as_object_mut() {
            obj.entry("jti".to_string())
                .or_insert_with(|| serde_json::Value::String(synctv_common::snanoid!(16)));

            obj.entry("iat".to_string())
                .or_insert_with(|| serde_json::Value::Number(now.timestamp().into()));

            if !obj.contains_key("exp") {
                obj.entry("exp".to_string())
                    .or_insert_with(|| serde_json::Value::Number((now.timestamp() + 86400).into()));
                // Default 24h
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
    pub fn verify_custom(&self, token: &str) -> Result<serde_json::Value> {
        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = self.clock_skew_leeway_secs;

        let token_data = decode(token, &self.decoding_key, &validation)
            .map_err(|e| map_jwt_error(&e, "Token"))?;

        Ok(token_data.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_jwt_service() -> JwtService {
        // Use a sufficiently long secret to pass entropy validation
        JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890").unwrap()
    }

    #[test]
    fn test_sign_and_verify_access_token() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = jwt.verify_access_token(&token).unwrap();

        assert_eq!(claims.sub, user_id.to_string());
        assert!(claims.is_access_token());
    }

    #[test]
    fn test_sign_and_verify_refresh_token() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let token = jwt.sign_token(&user_id, TokenType::Refresh, 0).unwrap();
        let claims = jwt.verify_refresh_token(&token).unwrap();

        assert_eq!(claims.sub, user_id.to_string());
        assert!(claims.is_refresh_token());
    }

    #[test]
    fn test_verify_wrong_token_type() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let result = jwt.verify_refresh_token(&access_token);
        assert!(result.is_err());

        let refresh_token = jwt.sign_token(&user_id, TokenType::Refresh, 0).unwrap();
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

        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
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
        let access_token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        assert!(!jwt.is_guest_token(&access_token));
    }

    #[test]
    fn test_verify_regular_token_as_guest_fails() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let result = jwt.verify_guest_token(&access_token);
        assert!(result.is_err());
    }

    #[test]
    fn test_access_token_rejected_as_refresh() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let result = jwt.verify_refresh_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_refresh_token_rejected_as_access() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Refresh, 0).unwrap();
        let result = jwt.verify_access_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_guest_type_token_rejected_as_access() {
        // A token signed with TokenType::Guest via sign_token has typ="guest"
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Guest, 0).unwrap();
        let result = jwt.verify_access_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_guest_type_token_rejected_as_refresh() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Guest, 0).unwrap();
        let result = jwt.verify_refresh_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_claims_user_id_extraction() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = jwt.verify_token(&token).unwrap();
        assert_eq!(claims.user_id(), user_id);
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
            amr: None,
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
            amr: None,
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
            amr: None,
            iss: None,
            aud: None,
        };
        assert!(!guest.is_access_token());
        assert!(!guest.is_refresh_token());
        assert!(guest.is_guest_token());
    }

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

        let token = jwt1.sign_token(&user_id, TokenType::Access, 0).unwrap();
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
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = jwt.verify_token(&token).unwrap();

        // Verify token has exp roughly 2 hours from iat
        let duration = claims.exp - claims.iat;
        assert_eq!(duration, 7200); // 2 hours in seconds
    }

    #[test]
    fn test_refresh_token_duration() {
        let jwt = create_jwt_service();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Refresh, 0).unwrap();
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
            amr: None,
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

        let token1 = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let token2 = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();

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
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
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
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = jwt.verify_token(&token).unwrap();

        assert_eq!(claims.iss.as_deref(), Some("synctv"));
        assert_eq!(claims.aud.as_deref(), Some("synctv-api"));
    }

    #[test]
    fn test_token_without_issuer_accepted_when_no_issuer_expected() {
        // Service without issuer validation
        let jwt = JwtService::new("secret-no-issuer-validation-LONG-ENOUGH-1234567890").unwrap();
        let user_id = UserId::new();
        let token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
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
        let token = jwt_other
            .sign_token(&user_id, TokenType::Access, 0)
            .unwrap();
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
        let token = jwt_other
            .sign_token(&user_id, TokenType::Access, 0)
            .unwrap();
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
    fn test_weak_secret_common_password_rejected() {
        // Common password pattern with padding - using simple repeated pattern
        let result = JwtService::new("passpasspasspasspasspasspass12");
        assert!(
            result.is_err(),
            "Repeated common password should be rejected"
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
}
