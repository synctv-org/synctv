use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const THUMBNAIL_ROUTE: &str = "/api/providers/fnos/thumbnail";
pub const PLAYBACK_IMAGE_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const SIGNATURE_PROVIDER: &str = "fnos";

#[derive(Clone, Copy)]
pub struct FnosThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub image_path: &'a str,
    pub width: u32,
}

pub fn signature_version(scope: FnosThumbnailScope<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.credential_owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.image_path.as_bytes());
    hasher.update([0]);
    hasher.update(scope.width.to_be_bytes());
    hex::encode(hasher.finalize())
}

pub fn playback_image_url(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: FnosThumbnailScope<'_>,
) -> String {
    let scope = FnosThumbnailScope {
        width: scope.width.clamp(1, 1920),
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
        .append_pair("imagePath", scope.image_path)
        .append_pair("width", &scope.width.to_string())
        .finish();
    let signed_query = signing_key.build_signed_playback_query(&claims);
    format!("{PLAYBACK_IMAGE_ROUTE_PREFIX}/{room_id}/fnos/image?{resource_query}&{signed_query}")
}

pub fn provider_thumbnail_url(server_id: &str, image_path: &str, width: u32) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("imagePath", image_path)
        .append_pair("width", &width.clamp(1, 1920).to_string())
        .finish();
    format!("{THUMBNAIL_ROUTE}?{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_room_scoped_image_url() {
        let key = ProxySigningKey::try_derive_from(b"fnos-thumbnail-test-key-at-least-32-bytes")
            .expect("key should derive");
        let scope = FnosThumbnailScope {
            server_id: "server",
            credential_owner_id: "owner",
            image_path: "/aa/bb/poster.jpg",
            width: 800,
        };
        let signed = playback_image_url(&key, "room", "viewer", scope);
        let raw_query = signed
            .split_once('?')
            .expect("signed thumbnail URL should contain a query")
            .1;
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
            .expect("signed image access should verify");
        assert_eq!(claims.user_id, "viewer");
        assert!(!raw_query.contains("rid="));
    }
}
