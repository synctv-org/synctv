//! API-facing signed proxy query formatting and parsing.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use url::form_urlencoded;
use urlencoding::encode as url_encode;

type HmacSha256 = Hmac<Sha256>;

/// Domain separator for deriving the proxy URL signing key.
const DOMAIN_SEPARATOR: &[u8] = b"synctv-proxy-sign";
const MEDIA_SWARM_KEY_DOMAIN_SEPARATOR: &[u8] = b"synctv-media-swarm-signing-key-v1";

/// Default proxy URL lifetime (30 minutes).
const DEFAULT_EXPIRY_SECS: i64 = 30 * 60;
const MAX_EXPIRY_SECS: i64 = 24 * 60 * 60;
const MEDIA_SWARM_TICKET_EXPIRY_SECS: i64 = 24 * 60 * 60;
const MEDIA_SWARM_TICKET_DOMAIN: &str = "synctv-media-swarm-v2";

/// HMAC-SHA256 signing key for proxy URLs.
///
pub struct ProxySigningKey {
    key: HmacSha256,
}

/// HMAC-SHA256 signing key for WebRTC media swarm capability tickets.
pub struct MediaSwarmSigningKey {
    key: HmacSha256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSwarmTicketClaims {
    pub playback_generation: i64,
    pub resource_owner_id: Option<String>,
}

/// Claims used for signed proxy access.
///
/// `resource` is the API/provider-specific semantic resource derived from the
/// route/request, such as `streams/direct/0` or `dash-manifests/720p/proxy`.
/// `target_url` is bound into the signature when present so rewritten M3U8
/// segment targets cannot be retargeted by changing exposed transport fields.
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
    /// The signed lifetime is invalid or exceeds the maximum.
    InvalidLifetime,
}

impl fmt::Display for ProxySignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSigningKey => write!(f, "invalid proxy signing key"),
            Self::InvalidSignature => write!(f, "invalid proxy signature"),
            Self::Expired => write!(f, "proxy URL expired"),
            Self::InvalidLifetime => write!(f, "invalid proxy URL lifetime"),
        }
    }
}

impl std::error::Error for ProxySignatureError {}

#[derive(Debug)]
pub enum MediaSwarmTicketError {
    InvalidSigningKey,
    InvalidSignature,
    Expired,
    InvalidLifetime,
}

impl fmt::Display for MediaSwarmTicketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSigningKey => write!(f, "invalid media swarm signing key"),
            Self::InvalidSignature => write!(f, "invalid media swarm ticket signature"),
            Self::Expired => write!(f, "media swarm ticket expired"),
            Self::InvalidLifetime => write!(f, "invalid media swarm ticket lifetime"),
        }
    }
}

impl std::error::Error for MediaSwarmTicketError {}

impl ProxySigningKey {
    /// Derive a proxy signing key from its dedicated security-domain secret.
    pub fn try_derive_from(signing_secret: &[u8]) -> Result<Self, ProxySignatureError> {
        let mut derivation_mac = HmacSha256::new_from_slice(signing_secret)
            .map_err(|_| ProxySignatureError::InvalidSigningKey)?;
        derivation_mac.update(DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();

        let key = HmacSha256::new_from_slice(&derived)
            .map_err(|_| ProxySignatureError::InvalidSigningKey)?;
        Ok(Self { key })
    }

    /// Sign claims and return the signing timestamp plus hex-encoded HMAC-SHA256.
    #[must_use]
    pub fn sign(&self, claims: &ProxyUrlClaims) -> String {
        let issued_at = synctv_core::SystemClock.now().timestamp();
        self.sign_at(claims, issued_at)
    }

    fn sign_at(&self, claims: &ProxyUrlClaims, issued_at: i64) -> String {
        self.sign_with_overrides_at(
            claims,
            &claims.resource,
            claims.target_url.as_deref(),
            issued_at,
        )
    }

    fn sign_with_overrides(
        &self,
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: Option<&str>,
    ) -> String {
        self.sign_with_overrides_at(
            claims,
            resource,
            target_url,
            synctv_core::SystemClock.now().timestamp(),
        )
    }

    fn sign_with_overrides_at(
        &self,
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: Option<&str>,
        issued_at: i64,
    ) -> String {
        let mut mac = self.key.clone();
        mac.update(Self::canonical_message(claims, resource, target_url, issued_at).as_bytes());
        format!("{issued_at}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    /// Verify claims against a signing timestamp and hex-encoded signature.
    pub fn verify(
        &self,
        claims: &ProxyUrlClaims,
        signature: &str,
    ) -> Result<(), ProxySignatureError> {
        let now = synctv_core::SystemClock.now().timestamp();
        if now >= claims.expires_at {
            return Err(ProxySignatureError::Expired);
        }
        let (issued_at, signature) = signature
            .split_once('.')
            .ok_or(ProxySignatureError::InvalidSignature)?;
        let issued_at = issued_at
            .parse::<i64>()
            .map_err(|_| ProxySignatureError::InvalidSignature)?;
        let lifetime = claims
            .expires_at
            .checked_sub(issued_at)
            .ok_or(ProxySignatureError::InvalidLifetime)?;
        if issued_at > now || !(1..=MAX_EXPIRY_SECS).contains(&lifetime) {
            return Err(ProxySignatureError::InvalidLifetime);
        }

        let sig_bytes =
            hex::decode(signature).map_err(|_| ProxySignatureError::InvalidSignature)?;
        let mut mac = self.key.clone();
        mac.update(
            Self::canonical_message(
                claims,
                &claims.resource,
                claims.target_url.as_deref(),
                issued_at,
            )
            .as_bytes(),
        );
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ProxySignatureError::InvalidSignature)
    }

    fn canonical_message(
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: Option<&str>,
        issued_at: i64,
    ) -> String {
        let mut message = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            claims.provider,
            claims.version,
            resource,
            claims.room_id,
            claims.user_id,
            issued_at,
            claims.expires_at
        );
        if let Some(target_url) = target_url {
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

impl MediaSwarmSigningKey {
    pub fn try_derive_from(signing_secret: &[u8]) -> Result<Self, MediaSwarmTicketError> {
        let mut derivation_mac = HmacSha256::new_from_slice(signing_secret)
            .map_err(|_| MediaSwarmTicketError::InvalidSigningKey)?;
        derivation_mac.update(MEDIA_SWARM_KEY_DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();
        let key = HmacSha256::new_from_slice(&derived)
            .map_err(|_| MediaSwarmTicketError::InvalidSigningKey)?;
        Ok(Self { key })
    }

    /// Create a capability bound to one actor and one room playback generation.
    #[must_use]
    pub fn sign_media_swarm_ticket(
        &self,
        room_id: &str,
        actor_id: &str,
        swarm_id: &str,
        playback_generation: i64,
        resource_owner_id: Option<&str>,
    ) -> String {
        let expires_at = synctv_core::SystemClock
            .now()
            .timestamp()
            .saturating_add(MEDIA_SWARM_TICKET_EXPIRY_SECS);
        let encoded_resource_owner_id = resource_owner_id.map_or_else(String::new, hex::encode);
        let mut mac = self.key.clone();
        mac.update(
            Self::media_swarm_ticket_message(
                room_id,
                actor_id,
                swarm_id,
                playback_generation,
                resource_owner_id,
                expires_at,
            )
            .as_bytes(),
        );
        format!(
            "2.{expires_at}.{playback_generation}.{encoded_resource_owner_id}.{}",
            hex::encode(mac.finalize().into_bytes())
        )
    }

    /// Verify and decode a media swarm capability.
    pub fn verify_media_swarm_ticket(
        &self,
        room_id: &str,
        actor_id: &str,
        swarm_id: &str,
        ticket: &str,
    ) -> Result<MediaSwarmTicketClaims, MediaSwarmTicketError> {
        let mut parts = ticket.split('.');
        if parts.next() != Some("2") {
            return Err(MediaSwarmTicketError::InvalidSignature);
        }
        let expires_at = parts
            .next()
            .ok_or(MediaSwarmTicketError::InvalidSignature)?;
        let playback_generation = parts
            .next()
            .ok_or(MediaSwarmTicketError::InvalidSignature)?
            .parse::<i64>()
            .map_err(|_| MediaSwarmTicketError::InvalidSignature)?;
        let encoded_resource_owner_id = parts
            .next()
            .ok_or(MediaSwarmTicketError::InvalidSignature)?;
        let signature = parts
            .next()
            .ok_or(MediaSwarmTicketError::InvalidSignature)?;
        if parts.next().is_some() || playback_generation < 0 {
            return Err(MediaSwarmTicketError::InvalidSignature);
        }
        let resource_owner_id = if encoded_resource_owner_id.is_empty() {
            None
        } else {
            Some(
                String::from_utf8(
                    hex::decode(encoded_resource_owner_id)
                        .map_err(|_| MediaSwarmTicketError::InvalidSignature)?,
                )
                .map_err(|_| MediaSwarmTicketError::InvalidSignature)?,
            )
        };
        let expires_at = expires_at
            .parse::<i64>()
            .map_err(|_| MediaSwarmTicketError::InvalidSignature)?;
        let now = synctv_core::SystemClock.now().timestamp();
        if now >= expires_at {
            return Err(MediaSwarmTicketError::Expired);
        }
        let lifetime = expires_at.saturating_sub(now);
        if !(1..=MEDIA_SWARM_TICKET_EXPIRY_SECS).contains(&lifetime) {
            return Err(MediaSwarmTicketError::InvalidLifetime);
        }
        let signature =
            hex::decode(signature).map_err(|_| MediaSwarmTicketError::InvalidSignature)?;
        let mut mac = self.key.clone();
        mac.update(
            Self::media_swarm_ticket_message(
                room_id,
                actor_id,
                swarm_id,
                playback_generation,
                resource_owner_id.as_deref(),
                expires_at,
            )
            .as_bytes(),
        );
        mac.verify_slice(&signature)
            .map_err(|_| MediaSwarmTicketError::InvalidSignature)?;
        Ok(MediaSwarmTicketClaims {
            playback_generation,
            resource_owner_id,
        })
    }

    fn media_swarm_ticket_message(
        room_id: &str,
        actor_id: &str,
        swarm_id: &str,
        playback_generation: i64,
        resource_owner_id: Option<&str>,
        expires_at: i64,
    ) -> String {
        let resource_owner_id = resource_owner_id.unwrap_or_default();
        format!(
            "{MEDIA_SWARM_TICKET_DOMAIN}\nroom:{room_id}\nactor:{actor_id}\nswarm:{swarm_id}\ngeneration:{playback_generation}\nowner:{resource_owner_id}\nexpires:{expires_at}"
        )
    }
}

pub trait ProxySigningKeyQueryExt {
    fn build_signed_playback_query(&self, claims: &ProxyUrlClaims) -> String;

    fn build_signed_playback_query_with_target_url(
        &self,
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: &str,
    ) -> String;

    fn parse_and_verify_playback_query(
        &self,
        query: &str,
        provider: &str,
        version: &str,
        resource: &str,
        room_id: &str,
    ) -> Result<ProxyUrlClaims, ProxySignatureQueryError>;
}

impl ProxySigningKeyQueryExt for ProxySigningKey {
    fn build_signed_playback_query(&self, claims: &ProxyUrlClaims) -> String {
        build_signed_playback_query(self, claims)
    }

    fn build_signed_playback_query_with_target_url(
        &self,
        claims: &ProxyUrlClaims,
        resource: &str,
        target_url: &str,
    ) -> String {
        build_signed_playback_query_with_target_url(self, claims, resource, target_url)
    }

    fn parse_and_verify_playback_query(
        &self,
        query: &str,
        provider: &str,
        version: &str,
        resource: &str,
        room_id: &str,
    ) -> Result<ProxyUrlClaims, ProxySignatureQueryError> {
        parse_and_verify_playback_query(self, query, provider, version, resource, room_id)
    }
}

#[derive(Debug)]
pub enum ProxySignatureQueryError {
    Signature(ProxySignatureError),
    MissingParam(&'static str),
    InvalidParam(&'static str),
    UnknownParam(String),
}

impl fmt::Display for ProxySignatureQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signature(error) => write!(f, "{error}"),
            Self::MissingParam(name) => write!(f, "missing query param: {name}"),
            Self::InvalidParam(name) => write!(f, "invalid query param: {name}"),
            Self::UnknownParam(name) => write!(f, "unknown query param: {name}"),
        }
    }
}

impl std::error::Error for ProxySignatureQueryError {}

impl From<ProxySignatureError> for ProxySignatureQueryError {
    fn from(error: ProxySignatureError) -> Self {
        Self::Signature(error)
    }
}

/// Build the query for a playback-provider URL whose room is represented by
/// the route path. The room remains part of the signed claims.
#[must_use]
pub fn build_signed_playback_query(
    signing_key: &ProxySigningKey,
    claims: &ProxyUrlClaims,
) -> String {
    let sig = signing_key.sign(claims);
    build_playback_query(claims, &sig, claims.target_url.as_deref())
}

#[must_use]
pub fn build_signed_playback_query_with_target_url(
    signing_key: &ProxySigningKey,
    claims: &ProxyUrlClaims,
    resource: &str,
    target_url: &str,
) -> String {
    let sig = signing_key.sign_with_overrides(claims, resource, Some(target_url));
    build_playback_query(claims, &sig, Some(target_url))
}

fn build_playback_query(claims: &ProxyUrlClaims, sig: &str, target_url: Option<&str>) -> String {
    let mut query = format!(
        "sig={}&uid={}&exp={}",
        url_encode(sig),
        url_encode(&claims.user_id),
        claims.expires_at
    );
    if let Some(target_url) = target_url {
        query.push_str("&targetUrl=");
        query.push_str(&url_encode(target_url));
    }
    query
}

pub fn parse_and_verify_playback_query(
    signing_key: &ProxySigningKey,
    query: &str,
    provider: &str,
    version: &str,
    resource: &str,
    room_id: &str,
) -> Result<ProxyUrlClaims, ProxySignatureQueryError> {
    let mut sig: Option<String> = None;
    let mut uid: Option<String> = None;
    let mut exp_str: Option<String> = None;
    let mut target_url: Option<String> = None;

    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "sig" => sig = Some(value.into_owned()),
            "uid" => uid = Some(value.into_owned()),
            "exp" => exp_str = Some(value.into_owned()),
            "targetUrl" => target_url = Some(value.into_owned()),
            _ => return Err(ProxySignatureQueryError::UnknownParam(key.into_owned())),
        }
    }

    let sig = sig.ok_or(ProxySignatureQueryError::MissingParam("sig"))?;
    let uid = uid.ok_or(ProxySignatureQueryError::MissingParam("uid"))?;
    let exp_str = exp_str.ok_or(ProxySignatureQueryError::MissingParam("exp"))?;

    if uid.is_empty() {
        return Err(ProxySignatureQueryError::InvalidParam(
            "uid cannot be empty",
        ));
    }
    if room_id.is_empty() {
        return Err(ProxySignatureQueryError::InvalidParam(
            "roomId cannot be empty",
        ));
    }

    let expires_at = exp_str
        .parse()
        .map_err(|_| ProxySignatureQueryError::InvalidParam("exp"))?;

    let claims = ProxyUrlClaims {
        provider: provider.to_string(),
        version: version.to_string(),
        resource: resource.to_string(),
        room_id: room_id.to_string(),
        user_id: uid,
        expires_at,
        target_url,
    };

    signing_key.verify(&claims, &sig)?;
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: fmt::Display>(result: Result<T, E>, context: &str) -> T {
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
            ProxySigningKey::try_derive_from(b"test-proxy-signing-secret-that-is-long-enough"),
            "test proxy signing key should derive",
        )
    }

    fn test_swarm_key() -> MediaSwarmSigningKey {
        ok(
            MediaSwarmSigningKey::try_derive_from(
                b"test-media-swarm-signing-secret-that-is-long-enough",
            ),
            "test media swarm signing key should derive",
        )
    }

    fn test_claims() -> ProxyUrlClaims {
        ProxyUrlClaims {
            provider: "emby".to_string(),
            version: "abc123".to_string(),
            resource: "media-streams/main/0".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 3600,
            target_url: None,
        }
    }

    #[test]
    fn media_swarm_ticket_roundtrip() {
        let key = test_swarm_key();
        let ticket =
            key.sign_media_swarm_ticket("room_1", "usr_1", "sm1_resource", 7, Some("usr_owner"));

        assert_eq!(
            key.verify_media_swarm_ticket("room_1", "usr_1", "sm1_resource", &ticket)
                .expect("ticket should verify"),
            MediaSwarmTicketClaims {
                playback_generation: 7,
                resource_owner_id: Some("usr_owner".to_string()),
            }
        );
    }

    #[test]
    fn media_swarm_ticket_binds_room_actor_and_swarm() {
        let key = test_swarm_key();
        let ticket = key.sign_media_swarm_ticket("room_1", "usr_1", "sm1_resource", 7, None);

        for (room_id, actor_id, swarm_id) in [
            ("room_2", "usr_1", "sm1_resource"),
            ("room_1", "usr_2", "sm1_resource"),
            ("room_1", "usr_1", "sm1_other"),
        ] {
            assert!(matches!(
                key.verify_media_swarm_ticket(room_id, actor_id, swarm_id, &ticket),
                Err(MediaSwarmTicketError::InvalidSignature)
            ));
        }
    }

    #[test]
    fn build_and_parse_query_roundtrip() {
        let key = test_key();
        let claims = test_claims();
        let query = build_signed_playback_query(&key, &claims);
        let parsed = ok(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            "signed query should parse",
        );
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
    }

    #[test]
    fn playback_query_binds_the_path_room_id() {
        let key = test_key();
        let claims = test_claims();
        let query = build_signed_playback_query(&key, &claims);
        assert!(!query.contains("rid="));

        let parsed = ok(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            "playback query should parse with its path room id",
        );
        assert_eq!(parsed.provider, claims.provider);
        assert_eq!(parsed.version, claims.version);
        assert_eq!(parsed.resource, claims.resource);
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
        assert_eq!(parsed.target_url, claims.target_url);
        assert!(matches!(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                "room-2",
            ),
            Err(ProxySignatureQueryError::Signature(
                ProxySignatureError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn playback_query_rejects_legacy_room_id() {
        let key = test_key();
        let claims = test_claims();
        let query = format!(
            "{}&rid={}",
            build_signed_playback_query(&key, &claims),
            claims.room_id
        );
        assert!(matches!(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            Err(ProxySignatureQueryError::UnknownParam(param)) if param == "rid"
        ));
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
        claims.expires_at = synctv_core::SystemClock.now().timestamp() - 1;
        let sig = key.sign(&claims);
        assert!(matches!(
            key.verify(&claims, &sig),
            Err(ProxySignatureError::Expired)
        ));
    }

    #[test]
    fn verify_rejects_the_expiry_boundary() {
        let key = test_key();
        let mut claims = test_claims();
        claims.expires_at = synctv_core::SystemClock.now().timestamp();
        let sig = key.sign(&claims);
        assert!(matches!(
            key.verify(&claims, &sig),
            Err(ProxySignatureError::Expired)
        ));
    }

    #[test]
    fn verify_rejects_lifetime_over_24_hours_after_time_has_elapsed() {
        let key = test_key();
        let now = synctv_core::SystemClock.now().timestamp();
        let mut claims = test_claims();
        claims.expires_at = now + 23 * 60 * 60;
        let signature = key.sign_at(&claims, now - 2 * 60 * 60);

        assert!(matches!(
            key.verify(&claims, &signature),
            Err(ProxySignatureError::InvalidLifetime)
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
    fn dash_segment_claims_derived_from_manifest_claims_verify_as_segment_resource() {
        let key = test_key();
        let manifest_claims = ProxyUrlClaims {
            provider: "bilibili".to_string(),
            version: "v1".to_string(),
            resource: "dash-manifests/dash/proxy".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 3600,
            target_url: None,
        };
        let mut segment_claims = manifest_claims.clone();
        segment_claims.resource = "dash-segments/dash/0".to_string();
        segment_claims.target_url = None;

        let sig = key.sign(&segment_claims);
        assert!(key.verify(&segment_claims, &sig).is_ok());

        assert_eq!(segment_claims.room_id, manifest_claims.room_id);
        assert_eq!(segment_claims.user_id, manifest_claims.user_id);
        assert_eq!(segment_claims.expires_at, manifest_claims.expires_at);
        assert_eq!(segment_claims.target_url, None);
        let mut tampered = segment_claims;
        tampered.resource = "dash-segments/dash/1".to_string();
        assert!(matches!(
            key.verify(&tampered, &sig),
            Err(ProxySignatureError::InvalidSignature)
        ));
    }

    #[test]
    fn parse_query_binds_url_param_to_signature() {
        let key = test_key();
        let mut claims = test_claims();
        claims.target_url = Some("http://example.com/seg.ts".to_string());
        let query = build_signed_playback_query(&key, &claims);
        let parsed = ok(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
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
        let query = build_signed_playback_query(&key, &claims);
        let (prefix, _) = some(
            query.split_once("&targetUrl="),
            "signed target query should include targetUrl",
        );
        let tampered = format!(
            "{prefix}&targetUrl={}",
            urlencoding::encode("http://evil.example/seg.ts")
        );

        assert!(matches!(
            parse_and_verify_playback_query(
                &key,
                &tampered,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            Err(ProxySignatureQueryError::Signature(
                ProxySignatureError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn parse_query_missing_sig() {
        let key = test_key();
        assert!(matches!(
            parse_and_verify_playback_query(
                &key,
                "uid=u1&exp=999999999999",
                "emby",
                "v1",
                "media-streams/main/0",
                "room-1",
            ),
            Err(ProxySignatureQueryError::MissingParam("sig"))
        ));
    }

    #[test]
    fn parse_query_rejects_unknown_query_param() {
        let key = test_key();
        let claims = test_claims();
        let query = format!("{}&extra=1", build_signed_playback_query(&key, &claims));

        assert!(matches!(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            Err(ProxySignatureQueryError::UnknownParam(param)) if param == "extra"
        ));
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
            expires_at: synctv_core::SystemClock.now().timestamp() + 3600,
            target_url: Some("https://cdn.example.com/a segment.ts?x=1&y=2".to_string()),
        };
        let query = build_signed_playback_query(&key, &claims);

        let parsed = ok(
            parse_and_verify_playback_query(
                &key,
                &query,
                &claims.provider,
                &claims.version,
                &claims.resource,
                &claims.room_id,
            ),
            "encoded signed query should parse",
        );
        assert_eq!(parsed.room_id, claims.room_id);
        assert_eq!(parsed.user_id, claims.user_id);
        assert_eq!(parsed.expires_at, claims.expires_at);
        assert_eq!(parsed.target_url, claims.target_url);
    }
}
