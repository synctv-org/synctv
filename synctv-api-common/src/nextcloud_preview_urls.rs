use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const PLAYBACK_PREVIEW_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const SIGNATURE_PROVIDER: &str = "nextcloud";

#[derive(Clone, Copy)]
pub struct NextcloudPreviewScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub file_id: u64,
    pub width: u32,
    pub height: u32,
    pub crop: bool,
}

pub fn signature_version(scope: NextcloudPreviewScope<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.credential_owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.file_id.to_be_bytes());
    hasher.update(scope.width.to_be_bytes());
    hasher.update(scope.height.to_be_bytes());
    hasher.update([u8::from(scope.crop)]);
    hex::encode(hasher.finalize())
}

pub fn playback_preview_url(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: NextcloudPreviewScope<'_>,
) -> String {
    let scope = NextcloudPreviewScope {
        width: scope.width.clamp(1, 2048),
        height: scope.height.clamp(1, 2048),
        ..scope
    };
    let claims = ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "preview".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at: synctv_core::SystemClock.now().timestamp()
            + ProxySigningKey::default_expiry_secs(),
        target_url: None,
    };
    let resource_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", scope.server_id)
        .append_pair("credentialOwnerId", scope.credential_owner_id)
        .append_pair("fileId", &scope.file_id.to_string())
        .append_pair("width", &scope.width.to_string())
        .append_pair("height", &scope.height.to_string())
        .append_pair("crop", if scope.crop { "true" } else { "false" })
        .finish();
    let signed_query = signing_key.build_signed_playback_query(&claims);
    format!(
        "{PLAYBACK_PREVIEW_ROUTE_PREFIX}/{room_id}/nextcloud/preview?{resource_query}&{signed_query}"
    )
}
