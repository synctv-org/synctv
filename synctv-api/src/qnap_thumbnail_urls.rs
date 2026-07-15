use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub(crate) const THUMBNAIL_ROUTE: &str = "/api/providers/qnap/thumbnail";
const SIGNATURE_PROVIDER: &str = "qnap-thumbnail";

#[derive(Clone, Copy)]
pub(crate) struct QnapThumbnailScope<'a> {
    pub server_id: &'a str,
    pub credential_owner_id: &'a str,
    pub path: &'a str,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QnapThumbnailAccessError {
    Invalid,
    WrongUser,
}

fn signature_version(scope: QnapThumbnailScope<'_>) -> String {
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

pub(crate) fn qnap_thumbnail_url(
    server_id: &str,
    credential_owner_id: &str,
    path: &str,
    size: u32,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("serverId", server_id)
        .append_pair("credentialOwnerId", credential_owner_id)
        .append_pair("path", path)
        .append_pair("size", &size.clamp(1, 640).to_string())
        .finish();
    format!("{THUMBNAIL_ROUTE}?{query}")
}

pub(crate) fn sign_qnap_thumbnail_url(
    thumbnail_url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
    let raw_query = thumbnail_url
        .strip_prefix(THUMBNAIL_ROUTE)
        .and_then(|suffix| suffix.strip_prefix('?'))
        .ok_or_else(|| "Invalid QNAP thumbnail URL".to_string())?;
    let params = url::form_urlencoded::parse(raw_query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let required = |key: &str| {
        params
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("QNAP thumbnail URL missing {key}"))
    };
    let size = required("size")?
        .parse::<u32>()
        .map_err(|error| format!("Invalid QNAP thumbnail size: {error}"))?
        .clamp(1, 640);
    let scope = QnapThumbnailScope {
        server_id: required("serverId")?,
        credential_owner_id: required("credentialOwnerId")?,
        path: required("path")?,
        size,
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
    Ok(format!("{thumbnail_url}&{query}"))
}

pub(crate) fn verify_qnap_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: QnapThumbnailScope<'_>,
) -> Result<String, QnapThumbnailAccessError> {
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
        .map_err(|_| QnapThumbnailAccessError::Invalid)?;
    if claims.user_id != auth_user_id {
        return Err(QnapThumbnailAccessError::WrongUser);
    }
    Ok(claims.room_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies_qnap_thumbnail() {
        let key = ProxySigningKey::try_derive_from(b"qnap-thumbnail-test-key-at-least-32-bytes")
            .expect("key should derive");
        let url = qnap_thumbnail_url("server", "owner", "/Multimedia/Movie.mkv", 640);
        let signed = sign_qnap_thumbnail_url(&url, "room", "viewer", &key).expect("sign");
        let scope = QnapThumbnailScope {
            server_id: "server",
            credential_owner_id: "owner",
            path: "/Multimedia/Movie.mkv",
            size: 640,
        };
        assert_eq!(
            verify_qnap_thumbnail_access(
                &key,
                "viewer",
                signed.split_once('?').expect("query").1,
                scope,
            )
            .expect("verify"),
            "room"
        );
    }
}
