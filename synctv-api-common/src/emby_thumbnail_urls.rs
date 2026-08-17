use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};
use sha2::{Digest, Sha256};

const DEFAULT_THUMBNAIL_HEIGHT: u32 = 300;
const MAX_THUMBNAIL_DIMENSION: u32 = 1920;
pub const PROVIDER_THUMBNAIL_ROUTE_PREFIX: &str = "/api/providers/emby/thumbnail/";
pub const PLAYBACK_THUMBNAIL_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const THUMBNAIL_SIGNATURE_PROVIDER: &str = "emby";

#[derive(Clone, Copy)]
pub struct ThumbnailSignatureScope<'a> {
    pub item_id: &'a str,
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub max_height: u32,
    pub max_width: u32,
}

pub fn clamp_thumbnail_dimension(value: Option<u32>, default: u32) -> u32 {
    value
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(MAX_THUMBNAIL_DIMENSION)
}

pub fn provider_thumbnail_url(
    server_id: &str,
    item_id: &str,
    max_height: u32,
    max_width: u32,
) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("serverId", server_id).append_pair(
        "maxHeight",
        &clamp_thumbnail_dimension(Some(max_height), DEFAULT_THUMBNAIL_HEIGHT).to_string(),
    );
    if max_width > 0 {
        query.append_pair(
            "maxWidth",
            &clamp_thumbnail_dimension(Some(max_width), 0).to_string(),
        );
    }
    let item_id =
        percent_encoding::utf8_percent_encode(item_id, percent_encoding::NON_ALPHANUMERIC);
    format!(
        "{PROVIDER_THUMBNAIL_ROUTE_PREFIX}{item_id}?{}",
        query.finish()
    )
}

pub fn thumbnail_signature_version(scope: ThumbnailSignatureScope<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.item_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.credential_owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.max_height.to_be_bytes());
    hasher.update(scope.max_width.to_be_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

pub fn playback_thumbnail_url(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: ThumbnailSignatureScope<'_>,
) -> String {
    let scope = ThumbnailSignatureScope {
        max_height: clamp_thumbnail_dimension(Some(scope.max_height), DEFAULT_THUMBNAIL_HEIGHT),
        max_width: clamp_thumbnail_dimension(Some(scope.max_width), 0),
        ..scope
    };
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    let claims = ProxyUrlClaims {
        provider: THUMBNAIL_SIGNATURE_PROVIDER.to_string(),
        version: thumbnail_signature_version(scope),
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    };
    let signed_query = signing_key.build_signed_playback_query(&claims);
    let resource_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", scope.server_id)
        .append_pair("credentialOwnerId", scope.credential_owner_id)
        .append_pair("maxHeight", &scope.max_height.to_string())
        .append_pair("maxWidth", &scope.max_width.to_string())
        .finish();
    let item_id =
        percent_encoding::utf8_percent_encode(scope.item_id, percent_encoding::NON_ALPHANUMERIC);
    format!(
        "{PLAYBACK_THUMBNAIL_ROUTE_PREFIX}/{room_id}/emby/thumbnail/{item_id}?{resource_query}&{signed_query}"
    )
}
