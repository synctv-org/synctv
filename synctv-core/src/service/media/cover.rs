use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, CreateFileUploadSession,
        FileBlob, FileMetadata, FileObjectDownload, FileRangeRequest, FileUploadManifestPart,
        FileUploadRange, FileUploadSessionCreateResult, GetFileObject, Media, MediaId, RoomId,
        StoreFileUpload, StoreFileUploadResult, SubmittedFileReference, UserId,
    },
    service::{
        media::{ensure_media_creator_can_edit, MediaService},
        media_cover_upload_policy, FileStorageCleanupOrigin, FileStorageContext,
    },
    Error, Result,
};

const MEDIA_COVER_REFERENCE_KIND: &str = "media_cover";

#[derive(Debug, Clone)]
pub struct CreateMediaCoverUploadSession {
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

fn media_cover_storage_scope(room_id: RoomId, media_id: MediaId) -> String {
    format!(
        "rooms/{}/media/{}/cover",
        room_id.as_i64(),
        media_id.as_i64()
    )
}

impl MediaService {
    pub async fn create_media_cover_upload_session(
        &self,
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
        request: CreateMediaCoverUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for media covers".to_string())
        })?;
        let media = self
            .media_repo
            .get_by_room_and_id(&room_id, &media_id)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;
        ensure_media_creator_can_edit(&media, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        storage
            .create_upload_session(CreateFileUploadSession {
                user_id,
                storage_scope: media_cover_storage_scope(room_id, media_id),
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
                policy: media_cover_upload_policy(),
            })
            .await
    }

    pub async fn store_media_cover_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        range: Option<FileUploadRange>,
        data: Vec<u8>,
    ) -> Result<StoreFileUploadResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for media covers".to_string())
            })?
            .store_upload(StoreFileUpload {
                encoded_object_key: encoded_object_key.to_string(),
                upload_token: upload_token.to_string(),
                content_type: content_type.map(str::to_string),
                range,
                data,
            })
            .await
    }

    pub async fn complete_media_cover_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for media covers".to_string())
            })?
            .complete_upload_session(request)
            .await
    }

    pub async fn get_media_cover_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.get_media_cover_object_range(encoded_object_key, read_token, None)
            .await
    }

    pub async fn get_media_cover_object_range(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileBlob> {
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

    pub async fn get_media_cover_object_stream(
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

    pub async fn update_media_cover(
        &self,
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
        file: SubmittedFileReference,
    ) -> Result<Media> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for media covers".to_string())
        })?;
        let mut tx = self.media_repo.pool().begin().await?;
        let current_media = self
            .media_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &media_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;
        ensure_media_creator_can_edit(&current_media, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        let storage_scope = media_cover_storage_scope(room_id, media_id);
        let upload_policy = media_cover_upload_policy();
        let prepared = storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    database_object_route_prefix: &upload_policy.database_object_route_prefix,
                    client_request_id: None,
                },
                vec![file],
            )
            .await?;
        let file = prepared
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("media cover file is required".to_string()))?;

        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            MEDIA_COVER_REFERENCE_KIND,
            &media_id.as_i64().to_string(),
            None,
            &crate::models::FileReferenceMetadata::File(crate::models::FileMetadata::default()),
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput("media cover file object is not registered".to_string())
        })?;
        let old_reference = if let Some(reference_id) = current_media.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.media_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| current_media.cover_file_reference_target(&reference))
        } else {
            None
        };

        let updated_media = self
            .media_repo
            .update_cover_with_executor(
                &room_id,
                &media_id,
                Some(new_reference_id),
                current_media.version,
                &mut *tx,
            )
            .await?
            .ok_or(Error::OptimisticLockConflict)?;
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

        Ok(updated_media)
    }

    pub async fn clear_media_cover(
        &self,
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
    ) -> Result<Media> {
        let mut tx = self.media_repo.pool().begin().await?;
        let current_media = self
            .media_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &media_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;
        ensure_media_creator_can_edit(&current_media, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;
        let old_reference = if let Some(reference_id) = current_media.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.media_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| current_media.cover_file_reference_target(&reference))
        } else {
            None
        };
        let updated_media = self
            .media_repo
            .update_cover_with_executor(&room_id, &media_id, None, current_media.version, &mut *tx)
            .await?
            .ok_or(Error::OptimisticLockConflict)?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.file_storage_service.as_ref(), old_reference)
        {
            storage
                .schedule_delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }

        Ok(updated_media)
    }
}
