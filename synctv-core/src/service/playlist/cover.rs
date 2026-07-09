use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, FileMetadata,
        FileObjectDownload, FileRangeRequest, FileReferenceTarget, FileUploadManifestPart,
        FileUploadSessionCreateResult, GetFileObject, Playlist, PlaylistId, RoomId,
        SubmittedFileReference, UserId,
    },
    service::{
        file_storage::FileStorageContext, playlist_cover_upload_policy, FileStorageCleanupOrigin,
    },
    Error, Result,
};

use super::{ensure_playlist_creator_can_edit, PlaylistService};

const PLAYLIST_COVER_REFERENCE_KIND: &str = "playlist_cover";

#[derive(Debug, Clone)]
pub struct CreatePlaylistCoverUploadSession {
    pub client_cover_id: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub bitrate_bps: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: FileMetadata,
}

fn playlist_cover_storage_scope(room_id: RoomId, playlist_id: PlaylistId) -> String {
    format!(
        "rooms/{}/playlists/{}/cover",
        room_id.as_i64(),
        playlist_id.as_i64()
    )
}

fn playlist_cover_reference_target(
    playlist_id: PlaylistId,
    file: &crate::models::StoredFileReference,
) -> FileReferenceTarget {
    file.reference_target(
        PLAYLIST_COVER_REFERENCE_KIND,
        playlist_id.as_i64().to_string(),
    )
}

impl PlaylistService {
    pub async fn create_cover_upload_session(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
        request: CreatePlaylistCoverUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for playlist covers".to_string())
        })?;
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, &playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        storage
            .create_upload_session(crate::models::CreateFileUploadSession {
                user_id,
                storage_scope: playlist_cover_storage_scope(room_id, playlist_id),
                client_file_id: request.client_cover_id,
                filename: None,
                mime_type: request.mime_type,
                size_bytes: request.size_bytes,
                width: request.width,
                height: request.height,
                duration_seconds: request.duration_seconds,
                bitrate_bps: request.bitrate_bps,
                parts: request.parts,
                metadata: request.metadata,
                policy: playlist_cover_upload_policy(),
            })
            .await
    }

    pub async fn store_cover_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        range: Option<crate::models::FileUploadRange>,
        data: Vec<u8>,
    ) -> Result<crate::models::StoreFileUploadResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput(
                    "file storage is not configured for playlist covers".to_string(),
                )
            })?
            .store_upload(crate::models::StoreFileUpload {
                encoded_object_key: encoded_object_key.to_string(),
                upload_token: upload_token.to_string(),
                content_type: content_type.map(str::to_string),
                range,
                data,
            })
            .await
    }

    pub async fn complete_cover_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput(
                    "file storage is not configured for playlist covers".to_string(),
                )
            })?
            .complete_upload_session(request)
            .await
    }

    pub async fn get_cover_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<crate::models::FileBlob> {
        self.get_cover_object_range(encoded_object_key, read_token, None)
            .await
    }

    pub async fn get_cover_object_range(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<crate::models::FileBlob> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn get_cover_object_stream(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileObjectDownload> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object_stream(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn update_cover(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
        file: SubmittedFileReference,
    ) -> Result<Playlist> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for playlist covers".to_string())
        })?;
        let mut tx = self.playlist_repo.pool().begin().await?;
        let mut playlist = self
            .playlist_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &playlist_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        let storage_scope = playlist_cover_storage_scope(room_id, playlist_id);
        let upload_policy = playlist_cover_upload_policy();
        let prepared = storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    object_kind: upload_policy.object_kind,
                    client_request_id: None,
                },
                vec![file],
            )
            .await?;
        let file = prepared
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("playlist cover file is required".to_string()))?;
        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            PLAYLIST_COVER_REFERENCE_KIND,
            &playlist_id.as_i64().to_string(),
            None,
            &crate::models::FileReferenceMetadata::File(crate::models::FileMetadata::default()),
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput("playlist cover file object is not registered".to_string())
        })?;
        let old_reference = if let Some(reference_id) = playlist.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.playlist_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| playlist_cover_reference_target(playlist_id, &reference))
        } else {
            None
        };

        playlist.cover_file_reference_id = Some(new_reference_id);
        let updated_playlist = self
            .playlist_repo
            .update_with_version_with_executor(&playlist, playlist.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let Some(old_reference) = old_reference {
            if old_reference.storage_backend != file.storage_backend
                || old_reference.object_key != file.object_key
            {
                storage
                    .schedule_delete_files(
                        FileStorageCleanupOrigin::ReferenceReleased,
                        &[old_reference],
                    )
                    .await?;
            }
        }

        Ok(updated_playlist)
    }

    pub async fn clear_cover(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
    ) -> Result<Playlist> {
        let mut tx = self.playlist_repo.pool().begin().await?;
        let mut playlist = self
            .playlist_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &playlist_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        let old_reference = if let Some(reference_id) = playlist.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.playlist_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| playlist_cover_reference_target(playlist_id, &reference))
        } else {
            None
        };
        playlist.cover_file_reference_id = None;
        let updated_playlist = self
            .playlist_repo
            .update_with_version_with_executor(&playlist, playlist.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.file_storage_service.as_ref(), old_reference)
        {
            storage
                .schedule_delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }

        Ok(updated_playlist)
    }
}
