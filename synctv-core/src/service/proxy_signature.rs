// HMAC-signed proxy URL generation and verification.
//
// Proxy URLs embed room_id, user_id, version, and expiry directly in the query string,
// authenticated by an HMAC-SHA256 signature. This replaces JWT auth on proxy routes,
// allowing URLs to be shared (e.g., in M3U8 playlists) without leaking JWT tokens.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::fmt;
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
#[derive(Debug, Clone)]
pub struct ProxyUrlClaims {
    pub provider: String,
    pub version: String,
    pub room_id: String,
    pub user_id: String,
    pub expires_at: i64,
}

/// Errors from proxy signature operations.
#[derive(Debug)]
pub enum ProxySignatureError {
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
    #[must_use]
    pub fn derive_from(jwt_secret: &[u8]) -> Self {
        // Use HMAC(secret, domain) to derive key material
        let mut derivation_mac =
            HmacSha256::new_from_slice(jwt_secret).expect("HMAC accepts any key length");
        derivation_mac.update(DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();

        let key = HmacSha256::new_from_slice(&derived).expect("derived key is valid HMAC key");
        Self { key }
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
    /// Returns: `"sig={hex}&uid={uid}&rid={rid}&exp={exp}"`
    #[must_use]
    pub fn build_signed_query(&self, claims: &ProxyUrlClaims) -> String {
        let sig = self.sign(claims);
        format!(
            "sig={}&uid={}&rid={}&exp={}",
            url_encode(&sig),
            url_encode(&claims.user_id),
            url_encode(&claims.room_id),
            claims.expires_at
        )
    }

    /// Parse query parameters and verify the HMAC signature.
    ///
    /// The `provider` and `version` are passed from the URL path (not query params).
    pub fn parse_and_verify_query(
        &self,
        query: &str,
        provider: &str,
        version: &str,
    ) -> Result<ProxyUrlClaims, ProxySignatureError> {
        let mut sig = None;
        let mut uid = None;
        let mut rid = None;
        let mut exp = None;

        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                match key {
                    "sig" => sig = Some(value),
                    "uid" => uid = Some(value),
                    "rid" => rid = Some(value),
                    "exp" => exp = Some(value),
                    _ => {} // Ignore extra params (e.g., url= for M3U8 segments)
                }
            }
        }

        let sig = sig.ok_or(ProxySignatureError::MissingParam("sig"))?;
        let uid = uid.ok_or(ProxySignatureError::MissingParam("uid"))?;
        let rid = rid.ok_or(ProxySignatureError::MissingParam("rid"))?;
        let exp_str = exp.ok_or(ProxySignatureError::MissingParam("exp"))?;

        // URL-decode values since build_signed_query encodes them.
        let uid_decoded =
            urlencoding::decode(uid).map_err(|_| ProxySignatureError::InvalidParam("uid"))?;
        let rid_decoded =
            urlencoding::decode(rid).map_err(|_| ProxySignatureError::InvalidParam("rid"))?;

        let expires_at: i64 = exp_str
            .parse()
            .map_err(|_| ProxySignatureError::InvalidParam("exp"))?;

        let claims = ProxyUrlClaims {
            provider: provider.to_string(),
            version: version.to_string(),
            room_id: rid_decoded.into_owned(),
            user_id: uid_decoded.into_owned(),
            expires_at,
        };

        self.verify(&claims, sig)?;

        Ok(claims)
    }

    /// Build the canonical message string for HMAC signing.
    fn canonical_message(claims: &ProxyUrlClaims) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            claims.provider, claims.version, claims.room_id, claims.user_id, claims.expires_at
        )
    }

    /// Return the default expiry duration for proxy URLs.
    #[must_use]
    pub const fn default_expiry_secs() -> i64 {
        DEFAULT_EXPIRY_SECS
    }
}

/// Build a signed proxy URL (relative) for a playback action.
///
/// Returns a path like `/api/providers/proxy/{provider}/{version}/{action}?sig=...&uid=...&rid=...&exp=...`
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_signed_proxy_url(
    provider: &str,
    version: &str,
    action: &str,
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) -> String {
    let claims = ProxyUrlClaims {
        provider: provider.to_string(),
        version: version.to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
    };
    let query = signing_key.build_signed_query(&claims);
    format!(
        "/api/providers/proxy/{}/{}/{}?{}",
        url_encode(provider),
        url_encode(version),
        url_encode(action),
        query
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> ProxySigningKey {
        ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough")
    }

    fn test_claims() -> ProxyUrlClaims {
        ProxyUrlClaims {
            provider: "emby".to_string(),
            version: "abc123".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
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
        let last = sig.pop().expect("signature should not be empty");
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
        let parsed = key
            .parse_and_verify_query(&query, &claims.provider, &claims.version)
            .unwrap();
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
    }

    #[test]
    fn parse_query_with_extra_params() {
        let key = test_key();
        let claims = test_claims();
        let mut query = key.build_signed_query(&claims);
        query.push_str("&url=http%3A%2F%2Fexample.com%2Fseg.ts");
        let parsed = key
            .parse_and_verify_query(&query, &claims.provider, &claims.version)
            .unwrap();
        assert_eq!(parsed.room_id, claims.room_id);
    }

    #[test]
    fn parse_query_missing_sig() {
        let key = test_key();
        assert!(matches!(
            key.parse_and_verify_query("uid=u1&rid=r1&exp=999999999999", "emby", "v1"),
            Err(ProxySignatureError::MissingParam("sig"))
        ));
    }

    #[test]
    fn build_signed_proxy_url_format() {
        let key = test_key();
        let url = build_signed_proxy_url(
            "emby",
            "v1",
            "stream",
            &key,
            "room-1",
            "user-1",
            chrono::Utc::now().timestamp() + 3600,
        );
        assert!(url.starts_with("/api/providers/proxy/emby/v1/stream?sig="));
        assert!(url.contains("&uid=user-1"));
        assert!(url.contains("&rid=room-1"));
    }

    #[test]
    fn different_secrets_produce_different_signatures() {
        let key1 = ProxySigningKey::derive_from(b"secret-1-long-enough-for-hmac");
        let key2 = ProxySigningKey::derive_from(b"secret-2-long-enough-for-hmac");
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
            room_id: "room&id=tricky".to_string(),
            user_id: "user with spaces&more=yes".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        let query = key.build_signed_query(&claims);

        // Verify that special chars are encoded (no raw & or = in uid/rid values)
        // The query should still be parseable back to the original claims
        let parsed = key
            .parse_and_verify_query(&query, &claims.provider, &claims.version)
            .unwrap();
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
    }

    #[test]
    fn build_signed_proxy_url_encodes_path_segments() {
        let key = test_key();
        let url = build_signed_proxy_url(
            "provider/name",
            "v1&bad",
            "action=evil",
            &key,
            "room-1",
            "user-1",
            chrono::Utc::now().timestamp() + 3600,
        );
        // Path segments with special chars should be percent-encoded
        assert!(url.contains("provider%2Fname"));
        assert!(url.contains("v1%26bad"));
        assert!(url.contains("action%3Devil"));
    }
}
