use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const PREVIEW_ROUTE: &str = "/api/providers/nextcloud/preview";
const SIGNATURE_PROVIDER: &str = "nextcloud-preview";

#[derive(Clone, Copy)]
pub struct NextcloudPreviewScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub file_id: u64,
    pub width: u32,
    pub height: u32,
    pub crop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextcloudPreviewAccessError {
    Invalid,
    WrongUser,
}

fn signature_version(scope: NextcloudPreviewScope<'_>) -> String {
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

pub fn nextcloud_preview_url(
    server_id: &str,
    credential_owner_id: &str,
    file_id: u64,
    width: u32,
    height: u32,
    crop: bool,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("fileId", &file_id.to_string())
        .append_pair("width", &width.clamp(1, 2048).to_string())
        .append_pair("height", &height.clamp(1, 2048).to_string())
        .append_pair("crop", if crop { "true" } else { "false" })
        .finish();
    format!("{PREVIEW_ROUTE}?{query}")
}

pub fn sign_nextcloud_preview_url(
    url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    let raw_query = url
        .strip_prefix(PREVIEW_ROUTE)
        .and_then(|suffix| suffix.strip_prefix('?'))
        .ok_or_else(|| "Invalid Nextcloud preview URL".to_string())?;
    let params = url::form_urlencoded::parse(raw_query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let required = |key: &str| {
        params
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Nextcloud preview URL missing {key}"))
    };
    let scope = NextcloudPreviewScope {
        server_id: required("serverId")?,
        credential_owner_id: required("credentialOwnerId")?,
        file_id: required("fileId")?
            .parse()
            .map_err(|_| "Invalid Nextcloud fileId".to_string())?,
        width: required("width")?
            .parse::<u32>()
            .map_err(|_| "Invalid Nextcloud width".to_string())?
            .clamp(1, 2048),
        height: required("height")?
            .parse::<u32>()
            .map_err(|_| "Invalid Nextcloud height".to_string())?
            .clamp(1, 2048),
        crop: required("crop")?
            .parse()
            .map_err(|_| "Invalid Nextcloud crop".to_string())?,
    };
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    let query = signing_key.build_signed_query(&ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "preview".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    });
    Ok(format!("{url}&{query}"))
}

pub fn verify_nextcloud_preview_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: NextcloudPreviewScope<'_>,
) -> Result<String, NextcloudPreviewAccessError> {
    let signature_query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            url::form_urlencoded::parse(raw_query.as_bytes())
                .filter(|(key, _)| matches!(key.as_ref(), "sig" | "uid" | "rid" | "exp")),
        )
        .finish();
    let claims = signing_key
        .parse_and_verify_query(
            &signature_query,
            SIGNATURE_PROVIDER,
            &signature_version(scope),
            "preview",
        )
        .map_err(|_| NextcloudPreviewAccessError::Invalid)?;
    if claims.user_id != auth_user_id {
        return Err(NextcloudPreviewAccessError::WrongUser);
    }
    Ok(claims.room_id)
}
