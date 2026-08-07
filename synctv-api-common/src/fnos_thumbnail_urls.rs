use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const THUMBNAIL_ROUTE: &str = "/api/providers/fnos/thumbnail";
const SIGNATURE_PROVIDER: &str = "fnos-thumbnail";

#[derive(Clone, Copy)]
pub struct FnosThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub image_path: &'a str,
    pub width: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnosThumbnailAccessError {
    Invalid,
    WrongUser,
}

fn signature_version(scope: FnosThumbnailScope<'_>) -> String {
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

fn signed_query(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: FnosThumbnailScope<'_>,
    expires_at: i64,
) -> String {
    signing_key.build_signed_query(&ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    })
}

pub fn fnos_thumbnail_url(
    server_id: &str,
    credential_owner_id: &str,
    image_path: &str,
    width: u32,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("imagePath", image_path)
        .append_pair("width", &width.clamp(1, 1920).to_string())
        .finish();
    format!("{THUMBNAIL_ROUTE}?{query}")
}

pub fn fnos_own_thumbnail_url(server_id: &str, image_path: &str, width: u32) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("imagePath", image_path)
        .append_pair("width", &width.clamp(1, 1920).to_string())
        .finish();
    format!("{THUMBNAIL_ROUTE}?{query}")
}

pub fn sign_fnos_thumbnail_url(
    thumbnail_url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    let Some(raw_query) = thumbnail_url
        .strip_prefix(THUMBNAIL_ROUTE)
        .and_then(|suffix| suffix.strip_prefix('?'))
    else {
        return Ok(thumbnail_url.to_string());
    };
    if url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key == "sig") {
        return Ok(thumbnail_url.to_string());
    }
    let params = url::form_urlencoded::parse(raw_query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let server_id = required(&params, "serverId")?;
    let credential_owner_id = required(&params, "credentialOwnerId")?;
    let image_path = required(&params, "imagePath")?;
    let width = required(&params, "width")?
        .parse::<u32>()
        .map_err(|error| format!("Invalid FNOS thumbnail width: {error}"))?
        .clamp(1, 1920);
    let scope = FnosThumbnailScope {
        server_id,
        credential_owner_id,
        image_path,
        width,
    };
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    Ok(format!(
        "{thumbnail_url}&{}",
        signed_query(signing_key, room_id, user_id, scope, expires_at)
    ))
}

pub fn verify_fnos_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: FnosThumbnailScope<'_>,
) -> Result<String, FnosThumbnailAccessError> {
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
        .map_err(|_| FnosThumbnailAccessError::Invalid)?;
    if claims.user_id != auth_user_id {
        return Err(FnosThumbnailAccessError::WrongUser);
    }
    Ok(claims.room_id)
}

fn required<'a>(
    params: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Result<&'a str, String> {
    params
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("FNOS thumbnail URL missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies_room_scoped_thumbnail() {
        let key = ProxySigningKey::try_derive_from(b"fnos-thumbnail-test-key-at-least-32-bytes")
            .expect("key should derive");
        let url = fnos_thumbnail_url("server", "owner", "/aa/bb/poster.jpg", 800);
        let signed = sign_fnos_thumbnail_url(&url, "room", "viewer", &key)
            .expect("thumbnail URL should sign");
        let raw_query = signed
            .split_once('?')
            .expect("signed thumbnail URL should contain a query")
            .1;
        let scope = FnosThumbnailScope {
            server_id: "server",
            credential_owner_id: "owner",
            image_path: "/aa/bb/poster.jpg",
            width: 800,
        };
        assert_eq!(
            verify_fnos_thumbnail_access(&key, "viewer", raw_query, scope)
                .expect("signed thumbnail access should verify"),
            "room"
        );
    }
}
