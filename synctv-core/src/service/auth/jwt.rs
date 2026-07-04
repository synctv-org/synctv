use base64::Engine as _;
use chrono::{Duration, Utc};
use jsonwebtoken::{
    decode, encode, Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    models::{RoomId, UserId},
    Error, InternalExt, Result,
};

mod types;

use types::UserTokenSigningKind;
pub use types::{Claims, GuestClaims, TokenAuthContext, TokenCredentialBinding, TokenType};

fn usize_to_f64(value: usize) -> Result<f64> {
    let value = u32::try_from(value)
        .map_err(|_| Error::InvalidInput("value exceeds u32::MAX".to_string()))?;
    Ok(f64::from(value))
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    let value = u32::try_from(value).unwrap_or(u32::MAX);
    f64::from(value)
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidInput(format!("{field} exceeds i64::MAX")))
}

fn duration_hours(value: u64, field: &'static str) -> Result<Duration> {
    Ok(Duration::hours(u64_to_i64(value, field)?))
}

fn duration_days(value: u64, field: &'static str) -> Result<Duration> {
    Ok(Duration::days(u64_to_i64(value, field)?))
}

fn duration_hours_to_seconds(value: u64, field: &'static str) -> Result<i64> {
    u64_to_i64(value, field)?
        .checked_mul(3600)
        .ok_or_else(|| Error::InvalidInput(format!("{field} seconds exceed i64::MAX")))
}

fn decode_untrusted_token_type_hint(token: &str) -> Option<TokenTypeHint> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<TokenTypeHint>(&decoded).ok()
}

const MIN_JWT_SECRET_ENTROPY_BITS_F64: f64 = 128.0;
const MIN_SHANNON_ENTROPY: f64 = 3.5;

#[derive(Deserialize)]
struct TokenTypeHint {
    typ: String,
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
    /// Return the untrusted `typ` claim from a JWT payload.
    ///
    /// This is only suitable for routing a token to the correct verifier. It
    /// does not validate the signature, issuer, audience, expiration, or token
    /// revocation state.
    #[must_use]
    pub fn token_type_hint(token: &str) -> Option<TokenType> {
        let hint = decode_untrusted_token_type_hint(token)?;
        TokenType::from_claim_typ(&hint.typ)
    }

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
    /// * `audience` - Optional audience identifier (e.g., "primary-api")
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

        crate::install_process_crypto_provider();

        // Always validate secret entropy
        Self::validate_secret_entropy(secret)?;
        duration_hours(access_token_duration_hours, "access token duration hours")?;
        duration_days(refresh_token_duration_days, "refresh token duration days")?;
        duration_hours(guest_token_duration_hours, "guest token duration hours")?;
        duration_hours_to_seconds(access_token_duration_hours, "access token duration hours")?;

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
        let unique_ratio = usize_to_f64(unique_chars.len())? / usize_to_f64(secret.len())?;

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
        let entropy = Self::calculate_shannon_entropy(secret)?;
        if entropy < MIN_SHANNON_ENTROPY {
            return Err(Error::Internal(format!(
                "JWT secret has low entropy ({entropy:.1} bits/char, need at least {MIN_SHANNON_ENTROPY:.1}). \
                 Use a more random secret with varied characters."
            )));
        }

        // Requirement 6: Estimate total entropy bits
        // Entropy = length * entropy_per_char
        let estimated_entropy_bits = usize_to_f64(secret.len())? * entropy;

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
    fn calculate_shannon_entropy(s: &str) -> Result<f64> {
        use std::collections::HashMap;

        if s.is_empty() {
            return Ok(0.0);
        }

        let mut freq: HashMap<char, usize> = HashMap::new();
        for c in s.chars() {
            *freq.entry(c).or_insert(0) += 1;
        }

        let len = usize_to_f64(s.len())?;
        let mut entropy = 0.0;

        for &count in freq.values() {
            let p = usize_to_f64(count)? / len;
            if p > 0.0 {
                entropy = p.mul_add(-p.log2(), entropy);
            }
        }

        Ok(entropy)
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
        let threshold = usize_to_f64_saturating(chars.len() - 1) * 0.7;
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
                if f64::from(matches) / usize_to_f64_saturating(repetitions) > 0.8 {
                    return true;
                }
            }
        }

        false
    }

    pub fn sign_access_token(&self, user_id: &UserId, password_version: i32) -> Result<String> {
        self.sign_access_token_with_auth_context(user_id, password_version, None)
    }

    pub fn sign_access_token_with_auth_context(
        &self,
        user_id: &UserId,
        password_version: i32,
        auth_context: Option<TokenAuthContext>,
    ) -> Result<String> {
        let credential_binding = TokenCredentialBinding::Password {
            version: password_version,
        };
        self.sign_access_token_with_auth_context_and_session(
            user_id,
            password_version,
            auth_context,
            None,
            &credential_binding,
        )
    }

    pub fn sign_access_token_with_auth_context_and_session(
        &self,
        user_id: &UserId,
        password_version: i32,
        auth_context: Option<TokenAuthContext>,
        session_id: Option<&str>,
        credential_binding: &TokenCredentialBinding,
    ) -> Result<String> {
        self.sign_user_token_with_auth_context_and_session(
            user_id,
            UserTokenSigningKind::Access,
            password_version,
            auth_context,
            session_id,
            credential_binding,
        )
    }

    pub fn sign_refresh_token_with_session(
        &self,
        user_id: &UserId,
        password_version: i32,
        auth_context: Option<TokenAuthContext>,
        session_id: &str,
        credential_binding: &TokenCredentialBinding,
    ) -> Result<String> {
        self.sign_user_token_with_auth_context_and_session(
            user_id,
            UserTokenSigningKind::Refresh,
            password_version,
            auth_context,
            Some(session_id),
            credential_binding,
        )
    }

    fn sign_user_token_with_auth_context_and_session(
        &self,
        user_id: &UserId,
        token_kind: UserTokenSigningKind,
        password_version: i32,
        auth_context: Option<TokenAuthContext>,
        session_id: Option<&str>,
        credential_binding: &TokenCredentialBinding,
    ) -> Result<String> {
        if matches!(token_kind, UserTokenSigningKind::Refresh)
            && session_id.is_none_or(str::is_empty)
        {
            return Err(Error::InvalidInput(
                "Refresh token signing requires a session id".to_string(),
            ));
        }
        let now = Utc::now();
        let duration = match token_kind {
            UserTokenSigningKind::Access => duration_hours(
                self.access_token_duration_hours,
                "access token duration hours",
            )?,
            UserTokenSigningKind::Refresh => duration_days(
                self.refresh_token_duration_days,
                "refresh token duration days",
            )?,
        };
        let (cbm, opi, ops, eml, wcid) = match credential_binding {
            TokenCredentialBinding::Password { .. } => {
                (Some("password".to_string()), None, None, None, None)
            }
            TokenCredentialBinding::OAuth2 {
                provider_instance_name,
                provider_user_id,
            } => (
                Some("oauth2".to_string()),
                Some(provider_instance_name.clone()),
                Some(provider_user_id.clone()),
                None,
                None,
            ),
            TokenCredentialBinding::WebAuthn { credential_id } => (
                Some("webauthn".to_string()),
                None,
                None,
                None,
                Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(credential_id)),
            ),
            TokenCredentialBinding::Email { email } => (
                Some("email".to_string()),
                None,
                None,
                Some(email.clone()),
                None,
            ),
        };

        let claims = Claims {
            sub: user_id.to_string(),
            typ: token_kind.claim_typ().to_string(),
            jti: synctv_common::snanoid!(16),
            iat: now.timestamp(),
            exp: (now + duration).timestamp(),
            pv: password_version,
            sid: session_id.map(ToString::to_string),
            amr: auth_context.map(|value| value.as_str().to_string()),
            cbm,
            opi,
            ops,
            eml,
            wcid,
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

        let claims = token_data.claims;
        claims.user_id()?;

        Ok(claims)
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
        if claims.sid.as_deref().is_none_or(str::is_empty) {
            return Err(Error::Authentication(
                "Refresh token missing session id".to_string(),
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
        let duration = duration_hours(
            self.guest_token_duration_hours,
            "guest token duration hours",
        )?;
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
        claims.room_id()?;

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
    pub fn access_token_duration_seconds(&self) -> Result<i64> {
        duration_hours_to_seconds(
            self.access_token_duration_hours,
            "access token duration hours",
        )
    }

    /// Get refresh token duration in seconds
    ///
    /// Used by token family revocation to ensure the TTL covers the full refresh
    /// token lifetime, preventing attackers from outlasting a short TTL.
    #[must_use]
    pub const fn refresh_token_duration_seconds(&self) -> u64 {
        self.refresh_token_duration_days * 86400
    }

    pub fn sign_custom<T>(&self, claims: &T) -> Result<String>
    where
        T: Serialize,
    {
        let header = Header::new(self.algorithm);
        encode(&header, claims, &self.encoding_key).internal_with_err("Failed to sign custom token")
    }

    pub fn verify_custom<T>(&self, token: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
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
mod tests;
