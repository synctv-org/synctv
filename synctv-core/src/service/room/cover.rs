use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, FileBlob, FileMetadata,
        FileObjectDownload, FileRangeRequest, FileUploadManifestPart, FileUploadRange,
        FileUploadSessionCreateResult, GetFileObject, Room, RoomId, StoreFileUpload,
        StoreFileUploadResult, SubmittedFileReference, UserId,
    },
    service::{
        file_storage::FileStorageContext, room_cover_upload_policy, FileStorageCleanupOrigin,
    },
    Error, Result,
};

use super::RoomService;

const ROOM_COVER_REFERENCE_KIND: &str = "room_cover";

#[derive(Debug, Clone)]
pub struct CreateRoomCoverUploadSession {
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

fn room_cover_storage_scope(room_id: RoomId) -> String {
    format!("rooms/{}/cover", room_id.as_i64())
}

impl RoomService {
    pub async fn create_room_cover_upload_session(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreateRoomCoverUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        let storage = self.room_file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for room covers".to_string())
        })?;
        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        storage
            .create_upload_session(crate::models::CreateFileUploadSession {
                user_id,
                storage_scope: room_cover_storage_scope(room_id),
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
                policy: room_cover_upload_policy(),
            })
            .await
    }

    pub async fn store_room_cover_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        range: Option<FileUploadRange>,
        data: Vec<u8>,
    ) -> Result<StoreFileUploadResult> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for room covers".to_string())
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

    pub async fn complete_room_cover_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for room covers".to_string())
            })?
            .complete_upload_session(request)
            .await
    }

    pub async fn get_room_cover_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.get_room_cover_object_range(encoded_object_key, read_token, None)
            .await
    }

    pub async fn get_room_cover_object_range(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileBlob> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn get_room_cover_object_stream(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileObjectDownload> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object_stream(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn update_room_cover(
        &self,
        room_id: RoomId,
        user_id: UserId,
        file: SubmittedFileReference,
    ) -> Result<Room> {
        let storage = self.room_file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for room covers".to_string())
        })?;
        let mut tx = self.room_repo.pool().begin().await?;
        let mut room = self
            .room_repo
            .get_by_id_for_update_with_executor(&room_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        let storage_scope = room_cover_storage_scope(room_id);
        let upload_policy = room_cover_upload_policy();
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
            .ok_or_else(|| Error::InvalidInput("room cover file is required".to_string()))?;
        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            ROOM_COVER_REFERENCE_KIND,
            &room_id.as_i64().to_string(),
            None,
            &crate::models::FileReferenceMetadata::File(crate::models::FileMetadata::default()),
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput("room cover file object is not registered".to_string())
        })?;
        let old_reference = if let Some(reference_id) = room.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.pool.clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(ROOM_COVER_REFERENCE_KIND, room_id.as_i64().to_string())
                })
        } else {
            None
        };

        room.cover_file_reference_id = Some(new_reference_id);
        let updated_room = self
            .room_repo
            .update_with_executor(&room, room.version, &mut *tx)
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
        self.notify_room_invalidation(&room_id).await;
        Ok(updated_room)
    }

    pub async fn clear_room_cover(&self, room_id: RoomId, user_id: UserId) -> Result<Room> {
        let mut tx = self.room_repo.pool().begin().await?;
        let mut room = self
            .room_repo
            .get_by_id_for_update_with_executor(&room_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let old_reference = if let Some(reference_id) = room.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.pool.clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(ROOM_COVER_REFERENCE_KIND, room_id.as_i64().to_string())
                })
        } else {
            None
        };
        room.cover_file_reference_id = None;
        let updated_room = self
            .room_repo
            .update_with_executor(&room, room.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.room_file_storage_service.as_ref(), old_reference)
        {
            storage
                .schedule_delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }
        self.notify_room_invalidation(&room_id).await;
        Ok(updated_room)
    }
}
