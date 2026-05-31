use crate::models::FileUploadPolicy;

pub const MAX_CHAT_IMAGE_SIZE_BYTES: i64 = 20 * 1024 * 1024;
pub const MAX_USER_AVATAR_SIZE_BYTES: i64 = 5 * 1024 * 1024;
pub const MAX_VIDEO_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_ROOM_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_PLAYLIST_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;

#[must_use]
pub fn chat_image_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "chat_image".to_string(),
        max_size_bytes: MAX_CHAT_IMAGE_SIZE_BYTES,
        allowed_mime_prefixes: vec!["image/".to_string()],
        allowed_mime_types: Vec::new(),
        storage_namespace: "chat/images".to_string(),
        database_object_route_prefix: "/api/chat/image-objects".to_string(),
    }
}

#[must_use]
pub fn user_avatar_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "user_avatar".to_string(),
        max_size_bytes: MAX_USER_AVATAR_SIZE_BYTES,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/webp".to_string(),
            "image/avif".to_string(),
        ],
        storage_namespace: "users/avatars".to_string(),
        database_object_route_prefix: "/api/user/avatar-objects".to_string(),
    }
}

#[must_use]
pub fn video_cover_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "video_cover".to_string(),
        max_size_bytes: MAX_VIDEO_COVER_SIZE_BYTES,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/webp".to_string(),
            "image/avif".to_string(),
        ],
        storage_namespace: "videos/covers".to_string(),
        database_object_route_prefix: "/api/video/cover-objects".to_string(),
    }
}

#[must_use]
pub fn room_cover_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "room_cover".to_string(),
        max_size_bytes: MAX_ROOM_COVER_SIZE_BYTES,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/webp".to_string(),
            "image/avif".to_string(),
        ],
        storage_namespace: "rooms/covers".to_string(),
        database_object_route_prefix: "/api/room/cover-objects".to_string(),
    }
}

#[must_use]
pub fn playlist_cover_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "playlist_cover".to_string(),
        max_size_bytes: MAX_PLAYLIST_COVER_SIZE_BYTES,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/webp".to_string(),
            "image/avif".to_string(),
        ],
        storage_namespace: "playlists/covers".to_string(),
        database_object_route_prefix: "/api/playlist/cover-objects".to_string(),
    }
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
