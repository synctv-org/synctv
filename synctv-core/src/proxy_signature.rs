// HMAC-signed playback provider URL generation and verification.
// Playback provider URLs embed room_id, user_id, expiry, and optionally a target
// URL directly in the query string. Provider, version, and semantic resource
// come from the fixed route/request and are also bound by HMAC-SHA256.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::fmt;
use url::form_urlencoded;
use urlencoding::encode as url_encode;

type HmacSha256 = Hmac<Sha256>;

/// Domain separator for deriving the proxy signing key from the JWT secret.
const DOMAIN_SEPARATOR: &[u8] = b"synctv-proxy-sign";

/// Default proxy URL lifetime (30 minutes).
const DEFAULT_EXPIRY_SECS: i64 = 30 * 60;

/// HMAC-SHA256 signing key for proxy URLs.
///
/// Derived from the application's JWT secret with a domain separator,
/// ensuring proxy signatures and JWT tokens use independent key material.
pub struct ProxySigningKey {
    key: HmacSha256,
}

/// Claims embedded in a signed proxy URL.
///
/// `resource` is the provider-specific semantic resource derived from the
/// route/request, such as `streams/direct/0` or `dash-manifests/720p/proxy`.
/// `target_url` is bound into the signature when present so rewritten M3U8
/// segment URLs cannot be retargeted by editing the `url` query parameter.
#[derive(Debug, Clone)]
pub struct ProxyUrlClaims {
    pub provider: String,
    pub version: String,
    pub resource: String,
    pub room_id: String,
    pub user_id: String,
    pub expires_at: i64,
    pub target_url: Option<String>,
}

/// Errors from proxy signature operations.
#[derive(Debug)]
pub enum ProxySignatureError {
    /// The signing key could not be initialized.
    InvalidSigningKey,
    /// The signature does not match the expected HMAC.
    InvalidSignature,
    /// The URL has expired.
    Expired,
    /// A required query parameter is missing.
    MissingParam(&'static str),
    /// A query parameter could not be parsed.
    InvalidParam(&'static str),
}

impl fmt::Display for ProxySignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSigningKey => write!(f, "invalid proxy signing key"),
            Self::InvalidSignature => write!(f, "invalid proxy signature"),
            Self::Expired => write!(f, "proxy URL expired"),
            Self::MissingParam(name) => write!(f, "missing query param: {name}"),
            Self::InvalidParam(name) => write!(f, "invalid query param: {name}"),
        }
    }
}

impl std::error::Error for ProxySignatureError {}

impl ProxySigningKey {
    /// Derive a proxy signing key from the JWT secret.
    ///
    /// Uses HMAC(jwt_secret, domain_separator) as the derived key material,
    /// ensuring proxy signatures are cryptographically independent from JWT tokens.
    pub fn try_derive_from(jwt_secret: &[u8]) -> Result<Self, ProxySignatureError> {
        let mut derivation_mac = HmacSha256::new_from_slice(jwt_secret)
            .map_err(|_| ProxySignatureError::InvalidSigningKey)?;
        derivation_mac.update(DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();

        let key = HmacSha256::new_from_slice(&derived)
            .map_err(|_| ProxySignatureError::InvalidSigningKey)?;
        Ok(Self { key })
    }

    /// Sign claims and return the hex-encoded HMAC-SHA256 signature.
    #[must_use]
    pub fn sign(&self, claims: &ProxyUrlClaims) -> String {
        let mut mac = self.key.clone();
        mac.update(Self::canonical_message(claims).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Verify claims against a hex-encoded signature.
    pub fn verify(
        &self,
        claims: &ProxyUrlClaims,
        signature: &str,
    ) -> Result<(), ProxySignatureError> {
        // Check expiry first
        let now = chrono::Utc::now().timestamp();
        if now > claims.expires_at {
            return Err(ProxySignatureError::Expired);
        }

        let sig_bytes =
            hex::decode(signature).map_err(|_| ProxySignatureError::InvalidSignature)?;
        let mut mac = self.key.clone();
        mac.update(Self::canonical_message(claims).as_bytes());
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ProxySignatureError::InvalidSignature)
    }

    /// Build a query string with all claims and signature.
    ///
    /// Returns: `"sig={hex}&uid={uid}&rid={rid}&exp={exp}"`, plus
    /// `target_url=...` when `claims.target_url` is set.
    #[must_use]
    pub fn build_signed_query(&self, claims: &ProxyUrlClaims) -> String {
        let sig = self.sign(claims);
        let mut query = format!(
            "sig={}&uid={}&rid={}&exp={}",
            url_encode(&sig),
            url_encode(&claims.user_id),
            url_encode(&claims.room_id),
            claims.expires_at
        );
        if let Some(target_url) = &claims.target_url {
            query.push_str("&target_url=");
            query.push_str(&url_encode(target_url));
        }
        query
    }

    #[must_use]
    pub fn build_signed_query_with_target_url(
        &self,
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: &str,
    ) -> String {
        let mut claims = claims.clone();
        claims.resource = resource.to_string();
        claims.target_url = Some(target_url.to_string());
        self.build_signed_query(&claims)
    }

    /// Parse query parameters and verify the HMAC signature.
    ///
    /// The `provider`, `version`, and semantic `resource` are passed from the
    /// URL path/request instead of query params.
    pub fn parse_and_verify_query(
        &self,
        query: &str,
        provider: &str,
        version: &str,
        resource: &str,
    ) -> Result<ProxyUrlClaims, ProxySignatureError> {
        // Manual parsing to avoid HashMap allocation on hot path
        // We need exactly 4-5 params, so iterate once and extract directly
        let mut sig: Option<String> = None;
        let mut uid: Option<String> = None;
        let mut rid: Option<String> = None;
        let mut exp_str: Option<String> = None;
        let mut target_url: Option<String> = None;

        // Parse using form_urlencoded iterator without collecting into HashMap
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "sig" => sig = Some(value.into_owned()),
                "uid" => uid = Some(value.into_owned()),
                "rid" => rid = Some(value.into_owned()),
                "exp" => exp_str = Some(value.into_owned()),
                "target_url" => target_url = Some(value.into_owned()),
                _ => {} // Ignore unknown params
            }
        }

        let sig = sig.ok_or(ProxySignatureError::MissingParam("sig"))?;
        let uid = uid.ok_or(ProxySignatureError::MissingParam("uid"))?;
        let rid = rid.ok_or(ProxySignatureError::MissingParam("rid"))?;
        let exp_str = exp_str.ok_or(ProxySignatureError::MissingParam("exp"))?;

        // Reject empty room_id and user_id to prevent authorization bypass
        if uid.is_empty() {
            return Err(ProxySignatureError::InvalidParam("uid cannot be empty"));
        }
        if rid.is_empty() {
            return Err(ProxySignatureError::InvalidParam("rid cannot be empty"));
        }

        let expires_at: i64 = exp_str
            .parse()
            .map_err(|_| ProxySignatureError::InvalidParam("exp"))?;

        let claims = ProxyUrlClaims {
            provider: provider.to_string(),
            version: version.to_string(),
            resource: resource.to_string(),
            room_id: rid,
            user_id: uid,
            expires_at,
            target_url,
        };

        self.verify(&claims, &sig)?;

        Ok(claims)
    }

    /// Build the canonical message string for HMAC signing.
    fn canonical_message(claims: &ProxyUrlClaims) -> String {
        let mut message = format!(
            "{}:{}:{}:{}:{}:{}",
            claims.provider,
            claims.version,
            claims.resource,
            claims.room_id,
            claims.user_id,
            claims.expires_at
        );
        if let Some(target_url) = &claims.target_url {
            message.push_str(":url:");
            message.push_str(target_url);
        }
        message
    }

    /// Return the default expiry duration for proxy URLs.
    #[must_use]
    pub const fn default_expiry_secs() -> i64 {
        DEFAULT_EXPIRY_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => std::panic::panic_any(context.to_string()),
        }
    }

    fn test_key() -> ProxySigningKey {
        ok(
            ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough"),
            "test proxy signing key should derive",
        )
    }

    fn test_claims() -> ProxyUrlClaims {
        ProxyUrlClaims {
            provider: "emby".to_string(),
            version: "abc123".to_string(),
            resource: "media-streams/main/0".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            target_url: None,
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = test_key();
        let claims = test_claims();
        let sig = key.sign(&claims);
        assert!(key.verify(&claims, &sig).is_ok());
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let key = test_key();
        let claims = test_claims();
        let mut sig = key.sign(&claims);
        let last = some(sig.pop(), "signature should not be empty");
        let tampered = if last == '0' { '1' } else { '0' };
        sig.push(tampered);
        assert!(matches!(
            key.verify(&claims, &sig),
            Err(ProxySignatureError::InvalidSignature)
        ));
    }

    #[test]
    fn verify_rejects_tampered_claims() {
        let key = test_key();
        let claims = test_claims();
        let sig = key.sign(&claims);

        let mut tampered = claims;
        tampered.room_id = "room-2".to_string();
        assert!(matches!(
            key.verify(&tampered, &sig),
            Err(ProxySignatureError::InvalidSignature)
        ));
    }

    #[test]
    fn verify_rejects_expired() {
        let key = test_key();
        let mut claims = test_claims();
        claims.expires_at = chrono::Utc::now().timestamp() - 1; // Already expired
        let sig = key.sign(&claims);
        assert!(matches!(
            key.verify(&claims, &sig),
            Err(ProxySignatureError::Expired)
        ));
    }

    #[test]
    fn build_and_parse_query_roundtrip() {
        let key = test_key();
        let claims = test_claims();
        let query = key.build_signed_query(&claims);
        let parsed = ok(
            key.parse_and_verify_query(&query, &claims.provider, &claims.version, &claims.resource),
            "signed query should parse",
        );
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
    }

    #[test]
    fn parse_query_binds_url_param_to_signature() {
        let key = test_key();
        let mut claims = test_claims();
        claims.target_url = Some("http://example.com/seg.ts".to_string());
        let query = key.build_signed_query(&claims);
        let parsed = ok(
            key.parse_and_verify_query(&query, &claims.provider, &claims.version, &claims.resource),
            "signed query with target URL should parse",
        );
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.target_url, claims.target_url);
    }

    #[test]
    fn parse_query_rejects_tampered_target_url_param() {
        let key = test_key();
        let mut claims = test_claims();
        claims.target_url = Some("http://example.com/seg.ts".to_string());
        let query = key.build_signed_query(&claims);
        let (prefix, _) = some(
            query.split_once("&target_url="),
            "signed target query should include target_url",
        );
        let tampered = format!(
            "{prefix}&target_url={}",
            urlencoding::encode("http://evil.example/seg.ts")
        );

        assert!(matches!(
            key.parse_and_verify_query(
                &tampered,
                &claims.provider,
                &claims.version,
                &claims.resource,
            ),
            Err(ProxySignatureError::InvalidSignature)
        ));
    }

    #[test]
    fn parse_query_missing_sig() {
        let key = test_key();
        assert!(matches!(
            key.parse_and_verify_query(
                "uid=u1&rid=r1&exp=999999999999",
                "emby",
                "v1",
                "media-streams/main/0",
            ),
            Err(ProxySignatureError::MissingParam("sig"))
        ));
    }

    #[test]
    fn different_secrets_produce_different_signatures() {
        let key1 = ok(
            ProxySigningKey::try_derive_from(b"secret-1-long-enough-for-hmac"),
            "test proxy signing key should derive",
        );
        let key2 = ok(
            ProxySigningKey::try_derive_from(b"secret-2-long-enough-for-hmac"),
            "test proxy signing key should derive",
        );
        let claims = test_claims();
        let sig1 = key1.sign(&claims);
        let sig2 = key2.sign(&claims);
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn url_encoding_roundtrip_with_special_chars() {
        let key = test_key();
        let claims = ProxyUrlClaims {
            provider: "emby".to_string(),
            version: "abc123".to_string(),
            resource: "media-streams/main/0".to_string(),
            room_id: "room&id=tricky".to_string(),
            user_id: "user with spaces&more=yes".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            target_url: Some("https://cdn.example.com/a segment.ts?x=1&y=2".to_string()),
        };
        let query = key.build_signed_query(&claims);

        let parsed = ok(
            key.parse_and_verify_query(&query, &claims.provider, &claims.version, &claims.resource),
            "encoded signed query should parse",
        );
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
        assert_eq!(parsed.target_url, claims.target_url);
    }

    #[test]
    fn dash_segment_query_derived_from_manifest_claims_verifies_as_segment_resource() {
        let key = test_key();
        let manifest_claims = ProxyUrlClaims {
            provider: "bilibili".to_string(),
            version: "v1".to_string(),
            resource: "dash-manifests/dash/proxy".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            target_url: None,
        };
        let mut segment_claims = manifest_claims.clone();
        segment_claims.resource = "dash-segments/dash/0".to_string();
        segment_claims.target_url = None;

        let query = key.build_signed_query(&segment_claims);
        let parsed = ok(
            key.parse_and_verify_query(&query, "bilibili", "v1", "dash-segments/dash/0"),
            "DASH segment query should verify against segment resource",
        );

        assert_eq!(parsed.room_id, manifest_claims.room_id);
        assert_eq!(parsed.user_id, manifest_claims.user_id);
        assert_eq!(parsed.expires_at, manifest_claims.expires_at);
        assert_eq!(parsed.target_url, None);
        assert!(matches!(
            key.parse_and_verify_query(&query, "bilibili", "v1", "dash-segments/dash/1"),
            Err(ProxySignatureError::InvalidSignature)
        ));
    }
}
