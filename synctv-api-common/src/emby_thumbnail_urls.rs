use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};
use sha2::{Digest, Sha256};

const DEFAULT_THUMBNAIL_HEIGHT: u32 = 300;
const MAX_THUMBNAIL_DIMENSION: u32 = 1920;
pub const THUMBNAIL_ROUTE_PREFIX: &str = "/api/providers/emby/thumbnail/";
pub const THUMBNAIL_SIGNATURE_PROVIDER: &str = "emby-thumbnail";

#[derive(Clone, Copy)]
pub struct ThumbnailSignatureScope<'a> {
    pub item_id: &'a str,
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub max_height: u32,
    pub max_width: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSignatureAccessError {
    Invalid,
    WrongUser,
}

pub fn clamp_thumbnail_dimension(value: Option<u32>, default: u32) -> u32 {
    value
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(MAX_THUMBNAIL_DIMENSION)
}

pub fn emby_thumbnail_url(server_id: &str, credential_owner_id: &str, item_id: &str) -> String {
    format!(
        "{THUMBNAIL_ROUTE_PREFIX}{item_id}?serverId={server_id}&credentialOwnerId={credential_owner_id}&maxHeight=300",
        item_id = percent_encoding::utf8_percent_encode(item_id, percent_encoding::NON_ALPHANUMERIC),
        server_id = percent_encoding::utf8_percent_encode(server_id, percent_encoding::NON_ALPHANUMERIC),
        credential_owner_id = percent_encoding::utf8_percent_encode(credential_owner_id, percent_encoding::NON_ALPHANUMERIC),
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

pub fn build_signed_thumbnail_query(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: ThumbnailSignatureScope<'_>,
    expires_at: i64,
) -> String {
    let claims = ProxyUrlClaims {
        provider: THUMBNAIL_SIGNATURE_PROVIDER.to_string(),
        version: thumbnail_signature_version(scope),
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    };
    signing_key.build_signed_query(&claims)
}

pub fn thumbnail_signature_present(raw_query: &str) -> bool {
    url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key.as_ref() == "sig")
}

pub fn thumbnail_signature_query(raw_query: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            url::form_urlencoded::parse(raw_query.as_bytes())
                .filter(|(key, _)| matches!(key.as_ref(), "sig" | "uid" | "rid" | "exp")),
        )
        .finish()
}

pub fn verify_signed_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: ThumbnailSignatureScope<'_>,
) -> Result<String, ThumbnailSignatureAccessError> {
    let signature_query = thumbnail_signature_query(raw_query);
    let claims = signing_key
        .parse_and_verify_query(
            &signature_query,
            THUMBNAIL_SIGNATURE_PROVIDER,
            &thumbnail_signature_version(scope),
            "thumbnail",
        )
        .map_err(|_| ThumbnailSignatureAccessError::Invalid)?;

    if claims.user_id != auth_user_id {
        return Err(ThumbnailSignatureAccessError::WrongUser);
    }

    Ok(claims.room_id)
}

pub fn sign_emby_thumbnail_url(
    thumbnail_url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    let Some(path_with_item) = thumbnail_url.strip_prefix(THUMBNAIL_ROUTE_PREFIX) else {
        return Ok(thumbnail_url.to_string());
    };
    let Some((item_id, raw_query)) = path_with_item.split_once('?') else {
        return Ok(thumbnail_url.to_string());
    };
    if thumbnail_signature_present(raw_query) {
        return Ok(thumbnail_url.to_string());
    }

    let mut server_id = None;
    let mut credential_owner_id = None;
    let mut max_height = None;
    let mut max_width = None;
    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        match key.as_ref() {
            "serverId" => server_id = Some(value.into_owned()),
            "credentialOwnerId" => credential_owner_id = Some(value.into_owned()),
            "maxHeight" => {
                max_height = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail maxHeight: {error}")
                })?);
            }
            "maxWidth" => {
                max_width = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail maxWidth: {error}")
                })?);
            }
            _ => {}
        }
    }
    let server_id = server_id
        .map(|value: String| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Emby thumbnail URL missing serverId".to_string())?;
    let credential_owner_id = credential_owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(user_id)
        .to_string();
    let max_height = clamp_thumbnail_dimension(max_height, DEFAULT_THUMBNAIL_HEIGHT);
    let max_width = clamp_thumbnail_dimension(max_width, 0);
    let item_id = percent_encoding::percent_decode_str(item_id)
        .decode_utf8()
        .map_err(|error| format!("Failed to decode Emby thumbnail item_id: {error}"))?
        .into_owned();
    let scope = ThumbnailSignatureScope {
        item_id: &item_id,
        server_id: &server_id,
        credential_owner_id: &credential_owner_id,
        max_height,
        max_width,
    };
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    let signed_query =
        build_signed_thumbnail_query(signing_key, room_id, user_id, scope, expires_at);

    Ok(format!("{thumbnail_url}&{signed_query}"))
}
