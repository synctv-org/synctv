use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const THUMBNAIL_ROUTE: &str = "/api/providers/qnap/thumbnail";
pub const PLAYBACK_THUMBNAIL_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const SIGNATURE_PROVIDER: &str = "qnap";

#[derive(Clone, Copy)]
pub struct QnapThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub path: &'a str,
    pub size: u32,
}

pub fn signature_version(scope: QnapThumbnailScope<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.credential_owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.path.as_bytes());
    hasher.update([0]);
    hasher.update(scope.size.to_be_bytes());
    hex::encode(hasher.finalize())
}

pub fn playback_thumbnail_url(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: QnapThumbnailScope<'_>,
) -> String {
    let scope = QnapThumbnailScope {
        size: scope.size.clamp(1, 640),
        ..scope
    };
    let claims = ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at: synctv_core::SystemClock.now().timestamp()
            + ProxySigningKey::default_expiry_secs(),
        target_url: None,
    };
    let resource_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", scope.server_id)
        .append_pair("credentialOwnerId", scope.credential_owner_id)
        .append_pair("path", scope.path)
        .append_pair("size", &scope.size.to_string())
        .finish();
    let signed_query = signing_key.build_signed_playback_query(&claims);
    format!(
        "{PLAYBACK_THUMBNAIL_ROUTE_PREFIX}/{room_id}/qnap/thumbnail?{resource_query}&{signed_query}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_room_scoped_qnap_thumbnail() {
        let key = ProxySigningKey::try_derive_from(b"qnap-thumbnail-test-key-at-least-32-bytes")
            .expect("key should derive");
        let scope = QnapThumbnailScope {
            server_id: "server",
            credential_owner_id: "owner",
            path: "/Multimedia/Movie.mkv",
            size: 640,
        };
        let signed = playback_thumbnail_url(&key, "room", "viewer", scope);
        let raw_query = signed.split_once('?').expect("query").1;
        let signature_query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                url::form_urlencoded::parse(raw_query.as_bytes())
                    .filter(|(key, _)| matches!(key.as_ref(), "sig" | "uid" | "exp")),
            )
            .finish();
        let claims = key
            .parse_and_verify_playback_query(
                &signature_query,
                SIGNATURE_PROVIDER,
                &signature_version(scope),
                "thumbnail",
                "room",
            )
            .expect("signed thumbnail access should verify");
        assert_eq!(claims.user_id, "viewer");
        assert!(!raw_query.contains("rid="));
    }
}
