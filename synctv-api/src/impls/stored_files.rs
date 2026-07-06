use std::sync::Arc;

use synctv_core::{
    models::{FileObjectAccess, FileObjectKind, FileUploadPolicy, StoredFileReference},
    service::FileStorageService,
};

use crate::impls::ApiError;

#[derive(Debug, Clone, Default)]
pub(crate) struct StoredFileObjectAccess {
    pub url: Option<String>,
    pub object_access: Option<FileObjectAccess>,
}

impl StoredFileObjectAccess {
    #[must_use]
    #[cfg(test)]
    pub(crate) fn external_url(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            object_access: None,
        }
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn object_access(access: FileObjectAccess) -> Self {
        Self {
            url: None,
            object_access: Some(access),
        }
    }

    #[must_use]
    pub(crate) fn from_parts(url: Option<String>, object_access: Option<FileObjectAccess>) -> Self {
        Self { url, object_access }
    }
}

pub(crate) fn first_file_storage<'a>(
    storages: impl IntoIterator<Item = Option<&'a Arc<dyn FileStorageService>>>,
) -> Option<&'a Arc<dyn FileStorageService>> {
    storages.into_iter().flatten().next()
}

pub(crate) fn stored_file_reference_access(
    storage: &dyn FileStorageService,
    file: &StoredFileReference,
    policy: &FileUploadPolicy,
) -> Result<Option<StoredFileObjectAccess>, ApiError> {
    stored_file_reference_access_for_kind(storage, file, policy.object_kind)
}

pub(crate) fn stored_file_reference_access_for_kind(
    storage: &dyn FileStorageService,
    file: &StoredFileReference,
    object_kind: FileObjectKind,
) -> Result<Option<StoredFileObjectAccess>, ApiError> {
    let object_access = storage
        .file_object_access(&file.storage_backend, &file.object_key, object_kind)
        .map_err(ApiError::from)?;
    let url = storage
        .public_object_url(&file.storage_backend, &file.object_key)
        .map_err(ApiError::from)?
        .or_else(|| {
            object_access
                .as_ref()
                .and_then(render_file_object_access_url)
        });

    Ok((url.is_some() || object_access.is_some())
        .then(|| StoredFileObjectAccess::from_parts(url, object_access)))
}

pub(crate) const fn file_object_route_prefix(kind: FileObjectKind) -> Option<&'static str> {
    match kind {
        FileObjectKind::ChatAttachment => Some("/api/chat/attachment-objects"),
        FileObjectKind::UserAvatar => Some("/api/user/avatar-objects"),
        FileObjectKind::MediaCover => Some("/api/media/cover-objects"),
        FileObjectKind::MediaThumbnail => Some("/api/media/thumbnail-objects"),
        FileObjectKind::RoomCover => Some("/api/room/cover-objects"),
        FileObjectKind::PlaylistCover => Some("/api/playlist/cover-objects"),
        FileObjectKind::Generic => None,
    }
}

pub(crate) fn stored_file_object_access_url(access: &StoredFileObjectAccess) -> Option<String> {
    access
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            access
                .object_access
                .as_ref()
                .and_then(render_file_object_access_url)
        })
}

pub(crate) fn render_file_object_access_url(access: &FileObjectAccess) -> Option<String> {
    Some(format!(
        "{}/{encoded_object_key}?token={read_token}",
        file_object_route_prefix(access.object_kind)?,
        encoded_object_key = access.encoded_object_key,
        read_token = access.read_token
    ))
}

pub(crate) fn render_file_object_upload_url(access: &FileObjectAccess) -> Option<String> {
    Some(format!(
        "{}/{encoded_object_key}",
        file_object_route_prefix(access.object_kind)?,
        encoded_object_key = access.encoded_object_key,
    ))
}

pub(crate) fn file_object_access_to_proto(
    access: &FileObjectAccess,
) -> synctv_proto::client::FileObjectAccess {
    synctv_proto::client::FileObjectAccess {
        object_kind: file_object_access_kind_to_proto(access.object_kind) as i32,
        encoded_object_key: access.encoded_object_key.clone(),
        read_token: access.read_token.clone(),
    }
}

fn file_object_access_kind_to_proto(
    kind: FileObjectKind,
) -> synctv_proto::client::FileObjectAccessKind {
    match kind {
        FileObjectKind::ChatAttachment => {
            synctv_proto::client::FileObjectAccessKind::ChatAttachment
        }
        FileObjectKind::UserAvatar => synctv_proto::client::FileObjectAccessKind::UserAvatar,
        FileObjectKind::MediaCover => synctv_proto::client::FileObjectAccessKind::MediaCover,
        FileObjectKind::MediaThumbnail => {
            synctv_proto::client::FileObjectAccessKind::MediaThumbnail
        }
        FileObjectKind::RoomCover => synctv_proto::client::FileObjectAccessKind::RoomCover,
        FileObjectKind::PlaylistCover => synctv_proto::client::FileObjectAccessKind::PlaylistCover,
        FileObjectKind::Generic => synctv_proto::client::FileObjectAccessKind::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_object_access_has_no_public_route() {
        let access = FileObjectAccess {
            object_kind: FileObjectKind::Generic,
            encoded_object_key: "encoded".to_string(),
            read_token: "token".to_string(),
        };

        assert!(render_file_object_access_url(&access).is_none());
        assert!(render_file_object_upload_url(&access).is_none());
    }
}
