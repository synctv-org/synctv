use std::collections::BTreeMap;

use super::{multipart_presign::s3_upload_part_urls, S3CompatibleFileStorageService};
use crate::{
    models::{
        CreateFileUploadSession, FileUploadOwnershipProofMetadata, FileUploadSession,
        FileUploadSessionCreateResult, FileUploadSessionKind, NewStoredFile,
    },
    repository::{UpsertFileObject, UpsertFileUploadSession},
    service::file_storage::{
        attach_file_ownership_proof_token, encode_file_object_key, file_object_access,
        file_object_key, file_storage_object_base_path, new_file_id,
        optional_file_storage_public_url, register_upload_session_reference,
        upload_manifest_is_single_object, upload_session_file_id, upload_session_metadata,
        upload_session_metadata_with_manifest, upload_session_progress,
        validate_create_file_upload_session, validate_file_mime_type,
        validate_session_file_for_storage, validated_upload_manifest, UploadSessionMetadataInput,
        FILE_UPLOAD_EXPIRES_SECONDS,
    },
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn create_s3_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        validate_create_file_upload_session(&request)?;
        if request.parts.is_empty() {
            return Ok(FileUploadSessionCreateResult::Plan(
                super::super::create_file_upload_plan(
                    request.size_bytes,
                    request.policy.max_size_bytes,
                )?,
            ));
        }
        let (plan, content_manifest_sha256) = validated_upload_manifest(
            request.size_bytes,
            request.policy.max_size_bytes,
            &request.parts,
        )?;
        let file_id = new_file_id();
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            file_id: &file_id,
            user_id: request.user_id,
            storage_scope: &request.storage_scope,
            client_file_id: request.client_file_id.as_deref(),
            filename: request.filename.as_deref(),
            width: request.width,
            height: request.height,
            metadata: request.metadata.clone(),
            upload_policy: &request.policy,
        });
        let now = crate::SystemClock.now();
        let expires = self
            .config
            .upload_expires_seconds
            .clamp(60, FILE_UPLOAD_EXPIRES_SECONDS);
        let expires_at = now + chrono::Duration::seconds(expires);
        let repository = self.repository()?;

        let object_base_path = file_storage_object_base_path(
            &self.config.base_path,
            &request.policy.storage_namespace,
        );
        let object_key_prefix = format!("{object_base_path}/");
        if let Some(existing) = repository
            .get_object_by_manifest(
                &self.config.storage_backend,
                &object_key_prefix,
                &content_manifest_sha256,
                request.size_bytes,
            )
            .await?
        {
            if self.operator.stat(&existing.object_key).await.is_ok() {
                validate_file_mime_type(&request.policy, &existing.mime_type)?;
                let mut file = NewStoredFile {
                    id: file_id,
                    filename: request.filename.clone(),
                    storage_backend: self.config.storage_backend.clone(),
                    object_key: existing.object_key.clone(),
                    object_access: None,
                    url: None,
                    mime_type: Some(existing.mime_type),
                    size_bytes: Some(existing.size_bytes),
                    width: request.width,
                    height: request.height,
                    metadata: request.metadata,
                };
                let (nonce, ranges) = attach_file_ownership_proof_token(
                    &mut file,
                    request.user_id,
                    &request.storage_scope,
                    expires_at,
                    &self.config.upload_token_secret,
                    &content_manifest_sha256,
                    request.size_bytes,
                )?;
                validate_session_file_for_storage(&file)?;
                let mut reference_metadata = session_metadata.clone();
                reference_metadata.ownership_proof = Some(FileUploadOwnershipProofMetadata {
                    nonce: nonce.clone(),
                    ranges: ranges.clone(),
                });
                register_upload_session_reference(
                    repository,
                    &self.config.storage_backend,
                    &file.object_key,
                    &file.id,
                    expires_at,
                    &reference_metadata,
                )
                .await?;
                return Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
                    file,
                    encoded_object_key: encode_file_object_key(&existing.object_key),
                    upload_required: false,
                    ownership_proof_required: true,
                    ownership_proof_nonce: Some(nonce),
                    ownership_proof_ranges: ranges,
                    upload_object_access: None,
                    upload_url: None,
                    upload_method: None,
                    upload_headers: Default::default(),
                    expires_at: Some(expires_at),
                    max_size_bytes: request.policy.max_size_bytes,
                    resumable: true,
                    part_size_bytes: plan.part_size_bytes,
                    uploaded_size_bytes: 0,
                    uploaded_parts: Vec::new(),
                    upload_id: None,
                    part_urls: Vec::new(),
                }));
            }
        }
        let existing_session = repository
            .get_pending_upload_session_by_manifest(
                &self.config.storage_backend,
                request.user_id,
                &request.storage_scope,
                &content_manifest_sha256,
                request.size_bytes,
                now,
            )
            .await?;
        let object_key = file_object_key(
            &object_base_path,
            "manifest",
            &content_manifest_sha256,
            &request.mime_type,
        );
        let upload_session_key = if let Some(existing) = existing_session.as_ref() {
            existing.upload_session_key.clone()
        } else {
            file_object_key(&object_base_path, "sessions", &file_id, &request.mime_type)
        };
        let single_object_upload =
            upload_manifest_is_single_object(request.size_bytes, &request.parts);
        let session_kind = if single_object_upload {
            FileUploadSessionKind::S3Single
        } else {
            FileUploadSessionKind::S3Multipart
        };
        let existing_upload_id = if single_object_upload {
            None
        } else {
            existing_session
                .as_ref()
                .filter(|session| session.completed_at.is_none() && session.expires_at > now)
                .and_then(|session| session.upload_id.clone())
        };
        let file_id = if let Some(existing) = existing_session.as_ref() {
            upload_session_file_id(&existing.metadata)?
        } else {
            file_id
        };
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            file_id: &file_id,
            user_id: request.user_id,
            storage_scope: &request.storage_scope,
            client_file_id: request.client_file_id.as_deref(),
            filename: request.filename.as_deref(),
            width: request.width,
            height: request.height,
            metadata: request.metadata.clone(),
            upload_policy: &request.policy,
        });
        let session_metadata =
            upload_session_metadata_with_manifest(&session_metadata, &request.parts);
        let mut admission = repository
            .begin_upload_session_admission(
                request.user_id,
                &self.config.storage_backend,
                &upload_session_key,
            )
            .await?;
        admission
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key: &object_key,
                mime_type: &request.mime_type,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &request.metadata,
            })
            .await?;
        let created_upload_id = if single_object_upload || existing_upload_id.is_some() {
            None
        } else {
            Some(
                self.create_multipart_upload(&object_key, &request.mime_type)
                    .await?,
            )
        };
        let upload_id = existing_upload_id.or_else(|| created_upload_id.clone());
        let session_result = admission
            .commit_upload_session(UpsertFileUploadSession {
                storage_backend: &self.config.storage_backend,
                upload_session_key: &upload_session_key,
                object_key: &object_key,
                session_kind,
                upload_id: upload_id.as_deref(),
                user_id: request.user_id,
                storage_scope: &request.storage_scope,
                mime_type: &request.mime_type,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                part_size_bytes: plan.part_size_bytes,
                metadata: &session_metadata,
                expires_at,
            })
            .await;
        if let Err(error) = session_result {
            if let Some(upload_id) = created_upload_id.as_deref() {
                if let Err(abort_error) =
                    self.abort_s3_multipart_upload(&object_key, upload_id).await
                {
                    tracing::warn!(
                        error = %abort_error,
                        storage_backend = %self.config.storage_backend,
                        object_key,
                        upload_id,
                        "Failed to abort S3 multipart upload after session commit failure"
                    );
                }
            }
            return Err(error);
        }
        let public_url = optional_file_storage_public_url(&self.config, &object_key)?;
        let mut file = NewStoredFile {
            id: file_id,
            filename: request.filename,
            storage_backend: self.config.storage_backend.clone(),
            object_key,
            object_access: None,
            url: public_url,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        let upload_token = super::super::file_upload_token_for_object_key(
            &file,
            &upload_session_key,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.config.upload_token_secret,
            Some(&content_manifest_sha256),
        )?;
        file.metadata.upload_token = Some(upload_token);
        validate_session_file_for_storage(&file)?;
        register_upload_session_reference(
            repository,
            &self.config.storage_backend,
            &file.object_key,
            &file.id,
            expires_at,
            &session_metadata,
        )
        .await?;
        let (uploaded_size_bytes, uploaded_parts) = upload_session_progress(
            repository,
            &self.config.storage_backend,
            &upload_session_key,
        )
        .await?;
        let part_urls = s3_upload_part_urls(
            &self.config,
            &file.object_key,
            upload_id.as_deref().unwrap_or_default(),
            expires_at,
            if single_object_upload {
                &[]
            } else {
                &request.parts
            },
        )?;
        let mut upload_headers = BTreeMap::new();
        upload_headers.insert(
            "content-type".to_string(),
            file.mime_type
                .as_deref()
                .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?
                .to_string(),
        );
        if let Some(token) = file.metadata.upload_token.as_deref() {
            upload_headers.insert(
                super::super::FILE_UPLOAD_TOKEN_HEADER.to_string(),
                token.to_string(),
            );
        }
        let upload_object_access = file_object_access(
            request.policy.object_kind,
            &self.config.storage_backend,
            &upload_session_key,
            &self.config.upload_token_secret,
        )?;
        Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
            encoded_object_key: encode_file_object_key(&upload_session_key),
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_object_access: Some(upload_object_access),
            upload_url: None,
            upload_method: Some("PUT".to_string()),
            upload_headers,
            expires_at: Some(expires_at),
            max_size_bytes: request.policy.max_size_bytes,
            resumable: !single_object_upload,
            part_size_bytes: plan.part_size_bytes,
            uploaded_size_bytes,
            uploaded_parts,
            upload_id,
            part_urls,
        }))
    }
}
