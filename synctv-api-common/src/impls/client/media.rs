//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use futures::{StreamExt, TryStreamExt};
use hex::encode as hex_encode;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use synctv_core::models::{
    CompleteFileUploadPart, CompleteFileUploadSession, CompleteFileUploadSessionResult, FileBlob,
    FileUploadManifestPart, FileUploadPartUrl, FileUploadPlan, FileUploadSession,
    FileUploadSessionCreateResult, MediaId, MediaListQuery as CoreMediaListQuery,
    MediaListSortBy as CoreMediaListSortBy, NewStoredFile, Playlist,
    PlaylistListQuery as CorePlaylistListQuery, PlaylistListSortBy as CorePlaylistListSortBy,
    RoomId, SortDirection as CoreSortDirection, StoreFileUploadResult, UserId,
};
use synctv_core::provider::{DynamicListQuery, DynamicPagination, DynamicPlaylistItemSourceConfig};
use synctv_core::service::{
    AddMediaRequest as CoreAddMediaRequest, CreateMediaCoverUploadSession,
    CreateMediaThumbnailUploadSession, MoveMediaRequest as CoreMoveMediaRequest,
};
use synctv_core::service::{
    DeleteEntriesPlan, DeleteEntriesRequest as CoreDeleteEntriesRequest, MediaListItem,
    MemberResourceCleanupResult, PlaylistListItem, RealtimeOutboxDeleteEntriesEventFactory,
    RealtimeOutboxMemberResourceCleanupEventFactory,
};
use synctv_core::service::{MediaService, NewRealtimeOutboxEvent};

use super::convert::try_playlist_path_node_to_proto;
use super::convert::{
    file_metadata_from_proto, media_source_config_to_proto, optional_proto_source_provider_to_core,
    optional_provider_target_to_proto, playlist_source_config_to_proto, provider_target_from_proto,
};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};
use crate::media_fanout::{MediaFanoutService, PreparedMediaRemovedFanout};
use crate::playback_fanout::{PlaybackFanoutService, PreparedPlaybackStateFanout};
use crate::playlist_fanout::{PlaylistFanoutService, PreparedPlaylistDeletedFanout};
use synctv_realtime::fanout::RealtimeFanoutService;

#[derive(Debug)]
struct AddMediaBatchBuildResult {
    items: Vec<synctv_core::service::AddMediaRequest>,
    playlist_id: Option<synctv_core::models::PlaylistId>,
}

fn optional_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn dynamic_pagination_from_request(
    pagination: Option<&synctv_proto::client::list_playlist_items_request::Pagination>,
    source_provider: synctv_core::models::SourceProvider,
    has_search: bool,
) -> Result<DynamicPagination, ApiError> {
    if has_search {
        let page = match pagination {
            Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(page)) => {
                page_u32_to_usize(page.page)?
            }
            _ => 1,
        };
        return Ok(DynamicPagination::Page { page });
    }

    match pagination {
        Some(synctv_proto::client::list_playlist_items_request::Pagination::Cursor(cursor)) => {
            Ok(DynamicPagination::Cursor {
                cursor: Some(cursor.cursor.clone()).filter(|value| !value.is_empty()),
            })
        }
        Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(page)) => {
            Ok(DynamicPagination::Page {
                page: page_u32_to_usize(page.page)?,
            })
        }
        None if source_provider == synctv_core::models::SourceProvider::Cloudreve => {
            Ok(DynamicPagination::Cursor { cursor: None })
        }
        None => Ok(DynamicPagination::Page { page: 1 }),
    }
}

fn dynamic_response_pagination(
    pagination: DynamicPagination,
) -> Result<synctv_proto::client::list_playlist_items_response::Pagination, ApiError> {
    Ok(match pagination {
        DynamicPagination::Page { page } => {
            synctv_proto::client::list_playlist_items_response::Pagination::Page(
                synctv_proto::client::PagePagination {
                    page: u32::try_from(page).map_err(|_| {
                        ApiError::Internal("dynamic playlist page exceeds u32::MAX".to_string())
                    })?,
                },
            )
        }
        DynamicPagination::Cursor { cursor } => {
            synctv_proto::client::list_playlist_items_response::Pagination::Cursor(
                synctv_proto::client::CursorPagination {
                    cursor: cursor.unwrap_or_default(),
                },
            )
        }
    })
}

fn dynamic_playlist_item_source_config_to_proto(
    source_config: Option<DynamicPlaylistItemSourceConfig>,
) -> Result<Option<synctv_proto::client::playlist_item::SourceConfig>, ApiError> {
    source_config
        .map(|source_config| match source_config {
            DynamicPlaylistItemSourceConfig::Media(config) => Ok(
                synctv_proto::client::playlist_item::SourceConfig::MediaSourceConfig(
                    media_source_config_to_proto(&config)?,
                ),
            ),
            DynamicPlaylistItemSourceConfig::Playlist(config) => Ok(
                synctv_proto::client::playlist_item::SourceConfig::PlaylistSourceConfig(
                    playlist_source_config_to_proto(&config)?,
                ),
            ),
        })
        .transpose()
}

#[cfg(test)]
pub struct RequiredStoredFileFields {
    pub url: String,
    pub object_access: Option<synctv_proto::client::FileObjectAccess>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: i32,
    pub height: i32,
    pub metadata: Option<synctv_proto::client::FileMetadata>,
}

#[cfg(test)]
pub fn required_stored_file_fields(
    file: &NewStoredFile,
    metadata_field: &'static str,
) -> Result<RequiredStoredFileFields, ApiError> {
    let url = required_stored_file_url(file)?;
    let mime_type = required_stored_file_mime_type(file)?;
    let size_bytes = required_stored_file_size_bytes(file)?;
    Ok(RequiredStoredFileFields {
        url,
        object_access: file
            .object_access
            .as_ref()
            .map(crate::impls::stored_files::file_object_access_to_proto),
        mime_type,
        size_bytes,
        width: stored_file_dimension(file.width, "width")?,
        height: stored_file_dimension(file.height, "height")?,
        metadata: super::convert::file_metadata_to_proto(&file.metadata).map_err(|error| {
            ApiError::Internal(format!("Failed to convert {metadata_field}: {error:?}"))
        })?,
    })
}

#[cfg(test)]
fn required_stored_file_url(file: &NewStoredFile) -> Result<String, ApiError> {
    let url = file
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            file.object_access
                .as_ref()
                .and_then(crate::impls::stored_files::render_file_object_access_url)
        })
        .ok_or_else(|| ApiError::Internal("stored file url is missing".to_string()))?;
    if url.is_empty() {
        return Err(ApiError::Internal("stored file url is empty".to_string()));
    }
    Ok(url)
}

#[cfg(test)]
fn required_stored_file_mime_type(file: &NewStoredFile) -> Result<String, ApiError> {
    let mime_type = file
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| ApiError::Internal("stored file mime_type is missing".to_string()))?;
    if mime_type.is_empty() {
        return Err(ApiError::Internal(
            "stored file mime_type is empty".to_string(),
        ));
    }
    Ok(mime_type.to_string())
}

#[cfg(test)]
fn required_stored_file_size_bytes(file: &NewStoredFile) -> Result<i64, ApiError> {
    match file.size_bytes {
        Some(size_bytes) if size_bytes > 0 => Ok(size_bytes),
        _ => Err(ApiError::Internal(
            "stored file size_bytes is missing or invalid".to_string(),
        )),
    }
}

#[cfg(test)]
fn stored_file_dimension(value: Option<i32>, field: &'static str) -> Result<i32, ApiError> {
    match value {
        Some(value) if value > 0 => Ok(value),
        Some(_) => Err(ApiError::Internal(format!(
            "stored file {field} is invalid"
        ))),
        None => Ok(0),
    }
}

pub struct UploadSessionFields {
    pub upload_url: Option<String>,
    pub upload_object_access: Option<synctv_proto::client::FileObjectAccess>,
    pub upload_method: Option<String>,
    pub expires_at: Option<i64>,
    pub ownership_proof_nonce: Option<String>,
    pub upload_token: String,
}

pub fn proto_file_upload_range(
    range: Option<synctv_proto::client::FileUploadRange>,
) -> Option<synctv_core::models::FileUploadRange> {
    range.map(|range| synctv_core::models::FileUploadRange {
        start: range.start,
        end_inclusive: range.end_inclusive,
        total_size: range.total_size,
    })
}

pub fn proto_file_range_request(
    range: Option<synctv_proto::client::FileRangeRequest>,
) -> Option<synctv_core::models::FileRangeRequest> {
    use synctv_proto::client::file_range_request::Range;

    range.and_then(|range| {
        range.range.map(|range| match range {
            Range::Exact(range) => {
                synctv_core::models::FileRangeRequest::Exact(synctv_core::models::FileByteRange {
                    start: range.start,
                    end_inclusive: range.end_inclusive,
                })
            }
            Range::FromStart(start) => synctv_core::models::FileRangeRequest::From { start },
            Range::SuffixLength(length) => synctv_core::models::FileRangeRequest::Suffix { length },
        })
    })
}

pub fn file_byte_range_to_proto(
    range: synctv_core::models::FileByteRange,
) -> synctv_proto::client::FileByteRange {
    synctv_proto::client::FileByteRange {
        start: range.start,
        end_inclusive: range.end_inclusive,
    }
}

pub fn uploaded_parts_response_fields(result: &StoreFileUploadResult) -> (bool, i64, Vec<i32>) {
    match result {
        StoreFileUploadResult::Complete(blob) => (true, blob.size_bytes, Vec::new()),
        StoreFileUploadResult::PartAccepted {
            uploaded_size_bytes,
            uploaded_parts,
        } => (false, *uploaded_size_bytes, uploaded_parts.clone()),
    }
}

pub fn complete_upload_response_fields(
    result: &CompleteFileUploadSessionResult,
) -> (bool, i64, Vec<i32>) {
    (
        result.object.is_some(),
        result.uploaded_size_bytes,
        result.uploaded_parts.clone(),
    )
}

fn file_upload_part_url_to_proto(
    part_url: FileUploadPartUrl,
) -> synctv_proto::client::FileUploadPartUrl {
    synctv_proto::client::FileUploadPartUrl {
        part_number: part_url.part_number,
        offset_bytes: part_url.offset_bytes,
        size_bytes: part_url.size_bytes,
        upload_url: part_url.upload_url,
        upload_method: part_url.upload_method,
        upload_headers: part_url.upload_headers.into_iter().collect(),
        expires_at: part_url.expires_at.map(|expires_at| expires_at.timestamp()),
    }
}

pub fn proto_upload_manifest_parts(
    parts: Vec<synctv_proto::client::FileUploadManifestPart>,
) -> Vec<FileUploadManifestPart> {
    parts
        .into_iter()
        .map(|part| FileUploadManifestPart {
            part_number: part.part_number,
            offset_bytes: part.offset_bytes,
            size_bytes: part.size_bytes,
            checksum_sha256: part.checksum_sha256,
        })
        .collect()
}

pub fn file_upload_plan_to_proto(plan: FileUploadPlan) -> synctv_proto::client::FileUploadPlan {
    synctv_proto::client::FileUploadPlan {
        checksum_algorithm: plan.checksum_algorithm,
        part_size_bytes: plan.part_size_bytes,
        parts: plan
            .parts
            .into_iter()
            .map(|part| synctv_proto::client::FileUploadPlanPart {
                part_number: part.part_number,
                offset_bytes: part.offset_bytes,
                size_bytes: part.size_bytes,
            })
            .collect(),
    }
}

pub fn proto_complete_upload_parts(
    parts: Vec<synctv_proto::client::CompleteFileUploadPart>,
) -> Vec<CompleteFileUploadPart> {
    parts
        .into_iter()
        .map(|part| CompleteFileUploadPart {
            part_number: part.part_number,
            etag: part.etag,
            size_bytes: part.size_bytes,
            checksum_sha256: optional_trimmed_string(&part.checksum_sha256),
        })
        .collect()
}

pub fn complete_upload_session_request(
    file_id: &str,
    encoded_object_key: String,
    token: String,
    upload_id: Option<String>,
    ownership_proof: &str,
    parts: Vec<synctv_proto::client::CompleteFileUploadPart>,
) -> CompleteFileUploadSession {
    CompleteFileUploadSession {
        file_id: optional_trimmed_string(file_id),
        encoded_object_key,
        upload_token: token,
        upload_id,
        ownership_proof: optional_trimmed_string(ownership_proof),
        parts: proto_complete_upload_parts(parts),
    }
}

pub fn upload_session_fields(session: &FileUploadSession) -> Result<UploadSessionFields, ApiError> {
    let upload_token = synctv_core::service::upload_token_from_session_file(&session.file)
        .map_err(ApiError::from)?;
    let expires_at = if session.upload_required {
        Some(
            session
                .expires_at
                .ok_or_else(|| {
                    ApiError::Internal(
                        "upload session marked upload_required is missing expires_at".to_string(),
                    )
                })?
                .timestamp(),
        )
    } else {
        None
    };
    let (upload_url, upload_object_access, upload_method) = if session.upload_required {
        let upload_object_access = session
            .upload_object_access
            .as_ref()
            .map(crate::impls::stored_files::file_object_access_to_proto);
        let upload_url = session
            .upload_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                session
                    .upload_object_access
                    .as_ref()
                    .and_then(crate::impls::stored_files::render_file_object_upload_url)
            })
            .ok_or_else(|| {
                ApiError::Internal("upload session is missing upload_url".to_string())
            })?;
        (
            Some(upload_url),
            upload_object_access,
            Some(required_upload_session_string(
                session.upload_method.as_deref(),
                "upload_method",
            )?),
        )
    } else {
        (None, None, None)
    };

    let ownership_proof_nonce = if session.ownership_proof_required {
        Some(required_upload_session_string(
            session.ownership_proof_nonce.as_deref(),
            "ownership_proof_nonce",
        )?)
    } else {
        None
    };

    Ok(UploadSessionFields {
        upload_url,
        upload_object_access,
        upload_method,
        expires_at,
        ownership_proof_nonce,
        upload_token,
    })
}

fn required_upload_session_string(
    value: Option<&str>,
    field: &'static str,
) -> Result<String, ApiError> {
    let value = value
        .map(str::trim)
        .ok_or_else(|| ApiError::Internal(format!("upload session is missing {field}")))?;
    if value.is_empty() {
        return Err(ApiError::Internal(format!(
            "upload session {field} is empty"
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
pub fn stored_file_to_file_cover_proto(
    file: &NewStoredFile,
) -> Result<synctv_proto::client::FileCover, ApiError> {
    let fields = required_stored_file_fields(file, "file cover metadata")?;
    Ok(synctv_proto::client::FileCover {
        id: file.id.clone(),
        url: fields.url,
        object_access: fields.object_access,
        mime_type: fields.mime_type,
        size_bytes: fields.size_bytes,
        width: fields.width,
        height: fields.height,
        metadata: fields.metadata,
        variants: super::convert::file_object_variants_from_metadata(&file.metadata, "file cover")?,
    })
}

fn upload_session_file_cover_proto(
    file: &NewStoredFile,
) -> Result<synctv_proto::client::FileUploadReference, ApiError> {
    file_upload_reference_to_proto(file)
}

pub fn file_upload_reference_to_proto(
    file: &NewStoredFile,
) -> Result<synctv_proto::client::FileUploadReference, ApiError> {
    let reference = synctv_core::service::submitted_file_reference_from_session_file(file)
        .map_err(ApiError::from)?;
    Ok(synctv_proto::client::FileUploadReference { id: reference.id })
}

pub fn file_upload_reference_to_core(
    reference: synctv_proto::client::FileUploadReference,
) -> synctv_core::models::SubmittedFileReference {
    synctv_core::models::SubmittedFileReference {
        id: reference.id,
        kind: synctv_core::models::SubmittedFileReferenceKind::Upload,
    }
}

pub fn required_file_upload_reference(
    reference: Option<synctv_proto::client::FileUploadReference>,
    field: &'static str,
) -> Result<synctv_core::models::SubmittedFileReference, ApiError> {
    reference
        .map(file_upload_reference_to_core)
        .ok_or_else(|| ApiError::InvalidInput(format!("{field} is required")))
}

fn upload_session_media_cover_proto(
    file: &NewStoredFile,
) -> Result<synctv_proto::client::FileUploadReference, ApiError> {
    file_upload_reference_to_proto(file)
}

pub fn media_cover_upload_session_to_proto(
    session: FileUploadSession,
) -> Result<synctv_proto::client::MediaCoverUploadSession, ApiError> {
    let fields = upload_session_fields(&session)?;
    Ok(synctv_proto::client::MediaCoverUploadSession {
        cover_reference: Some(upload_session_media_cover_proto(&session.file)?),
        upload_required: session.upload_required,
        upload_url: fields.upload_url,
        upload_object_access: fields.upload_object_access,
        upload_method: fields.upload_method,
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: fields.expires_at,
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: fields.ownership_proof_nonce,
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(
                |range| synctv_proto::client::MediaCoverOwnershipProofRange {
                    offset: range.offset,
                    length: range.length,
                },
            )
            .collect(),
        resumable: session.resumable,
        part_size_bytes: session.part_size_bytes,
        uploaded_size_bytes: session.uploaded_size_bytes,
        uploaded_parts: session.uploaded_parts,
        upload_id: session.upload_id,
        part_urls: session
            .part_urls
            .into_iter()
            .map(file_upload_part_url_to_proto)
            .collect(),
        upload_token: fields.upload_token,
        encoded_object_key: session.encoded_object_key,
    })
}

pub fn media_thumbnail_upload_session_to_proto(
    session: FileUploadSession,
) -> Result<synctv_proto::client::MediaThumbnailUploadSession, ApiError> {
    let fields = upload_session_fields(&session)?;
    Ok(synctv_proto::client::MediaThumbnailUploadSession {
        thumbnail_reference: Some(upload_session_media_cover_proto(&session.file)?),
        upload_required: session.upload_required,
        upload_url: fields.upload_url,
        upload_object_access: fields.upload_object_access,
        upload_method: fields.upload_method,
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: fields.expires_at,
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: fields.ownership_proof_nonce,
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(
                |range| synctv_proto::client::MediaCoverOwnershipProofRange {
                    offset: range.offset,
                    length: range.length,
                },
            )
            .collect(),
        resumable: session.resumable,
        part_size_bytes: session.part_size_bytes,
        uploaded_size_bytes: session.uploaded_size_bytes,
        uploaded_parts: session.uploaded_parts,
        upload_id: session.upload_id,
        part_urls: session
            .part_urls
            .into_iter()
            .map(file_upload_part_url_to_proto)
            .collect(),
        upload_token: fields.upload_token,
        encoded_object_key: session.encoded_object_key,
    })
}

pub fn media_cover_upload_create_result_to_proto(
    result: FileUploadSessionCreateResult,
) -> Result<synctv_proto::client::CreateMediaCoverUploadSessionResponse, ApiError> {
    use synctv_proto::client::create_media_cover_upload_session_response::Result as ProtoResult;
    Ok(
        synctv_proto::client::CreateMediaCoverUploadSessionResponse {
            result: Some(match result {
                FileUploadSessionCreateResult::Plan(plan) => {
                    ProtoResult::Plan(file_upload_plan_to_proto(plan))
                }
                FileUploadSessionCreateResult::Session(session) => {
                    ProtoResult::Session(media_cover_upload_session_to_proto(session)?)
                }
            }),
        },
    )
}

pub fn media_thumbnail_upload_create_result_to_proto(
    result: FileUploadSessionCreateResult,
) -> Result<synctv_proto::client::CreateMediaThumbnailUploadSessionResponse, ApiError> {
    use synctv_proto::client::create_media_thumbnail_upload_session_response::Result as ProtoResult;
    Ok(
        synctv_proto::client::CreateMediaThumbnailUploadSessionResponse {
            result: Some(match result {
                FileUploadSessionCreateResult::Plan(plan) => {
                    ProtoResult::Plan(file_upload_plan_to_proto(plan))
                }
                FileUploadSessionCreateResult::Session(session) => {
                    ProtoResult::Session(media_thumbnail_upload_session_to_proto(session)?)
                }
            }),
        },
    )
}

pub fn room_cover_upload_create_result_to_proto(
    result: FileUploadSessionCreateResult,
) -> Result<synctv_proto::client::CreateRoomCoverUploadSessionResponse, ApiError> {
    use synctv_proto::client::create_room_cover_upload_session_response::Result as ProtoResult;
    Ok(synctv_proto::client::CreateRoomCoverUploadSessionResponse {
        result: Some(match result {
            FileUploadSessionCreateResult::Plan(plan) => {
                ProtoResult::Plan(file_upload_plan_to_proto(plan))
            }
            FileUploadSessionCreateResult::Session(session) => {
                ProtoResult::Session(file_upload_session_to_room_cover_proto(session)?)
            }
        }),
    })
}

pub fn playlist_cover_upload_create_result_to_proto(
    result: FileUploadSessionCreateResult,
) -> Result<synctv_proto::client::CreatePlaylistCoverUploadSessionResponse, ApiError> {
    use synctv_proto::client::create_playlist_cover_upload_session_response::Result as ProtoResult;
    Ok(
        synctv_proto::client::CreatePlaylistCoverUploadSessionResponse {
            result: Some(match result {
                FileUploadSessionCreateResult::Plan(plan) => {
                    ProtoResult::Plan(file_upload_plan_to_proto(plan))
                }
                FileUploadSessionCreateResult::Session(session) => {
                    ProtoResult::Session(file_upload_session_to_playlist_cover_proto(session)?)
                }
            }),
        },
    )
}

pub fn file_upload_session_to_room_cover_proto(
    session: FileUploadSession,
) -> Result<synctv_proto::client::RoomCoverUploadSession, ApiError> {
    let fields = upload_session_fields(&session)?;
    Ok(synctv_proto::client::RoomCoverUploadSession {
        cover_reference: Some(upload_session_file_cover_proto(&session.file)?),
        upload_required: session.upload_required,
        upload_url: fields.upload_url,
        upload_object_access: fields.upload_object_access,
        upload_method: fields.upload_method,
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: fields.expires_at,
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: fields.ownership_proof_nonce,
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(|range| synctv_proto::client::FileOwnershipProofRange {
                offset: range.offset,
                length: range.length,
            })
            .collect(),
        resumable: session.resumable,
        part_size_bytes: session.part_size_bytes,
        uploaded_size_bytes: session.uploaded_size_bytes,
        uploaded_parts: session.uploaded_parts,
        upload_id: session.upload_id,
        part_urls: session
            .part_urls
            .into_iter()
            .map(file_upload_part_url_to_proto)
            .collect(),
        upload_token: fields.upload_token,
        encoded_object_key: session.encoded_object_key,
    })
}

pub fn file_upload_session_to_playlist_cover_proto(
    session: FileUploadSession,
) -> Result<synctv_proto::client::PlaylistCoverUploadSession, ApiError> {
    let fields = upload_session_fields(&session)?;
    Ok(synctv_proto::client::PlaylistCoverUploadSession {
        cover_reference: Some(upload_session_file_cover_proto(&session.file)?),
        upload_required: session.upload_required,
        upload_url: fields.upload_url,
        upload_object_access: fields.upload_object_access,
        upload_method: fields.upload_method,
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: fields.expires_at,
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: fields.ownership_proof_nonce,
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(|range| synctv_proto::client::FileOwnershipProofRange {
                offset: range.offset,
                length: range.length,
            })
            .collect(),
        resumable: session.resumable,
        part_size_bytes: session.part_size_bytes,
        uploaded_size_bytes: session.uploaded_size_bytes,
        uploaded_parts: session.uploaded_parts,
        upload_id: session.upload_id,
        part_urls: session
            .part_urls
            .into_iter()
            .map(file_upload_part_url_to_proto)
            .collect(),
        upload_token: fields.upload_token,
        encoded_object_key: session.encoded_object_key,
    })
}

pub fn media_cover_object_to_proto(
    blob: FileBlob,
) -> synctv_proto::client::MediaCoverObjectResponse {
    synctv_proto::client::MediaCoverObjectResponse {
        mime_type: blob.mime_type,
        content_manifest_sha256: blob.content_manifest_sha256,
        data: blob.data,
        content_range: blob.range.map(file_byte_range_to_proto),
        total_size_bytes: blob.total_size_bytes,
    }
}

pub fn media_thumbnail_object_to_proto(
    blob: FileBlob,
) -> synctv_proto::client::MediaThumbnailObjectResponse {
    synctv_proto::client::MediaThumbnailObjectResponse {
        mime_type: blob.mime_type,
        content_manifest_sha256: blob.content_manifest_sha256,
        data: blob.data,
        content_range: blob.range.map(file_byte_range_to_proto),
        total_size_bytes: blob.total_size_bytes,
    }
}

pub fn room_cover_object_to_proto(blob: FileBlob) -> synctv_proto::client::RoomCoverObjectResponse {
    synctv_proto::client::RoomCoverObjectResponse {
        mime_type: blob.mime_type,
        content_manifest_sha256: blob.content_manifest_sha256,
        data: blob.data,
        content_range: blob.range.map(file_byte_range_to_proto),
        total_size_bytes: blob.total_size_bytes,
    }
}

pub fn playlist_cover_object_to_proto(
    blob: FileBlob,
) -> synctv_proto::client::PlaylistCoverObjectResponse {
    synctv_proto::client::PlaylistCoverObjectResponse {
        mime_type: blob.mime_type,
        content_manifest_sha256: blob.content_manifest_sha256,
        data: blob.data,
        content_range: blob.range.map(file_byte_range_to_proto),
        total_size_bytes: blob.total_size_bytes,
    }
}

pub enum MoveMediaFanoutStep {
    Updated { media_id: MediaId },
    RemovedAndAdded { media_id: MediaId },
}

pub struct PreparedDeleteEntriesOutboxFanout {
    clock: Arc<dyn synctv_core::Clock>,
    media_fanout: Arc<dyn MediaFanoutService>,
    playlist_fanout: Arc<dyn PlaylistFanoutService>,
    playback_fanout: Arc<dyn PlaybackFanoutService>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    room_id: RoomId,
    user_id: UserId,
    username: String,
    events: Arc<Mutex<Vec<PreparedDeleteEntriesEvent>>>,
}

enum PreparedDeleteEntriesEvent {
    MediaRemoved(PreparedMediaRemovedFanout),
    PlaylistDeleted(PreparedPlaylistDeletedFanout),
    PlaybackReset(PreparedPlaybackStateFanout),
    KickPublisher(synctv_realtime::sync::RealtimeEvent),
}

impl PreparedDeleteEntriesOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxDeleteEntriesEventFactory {
        let media_fanout = self.media_fanout.clone();
        let playlist_fanout = self.playlist_fanout.clone();
        let playback_fanout = self.playback_fanout.clone();
        let realtime_fanout = self.realtime_fanout.clone();
        let clock = self.clock.clone();
        let room_id = self.room_id;
        let user_id = self.user_id;
        let username = self.username.clone();
        let events = self.events.clone();
        Arc::new(move |plan: &DeleteEntriesPlan| {
            let mut prepared_events = Vec::with_capacity(
                plan.deleted_media_ids.len() * 2
                    + plan.deleted_playlist_ids.len()
                    + usize::from(plan.playback_state.is_some()),
            );
            let mut outbox_events: Vec<NewRealtimeOutboxEvent> = Vec::with_capacity(
                plan.deleted_media_ids.len() * 2
                    + plan.deleted_playlist_ids.len()
                    + usize::from(plan.playback_state.is_some()),
            );

            for media_id in &plan.deleted_media_ids {
                let prepared = media_fanout
                    .prepare_removed_outbox_fanout(&room_id, &user_id, &username, media_id)?;
                outbox_events.push(prepared.cloned_outbox_event());
                prepared_events.push(PreparedDeleteEntriesEvent::MediaRemoved(prepared));

                let kick_event = synctv_realtime::sync::RealtimeEvent::KickPublisher {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    media_id: *media_id,
                    reason: "media_deleted".to_string(),
                    timestamp: clock.now(),
                };
                outbox_events.push(
                    realtime_fanout
                        .outbox_event(&kick_event)
                        .map_err(synctv_core::Error::Internal)?,
                );
                prepared_events.push(PreparedDeleteEntriesEvent::KickPublisher(kick_event));
            }

            for playlist_id in &plan.deleted_playlist_ids {
                let prepared = playlist_fanout.prepare_deleted_outbox_fanout(
                    &room_id,
                    &user_id,
                    &username,
                    playlist_id,
                )?;
                outbox_events.push(prepared.cloned_outbox_event());
                prepared_events.push(PreparedDeleteEntriesEvent::PlaylistDeleted(prepared));
            }

            if let Some(state) = &plan.playback_state {
                let prepared = playback_fanout.prepare_system_state_changed_outbox_fanout();
                let factory = prepared.outbox_factory();
                outbox_events.push(factory(state)?);
                prepared_events.push(PreparedDeleteEntriesEvent::PlaybackReset(prepared));
            }

            *events.lock() = prepared_events;
            Ok(outbox_events)
        })
    }

    #[must_use]
    pub fn member_cleanup_outbox_factory(&self) -> RealtimeOutboxMemberResourceCleanupEventFactory {
        let factory = self.outbox_factory();
        Arc::new(move |cleanup: &MemberResourceCleanupResult| {
            let plan = DeleteEntriesPlan {
                deleted_playlist_ids: cleanup.deleted_playlist_ids.clone(),
                deleted_media_ids: cleanup.deleted_media_ids.clone(),
                playback_reset: cleanup.playback_reset,
                playback_state: cleanup.playback_state.clone(),
            };
            factory(&plan)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(&mut *self.events.lock());
        for event in events {
            match event {
                PreparedDeleteEntriesEvent::MediaRemoved(event) => {
                    event.publish_after_outbox_commit();
                }
                PreparedDeleteEntriesEvent::PlaylistDeleted(event) => {
                    event.publish_after_outbox_commit();
                }
                PreparedDeleteEntriesEvent::PlaybackReset(event) => {
                    event.publish_after_outbox_commit();
                }
                PreparedDeleteEntriesEvent::KickPublisher(event) => {
                    self.realtime_fanout.publish_after_outbox_commit(event);
                }
            }
        }
    }
}

pub struct PrepareDeleteEntriesOutboxFanout {
    pub clock: Arc<dyn synctv_core::Clock>,
    pub media_fanout: Arc<dyn MediaFanoutService>,
    pub playlist_fanout: Arc<dyn PlaylistFanoutService>,
    pub playback_fanout: Arc<dyn PlaybackFanoutService>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub room_id: RoomId,
    pub user_id: UserId,
    pub username: String,
}

pub fn prepare_delete_entries_outbox_fanout(
    params: PrepareDeleteEntriesOutboxFanout,
) -> PreparedDeleteEntriesOutboxFanout {
    PreparedDeleteEntriesOutboxFanout {
        clock: params.clock,
        media_fanout: params.media_fanout,
        playlist_fanout: params.playlist_fanout,
        playback_fanout: params.playback_fanout,
        realtime_fanout: params.realtime_fanout,
        room_id: params.room_id,
        user_id: params.user_id,
        username: params.username,
        events: Arc::new(Mutex::new(Vec::new())),
    }
}

pub enum MoveMediaFanoutPlan {
    None,
    Reordered,
    PerMedia(Vec<MoveMediaFanoutStep>),
}

fn finalize_playlist_items_response_version(
    mut response: synctv_proto::client::ListPlaylistItemsResponse,
) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError> {
    response.version = compute_playlist_items_response_version(&response)?;
    Ok(response)
}

fn usize_to_u64_api(value: usize, field: &'static str) -> Result<u64, ApiError> {
    u64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds u64::MAX")))
}

fn hash_proto_message<M: prost::Message>(hasher: &mut Sha256, message: &M) -> Result<(), ApiError> {
    let encoded = message.encode_to_vec();
    hasher.update(usize_to_u64_api(encoded.len(), "encoded proto length")?.to_le_bytes());
    hasher.update(encoded);
    Ok(())
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub fn compute_playlist_items_response_version(
    response: &synctv_proto::client::ListPlaylistItemsResponse,
) -> Result<String, ApiError> {
    let mut hasher = Sha256::new();
    hasher.update(b"playlist-items-snapshot-v2");
    hash_optional_u64(&mut hasher, response.total);
    hasher.update(response.playlist_count.to_le_bytes());
    hasher.update(response.file_count.to_le_bytes());

    for playlist in &response.playlists {
        hash_proto_message(&mut hasher, playlist)?;
    }
    for media in &response.media {
        hash_proto_message(&mut hasher, media)?;
    }
    for item in &response.dynamic_items {
        hash_proto_message(&mut hasher, item)?;
    }
    for node in &response.current_path {
        hash_proto_message(&mut hasher, node)?;
    }
    match response.pagination.as_ref() {
        Some(synctv_proto::client::list_playlist_items_response::Pagination::Page(page)) => {
            hasher.update([1]);
            hash_proto_message(&mut hasher, page)?;
        }
        Some(synctv_proto::client::list_playlist_items_response::Pagination::Cursor(cursor)) => {
            hasher.update([2]);
            hash_proto_message(&mut hasher, cursor)?;
        }
        None => hasher.update([0]),
    }

    Ok(hex_encode(hasher.finalize()))
}

fn page_u32_to_usize(value: u32) -> Result<usize, ApiError> {
    usize::try_from(value.max(1)).map_err(|_| ApiError::Internal("page exceeds usize::MAX".into()))
}

pub fn playlist_items_page(
    req: &synctv_proto::client::ListPlaylistItemsRequest,
) -> Result<u32, ApiError> {
    match req.pagination.as_ref() {
        Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(page)) => {
            Ok(page.page.max(1))
        }
        Some(synctv_proto::client::list_playlist_items_request::Pagination::Cursor(_)) => {
            Err(ApiError::InvalidInput(
                "Cursor pagination is available only for provider playlists".to_string(),
            ))
        }
        None => Ok(1),
    }
}

fn i64_count_to_usize(value: i64, field: &'static str) -> Result<usize, ApiError> {
    if value < 0 {
        return Err(ApiError::Internal(format!(
            "{field} returned a negative count"
        )));
    }
    usize::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

fn usize_to_i32_api(value: usize, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

fn i64_to_i32_api(value: i64, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub struct MoveMediaFanoutPlanner<'a> {
    media_service: &'a MediaService,
}

impl<'a> MoveMediaFanoutPlanner<'a> {
    pub fn new(media_service: &'a MediaService) -> Self {
        Self { media_service }
    }

    pub async fn build(
        &self,
        room_id: &RoomId,
        request: &CoreMoveMediaRequest,
    ) -> Result<MoveMediaFanoutPlan, ApiError> {
        let original_media = if request.all_from_scope {
            match request.source_playlist_id.as_ref() {
                Some(playlist_id) => self
                    .media_service
                    .get_room_playlist_media(room_id, playlist_id)
                    .await
                    .map_err(ApiError::from)?,
                None => self
                    .media_service
                    .get_room_root_media(room_id)
                    .await
                    .map_err(ApiError::from)?,
            }
        } else {
            self.media_service
                .get_room_media_batch(room_id, &request.media_ids)
                .await
                .map_err(ApiError::from)?
        };

        if original_media.is_empty() {
            return Ok(MoveMediaFanoutPlan::None);
        }

        if !request.all_from_scope && original_media.len() != request.media_ids.len() {
            return Err(ApiError::NotFound("Media not found".to_string()));
        }

        let target_scope = request.target_playlist_id;
        let moved_within_same_scope = original_media
            .iter()
            .all(|media| media.playlist_id == target_scope);

        if moved_within_same_scope && original_media.len() > 1 {
            return Ok(MoveMediaFanoutPlan::Reordered);
        }

        let mut steps = Vec::with_capacity(original_media.len());
        for media in original_media {
            if media.playlist_id == target_scope {
                steps.push(MoveMediaFanoutStep::Updated { media_id: media.id });
            } else {
                steps.push(MoveMediaFanoutStep::RemovedAndAdded { media_id: media.id });
            }
        }

        Ok(MoveMediaFanoutPlan::PerMedia(steps))
    }
}

fn usize_to_i64_api(value: usize, field: &'static str) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i64::MAX")))
}

pub fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn map_sort_direction(sort_direction: i32) -> Result<CoreSortDirection, ApiError> {
    match synctv_proto::client::SortDirection::try_from(sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::client::SortDirection::Unspecified
        | synctv_proto::client::SortDirection::Asc => Ok(CoreSortDirection::Asc),
        synctv_proto::client::SortDirection::Desc => Ok(CoreSortDirection::Desc),
    }
}

fn map_playlist_sort_from_media_sort(sort_by: i32) -> Result<CorePlaylistListSortBy, ApiError> {
    match synctv_proto::client::MediaListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported media list sort field".to_string()))?
    {
        synctv_proto::client::MediaListSortBy::Unspecified
        | synctv_proto::client::MediaListSortBy::Position => Ok(CorePlaylistListSortBy::Position),
        synctv_proto::client::MediaListSortBy::Name => Ok(CorePlaylistListSortBy::Name),
        synctv_proto::client::MediaListSortBy::AddedAt => Ok(CorePlaylistListSortBy::CreatedAt),
        synctv_proto::client::MediaListSortBy::UpdatedAt => Ok(CorePlaylistListSortBy::UpdatedAt),
        synctv_proto::client::MediaListSortBy::SourceProvider
        | synctv_proto::client::MediaListSortBy::ProviderInstanceName => {
            Ok(CorePlaylistListSortBy::Position)
        }
    }
}

fn map_media_sort(sort_by: i32) -> Result<CoreMediaListSortBy, ApiError> {
    match synctv_proto::client::MediaListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported media list sort field".to_string()))?
    {
        synctv_proto::client::MediaListSortBy::Unspecified
        | synctv_proto::client::MediaListSortBy::Position => Ok(CoreMediaListSortBy::Position),
        synctv_proto::client::MediaListSortBy::Name => Ok(CoreMediaListSortBy::Name),
        synctv_proto::client::MediaListSortBy::AddedAt => Ok(CoreMediaListSortBy::AddedAt),
        synctv_proto::client::MediaListSortBy::UpdatedAt => Ok(CoreMediaListSortBy::UpdatedAt),
        synctv_proto::client::MediaListSortBy::SourceProvider => {
            Ok(CoreMediaListSortBy::SourceProvider)
        }
        synctv_proto::client::MediaListSortBy::ProviderInstanceName => {
            Ok(CoreMediaListSortBy::ProviderInstanceName)
        }
    }
}

fn map_availability_filter(filter: i32) -> Result<Option<bool>, ApiError> {
    match synctv_proto::client::ResourceAvailabilityFilter::try_from(filter)
        .map_err(|_| ApiError::InvalidInput("Unsupported availability filter".to_string()))?
    {
        synctv_proto::client::ResourceAvailabilityFilter::All => Ok(None),
        synctv_proto::client::ResourceAvailabilityFilter::Available => Ok(Some(true)),
        synctv_proto::client::ResourceAvailabilityFilter::Unavailable => Ok(Some(false)),
    }
}

pub(super) async fn playlist_list_items_to_proto(
    api: &ClientApiImpl,
    entries: &[PlaylistListItem],
    counts: &std::collections::HashMap<synctv_core::models::PlaylistId, i64>,
    viewer_id: Option<UserId>,
) -> Result<Vec<synctv_proto::client::Playlist>, ApiError> {
    futures::stream::iter(0..entries.len())
        .map(|index| {
            let item_count = i64_to_i32_api(
                crate::impls::playlist_media_count_or_zero(counts, &entries[index].playlist.id),
                "playlist item count",
            );
            async move {
                let entry = &entries[index];
                let item_count = item_count?;
                api.playlist_to_proto_for_viewer_with_loaded_cover(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                    viewer_id,
                )
                .await
            }
        })
        .buffered(16)
        .try_collect()
        .await
}

async fn media_list_items_to_proto(
    api: &ClientApiImpl,
    entries: &[MediaListItem],
    viewer_id: Option<UserId>,
) -> Result<Vec<synctv_proto::client::Media>, ApiError> {
    futures::stream::iter(0..entries.len())
        .map(|index| async move {
            let entry = &entries[index];
            api.media_to_proto_for_viewer_with_loaded_cover(
                &entry.media,
                entry.is_available,
                viewer_id,
            )
            .await
        })
        .buffered(16)
        .try_collect()
        .await
}

async fn media_models_to_proto(
    api: &ClientApiImpl,
    media: &[synctv_core::models::Media],
    viewer_id: Option<UserId>,
) -> Result<Vec<synctv_proto::client::Media>, ApiError> {
    futures::stream::iter(0..media.len())
        .map(|index| async move {
            api.media_to_proto_for_viewer_with_loaded_cover(&media[index], true, viewer_id)
                .await
        })
        .buffered(16)
        .try_collect()
        .await
}

pub fn require_dynamic_playlist_creator(
    playlist: &Playlist,
    viewer_id: UserId,
) -> Result<(), ApiError> {
    if playlist.creator_id == Some(viewer_id) {
        Ok(())
    } else {
        Err(ApiError::Authorization(
            "Only the playlist creator can browse dynamic provider playlists".to_string(),
        ))
    }
}

pub fn validate_dynamic_playlist_query_support(
    playlist: &Playlist,
    req: &synctv_proto::client::ListPlaylistItemsRequest,
) -> Result<bool, ApiError> {
    if let Some(source_provider) = optional_proto_source_provider_to_core(req.source_provider)? {
        if playlist.source_provider != Some(source_provider) {
            return Ok(false);
        }
    }
    if let Some(provider_instance_name) = normalize_non_empty_filter(&req.provider_instance_name) {
        if playlist.provider_instance_name.as_deref() != Some(provider_instance_name.as_str()) {
            return Ok(false);
        }
    }

    let sort_by = synctv_proto::client::MediaListSortBy::try_from(req.sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported media list sort field".to_string()))?;
    let sort_direction = synctv_proto::client::SortDirection::try_from(req.sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?;
    let allows_default_sort = matches!(
        sort_by,
        synctv_proto::client::MediaListSortBy::Position
            | synctv_proto::client::MediaListSortBy::Unspecified
    ) && matches!(
        sort_direction,
        synctv_proto::client::SortDirection::Asc | synctv_proto::client::SortDirection::Unspecified
    );
    if !allows_default_sort {
        return Err(ApiError::InvalidInput(
            "dynamic playlist browsing does not support custom sorting yet".to_string(),
        ));
    }

    Ok(true)
}

pub fn build_move_media_request(
    req: synctv_proto::client::MoveMediaRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<CoreMoveMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let synctv_proto::client::MoveMediaRequest {
        media_ids,
        source_playlist_id,
        target_playlist_id,
        all_from_scope,
        before_media_id,
        after_media_id,
    } = req;

    Ok(CoreMoveMediaRequest {
        media_ids: crate::impls::proto_validated_media_ids(media_ids, public_id_codec)?,
        source_playlist_id: source_playlist_id
            .map(|id| crate::impls::proto_validated_playlist_id(id, public_id_codec))
            .transpose()?,
        target_playlist_id: target_playlist_id
            .map(|id| crate::impls::proto_validated_playlist_id(id, public_id_codec))
            .transpose()?,
        all_from_scope,
        before_media_id: before_media_id
            .map(|id| crate::impls::proto_validated_media_id(id, public_id_codec))
            .transpose()?,
        after_media_id: after_media_id
            .map(|id| crate::impls::proto_validated_media_id(id, public_id_codec))
            .transpose()?,
    })
}

pub fn build_add_media_request(
    req: synctv_proto::client::AddMediaRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<CoreAddMediaRequest, ApiError> {
    synctv_adapter::client::add_media_request_from_client_proto(req, public_id_codec)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub fn build_delete_entries_request(
    req: synctv_proto::client::DeleteEntriesRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<(CoreDeleteEntriesRequest, Vec<String>, Vec<String>), ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let synctv_proto::client::DeleteEntriesRequest {
        playlist_ids,
        media_ids,
        force,
    } = req;
    let media_id_strings = media_ids.clone();
    let playlist_id_strings = playlist_ids.clone();
    Ok((
        CoreDeleteEntriesRequest {
            playlist_ids: crate::impls::proto_validated_playlist_ids(
                playlist_ids,
                public_id_codec,
            )?,
            media_ids: crate::impls::proto_validated_media_ids(media_ids, public_id_codec)?,
            force,
        },
        media_id_strings,
        playlist_id_strings,
    ))
}

fn build_add_media_batch_request(
    req: synctv_proto::client::AddMediaBatchRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<AddMediaBatchBuildResult, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    if req.items.is_empty() {
        return Err(ApiError::InvalidInput(
            "items array cannot be empty".to_string(),
        ));
    }

    let mut playlist_targets = std::collections::HashSet::new();
    let mut items = Vec::with_capacity(req.items.len());

    for item in req.items {
        playlist_targets.insert(item.playlist_id.clone());
        items.push(build_add_media_request(item, public_id_codec)?);
    }

    if playlist_targets.len() != 1 {
        return Err(ApiError::InvalidInput(
            "Batch add must target exactly one location".to_string(),
        ));
    }

    let playlist_id = playlist_targets
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::InvalidInput("Batch add must target one location".to_string()))?
        .map(|id| crate::impls::proto_validated_playlist_id(id, public_id_codec))
        .transpose()?;

    Ok(AddMediaBatchBuildResult { items, playlist_id })
}

pub fn build_delete_media_request(
    req: synctv_proto::client::DeleteMediaRequest,
) -> Result<synctv_proto::client::DeleteEntriesRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(synctv_proto::client::DeleteEntriesRequest {
        playlist_ids: Vec::new(),
        media_ids: vec![req.media_id],
        force: req.force,
    })
}

pub fn build_clear_playlist_request(
    req: synctv_proto::client::ClearPlaylistRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    if req.playlist_id.is_empty() {
        return Ok(None);
    }
    crate::impls::proto_validated_playlist_id(req.playlist_id, public_id_codec).map(Some)
}

pub fn build_edit_media_request(
    req: synctv_proto::client::EditMediaRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::service::EditMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    let name = if req.name.is_empty() {
        None
    } else {
        Some(
            crate::impls::validation::validate_media_name(&req.name)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media name: {e}")))?,
        )
    };

    Ok(synctv_core::service::EditMediaRequest {
        media_id: crate::impls::proto_validated_media_id(req.media_id, public_id_codec)?,
        name,
        description: (!req.description.trim().is_empty()).then_some(req.description),
    })
}

impl ClientApiImpl {
    async fn media_actor_username_for_event(&self, user_id: &UserId) -> Result<String, ApiError> {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
            .map_err(ApiError::from)
    }

    pub async fn add_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::AddMediaRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_add_media_request(req, &self.public_id_codec)?;
        let playlist_id = service_req.playlist_id;

        // Check total playlist size limit before adding
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            let count = self
                .room_service
                .media_service()
                .count_room_playlist_media(&rid, playlist_id)
                .await
                .map_err(ApiError::from)?;
            i64_count_to_usize(count, "playlist media count")?
        } else {
            let count = self
                .room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)?;
            i64_count_to_usize(count, "room root media count")?
        };
        if existing_count >= Self::MAX_PLAYLIST_SIZE {
            return Err(ApiError::InvalidInput(format!(
                "Playlist has reached maximum size of {} items",
                Self::MAX_PLAYLIST_SIZE
            )));
        }

        let username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout = self
            .media_fanout
            .prepare_added_outbox_fanout(rid, uid, username);
        let media = self
            .room_service
            .media_service()
            .add_media_with_outbox(
                rid,
                uid,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(uid))
            .await
    }

    pub async fn delete_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::DeleteMediaRequest,
    ) -> Result<synctv_proto::client::DeleteMediaResponse, ApiError> {
        self.delete_entries(user_id, room_id, build_delete_media_request(req)?)
            .await?;

        Ok(synctv_proto::client::DeleteMediaResponse { success: true })
    }

    /// Delete a mixed set of playlist and media entries.
    pub async fn delete_entries(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::DeleteEntriesRequest,
    ) -> Result<synctv_proto::client::DeleteEntriesResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let (service_req, _explicit_media_ids, _explicit_playlist_ids) =
            build_delete_entries_request(req, &self.public_id_codec)?;
        let username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout =
            prepare_delete_entries_outbox_fanout(PrepareDeleteEntriesOutboxFanout {
                clock: self.clock.clone(),
                media_fanout: self.media_fanout.clone(),
                playlist_fanout: self.playlist_fanout.clone(),
                playback_fanout: self.playback_fanout.clone(),
                realtime_fanout: self.realtime_fanout.clone(),
                room_id: rid,
                user_id: uid,
                username: username.clone(),
            });
        let result = self
            .room_service
            .delete_entries_with_outbox(
                rid,
                uid,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&rid);

        for media_id in &result.deleted_media_ids {
            if let Err(error) = self
                .realtime_lifecycle
                .kick_local_stream(&rid, media_id)
                .await
            {
                tracing::warn!(
                    room_id = %rid,
                    media_id = %media_id,
                    error = %error,
                    "Failed to kick local stream after playlist entry deletion"
                );
            }
        }

        Ok(synctv_proto::client::DeleteEntriesResponse {
            deleted_playlists: usize_to_i32_api(
                result.deleted_playlists,
                "deleted playlist count",
            )?,
            deleted_media: usize_to_i32_api(result.deleted_media, "deleted media count")?,
        })
    }

    /// Edit media metadata
    pub async fn edit_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::EditMediaRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_edit_media_request(req, &self.public_id_codec)?;

        let username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout = self
            .media_fanout
            .prepare_updated_outbox_fanout(rid, uid, username);
        let media = self
            .room_service
            .media_service()
            .edit_media_with_outbox(
                rid,
                uid,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        // Invalidate room cache on other replicas so they see updated metadata
        self.room_cache_fanout.publish_invalidation(&rid);

        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(uid))
            .await
    }

    pub async fn create_media_cover_upload_session(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::CreateMediaCoverUploadSessionRequest,
    ) -> Result<synctv_proto::client::CreateMediaCoverUploadSessionResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let session = self
            .room_service
            .media_service()
            .create_media_cover_upload_session(
                rid,
                media_id,
                *user_id,
                CreateMediaCoverUploadSession {
                    client_cover_id: optional_trimmed_string(&req.client_cover_id),
                    mime_type: req.mime_type,
                    size_bytes: req.size_bytes,
                    width: (req.width > 0).then_some(req.width),
                    height: (req.height > 0).then_some(req.height),
                    duration_seconds: (req.duration_seconds > 0).then_some(req.duration_seconds),
                    bitrate_bps: (req.bitrate_bps > 0).then_some(req.bitrate_bps),
                    parts: proto_upload_manifest_parts(req.parts),
                    metadata: file_metadata_from_proto(req.metadata.as_ref())?,
                },
            )
            .await
            .map_err(ApiError::from)?;
        media_cover_upload_create_result_to_proto(session)
    }

    pub async fn upload_media_cover_object(
        &self,
        req: synctv_proto::client::UploadMediaCoverObjectRequest,
    ) -> Result<synctv_proto::client::UploadMediaCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .media_service()
            .store_media_cover_upload_object(
                &req.encoded_object_key,
                &req.token,
                req.content_type.as_deref(),
                proto_file_upload_range(req.content_range),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) = uploaded_parts_response_fields(&blob);
        Ok(synctv_proto::client::UploadMediaCoverObjectResponse {
            object: match blob {
                StoreFileUploadResult::Complete(blob) => Some(media_cover_object_to_proto(blob)),
                StoreFileUploadResult::PartAccepted { .. } => None,
            },
            complete,
            uploaded_size_bytes,
            uploaded_parts,
        })
    }

    pub async fn complete_media_cover_upload_session(
        &self,
        req: synctv_proto::client::CompleteMediaCoverUploadSessionRequest,
    ) -> Result<synctv_proto::client::CompleteMediaCoverUploadSessionResponse, ApiError> {
        let result = self
            .room_service
            .media_service()
            .complete_media_cover_upload_session(complete_upload_session_request(
                &req.file_id,
                req.encoded_object_key,
                req.token,
                req.upload_id,
                &req.ownership_proof,
                req.parts,
            ))
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) =
            complete_upload_response_fields(&result);
        Ok(
            synctv_proto::client::CompleteMediaCoverUploadSessionResponse {
                object: result.object.map(media_cover_object_to_proto),
                complete,
                uploaded_size_bytes,
                uploaded_parts,
            },
        )
    }

    pub async fn get_media_cover_object(
        &self,
        req: synctv_proto::client::GetMediaCoverObjectRequest,
    ) -> Result<synctv_core::models::FileObjectDownload, ApiError> {
        self.room_service
            .media_service()
            .get_media_cover_object_stream(
                &req.encoded_object_key,
                &req.token,
                proto_file_range_request(req.range),
            )
            .await
            .map_err(ApiError::from)
    }

    pub async fn update_media_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateMediaCoverRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let cover = required_file_upload_reference(req.cover_reference, "cover_reference")?;
        let media = self
            .room_service
            .media_service()
            .update_media_cover(rid, media_id, *user_id, cover)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(*user_id))
            .await
    }

    pub async fn clear_media_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ClearMediaCoverRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let media = self
            .room_service
            .media_service()
            .clear_media_cover(rid, media_id, *user_id)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(*user_id))
            .await
    }

    pub async fn create_media_thumbnail_upload_session(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::CreateMediaThumbnailUploadSessionRequest,
    ) -> Result<synctv_proto::client::CreateMediaThumbnailUploadSessionResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let session = self
            .room_service
            .media_service()
            .create_media_thumbnail_upload_session(
                rid,
                media_id,
                *user_id,
                CreateMediaThumbnailUploadSession {
                    client_thumbnail_id: optional_trimmed_string(&req.client_thumbnail_id),
                    mime_type: req.mime_type,
                    size_bytes: req.size_bytes,
                    width: (req.width > 0).then_some(req.width),
                    height: (req.height > 0).then_some(req.height),
                    duration_seconds: (req.duration_seconds > 0).then_some(req.duration_seconds),
                    bitrate_bps: (req.bitrate_bps > 0).then_some(req.bitrate_bps),
                    parts: proto_upload_manifest_parts(req.parts),
                    metadata: file_metadata_from_proto(req.metadata.as_ref())?,
                },
            )
            .await
            .map_err(ApiError::from)?;
        media_thumbnail_upload_create_result_to_proto(session)
    }

    pub async fn upload_media_thumbnail_object(
        &self,
        req: synctv_proto::client::UploadMediaThumbnailObjectRequest,
    ) -> Result<synctv_proto::client::UploadMediaThumbnailObjectResponse, ApiError> {
        let blob = self
            .room_service
            .media_service()
            .store_media_thumbnail_upload_object(
                &req.encoded_object_key,
                &req.token,
                req.content_type.as_deref(),
                proto_file_upload_range(req.content_range),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) = uploaded_parts_response_fields(&blob);
        Ok(synctv_proto::client::UploadMediaThumbnailObjectResponse {
            object: match blob {
                StoreFileUploadResult::Complete(blob) => {
                    Some(media_thumbnail_object_to_proto(blob))
                }
                StoreFileUploadResult::PartAccepted { .. } => None,
            },
            complete,
            uploaded_size_bytes,
            uploaded_parts,
        })
    }

    pub async fn complete_media_thumbnail_upload_session(
        &self,
        req: synctv_proto::client::CompleteMediaThumbnailUploadSessionRequest,
    ) -> Result<synctv_proto::client::CompleteMediaThumbnailUploadSessionResponse, ApiError> {
        let result = self
            .room_service
            .media_service()
            .complete_media_thumbnail_upload_session(complete_upload_session_request(
                &req.file_id,
                req.encoded_object_key,
                req.token,
                req.upload_id,
                &req.ownership_proof,
                req.parts,
            ))
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) =
            complete_upload_response_fields(&result);
        Ok(
            synctv_proto::client::CompleteMediaThumbnailUploadSessionResponse {
                object: result.object.map(media_thumbnail_object_to_proto),
                complete,
                uploaded_size_bytes,
                uploaded_parts,
            },
        )
    }

    pub async fn get_media_thumbnail_object(
        &self,
        req: synctv_proto::client::GetMediaThumbnailObjectRequest,
    ) -> Result<synctv_core::models::FileObjectDownload, ApiError> {
        self.room_service
            .media_service()
            .get_media_thumbnail_object_stream(
                &req.encoded_object_key,
                &req.token,
                proto_file_range_request(req.range),
            )
            .await
            .map_err(ApiError::from)
    }

    pub async fn update_media_thumbnail(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateMediaThumbnailRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let thumbnail =
            required_file_upload_reference(req.thumbnail_reference, "thumbnail_reference")?;
        let media = self
            .room_service
            .media_service()
            .update_media_thumbnail(rid, media_id, *user_id, thumbnail)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(*user_id))
            .await
    }

    pub async fn clear_media_thumbnail(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ClearMediaThumbnailRequest,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let media_id =
            crate::impls::parse_media_id_param(&req.media_id, "media_id", &self.public_id_codec)?;
        let media = self
            .room_service
            .media_service()
            .clear_media_thumbnail(rid, media_id, *user_id)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(*user_id))
            .await
    }

    /// Clear all media directly under the room root
    pub async fn clear_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ClearPlaylistRequest,
    ) -> Result<synctv_proto::client::ClearPlaylistResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let playlist_id = build_clear_playlist_request(req, &self.public_id_codec)?;

        // Check permission
        self.room_service
            .check_permission(&rid, &uid, synctv_core::models::RoomPermission::CLEAR_MEDIA)
            .await
            .map_err(ApiError::from)?;

        let username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout =
            prepare_delete_entries_outbox_fanout(PrepareDeleteEntriesOutboxFanout {
                clock: self.clock.clone(),
                media_fanout: self.media_fanout.clone(),
                playlist_fanout: self.playlist_fanout.clone(),
                playback_fanout: self.playback_fanout.clone(),
                realtime_fanout: self.realtime_fanout.clone(),
                room_id: rid,
                user_id: uid,
                username,
            });
        let result = self
            .room_service
            .clear_playlist_with_outbox(
                rid,
                uid,
                playlist_id,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&rid);

        for media_id in &result.deleted_media_ids {
            if let Err(error) = self
                .realtime_lifecycle
                .kick_local_stream(&rid, media_id)
                .await
            {
                tracing::warn!(
                    room_id = %rid,
                    media_id = %media_id,
                    error = %error,
                    "Failed to kick local stream after playlist clear"
                );
            }
        }

        Ok(synctv_proto::client::ClearPlaylistResponse {
            success: true,
            deleted_count: i64_to_i32_api(result.deleted_count, "deleted item count")?,
            deleted_playlists: usize_to_i32_api(
                result.deleted_playlists,
                "deleted playlist count",
            )?,
        })
    }

    /// Maximum total media items allowed in a single room's playlist.
    ///
    /// Prevents unbounded playlist growth which could degrade database
    /// performance and client rendering.
    pub const MAX_PLAYLIST_SIZE: usize = synctv_core::validation::MEDIA_PLAYLIST_MAX_ITEMS;

    /// Add multiple media items in a batch (atomic - all succeed or all fail)
    pub async fn add_media_batch(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::AddMediaBatchRequest,
    ) -> Result<synctv_proto::client::AddMediaBatchResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let AddMediaBatchBuildResult { items, playlist_id } =
            build_add_media_batch_request(req, &self.public_id_codec)?;
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            let count = self
                .room_service
                .media_service()
                .count_room_playlist_media(&rid, playlist_id)
                .await
                .map_err(ApiError::from)?;
            i64_count_to_usize(count, "playlist media count")?
        } else {
            let count = self
                .room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)?;
            i64_count_to_usize(count, "room root media count")?
        };
        let new_total = existing_count + items.len();
        if new_total > Self::MAX_PLAYLIST_SIZE {
            let target = if playlist_id.is_some() {
                "playlist"
            } else {
                "room root"
            };
            return Err(ApiError::InvalidInput(format!(
                "{} would exceed maximum size of {} items \
                 (current: {}, adding: {})",
                target,
                Self::MAX_PLAYLIST_SIZE,
                existing_count,
                items.len()
            )));
        }

        let username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout = self
            .media_fanout
            .prepare_added_batch_outbox_fanout(rid, uid, username);
        let media_list = self
            .room_service
            .media_service()
            .add_media_batch_with_outbox(
                rid,
                uid,
                playlist_id,
                items,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        let results = media_models_to_proto(self, &media_list, Some(uid)).await?;

        Ok(synctv_proto::client::AddMediaBatchResponse { results })
    }

    pub async fn move_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::MoveMediaRequest,
    ) -> Result<synctv_proto::client::MoveMediaResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_move_media_request(req, &self.public_id_codec)?;

        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::REORDER_MEDIA,
            )
            .await
            .map_err(Self::map_room_access_error)?;
        let media_fanout_plan = MoveMediaFanoutPlanner::new(self.room_service.media_service())
            .build(&rid, &service_req)
            .await?;

        let actor_username = self.media_actor_username_for_event(&uid).await?;
        let prepared_outbox_fanout = self.media_fanout.prepare_move_outbox_fanout(
            rid,
            uid,
            actor_username,
            media_fanout_plan,
        );
        let media = self
            .room_service
            .media_service()
            .move_media_with_outbox(
                rid,
                uid,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&rid);

        let proto_media = media_models_to_proto(self, &media, Some(uid)).await?;

        Ok(synctv_proto::client::MoveMediaResponse {
            moved_count: usize_to_i32_api(media.len(), "moved media count")?,
            media: proto_media,
        })
    }

    /// List playlist items (supports both static and dynamic playlists)
    ///
    /// Empty `playlist_id` means the room root.
    ///
    /// For room root and static playlists: returns child playlists + media from database
    /// For dynamic playlists: returns remote provider items
    pub async fn list_playlist_items(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.list_playlist_items_for_actor(&actor, req).await
    }

    pub async fn list_playlist_items_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.list_playlist_items_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn list_playlist_items_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, synctv_core::models::RoomPermission::BROWSE_LIBRARY)
            .await?;
        let rid = actor.room_id();
        let viewer_id = actor.user_id();
        let target = provider_target_from_proto(req.target.clone())?;
        if let Some(preview_source_config) = req.preview_source_config.clone() {
            if !req.playlist_id.is_empty() {
                return Err(ApiError::InvalidInput(
                    "playlist_id must be empty for source preview".to_string(),
                ));
            }
            let Some(uid) = actor.user_id() else {
                return Err(ApiError::Authorization(
                    "Guests cannot preview provider playlists".to_string(),
                ));
            };
            let (source_provider, source_config) =
                synctv_adapter::source_config::playlist_source_config_from_proto(Some(
                    preview_source_config,
                ))
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
            let requested_provider = optional_proto_source_provider_to_core(req.source_provider)?;
            if requested_provider.is_some_and(|provider| provider != source_provider) {
                return Err(ApiError::InvalidInput(
                    "source_provider does not match preview_source_config".to_string(),
                ));
            }
            let search = normalize_non_empty_filter(&req.search);
            let pagination = dynamic_pagination_from_request(
                req.pagination.as_ref(),
                source_provider,
                search.is_some(),
            )?;
            let result = self
                .room_service
                .media_service()
                .preview_dynamic_playlist_items(
                    synctv_core::service::DynamicPlaylistPreviewRequest {
                        room_id: rid,
                        user_id: uid,
                        source_provider,
                        source_config,
                        provider_instance_name: normalize_non_empty_filter(
                            &req.provider_instance_name,
                        ),
                        target,
                        query: DynamicListQuery {
                            pagination,
                            page_size: crate::impls::proto_page_size_u32_usize(
                                req.page_size,
                                20,
                                100,
                            )?,
                            search,
                            refresh: req.refresh,
                        },
                    },
                )
                .await
                .map_err(ApiError::from)?;
            let response_pagination = result.pagination.clone();
            let dynamic_items = result
                .items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::{DynamicPlaylistItemThumbnail, ItemType};
                    let source_config =
                        dynamic_playlist_item_source_config_to_proto(item.source_config)?;
                    let metadata = item
                        .metadata
                        .as_ref()
                        .map(|metadata| {
                            super::convert::provider_resource_metadata_to_proto(
                                metadata,
                                &self.public_id_codec,
                            )
                        })
                        .transpose()?
                        .map(|provider| synctv_proto::client::ResourceMetadata {
                            source: None,
                            provider: Some(provider),
                        });
                    let thumbnail = match item.thumbnail {
                        Some(DynamicPlaylistItemThumbnail::Url(url)) => Some(url),
                        Some(DynamicPlaylistItemThumbnail::Emby {
                            server_id,
                            credential_owner_id,
                            item_id,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let url = crate::emby_thumbnail_urls::emby_thumbnail_url(
                                &server_id,
                                &public_owner_id,
                                &item_id,
                            );
                            Some(
                                crate::emby_thumbnail_urls::sign_emby_thumbnail_url(
                                    &url,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        _ => None,
                    };
                    Ok(synctv_proto::client::PlaylistItem {
                        name: item.name,
                        item_type: match item.item_type {
                            ItemType::Playlist => synctv_proto::client::ItemType::Playlist as i32,
                            ItemType::Media => synctv_proto::client::ItemType::Media as i32,
                        },
                        target: optional_provider_target_to_proto(Some(&item.target)),
                        size: item.size,
                        thumbnail,
                        modified_at: item.modified_at,
                        description: item.description.unwrap_or_default(),
                        source_config,
                        metadata,
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;
            return finalize_playlist_items_response_version(
                synctv_proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total: None,
                    playlist_count: 0,
                    file_count: 0,
                    dynamic_items,
                    current_path: Vec::new(),
                    version: String::new(),
                    pagination: Some(dynamic_response_pagination(response_pagination)?),
                },
            );
        }
        let Some(playlist_id) = (if req.playlist_id.is_empty() {
            None
        } else {
            Some(crate::impls::proto_validated_playlist_id(
                req.playlist_id.clone(),
                &self.public_id_codec,
            )?)
        }) else {
            if target.is_some() {
                return Err(ApiError::InvalidInput(
                    "target must be omitted when browsing the room root".to_string(),
                ));
            }
            let availability = map_availability_filter(req.availability)?;
            let playlist_sort_by = map_playlist_sort_from_media_sort(req.sort_by)?;
            let media_sort_by = map_media_sort(req.sort_by)?;
            let sort_direction = map_sort_direction(req.sort_direction)?;
            let playlist_query = CorePlaylistListQuery {
                pagination: crate::impls::proto_page_params_u32(
                    playlist_items_page(&req)?,
                    req.page_size,
                    50,
                    100,
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: optional_proto_source_provider_to_core(req.source_provider)?,
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability,
                sort_by: playlist_sort_by,
                sort_direction,
            };
            let media_query = CoreMediaListQuery {
                pagination: crate::impls::proto_page_params_u32(
                    playlist_items_page(&req)?,
                    req.page_size,
                    50,
                    100,
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: optional_proto_source_provider_to_core(req.source_provider)?,
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                availability,
                sort_by: media_sort_by,
                sort_direction,
            };
            let playlist_count = self
                .room_service
                .count_client_playlists(&rid, None, &playlist_query)
                .await
                .map_err(ApiError::from)
                .and_then(|count| i64_count_to_usize(count, "root playlist count"))?;
            let file_count = self
                .room_service
                .count_client_media(&rid, None, &media_query)
                .await
                .map_err(ApiError::from)
                .and_then(|count| i64_count_to_usize(count, "root media count"))?;
            let total = playlist_count + file_count;
            let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
            let skip = page_u32_to_usize(playlist_items_page(&req)?)?
                .saturating_sub(1)
                .saturating_mul(page_size);
            let (playlists, media) = if skip < playlist_count {
                let playlist_limit = usize_to_i64_api(page_size, "playlist page size")?;
                let playlist_offset = usize_to_i64_api(skip, "playlist offset")?;
                let playlists = self
                    .room_service
                    .list_client_playlists(
                        &rid,
                        None,
                        &playlist_query,
                        playlist_limit,
                        playlist_offset,
                    )
                    .await
                    .map_err(ApiError::from)?;
                let remaining = page_size.saturating_sub(playlists.len());
                let media = if remaining > 0 {
                    let media_limit = usize_to_i64_api(remaining, "media page size")?;
                    self.room_service
                        .list_client_media(&rid, None, &media_query, media_limit, 0)
                        .await
                        .map_err(ApiError::from)?
                } else {
                    Vec::new()
                };
                (playlists, media)
            } else {
                let media_skip = skip - playlist_count;
                let media_limit = usize_to_i64_api(page_size, "media page size")?;
                let media_offset = usize_to_i64_api(media_skip, "media offset")?;
                let media = self
                    .room_service
                    .list_client_media(&rid, None, &media_query, media_limit, media_offset)
                    .await
                    .map_err(ApiError::from)?;
                (Vec::new(), media)
            };
            let playlist_ids: Vec<synctv_core::models::PlaylistId> =
                playlists.iter().map(|pl| pl.playlist.id).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&playlist_ids)
                .await
                .map_err(ApiError::from)?;
            let (proto_playlists, proto_media) = tokio::try_join!(
                playlist_list_items_to_proto(self, &playlists, &counts, viewer_id),
                media_list_items_to_proto(self, &media, viewer_id),
            )?;

            return finalize_playlist_items_response_version(
                synctv_proto::client::ListPlaylistItemsResponse {
                    playlists: proto_playlists,
                    media: proto_media,
                    total: Some(usize_to_u64_api(total, "playlist item total")?),
                    playlist_count: usize_to_u64_api(playlist_count, "playlist count")?,
                    file_count: usize_to_u64_api(file_count, "playlist file count")?,
                    dynamic_items: Vec::new(),
                    current_path: Vec::new(),
                    version: String::new(),
                    pagination: None,
                },
            );
        };

        // Get playlist info to determine if static or dynamic
        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(&rid, &playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {} not found", req.playlist_id)))?;
        let static_path = self
            .room_service
            .playlist_service()
            .get_room_playlist_path(&rid, &playlist_id)
            .await
            .map_err(ApiError::from)?;
        let mut current_path: Vec<synctv_proto::client::PlaylistBrowsePathNode> = static_path
            .iter()
            .map(|playlist| try_playlist_path_node_to_proto(playlist, &self.public_id_codec))
            .collect::<Result<_, _>>()?;

        if playlist.is_dynamic() {
            let Some(uid) = actor.user_id() else {
                return Err(ApiError::Authorization(
                    "Guests cannot browse dynamic provider playlists".to_string(),
                ));
            };
            require_dynamic_playlist_creator(&playlist, uid)?;
            self.room_service
                .ensure_client_usable_playlist(&playlist)
                .await
                .map_err(ApiError::from)?;
            if !validate_dynamic_playlist_query_support(&playlist, &req)? {
                return finalize_playlist_items_response_version(
                    synctv_proto::client::ListPlaylistItemsResponse {
                        playlists: Vec::new(),
                        media: Vec::new(),
                        total: Some(0),
                        playlist_count: 0,
                        file_count: 0,
                        dynamic_items: Vec::new(),
                        current_path,
                        version: String::new(),
                        pagination: None,
                    },
                );
            }

            let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
            let search = normalize_non_empty_filter(&req.search);
            let source_provider = playlist.source_provider.ok_or_else(|| {
                ApiError::InvalidInput("Dynamic playlist is missing source_provider".to_string())
            })?;
            let pagination = dynamic_pagination_from_request(
                req.pagination.as_ref(),
                source_provider,
                search.is_some(),
            )?;
            let result = self
                .room_service
                .media_service()
                .list_dynamic_playlist_items(
                    rid,
                    uid,
                    &playlist_id,
                    target.as_ref(),
                    DynamicListQuery {
                        pagination: pagination.clone(),
                        page_size,
                        search,
                        refresh: req.refresh,
                    },
                )
                .await
                .map_err(ApiError::from)?;
            let response_pagination = result.pagination.clone();

            // Convert provider DynamicPlaylistItem to proto PlaylistItem
            let dynamic_items: Vec<_> = result
                .items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::{DynamicPlaylistItemThumbnail, ItemType};
                    let source_config =
                        dynamic_playlist_item_source_config_to_proto(item.source_config)?;
                    let metadata = item
                        .metadata
                        .as_ref()
                        .map(|metadata| {
                            super::convert::provider_resource_metadata_to_proto(
                                metadata,
                                &self.public_id_codec,
                            )
                        })
                        .transpose()?
                        .map(|provider| synctv_proto::client::ResourceMetadata {
                            source: None,
                            provider: Some(provider),
                        });
                    let item_type = match item.item_type {
                        ItemType::Playlist => synctv_proto::client::ItemType::Playlist as i32,
                        ItemType::Media => synctv_proto::client::ItemType::Media as i32,
                    };
                    let thumbnail = match item.thumbnail {
                        Some(DynamicPlaylistItemThumbnail::Url(thumbnail)) => Some(thumbnail),
                        Some(DynamicPlaylistItemThumbnail::Emby {
                            server_id,
                            credential_owner_id,
                            item_id,
                        }) => {
                            let public_room_id =
                                self.public_id_codec.encode_room_id(rid).map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode room public id: {error}"
                                    ))
                                })?;
                            let public_user_id =
                                self.public_id_codec.encode_user_id(uid).map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode user public id: {error}"
                                    ))
                                })?;
                            let public_credential_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode credential owner public id: {error}"
                                    ))
                                })?;
                            let thumbnail = crate::emby_thumbnail_urls::emby_thumbnail_url(
                                &server_id,
                                &public_credential_owner_id,
                                &item_id,
                            );
                            Some(
                                crate::emby_thumbnail_urls::sign_emby_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::Fnos {
                            server_id,
                            credential_owner_id,
                            image_path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::fnos_thumbnail_urls::fnos_thumbnail_url(
                                &server_id,
                                &public_owner_id,
                                &image_path,
                                400,
                            );
                            Some(
                                crate::fnos_thumbnail_urls::sign_fnos_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::Qnap {
                            server_id,
                            credential_owner_id,
                            path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::qnap_thumbnail_urls::qnap_thumbnail_url(
                                &server_id,
                                &public_owner_id,
                                &path,
                                320,
                            );
                            Some(
                                crate::qnap_thumbnail_urls::sign_qnap_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::Nextcloud {
                            server_id,
                            credential_owner_id,
                            file_id,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let preview = crate::nextcloud_preview_urls::nextcloud_preview_url(
                                &server_id,
                                &public_owner_id,
                                file_id,
                                320,
                                320,
                                true,
                            );
                            Some(
                                crate::nextcloud_preview_urls::sign_nextcloud_preview_url(
                                    &preview,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::Seafile {
                            server_id,
                            credential_owner_id,
                            repository_id,
                            path,
                        }) => {
                            let room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::seafile_thumbnail_urls::seafile_thumbnail_url(
                                &server_id,
                                &owner_id,
                                &repository_id,
                                &path,
                                320,
                            );
                            Some(
                                crate::seafile_thumbnail_urls::sign_seafile_thumbnail_url(
                                    &thumbnail,
                                    &room_id,
                                    &user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::SynologyFile {
                            server_id,
                            credential_owner_id,
                            path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let image = crate::synology_image_urls::synology_file_image_url(
                                &server_id,
                                &public_owner_id,
                                &path,
                                "medium",
                            );
                            Some(
                                crate::synology_image_urls::sign_synology_image_url(
                                    &image,
                                    crate::synology_image_urls::SynologyImageScope::File {
                                        server_id: &server_id,
                                        credential_owner_id: &public_owner_id,
                                        path: &path,
                                        size: "medium",
                                    },
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DynamicPlaylistItemThumbnail::SynologyPoster {
                            server_id,
                            credential_owner_id,
                            item_id,
                            media_type,
                            poster_mtime,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(uid)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let image = crate::synology_image_urls::synology_poster_url(
                                &server_id,
                                &public_owner_id,
                                item_id,
                                &media_type,
                                poster_mtime.as_deref(),
                            );
                            Some(
                                crate::synology_image_urls::sign_synology_image_url(
                                    &image,
                                    crate::synology_image_urls::SynologyImageScope::Poster {
                                        server_id: &server_id,
                                        credential_owner_id: &public_owner_id,
                                        item_id,
                                        media_type: &media_type,
                                        poster_mtime: poster_mtime.as_deref(),
                                    },
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        None => None,
                    };

                    Ok(synctv_proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        target: Some(
                            optional_provider_target_to_proto(Some(&item.target))
                                .expect("provider target conversion returns Some"),
                        ),
                        size: item.size,
                        thumbnail,
                        modified_at: item.modified_at,
                        description: item.description.unwrap_or_default(),
                        source_config,
                        metadata,
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;

            let browse_path = self
                .room_service
                .media_service()
                .get_dynamic_playlist_browse_path(rid, uid, &playlist_id, target.as_ref())
                .await
                .map_err(ApiError::from)?;
            current_path.extend(browse_path.into_iter().map(|segment| {
                synctv_proto::client::PlaylistBrowsePathNode {
                    playlist_id: String::new(),
                    name: segment.name,
                    target: Some(
                        optional_provider_target_to_proto(Some(&segment.target))
                            .expect("provider target conversion returns Some"),
                    ),
                }
            }));

            let pagination = Some(dynamic_response_pagination(response_pagination)?);

            return finalize_playlist_items_response_version(
                synctv_proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total: None,
                    playlist_count: 0,
                    file_count: 0,
                    dynamic_items,
                    current_path,
                    version: String::new(),
                    pagination,
                },
            );
        }

        if target.is_some() {
            return Err(ApiError::InvalidInput(
                "target must be omitted when browsing a static playlist".to_string(),
            ));
        }

        let availability = map_availability_filter(req.availability)?;
        let playlist_sort_by = map_playlist_sort_from_media_sort(req.sort_by)?;
        let media_sort_by = map_media_sort(req.sort_by)?;
        let sort_direction = map_sort_direction(req.sort_direction)?;
        let playlist_query = CorePlaylistListQuery {
            pagination: crate::impls::proto_page_params_u32(
                playlist_items_page(&req)?,
                req.page_size,
                50,
                100,
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: optional_proto_source_provider_to_core(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability,
            sort_by: playlist_sort_by,
            sort_direction,
        };
        let media_query = CoreMediaListQuery {
            pagination: crate::impls::proto_page_params_u32(
                playlist_items_page(&req)?,
                req.page_size,
                50,
                100,
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: optional_proto_source_provider_to_core(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            availability,
            sort_by: media_sort_by,
            sort_direction,
        };
        let playlist_count = self
            .room_service
            .count_client_playlists(&rid, Some(&playlist_id), &playlist_query)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_count_to_usize(count, "playlist child playlist count"))?;
        let file_count = self
            .room_service
            .count_client_media(&rid, Some(&playlist_id), &media_query)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_count_to_usize(count, "playlist child media count"))?;
        let total = playlist_count + file_count;
        let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
        let skip = page_u32_to_usize(playlist_items_page(&req)?)?
            .saturating_sub(1)
            .saturating_mul(page_size);
        let (playlists, media) = if skip < playlist_count {
            let playlist_limit = usize_to_i64_api(page_size, "playlist page size")?;
            let playlist_offset = usize_to_i64_api(skip, "playlist offset")?;
            let playlists = self
                .room_service
                .list_client_playlists(
                    &rid,
                    Some(&playlist_id),
                    &playlist_query,
                    playlist_limit,
                    playlist_offset,
                )
                .await
                .map_err(ApiError::from)?;
            let remaining = page_size.saturating_sub(playlists.len());
            let media = if remaining > 0 {
                let media_limit = usize_to_i64_api(remaining, "media page size")?;
                self.room_service
                    .list_client_media(&rid, Some(&playlist_id), &media_query, media_limit, 0)
                    .await
                    .map_err(ApiError::from)?
            } else {
                Vec::new()
            };
            (playlists, media)
        } else {
            let media_skip = skip - playlist_count;
            let media_limit = usize_to_i64_api(page_size, "media page size")?;
            let media_offset = usize_to_i64_api(media_skip, "media offset")?;
            let media = self
                .room_service
                .list_client_media(
                    &rid,
                    Some(&playlist_id),
                    &media_query,
                    media_limit,
                    media_offset,
                )
                .await
                .map_err(ApiError::from)?;
            (Vec::new(), media)
        };
        let playlist_ids: Vec<synctv_core::models::PlaylistId> =
            playlists.iter().map(|pl| pl.playlist.id).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .map_err(ApiError::from)?;
        let (proto_playlists, proto_media) = tokio::try_join!(
            playlist_list_items_to_proto(self, &playlists, &counts, viewer_id),
            media_list_items_to_proto(self, &media, viewer_id),
        )?;

        finalize_playlist_items_response_version(synctv_proto::client::ListPlaylistItemsResponse {
            playlists: proto_playlists,
            media: proto_media,
            total: Some(usize_to_u64_api(total, "playlist item total")?),
            playlist_count: usize_to_u64_api(playlist_count, "playlist count")?,
            file_count: usize_to_u64_api(file_count, "playlist file count")?,
            dynamic_items: Vec::new(),
            current_path,
            version: String::new(),
            pagination: None,
        })
    }

    /// Get a single media record from database
    pub async fn get_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        media_id: &str,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_media_for_actor(&actor, media_id).await
    }

    pub async fn get_media_as_guest(
        &self,
        access: &GuestRoomAccess,
        media_id: &str,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        self.get_media_for_actor(&RoomActor::Guest(access.clone()), media_id)
            .await
    }

    pub async fn get_media_for_actor(
        &self,
        actor: &RoomActor,
        media_id: &str,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let rid = actor.room_id();
        let mid = crate::impls::parse_media_id_param(media_id, "media_id", &self.public_id_codec)?;
        self.require_room_permission(actor, synctv_core::models::RoomPermission::BROWSE_LIBRARY)
            .await?;

        // Direct lookup by ID instead of loading the entire playlist.
        let media = self
            .room_service
            .media_service()
            .get_room_media(&rid, &mid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;
        let availability = self
            .room_service
            .media_availability(&media)
            .await
            .map_err(ApiError::from)?;

        self.media_to_proto_for_viewer_with_loaded_cover(
            &media,
            availability.is_available(),
            actor.user_id(),
        )
        .await
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService for ClientApiImpl {
    async fn get_playlist_items_snapshot(
        &self,
        actor: &crate::impls::client::RoomActor,
        req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.list_playlist_items_for_actor(actor, req.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_add_media_batch_request, build_add_media_request, build_delete_entries_request,
        build_delete_media_request, build_edit_media_request, build_move_media_request,
        compute_playlist_items_response_version, dynamic_pagination_from_request,
        file_upload_session_to_room_cover_proto, map_availability_filter, map_media_sort,
        map_playlist_sort_from_media_sort, map_sort_direction, require_dynamic_playlist_creator,
        stored_file_to_file_cover_proto, upload_session_fields,
        validate_dynamic_playlist_query_support,
    };
    use synctv_adapter::client::DEFAULT_MEDIA_TITLE;
    use synctv_core::models::{
        FileOwnershipProofRange, FileUploadSession, MediaId, NewStoredFile, Playlist, PlaylistId,
        RoomId, UserId,
    };
    use synctv_core::provider::DynamicPagination;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn codec_ok<T>(result: Result<T, String>) -> TestResult<T> {
        result.map_err(test_error)
    }

    fn require_error<T>(result: Result<T, crate::impls::ApiError>) -> crate::impls::ApiError {
        match result {
            Ok(_) => crate::impls::ApiError::Internal("expected error result".to_string()),
            Err(error) => error,
        }
    }

    fn alist_media_source_config(
        path: &str,
    ) -> Option<synctv_proto::source_config::MediaSourceConfig> {
        Some(synctv_proto::source_config::MediaSourceConfig {
            provider: Some(
                synctv_proto::source_config::media_source_config::Provider::Alist(
                    synctv_proto::source_config::AlistMediaSourceConfig {
                        server_id: "alist-server".to_string(),
                        path: path.to_string(),
                        password: None,
                    },
                ),
            ),
        })
    }

    fn direct_url_media_source_config(
        url: &str,
    ) -> Option<synctv_proto::source_config::MediaSourceConfig> {
        Some(synctv_proto::source_config::MediaSourceConfig {
            provider: Some(
                synctv_proto::source_config::media_source_config::Provider::DirectUrl(
                    synctv_proto::source_config::DirectUrlMediaSourceConfig {
                        medias: vec![synctv_proto::source_config::DirectUrlMediaResourceConfig {
                            name: String::new(),
                            url: url.to_string(),
                            headers: Default::default(),
                            format: String::new(),
                            expires_at: None,
                        }],
                        default_media_index: None,
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                        playback_kind: None,
                        duration_seconds: None,
                        prefer_proxy: None,
                        proxy_only: None,
                    },
                ),
            ),
        })
    }

    fn make_playlist(
        name: &str,
        source_provider: Option<synctv_core::models::SourceProvider>,
        provider_instance_name: Option<&str>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: name.to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider,
            source_config: source_provider.map(|provider| match provider {
                synctv_core::models::SourceProvider::Alist => {
                    synctv_core_testing::alist_directory_playlist_source_config(
                        "alist-server",
                        "/tv",
                    )
                }
                synctv_core::models::SourceProvider::Emby => {
                    synctv_core::models::PlaylistSourceConfig::Emby(
                        synctv_core::models::EmbyPlaylistSourceConfig {
                            server_id: "emby-server".to_string(),
                            source: synctv_core::models::EmbyPlaylistSource::Folder {
                                item_id: "library".to_string(),
                            },
                        },
                    )
                }
                _ => synctv_core_testing::alist_directory_playlist_source_config(
                    "alist-server",
                    "/tv",
                ),
            }),
            provider_instance_name: provider_instance_name.map(str::to_string),
            created_at: synctv_core::SystemClock.now(),
            updated_at: synctv_core::SystemClock.now(),
            version: 0,
        }
    }

    #[test]
    fn test_playlist_items_response_version_changes_when_only_thumbnail_url_changes(
    ) -> Result<(), crate::impls::ApiError> {
        let make_response = |thumbnail: &str| synctv_proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: Some(1),
            playlist_count: 0,
            file_count: 1,
            dynamic_items: vec![synctv_proto::client::PlaylistItem {
                name: "Episode 1".to_string(),
                item_type: synctv_proto::client::ItemType::Media as i32,
                target: Some(synctv_proto::client::ProviderTarget {
                    target: Some(synctv_proto::client::provider_target::Target::Alist(
                        synctv_proto::client::AlistTarget {
                            relative_path: "/tv/episode-1".to_string(),
                        },
                    )),
                }),
                size: Some(123),
                thumbnail: Some(thumbnail.to_string()),
                modified_at: Some(456),
                description: String::new(),
                source_config: None,
                metadata: None,
            }],
            current_path: Vec::new(),
            version: String::new(),
            pagination: None,
        };

        let original = compute_playlist_items_response_version(&make_response(
            "https://cdn.example.com/thumb-a.jpg",
        ))?;
        let changed = compute_playlist_items_response_version(&make_response(
            "https://cdn.example.com/thumb-b.jpg",
        ))?;

        assert_ne!(
            original, changed,
            "thumbnail-only changes must invalidate playlist item snapshots"
        );
        Ok(())
    }

    #[test]
    fn test_playlist_items_response_version_changes_with_provider_metadata(
    ) -> Result<(), crate::impls::ApiError> {
        let make_response =
            |is_currently_live: bool| synctv_proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total: Some(1),
                playlist_count: 0,
                file_count: 1,
                dynamic_items: vec![synctv_proto::client::PlaylistItem {
                    name: "Live room".to_string(),
                    item_type: synctv_proto::client::ItemType::Media as i32,
                    target: None,
                    size: None,
                    thumbnail: None,
                    modified_at: None,
                    description: String::new(),
                    source_config: None,
                    metadata: Some(synctv_proto::client::ResourceMetadata {
                        source: None,
                        provider: Some(synctv_proto::client::PlaybackMetadata {
                            metadata: Some(
                                synctv_proto::client::playback_metadata::Metadata::Bilibili(
                                    synctv_proto::client::BilibiliPlaybackMetadata {
                                        is_live: true,
                                        is_currently_live: Some(is_currently_live),
                                        ..Default::default()
                                    },
                                ),
                            ),
                        }),
                    }),
                }],
                current_path: Vec::new(),
                version: String::new(),
                pagination: None,
            };

        let live = compute_playlist_items_response_version(&make_response(true))?;
        let offline = compute_playlist_items_response_version(&make_response(false))?;

        assert_ne!(
            live, offline,
            "metadata-only changes must invalidate playlist item snapshots"
        );
        Ok(())
    }

    #[test]
    fn test_build_add_media_request_requires_source_config() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let err = require_error(build_add_media_request(
            synctv_proto::client::AddMediaRequest {
                playlist_id: None,
                provider_instance_name: String::new(),
                source_config: None,
                name: String::new(),
                description: String::new(),
            },
            &codec,
        ));

        assert!(err.to_string().contains("source_config"));
    }

    #[test]
    fn test_build_add_media_request_parses_dynamic_payload() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let playlist_id = PlaylistId::expect_positive(123);
        let request = api_ok(build_add_media_request(
            synctv_proto::client::AddMediaRequest {
                playlist_id: Some(codec_ok(codec.encode_playlist_id(playlist_id))?),
                provider_instance_name: "alist-main".into(),
                source_config: alist_media_source_config("/tv"),
                name: "Episode 1".into(),
                description: String::new(),
            },
            &codec,
        ))?;

        assert_eq!(request.playlist_id, Some(playlist_id));
        assert_eq!(request.name, "Episode 1");
        assert_eq!(
            request.source_provider,
            synctv_core::models::SourceProvider::Alist
        );
        assert_eq!(
            request.provider_instance_name.as_deref(),
            Some("alist-main")
        );
        assert_eq!(
            request.source_config,
            synctv_core_testing::alist_file_media_source_config("alist-server", "/tv")
        );
        Ok(())
    }

    #[test]
    fn test_build_add_media_request_maps_empty_provider_instance_to_none() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let request = api_ok(build_add_media_request(
            synctv_proto::client::AddMediaRequest {
                playlist_id: None,
                provider_instance_name: String::new(),
                source_config: direct_url_media_source_config("https://example.com/video.mp4"),
                name: "Example".into(),
                description: String::new(),
            },
            &codec,
        ))?;

        assert_eq!(
            request.source_provider,
            synctv_core::models::SourceProvider::DirectUrl
        );
        assert!(request.provider_instance_name.is_none());
        Ok(())
    }

    #[test]
    fn test_build_add_media_request_does_not_infer_title_from_source_config() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let request = api_ok(build_add_media_request(
            synctv_proto::client::AddMediaRequest {
                playlist_id: None,
                provider_instance_name: "alist-main".into(),
                source_config: alist_media_source_config("/tv"),
                name: String::new(),
                description: String::new(),
            },
            &codec,
        ))?;

        assert_eq!(request.name, DEFAULT_MEDIA_TITLE);
        Ok(())
    }

    #[test]
    fn test_build_add_media_batch_request_rejects_invalid_nested_playlist_id() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let err = require_error(build_add_media_batch_request(
            synctv_proto::client::AddMediaBatchRequest {
                items: vec![synctv_proto::client::AddMediaRequest {
                    playlist_id: Some("bad-playlist".into()),
                    provider_instance_name: "alist-main".into(),
                    source_config: alist_media_source_config("/tv"),
                    name: "Episode 1".into(),
                    description: String::new(),
                }],
            },
            &codec,
        ));

        assert!(err.to_string().contains("playlist_id"));
    }

    #[test]
    fn test_build_add_media_batch_request_reuses_single_item_builder_semantics() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let playlist_id = PlaylistId::expect_positive(123);
        let result = api_ok(build_add_media_batch_request(
            synctv_proto::client::AddMediaBatchRequest {
                items: vec![synctv_proto::client::AddMediaRequest {
                    playlist_id: Some(codec_ok(codec.encode_playlist_id(playlist_id))?),
                    provider_instance_name: "alist-main".into(),
                    source_config: alist_media_source_config("/tv"),
                    name: String::new(),
                    description: String::new(),
                }],
            },
            &codec,
        ))?;

        assert_eq!(result.playlist_id, Some(playlist_id));
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, DEFAULT_MEDIA_TITLE);
        assert_eq!(
            result.items[0].source_config,
            synctv_core_testing::alist_file_media_source_config("alist-server", "/tv")
        );
        Ok(())
    }

    #[test]
    fn test_build_delete_entries_request_rejects_empty_target_set() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let err = require_error(build_delete_entries_request(
            synctv_proto::client::DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: Vec::new(),
                force: false,
            },
            &codec,
        ));

        assert!(err.to_string().contains("delete_entries"));
    }

    #[test]
    fn test_build_delete_entries_request_parses_ids() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let playlist_id = PlaylistId::expect_positive(123);
        let media_id = MediaId::expect_positive(456);
        let playlist_public_id = codec_ok(codec.encode_playlist_id(playlist_id))?;
        let media_public_id = codec_ok(codec.encode_media_id(media_id))?;
        let (request, media_id_strings, playlist_id_strings) =
            api_ok(build_delete_entries_request(
                synctv_proto::client::DeleteEntriesRequest {
                    playlist_ids: vec![playlist_public_id.clone()],
                    media_ids: vec![media_public_id.clone()],
                    force: true,
                },
                &codec,
            ))?;

        assert_eq!(request.playlist_ids.len(), 1);
        assert_eq!(request.playlist_ids[0], playlist_id);
        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0], media_id);
        assert!(request.force);
        assert_eq!(media_id_strings, vec![media_public_id]);
        assert_eq!(playlist_id_strings, vec![playlist_public_id]);
        Ok(())
    }

    #[test]
    fn test_build_delete_media_request_rejects_invalid_media_id() {
        let err = require_error(build_delete_media_request(
            synctv_proto::client::DeleteMediaRequest {
                media_id: "bad-media".to_string(),
                force: false,
            },
        ));

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_delete_media_request_maps_to_delete_entries_request() -> TestResult {
        let media_id = codec_ok(
            synctv_adapter::PublicIdCodec::plain().encode_media_id(MediaId::expect_positive(123)),
        )?;
        let request = api_ok(build_delete_media_request(
            synctv_proto::client::DeleteMediaRequest {
                media_id: media_id.clone(),
                force: true,
            },
        ))?;

        assert!(request.playlist_ids.is_empty());
        assert_eq!(request.media_ids, vec![media_id]);
        assert!(request.force);
        Ok(())
    }

    #[test]
    fn test_build_edit_media_request_rejects_invalid_media_id() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let err = require_error(build_edit_media_request(
            synctv_proto::client::EditMediaRequest {
                media_id: "bad-media".to_string(),
                name: "Episode 1".to_string(),
                description: String::new(),
            },
            &codec,
        ));

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_edit_media_request_parses_title_and_id() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let media_id = MediaId::expect_positive(123);
        let request = api_ok(build_edit_media_request(
            synctv_proto::client::EditMediaRequest {
                media_id: codec_ok(codec.encode_media_id(media_id))?,
                name: "Episode 1".to_string(),
                description: String::new(),
            },
            &codec,
        ))?;

        assert_eq!(request.media_id, media_id);
        assert_eq!(request.name.as_deref(), Some("Episode 1"));
        Ok(())
    }

    fn make_stored_file() -> NewStoredFile {
        NewStoredFile {
            filename: None,
            id: "file-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "objects/file-1".to_string(),
            object_access: None,
            url: Some("/objects/file-1".to_string()),
            mime_type: Some("image/png".to_string()),
            size_bytes: Some(7),
            width: Some(16),
            height: Some(16),
            metadata: synctv_core::models::FileMetadata {
                upload_token: Some("v1.payload.signature".to_string()),
                ..Default::default()
            },
        }
    }

    fn make_upload_session() -> FileUploadSession {
        FileUploadSession {
            file: make_stored_file(),
            encoded_object_key: "encoded-file-1".to_string(),
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_object_access: None,
            upload_url: Some("https://upload.example.test/file-1".to_string()),
            upload_method: Some("PUT".to_string()),
            upload_headers: Default::default(),
            expires_at: Some(synctv_core::SystemClock.now()),
            max_size_bytes: 1024,
            resumable: true,
            part_size_bytes: 4 * 1024 * 1024,
            uploaded_size_bytes: 0,
            uploaded_parts: Vec::new(),
            upload_id: None,
            part_urls: Vec::new(),
        }
    }

    #[test]
    fn stored_file_proto_conversion_allows_missing_dimensions() -> TestResult {
        let file = make_stored_file();
        let proto = api_ok(stored_file_to_file_cover_proto(&file))?;
        assert_eq!(proto.mime_type, "image/png");
        assert_eq!(proto.size_bytes, 7);
        assert_eq!(proto.width, 16);
        assert_eq!(proto.height, 16);

        let mut missing_mime = file.clone();
        missing_mime.mime_type = None;
        assert!(matches!(
            stored_file_to_file_cover_proto(&missing_mime),
            Err(crate::impls::ApiError::Internal(message))
                if message.contains("mime_type is missing")
        ));

        let mut missing_size = file.clone();
        missing_size.size_bytes = None;
        assert!(matches!(
            stored_file_to_file_cover_proto(&missing_size),
            Err(crate::impls::ApiError::Internal(message))
                if message.contains("size_bytes is missing")
        ));

        let mut missing_width = file.clone();
        missing_width.width = None;
        let proto = api_ok(stored_file_to_file_cover_proto(&missing_width))?;
        assert_eq!(proto.width, 0);

        let mut missing_height = file.clone();
        missing_height.height = None;
        let proto = api_ok(stored_file_to_file_cover_proto(&missing_height))?;
        assert_eq!(proto.height, 0);

        let mut invalid_width = file.clone();
        invalid_width.width = Some(0);
        assert!(matches!(
            stored_file_to_file_cover_proto(&invalid_width),
            Err(crate::impls::ApiError::Internal(message))
                if message.contains("width is invalid")
        ));

        let mut invalid_height = file;
        invalid_height.height = Some(0);
        assert!(matches!(
            stored_file_to_file_cover_proto(&invalid_height),
            Err(crate::impls::ApiError::Internal(message))
                if message.contains("height is invalid")
        ));
        Ok(())
    }

    #[test]
    fn upload_session_fields_require_upload_metadata_when_upload_required() -> TestResult {
        let session = make_upload_session();
        let fields = api_ok(upload_session_fields(&session))?;
        assert_eq!(fields.upload_method.as_deref(), Some("PUT"));
        assert_eq!(
            fields.upload_url.as_deref(),
            Some("https://upload.example.test/file-1")
        );
        assert!(fields.expires_at.is_some_and(|expires_at| expires_at > 0));

        let mut missing_url = session;
        missing_url.upload_url = None;
        assert!(matches!(
            upload_session_fields(&missing_url),
            Err(crate::impls::ApiError::Internal(message)) if message.contains("upload_url")
        ));
        Ok(())
    }

    #[test]
    fn upload_session_fields_render_upload_url_from_object_access() -> TestResult {
        let mut session = make_upload_session();
        session.upload_url = None;
        session.upload_object_access = Some(synctv_core::models::FileObjectAccess {
            object_kind: synctv_core::models::FileObjectKind::MediaCover,
            encoded_object_key: "encoded-upload".to_string(),
            read_token: "read-token".to_string(),
        });

        let fields = api_ok(upload_session_fields(&session))?;

        assert_eq!(
            fields.upload_url.as_deref(),
            Some("/api/media/cover-objects/encoded-upload")
        );
        assert!(!fields
            .upload_url
            .as_deref()
            .is_some_and(|url| url.contains("token=")));
        Ok(())
    }

    #[test]
    fn upload_session_fields_accept_multipart_upload_targets() -> TestResult {
        let mut session = make_upload_session();
        session.upload_id = Some("upload-id".to_string());
        session
            .part_urls
            .push(synctv_core::models::FileUploadPartUrl {
                part_number: 1,
                offset_bytes: 0,
                size_bytes: 7,
                upload_url: "https://upload.example.test/file-1?partNumber=1".to_string(),
                upload_method: "PUT".to_string(),
                upload_headers: Default::default(),
                expires_at: Some(synctv_core::SystemClock.now()),
            });

        let fields = api_ok(upload_session_fields(&session))?;

        assert_eq!(
            fields.upload_url.as_deref(),
            Some("https://upload.example.test/file-1")
        );
        assert_eq!(fields.upload_method.as_deref(), Some("PUT"));
        assert!(fields.expires_at.is_some_and(|expires_at| expires_at > 0));
        Ok(())
    }

    #[test]
    fn upload_session_fields_require_ownership_proof_nonce_when_reused() -> TestResult {
        let mut session = make_upload_session();
        session.upload_required = false;
        session.file.url = None;
        session.upload_url = None;
        session.upload_method = None;
        session.expires_at = None;
        session.ownership_proof_required = true;
        session.ownership_proof_nonce = Some("nonce".to_string());
        session.ownership_proof_ranges = vec![FileOwnershipProofRange {
            offset: 0,
            length: 16,
        }];

        let fields = api_ok(upload_session_fields(&session))?;
        assert!(fields.upload_url.is_none());
        let session_proto = api_ok(file_upload_session_to_room_cover_proto(session.clone()))?;
        let cover = session_proto
            .cover_reference
            .ok_or_else(|| test_error("cover_reference should be returned"))?;
        assert_eq!(cover.id, "file-1");
        assert!(!session_proto.upload_token.is_empty());
        assert_eq!(fields.ownership_proof_nonce.as_deref(), Some("nonce"));

        session.ownership_proof_nonce = None;
        assert!(matches!(
            upload_session_fields(&session),
            Err(crate::impls::ApiError::Internal(message))
                if message.contains("ownership_proof_nonce")
        ));
        Ok(())
    }

    #[test]
    fn test_validate_dynamic_playlist_query_support_allows_search() -> TestResult {
        let playlist = make_playlist(
            "Dynamic Playlist",
            Some(synctv_core::models::SourceProvider::Alist),
            Some("alist-main"),
        );
        let supported = api_ok(validate_dynamic_playlist_query_support(
            &playlist,
            &synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist.id.to_string(),
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 20,
                search: "alpha".to_string(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
            },
        ))?;

        assert!(supported);
        Ok(())
    }

    #[test]
    fn dynamic_pagination_preserves_youtube_cursor() -> TestResult {
        let pagination = synctv_proto::client::list_playlist_items_request::Pagination::Cursor(
            synctv_proto::client::CursorPagination {
                cursor: "next-token%3D%3D".to_string(),
            },
        );

        let result = api_ok(dynamic_pagination_from_request(
            Some(&pagination),
            synctv_core::models::SourceProvider::Youtube,
            false,
        ))?;

        assert_eq!(
            result,
            DynamicPagination::Cursor {
                cursor: Some("next-token%3D%3D".to_string())
            }
        );
        Ok(())
    }

    #[test]
    fn dynamic_pagination_defaults_cloudreve_to_cursor() -> TestResult {
        let result = api_ok(dynamic_pagination_from_request(
            None,
            synctv_core::models::SourceProvider::Cloudreve,
            false,
        ))?;

        assert_eq!(result, DynamicPagination::Cursor { cursor: None });
        Ok(())
    }

    #[test]
    fn dynamic_pagination_preserves_page_for_page_providers() -> TestResult {
        let pagination = synctv_proto::client::list_playlist_items_request::Pagination::Page(
            synctv_proto::client::PagePagination { page: 3 },
        );

        let result = api_ok(dynamic_pagination_from_request(
            Some(&pagination),
            synctv_core::models::SourceProvider::Alist,
            false,
        ))?;

        assert_eq!(result, DynamicPagination::Page { page: 3 });
        Ok(())
    }

    #[test]
    fn test_require_dynamic_playlist_creator_allows_creator() -> TestResult {
        let playlist = make_playlist(
            "Dynamic Playlist",
            Some(synctv_core::models::SourceProvider::Alist),
            Some("alist-main"),
        );
        let creator_id = playlist
            .creator_id
            .ok_or_else(|| test_error("playlist should include creator id"))?;

        api_ok(require_dynamic_playlist_creator(&playlist, creator_id))?;
        Ok(())
    }

    #[test]
    fn test_require_dynamic_playlist_creator_rejects_other_user() {
        let playlist = make_playlist(
            "Dynamic Playlist",
            Some(synctv_core::models::SourceProvider::Alist),
            Some("alist-main"),
        );
        let err = require_error(require_dynamic_playlist_creator(
            &playlist,
            UserId::expect_positive(999),
        ));

        assert!(
            matches!(err, crate::impls::ApiError::Authorization(message) if message.contains("Only the playlist creator"))
        );
    }

    #[test]
    fn test_require_dynamic_playlist_creator_rejects_unowned_playlist() {
        let mut playlist = make_playlist(
            "Dynamic Playlist",
            Some(synctv_core::models::SourceProvider::Alist),
            Some("alist-main"),
        );
        playlist.creator_id = None;
        let err = require_error(require_dynamic_playlist_creator(
            &playlist,
            UserId::expect_positive(999),
        ));

        assert!(
            matches!(err, crate::impls::ApiError::Authorization(message) if message.contains("Only the playlist creator"))
        );
    }

    #[test]
    fn media_query_enum_mappers_reject_unknown_values_and_preserve_defaults() -> TestResult {
        assert_eq!(
            api_ok(map_sort_direction(
                synctv_proto::client::SortDirection::Unspecified as i32
            ))?,
            synctv_core::models::SortDirection::Asc
        );
        assert_eq!(
            api_ok(map_media_sort(
                synctv_proto::client::MediaListSortBy::Unspecified as i32
            ))?,
            synctv_core::models::MediaListSortBy::Position
        );
        assert_eq!(
            api_ok(map_playlist_sort_from_media_sort(
                synctv_proto::client::MediaListSortBy::Unspecified as i32
            ))?,
            synctv_core::models::PlaylistListSortBy::Position
        );
        assert_eq!(
            api_ok(map_availability_filter(
                synctv_proto::client::ResourceAvailabilityFilter::All as i32
            ))?,
            None
        );

        assert!(matches!(
            map_sort_direction(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("sort direction")
        ));
        assert!(matches!(
            map_media_sort(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("media list sort")
        ));
        assert!(matches!(
            map_availability_filter(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("availability")
        ));
        Ok(())
    }

    #[test]
    fn test_build_move_media_request_rejects_invalid_proto_payload() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let err = require_error(build_move_media_request(
            synctv_proto::client::MoveMediaRequest {
                media_ids: Vec::new(),
                source_playlist_id: Some("playlist-1".into()),
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some("media-before".into()),
                after_media_id: Some("media-after".into()),
            },
            &codec,
        ));

        assert!(
            err.to_string().contains("source_playlist_id")
                || err.to_string().contains("before_media_id")
        );
    }

    #[test]
    fn test_build_move_media_request_parses_ids() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let media_id = MediaId::expect_positive(123);
        let playlist_id = PlaylistId::expect_positive(456);
        let after_media_id = MediaId::expect_positive(789);
        let request = api_ok(build_move_media_request(
            synctv_proto::client::MoveMediaRequest {
                media_ids: vec![codec_ok(codec.encode_media_id(media_id))?],
                source_playlist_id: None,
                target_playlist_id: Some(codec_ok(codec.encode_playlist_id(playlist_id))?),
                all_from_scope: false,
                before_media_id: None,
                after_media_id: Some(codec_ok(codec.encode_media_id(after_media_id))?),
            },
            &codec,
        ))?;

        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0], media_id);
        assert_eq!(request.target_playlist_id, Some(playlist_id));
        assert_eq!(request.after_media_id, Some(after_media_id));
        Ok(())
    }
}
