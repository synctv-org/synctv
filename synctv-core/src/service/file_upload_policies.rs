use crate::models::FileUploadPolicy;

pub const MAX_CHAT_ATTACHMENT_SIZE_BYTES: i64 = 50 * 1024 * 1024;
pub const MAX_USER_AVATAR_SIZE_BYTES: i64 = 5 * 1024 * 1024;
pub const MAX_MEDIA_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_ROOM_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_PLAYLIST_COVER_SIZE_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_CHAT_ATTACHMENT_IMAGE_WIDTH: i32 = 8192;
pub const MAX_CHAT_ATTACHMENT_IMAGE_HEIGHT: i32 = 8192;
pub const MAX_CHAT_ATTACHMENT_AUDIO_DURATION_SECONDS: i32 = 10 * 60;
pub const MAX_CHAT_ATTACHMENT_AUDIO_BITRATE_BPS: i32 = 256 * 1000;
pub const MAX_USER_AVATAR_WIDTH: i32 = 2048;
pub const MAX_USER_AVATAR_HEIGHT: i32 = 2048;
pub const MAX_COVER_IMAGE_WIDTH: i32 = 4096;
pub const MAX_COVER_IMAGE_HEIGHT: i32 = 4096;

const COVER_IMAGE_MIME_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/avif"];
const CHAT_ATTACHMENT_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/pdf",
    "application/zip",
    "application/gzip",
    "application/x-7z-compressed",
    "application/x-tar",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.ms-word",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "text/csv",
    "text/markdown",
    "text/plain",
];

#[derive(Clone, Copy)]
struct FileUploadPolicySpec<'a> {
    kind: &'a str,
    max_size_bytes: i64,
    max_width: Option<i32>,
    max_height: Option<i32>,
    require_image_dimensions: bool,
    max_audio_duration_seconds: Option<i32>,
    max_audio_bitrate_bps: Option<i32>,
    require_audio_metadata: bool,
    allowed_mime_prefixes: &'a [&'a str],
    allowed_mime_types: &'a [&'a str],
    storage_namespace: &'a str,
    database_object_route_prefix: &'a str,
}

fn policy_from_spec(spec: FileUploadPolicySpec<'_>) -> FileUploadPolicy {
    FileUploadPolicy {
        kind: spec.kind.to_string(),
        max_size_bytes: spec.max_size_bytes,
        max_width: spec.max_width,
        max_height: spec.max_height,
        require_image_dimensions: spec.require_image_dimensions,
        max_audio_duration_seconds: spec.max_audio_duration_seconds,
        max_audio_bitrate_bps: spec.max_audio_bitrate_bps,
        require_audio_metadata: spec.require_audio_metadata,
        allowed_mime_prefixes: spec
            .allowed_mime_prefixes
            .iter()
            .map(|prefix| (*prefix).to_string())
            .collect(),
        allowed_mime_types: spec
            .allowed_mime_types
            .iter()
            .map(|mime_type| (*mime_type).to_string())
            .collect(),
        storage_namespace: spec.storage_namespace.to_string(),
        database_object_route_prefix: spec.database_object_route_prefix.to_string(),
    }
}

fn typed_image_policy(
    kind: &str,
    max_size_bytes: i64,
    max_width: i32,
    max_height: i32,
    storage_namespace: &str,
    database_object_route_prefix: &str,
) -> FileUploadPolicy {
    policy_from_spec(FileUploadPolicySpec {
        kind,
        max_size_bytes,
        max_width: Some(max_width),
        max_height: Some(max_height),
        require_image_dimensions: true,
        max_audio_duration_seconds: None,
        max_audio_bitrate_bps: None,
        require_audio_metadata: false,
        allowed_mime_prefixes: &[],
        allowed_mime_types: COVER_IMAGE_MIME_TYPES,
        storage_namespace,
        database_object_route_prefix,
    })
}

#[must_use]
pub fn chat_attachment_upload_policy() -> FileUploadPolicy {
    policy_from_spec(FileUploadPolicySpec {
        kind: "chat_attachment",
        max_size_bytes: MAX_CHAT_ATTACHMENT_SIZE_BYTES,
        max_width: Some(MAX_CHAT_ATTACHMENT_IMAGE_WIDTH),
        max_height: Some(MAX_CHAT_ATTACHMENT_IMAGE_HEIGHT),
        require_image_dimensions: false,
        max_audio_duration_seconds: Some(MAX_CHAT_ATTACHMENT_AUDIO_DURATION_SECONDS),
        max_audio_bitrate_bps: Some(MAX_CHAT_ATTACHMENT_AUDIO_BITRATE_BPS),
        require_audio_metadata: false,
        allowed_mime_prefixes: &["image/", "audio/", "video/"],
        allowed_mime_types: CHAT_ATTACHMENT_MIME_TYPES,
        storage_namespace: "chat/attachments",
        database_object_route_prefix: "/api/chat/attachment-objects",
    })
}

#[must_use]
pub fn user_avatar_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "user_avatar",
        MAX_USER_AVATAR_SIZE_BYTES,
        MAX_USER_AVATAR_WIDTH,
        MAX_USER_AVATAR_HEIGHT,
        "users/avatars",
        "/api/user/avatar-objects",
    )
}

#[must_use]
pub fn media_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "media_cover",
        MAX_MEDIA_COVER_SIZE_BYTES,
        MAX_COVER_IMAGE_WIDTH,
        MAX_COVER_IMAGE_HEIGHT,
        "media/covers",
        "/api/media/cover-objects",
    )
}

#[must_use]
pub fn room_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "room_cover",
        MAX_ROOM_COVER_SIZE_BYTES,
        MAX_COVER_IMAGE_WIDTH,
        MAX_COVER_IMAGE_HEIGHT,
        "rooms/covers",
        "/api/room/cover-objects",
    )
}

#[must_use]
pub fn playlist_cover_upload_policy() -> FileUploadPolicy {
    typed_image_policy(
        "playlist_cover",
        MAX_PLAYLIST_COVER_SIZE_BYTES,
        MAX_COVER_IMAGE_WIDTH,
        MAX_COVER_IMAGE_HEIGHT,
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

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn upload_request(
        policy: FileUploadPolicy,
        mime_type: &str,
        size_bytes: i64,
    ) -> CreateFileUploadSession {
        CreateFileUploadSession {
            user_id: UserId::expect_positive(1),
            storage_scope: "test-scope".to_string(),
            client_file_id: Some("client-file-1".to_string()),
            filename: None,
            mime_type: mime_type.to_string(),
            size_bytes,
            width: Some(320),
            height: Some(180),
            duration_seconds: None,
            bitrate_bps: None,
            parts: vec![crate::models::FileUploadManifestPart {
                part_number: 1,
                offset_bytes: 0,
                size_bytes,
                checksum_sha256: "a".repeat(64),
            }],
            metadata: serde_json::json!({}),
            policy,
        }
    }

    #[test]
    fn product_upload_policies_use_distinct_namespaces_and_kinds() {
        let chat = chat_attachment_upload_policy();
        let avatar = user_avatar_upload_policy();
        let cover = media_cover_upload_policy();
        let room_cover = room_cover_upload_policy();
        let playlist_cover = playlist_cover_upload_policy();

        assert_eq!(chat.kind, "chat_attachment");
        assert_eq!(avatar.kind, "user_avatar");
        assert_eq!(cover.kind, "media_cover");
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
                chat_attachment_upload_policy(),
                "application/pdf",
                MAX_CHAT_ATTACHMENT_SIZE_BYTES,
            ),
            (
                user_avatar_upload_policy(),
                "image/webp",
                MAX_USER_AVATAR_SIZE_BYTES,
            ),
            (
                media_cover_upload_policy(),
                "image/avif",
                MAX_MEDIA_COVER_SIZE_BYTES,
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
            ok(
                validate_create_file_upload_session(&upload_request(
                    policy.clone(),
                    mime_type,
                    max_size_bytes,
                )),
                "valid product upload request should pass policy validation",
            );
            assert!(matches!(
                validate_create_file_upload_session(&upload_request(
                    policy.clone(),
                    "application/x-msdownload",
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

    #[test]
    fn avatar_policy_limits_image_type_size_and_dimensions() {
        let policy = user_avatar_upload_policy();
        assert_eq!(policy.allowed_mime_types, COVER_IMAGE_MIME_TYPES);
        assert_eq!(policy.max_size_bytes, MAX_USER_AVATAR_SIZE_BYTES);
        assert_eq!(policy.max_width, Some(MAX_USER_AVATAR_WIDTH));
        assert_eq!(policy.max_height, Some(MAX_USER_AVATAR_HEIGHT));
        assert!(policy.require_image_dimensions);
        assert_eq!(policy.max_audio_duration_seconds, None);
        assert_eq!(policy.max_audio_bitrate_bps, None);

        let mut request = upload_request(policy.clone(), "image/webp", 1024);
        request.width = Some(MAX_USER_AVATAR_WIDTH + 1);
        assert!(matches!(
            validate_create_file_upload_session(&request),
            Err(Error::InvalidInput(message)) if message.contains("width")
        ));

        let mut request = upload_request(policy.clone(), "image/webp", 1024);
        request.height = Some(MAX_USER_AVATAR_HEIGHT + 1);
        assert!(matches!(
            validate_create_file_upload_session(&request),
            Err(Error::InvalidInput(message)) if message.contains("height")
        ));

        let mut request = upload_request(policy, "image/webp", 1024);
        request.width = None;
        assert!(matches!(
            validate_create_file_upload_session(&request),
            Err(Error::InvalidInput(message)) if message.contains("dimensions")
        ));
    }

    #[test]
    fn chat_attachment_policy_limits_audio_metadata() {
        let policy = chat_attachment_upload_policy();
        assert_eq!(
            policy.max_audio_duration_seconds,
            Some(MAX_CHAT_ATTACHMENT_AUDIO_DURATION_SECONDS)
        );
        assert_eq!(
            policy.max_audio_bitrate_bps,
            Some(MAX_CHAT_ATTACHMENT_AUDIO_BITRATE_BPS)
        );

        let mut request = upload_request(policy.clone(), "audio/mpeg", 1024);
        request.duration_seconds = Some(MAX_CHAT_ATTACHMENT_AUDIO_DURATION_SECONDS + 1);
        assert!(matches!(
            validate_create_file_upload_session(&request),
            Err(Error::InvalidInput(message)) if message.contains("duration")
        ));

        let mut request = upload_request(policy, "audio/mpeg", 1024);
        request.bitrate_bps = Some(MAX_CHAT_ATTACHMENT_AUDIO_BITRATE_BPS + 1);
        assert!(matches!(
            validate_create_file_upload_session(&request),
            Err(Error::InvalidInput(message)) if message.contains("bitrate")
        ));
    }
}
