use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub(crate) const THUMBNAIL_ROUTE: &str = "/api/providers/seafile/thumbnail";
const SIGNATURE_PROVIDER: &str = "seafile-thumbnail";

#[derive(Clone, Copy)]
pub(crate) struct SeafileThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub repository_id: &'a str,
    pub path: &'a str,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeafileThumbnailAccessError {
    Invalid,
    WrongUser,
}

fn signature_version(scope: SeafileThumbnailScope<'_>) -> String {
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

pub(crate) fn seafile_thumbnail_url(
    server_id: &str,
    credential_owner_id: &str,
    repository_id: &str,
    path: &str,
    size: u32,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("repositoryId", repository_id)
        .append_pair("path", path)
        .append_pair("size", &size.clamp(32, 2048).to_string())
        .finish();
    format!("{THUMBNAIL_ROUTE}?{query}")
}

pub(crate) fn sign_seafile_thumbnail_url(
    url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    let raw_query = url
        .strip_prefix(THUMBNAIL_ROUTE)
        .and_then(|suffix| suffix.strip_prefix('?'))
        .ok_or_else(|| "Invalid Seafile thumbnail URL".to_string())?;
    let params = url::form_urlencoded::parse(raw_query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let required = |key: &str| {
        params
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Seafile thumbnail URL missing {key}"))
    };
    let scope = SeafileThumbnailScope {
        server_id: required("serverId")?,
        credential_owner_id: required("credentialOwnerId")?,
        repository_id: required("repositoryId")?,
        path: required("path")?,
        size: required("size")?
            .parse::<u32>()
            .map_err(|_| "Invalid Seafile thumbnail size".to_string())?
            .clamp(32, 2048),
    };
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    let query = signing_key.build_signed_query(&ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    });
    Ok(format!("{url}&{query}"))
}

pub(crate) fn verify_seafile_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: SeafileThumbnailScope<'_>,
) -> Result<String, SeafileThumbnailAccessError> {
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
            "thumbnail",
        )
        .map_err(|_| SeafileThumbnailAccessError::Invalid)?;
    if claims.user_id != auth_user_id {
        return Err(SeafileThumbnailAccessError::WrongUser);
    }
    Ok(claims.room_id)
}
