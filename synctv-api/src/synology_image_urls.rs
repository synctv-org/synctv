use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub(crate) const IMAGE_ROUTE: &str = "/api/providers/synology/image";
const SIGNATURE_PROVIDER: &str = "synology-image";

#[derive(Clone, Copy)]
pub(crate) enum SynologyImageScope<'a> {
    File {
        server_id: &'a str,
        credential_owner_id: &'a str,
        path: &'a str,
        size: &'a str,
    },
    Poster {
        server_id: &'a str,
        credential_owner_id: &'a str,
        item_id: i64,
        media_type: &'a str,
        poster_mtime: Option<&'a str>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SynologyImageAccessError {
    Invalid,
    WrongUser,
}

fn signature_version(scope: SynologyImageScope<'_>) -> String {
    let mut hasher = Sha256::new();
    match scope {
        SynologyImageScope::File {
            server_id,
            credential_owner_id,
            path,
            size,
        } => {
            hasher.update(b"file\0");
            for value in [server_id, credential_owner_id, path, size] {
                hasher.update(value.as_bytes());
                hasher.update([0]);
            }
        }
        SynologyImageScope::Poster {
            server_id,
            credential_owner_id,
            item_id,
            media_type,
            poster_mtime,
        } => {
            hasher.update(b"poster\0");
            for value in [server_id, credential_owner_id, media_type] {
                hasher.update(value.as_bytes());
                hasher.update([0]);
            }
            hasher.update(item_id.to_be_bytes());
            hasher.update(poster_mtime.unwrap_or_default().as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn synology_file_image_url(
    server_id: &str,
    credential_owner_id: &str,
    path: &str,
    size: &str,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("kind", "file")
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("path", path)
        .append_pair("size", size)
        .finish();
    format!("{IMAGE_ROUTE}?{query}")
}

pub(crate) fn synology_poster_url(
    server_id: &str,
    credential_owner_id: &str,
    item_id: i64,
    media_type: &str,
    poster_mtime: Option<&str>,
) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("kind", "poster")
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("itemId", &item_id.to_string())
        .append_pair("mediaType", media_type);
    if let Some(poster_mtime) = poster_mtime {
        query.append_pair("posterMtime", poster_mtime);
    }
    format!("{IMAGE_ROUTE}?{}", query.finish())
}

pub(crate) fn sign_synology_image_url(
    image_url: &str,
    scope: SynologyImageScope<'_>,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    if !image_url.starts_with(IMAGE_ROUTE) {
        return Err("Invalid Synology image URL".to_string());
    }
    let expires_at =
        synctv_core::SystemClock.now().timestamp() + ProxySigningKey::default_expiry_secs();
    let query = signing_key.build_signed_query(&ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "image".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    });
    Ok(format!("{image_url}&{query}"))
}

pub(crate) fn verify_synology_image_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: SynologyImageScope<'_>,
) -> Result<String, SynologyImageAccessError> {
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
            "image",
        )
        .map_err(|_| SynologyImageAccessError::Invalid)?;
    if claims.user_id != auth_user_id {
        return Err(SynologyImageAccessError::WrongUser);
    }
    Ok(claims.room_id)
}
