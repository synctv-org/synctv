use crate::models::FileUploadPolicy;

pub const MAX_CHAT_IMAGE_SIZE_BYTES: i64 = 20 * 1024 * 1024;
pub const MAX_USER_AVATAR_SIZE_BYTES: i64 = 5 * 1024 * 1024;
pub const MAX_VIDEO_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_ROOM_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_PLAYLIST_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;

const COVER_IMAGE_MIME_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/avif"];

fn policy_from_slices(
    kind: &str,
    max_size_bytes: i64,
    allowed_mime_prefixes: &[&str],
    allowed_mime_types: &[&str],
    storage_namespace: &str,
    database_object_route_prefix: &str,
) -> FileUploadPolicy {
    FileUploadPolicy {
        kind: kind.to_string(),
        max_size_bytes,
        allowed_mime_prefixes: allowed_mime_prefixes
            .iter()
            .map(|prefix| (*prefix).to_string())
            .collect(),
        allowed_mime_types: allowed_mime_types
            .iter()
            .map(|mime_type| (*mime_type).to_string())
            .collect(),
        storage_namespace: storage_namespace.to_string(),
        database_object_route_prefix: database_object_route_prefix.to_string(),
    }
}

fn typed_image_policy(
    kind: &str,
    max_size_bytes: i64,
    storage_namespace: &str,
    database_object_route_prefix: &str,
) -> FileUploadPolicy {
    policy_from_slices(
        kind,
        max_size_bytes,
        &[],
        COVER_IMAGE_MIME_TYPES,
        storage_namespace,
        database_object_route_prefix,
    )
}

#[must_use]
pub fn chat_image_upload_policy() -> FileUploadPolicy {
    policy_from_slices(
        "chat_image",
        MAX_CHAT_IMAGE_SIZE_BYTES,
        &["image/"],
        &[],
        "chat/images",
        "/api/chat/image-objects",
    )
}

#[must_use]
pub fn user_avatar_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "user_avatar",
        MAX_USER_AVATAR_SIZE_BYTES,
        "users/avatars",
        "/api/user/avatar-objects",
    )
}

#[must_use]
pub fn video_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "video_cover",
        MAX_VIDEO_COVER_SIZE_BYTES,
        "videos/covers",
        "/api/video/cover-objects",
    )
}

#[must_use]
pub fn room_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "room_cover",
        MAX_ROOM_COVER_SIZE_BYTES,
        "rooms/covers",
        "/api/room/cover-objects",
    )
}

#[must_use]
pub fn playlist_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "playlist_cover",
        MAX_PLAYLIST_COVER_SIZE_BYTES,
        "playlists/covers",
        "/api/playlist/cover-objects",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{CreateFileUploadSession, UserId},
        service::file_storage::validate_create_file_upload_session,
        Error,
    };

    fn upload_request(
        policy: FileUploadPolicy,
        mime_type: &str,
        size_bytes: i64,
    ) -> CreateFileUploadSession {
        CreateFileUploadSession {
            user_id: UserId::expect_positive(1),
            storage_scope: "test-scope".to_string(),
            client_file_id: Some("client-file-1".to_string()),
            mime_type: mime_type.to_string(),
            size_bytes,
            width: Some(320),
            height: Some(180),
            checksum_sha256: Some("a".repeat(64)),
            metadata: serde_json::json!({}),
            policy,
        }
    }

    #[test]
    fn product_upload_policies_use_distinct_namespaces_and_kinds() {
        let chat = chat_image_upload_policy();
        let avatar = user_avatar_upload_policy();
        let cover = video_cover_upload_policy();
        let room_cover = room_cover_upload_policy();
        let playlist_cover = playlist_cover_upload_policy();

        assert_eq!(chat.kind, "chat_image");
        assert_eq!(avatar.kind, "user_avatar");
        assert_eq!(cover.kind, "video_cover");
        assert_eq!(room_cover.kind, "room_cover");
        assert_eq!(playlist_cover.kind, "playlist_cover");
        assert_ne!(chat.storage_namespace, avatar.storage_namespace);
        assert_ne!(avatar.storage_namespace, cover.storage_namespace);
        assert_ne!(chat.storage_namespace, cover.storage_namespace);
        assert_ne!(room_cover.storage_namespace, cover.storage_namespace);
        assert_ne!(
            playlist_cover.storage_namespace,
            room_cover.storage_namespace
        );
    }

    #[test]
    fn product_upload_policies_validate_expected_upload_requests() {
        let cases = [
            (
                chat_image_upload_policy(),
                "image/gif",
                MAX_CHAT_IMAGE_SIZE_BYTES,
            ),
            (
                user_avatar_upload_policy(),
                "image/webp",
                MAX_USER_AVATAR_SIZE_BYTES,
            ),
            (
                video_cover_upload_policy(),
                "image/avif",
                MAX_VIDEO_COVER_SIZE_BYTES,
            ),
            (
                room_cover_upload_policy(),
                "image/png",
                MAX_ROOM_COVER_SIZE_BYTES,
            ),
            (
                playlist_cover_upload_policy(),
                "image/jpeg",
                MAX_PLAYLIST_COVER_SIZE_BYTES,
            ),
        ];

        for (policy, mime_type, max_size_bytes) in cases {
            validate_create_file_upload_session(&upload_request(
                policy.clone(),
                mime_type,
                max_size_bytes,
            ))
            .expect("valid product upload request should pass policy validation");
            assert!(matches!(
                validate_create_file_upload_session(&upload_request(
                    policy.clone(),
                    "text/plain",
                    max_size_bytes
                )),
                Err(Error::InvalidInput(_))
            ));
            assert!(matches!(
                validate_create_file_upload_session(&upload_request(
                    policy,
                    mime_type,
                    max_size_bytes + 1
                )),
                Err(Error::InvalidInput(_))
            ));
        }
    }
}
