use sqlx::{Postgres, Transaction};

use crate::{
    models::{CreateFileUploadSession, FileBlob, FileUploadSession, NewStoredFile, User, UserId},
    service::{
        file_storage::{FileStorageCleanupOrigin, FileStorageContext},
        user::UserService,
        user_avatar_upload_policy,
    },
    Error, Result,
};

use super::CreateUserAvatarUploadSession;

const USER_AVATAR_REFERENCE_KIND: &str = "user_avatar";

fn user_avatar_storage_scope(user_id: UserId) -> String {
    format!("users/{}/avatars", user_id.as_i64())
}

impl UserService {
    pub async fn create_avatar_upload_session(
        &self,
        user_id: &UserId,
        request: CreateUserAvatarUploadSession,
    ) -> Result<FileUploadSession> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for user avatars".to_string())
        })?;
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: *user_id,
                storage_scope: user_avatar_storage_scope(*user_id),
                client_file_id: request.client_avatar_id,
                mime_type: request.mime_type,
                size_bytes: request.size_bytes,
                width: request.width,
                height: request.height,
                checksum_sha256: request.checksum_sha256,
                metadata: request.metadata,
                policy: user_avatar_upload_policy(),
            })
            .await
    }

    pub async fn store_avatar_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for user avatars".to_string())
            })?
            .store_upload_object(encoded_object_key, upload_token, content_type, data)
            .await
    }

    pub async fn get_avatar_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object(encoded_object_key, read_token)
            .await
    }

    pub async fn update_avatar(&self, user_id: &UserId, file: NewStoredFile) -> Result<User> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for user avatars".to_string())
        })?;
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let current_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        let storage_scope = user_avatar_storage_scope(*user_id);
        let prepared = storage
            .prepare_files(
                FileStorageContext {
                    user_id: *user_id,
                    storage_scope: &storage_scope,
                    client_request_id: None,
                },
                vec![file],
            )
            .await?;
        let file = prepared
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("avatar file is required".to_string()))?;

        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            USER_AVATAR_REFERENCE_KIND,
            &user_id.as_i64().to_string(),
            None,
            &file.metadata,
        )
        .await?
        .ok_or_else(|| Error::InvalidInput("avatar file object is not registered".to_string()))?;
        let old_reference = if let Some(reference_id) = current_user.avatar_file_reference_id {
            crate::repository::FileStorageRepository::new(self.repository.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(USER_AVATAR_REFERENCE_KIND, user_id.as_i64().to_string())
                })
        } else {
            None
        };

        let updated_user = self
            .repository
            .update_avatar_with_executor(
                user_id,
                Some(new_reference_id),
                current_user.version,
                &mut *tx,
            )
            .await?;
        tx.commit().await?;

        if let Some(old_reference) = old_reference {
            if old_reference.storage_backend != file.storage_backend
                || old_reference.object_key != file.object_key
            {
                storage
                    .delete_files(
                        FileStorageCleanupOrigin::ReferenceReleased,
                        &[old_reference],
                    )
                    .await?;
            }
        }
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }

    pub async fn clear_avatar(&self, user_id: &UserId) -> Result<User> {
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let current_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        let old_reference = if let Some(reference_id) = current_user.avatar_file_reference_id {
            crate::repository::FileStorageRepository::new(self.repository.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(USER_AVATAR_REFERENCE_KIND, user_id.as_i64().to_string())
                })
        } else {
            None
        };
        let updated_user = self
            .repository
            .update_avatar_with_executor(user_id, None, current_user.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.file_storage_service.as_ref(), old_reference)
        {
            storage
                .delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }
}
