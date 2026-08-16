use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const PLAYBACK_THUMBNAIL_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const SIGNATURE_PROVIDER: &str = "seafile";

#[derive(Clone, Copy)]
pub struct SeafileThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub repository_id: &'a str,
    pub path: &'a str,
    pub size: u32,
}

pub fn signature_version(scope: SeafileThumbnailScope<'_>) -> String {
    let mut hasher = Sha256::new();
    for value in [
        scope.server_id,
        scope.credential_owner_id,
        scope.repository_id,
        scope.path,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    hasher.update(scope.size.to_be_bytes());
    hex::encode(hasher.finalize())
}

pub fn playback_thumbnail_url(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: SeafileThumbnailScope<'_>,
) -> String {
    let scope = SeafileThumbnailScope {
        size: scope.size.clamp(32, 2048),
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
        .append_pair("repositoryId", scope.repository_id)
        .append_pair("path", scope.path)
        .append_pair("size", &scope.size.to_string())
        .finish();
    let signed_query = signing_key.build_signed_playback_query(&claims);
    format!(
        "{PLAYBACK_THUMBNAIL_ROUTE_PREFIX}/{room_id}/seafile/thumbnail?{resource_query}&{signed_query}"
    )
}
