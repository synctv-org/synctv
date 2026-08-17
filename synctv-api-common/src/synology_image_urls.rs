use sha2::{Digest, Sha256};

use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt, ProxyUrlClaims};

pub const PLAYBACK_IMAGE_ROUTE_PREFIX: &str = "/api/playback-providers";
pub const SIGNATURE_PROVIDER: &str = "synology";

#[derive(Clone, Copy)]
pub enum SynologyImageScope<'a> {
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

pub fn signature_version(scope: SynologyImageScope<'_>) -> String {
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

pub fn playback_image_url(
    signing_key: &ProxySigningKey,
    scope: SynologyImageScope<'_>,
    room_id: &str,
    user_id: &str,
) -> String {
    let claims = ProxyUrlClaims {
        provider: SIGNATURE_PROVIDER.to_string(),
        version: signature_version(scope),
        resource: "image".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at: synctv_core::SystemClock.now().timestamp()
            + ProxySigningKey::default_expiry_secs(),
        target_url: None,
    };
    let mut resource_query = url::form_urlencoded::Serializer::new(String::new());
    match scope {
        SynologyImageScope::File {
            server_id,
            credential_owner_id,
            path,
            size,
        } => {
            resource_query
                .append_pair("kind", "file")
                .append_pair("serverId", server_id)
                .append_pair("credentialOwnerId", credential_owner_id)
                .append_pair("path", path)
                .append_pair("size", size);
        }
        SynologyImageScope::Poster {
            server_id,
            credential_owner_id,
            item_id,
            media_type,
            poster_mtime,
        } => {
            resource_query
                .append_pair("kind", "poster")
                .append_pair("serverId", server_id)
                .append_pair("credentialOwnerId", credential_owner_id)
                .append_pair("itemId", &item_id.to_string())
                .append_pair("mediaType", media_type);
            if let Some(poster_mtime) = poster_mtime {
                resource_query.append_pair("posterMtime", poster_mtime);
            }
        }
    }
    let resource_query = resource_query.finish();
    let signed_query = signing_key.build_signed_playback_query(&claims);
    format!(
        "{PLAYBACK_IMAGE_ROUTE_PREFIX}/{room_id}/synology/image?{resource_query}&{signed_query}"
    )
}
