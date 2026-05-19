//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use hex::encode as hex_encode;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use synctv_core::models::{
    MediaId, MediaListQuery as CoreMediaListQuery, MediaListSortBy as CoreMediaListSortBy,
    PermissionBits, Playlist, PlaylistListQuery as CorePlaylistListQuery,
    PlaylistListSortBy as CorePlaylistListSortBy, RoomId, SortDirection as CoreSortDirection,
    UserId,
};
use synctv_core::provider::DynamicListQuery;
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::media::AddMediaRequest as CoreAddMediaRequest;
use synctv_core::service::media::MoveMediaRequest as CoreMoveMediaRequest;
use synctv_core::service::room::{
    DeleteEntriesPlan, DeleteEntriesRequest as CoreDeleteEntriesRequest,
    MemberResourceCleanupResult, RealtimeOutboxDeleteEntriesEventFactory,
    RealtimeOutboxMemberResourceCleanupEventFactory,
};
use synctv_core::service::MediaService;

use super::convert::{
    media_list_to_proto_with_availability, media_to_proto_for_viewer, playlist_list_to_proto,
    playlist_path_node_to_proto, playlist_to_proto_for_viewer,
};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};
use crate::media_fanout::{MediaFanoutService, PreparedMediaRemovedFanout};
use crate::playlist_fanout::{PlaylistFanoutService, PreparedPlaylistDeletedFanout};
use crate::realtime_fanout::RealtimeFanoutService;

#[derive(Debug)]
struct AddMediaBatchBuildResult {
    items: Vec<synctv_core::service::media::AddMediaRequest>,
    playlist_id: Option<synctv_core::models::PlaylistId>,
}

pub enum MoveMediaFanoutStep {
    Updated { media_id: MediaId },
    RemovedAndAdded { media_id: MediaId },
}

pub(crate) struct PreparedDeleteEntriesOutboxFanout {
    media_fanout: Arc<dyn MediaFanoutService>,
    playlist_fanout: Arc<dyn PlaylistFanoutService>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    room_id: RoomId,
    user_id: UserId,
    username: String,
    events: Arc<std::sync::Mutex<Vec<PreparedDeleteEntriesEvent>>>,
}

enum PreparedDeleteEntriesEvent {
    MediaRemoved(PreparedMediaRemovedFanout),
    PlaylistDeleted(PreparedPlaylistDeletedFanout),
    KickPublisher(synctv_realtime::sync::RealtimeEvent),
}

impl PreparedDeleteEntriesOutboxFanout {
    #[must_use]
    pub(crate) fn outbox_factory(&self) -> RealtimeOutboxDeleteEntriesEventFactory {
        let media_fanout = self.media_fanout.clone();
        let playlist_fanout = self.playlist_fanout.clone();
        let realtime_fanout = self.realtime_fanout.clone();
        let room_id = self.room_id;
        let user_id = self.user_id;
        let username = self.username.clone();
        let events = self.events.clone();
        Arc::new(move |plan: &DeleteEntriesPlan| {
            let mut prepared_events = Vec::with_capacity(
                plan.deleted_media_ids.len() * 2 + plan.deleted_playlist_ids.len(),
            );
            let mut outbox_events: Vec<NewRealtimeOutboxEvent> = Vec::with_capacity(
                plan.deleted_media_ids.len() * 2 + plan.deleted_playlist_ids.len(),
            );

            for media_id in &plan.deleted_media_ids {
                let prepared = media_fanout
                    .prepare_removed_outbox_fanout(&room_id, &user_id, &username, media_id);
                if let Some(outbox_event) = prepared.cloned_outbox_event() {
                    outbox_events.push(outbox_event);
                }
                prepared_events.push(PreparedDeleteEntriesEvent::MediaRemoved(prepared));

                let kick_event = synctv_realtime::sync::RealtimeEvent::KickPublisher {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    media_id: *media_id,
                    reason: "media_deleted".to_string(),
                    timestamp: chrono::Utc::now(),
                };
                if let Some(outbox_event) = realtime_fanout.outbox_event(&kick_event) {
                    outbox_events.push(outbox_event);
                }
                prepared_events.push(PreparedDeleteEntriesEvent::KickPublisher(kick_event));
            }

            for playlist_id in &plan.deleted_playlist_ids {
                let prepared = playlist_fanout.prepare_deleted_outbox_fanout(
                    &room_id,
                    &user_id,
                    &username,
                    playlist_id,
                );
                if let Some(outbox_event) = prepared.cloned_outbox_event() {
                    outbox_events.push(outbox_event);
                }
                prepared_events.push(PreparedDeleteEntriesEvent::PlaylistDeleted(prepared));
            }

            *events
                .lock()
                .expect("delete entries fanout events mutex should not be poisoned") =
                prepared_events;
            outbox_events
        })
    }

    #[must_use]
    pub(crate) fn member_cleanup_outbox_factory(
        &self,
    ) -> RealtimeOutboxMemberResourceCleanupEventFactory {
        let factory = self.outbox_factory();
        Arc::new(move |cleanup: &MemberResourceCleanupResult| {
            let plan = DeleteEntriesPlan {
                deleted_playlist_ids: cleanup.deleted_playlist_ids.clone(),
                deleted_media_ids: cleanup.deleted_media_ids.clone(),
                playback_reset: cleanup.playback_reset,
            };
            factory(&plan)
        })
    }

    pub(crate) fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(
            &mut *self
                .events
                .lock()
                .expect("delete entries fanout events mutex should not be poisoned"),
        );
        for event in events {
            match event {
                PreparedDeleteEntriesEvent::MediaRemoved(event) => {
                    event.publish_after_outbox_commit();
                }
                PreparedDeleteEntriesEvent::PlaylistDeleted(event) => {
                    event.publish_after_outbox_commit();
                }
                PreparedDeleteEntriesEvent::KickPublisher(event) => {
                    self.realtime_fanout.publish_after_outbox_commit(event);
                }
            }
        }
    }
}

pub(crate) fn prepare_delete_entries_outbox_fanout(
    media_fanout: Arc<dyn MediaFanoutService>,
    playlist_fanout: Arc<dyn PlaylistFanoutService>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    room_id: RoomId,
    user_id: UserId,
    username: String,
) -> PreparedDeleteEntriesOutboxFanout {
    PreparedDeleteEntriesOutboxFanout {
        media_fanout,
        playlist_fanout,
        realtime_fanout,
        room_id,
        user_id,
        username,
        events: Arc::new(std::sync::Mutex::new(Vec::new())),
    }
}

pub enum MoveMediaFanoutPlan {
    None,
    Reordered,
    PerMedia(Vec<MoveMediaFanoutStep>),
}
const DEFAULT_MEDIA_TITLE: &str = "Unknown";

fn finalize_playlist_items_response_version(
    mut response: crate::proto::client::ListPlaylistItemsResponse,
) -> crate::proto::client::ListPlaylistItemsResponse {
    response.version = compute_playlist_items_response_version(&response);
    response
}

fn hash_proto_message<M: prost::Message>(hasher: &mut Sha256, message: &M) {
    let encoded = message.encode_to_vec();
    hasher.update(
        u64::try_from(encoded.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(encoded);
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(crate) fn compute_playlist_items_response_version(
    response: &crate::proto::client::ListPlaylistItemsResponse,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"playlist-items-snapshot-v1");
    hasher.update(response.total.to_le_bytes());
    hasher.update(response.folder_count.to_le_bytes());
    hasher.update(response.file_count.to_le_bytes());

    for playlist in &response.playlists {
        hash_proto_message(&mut hasher, playlist);
    }
    for media in &response.media {
        hash_proto_message(&mut hasher, media);
    }
    for item in &response.dynamic_items {
        hash_string(&mut hasher, &item.name);
        hasher.update(item.item_type.to_le_bytes());
        hash_bytes(&mut hasher, &item.target);
        hash_optional_i64(&mut hasher, item.size);
        match item.thumbnail.as_deref() {
            Some(thumbnail) => {
                hasher.update([1]);
                hash_string(&mut hasher, thumbnail);
            }
            None => hasher.update([0]),
        }
        hash_optional_i64(&mut hasher, item.modified_at);
    }
    for node in &response.current_path {
        hash_proto_message(&mut hasher, node);
    }

    hex_encode(hasher.finalize())
}

fn page_i32_to_usize(value: i32) -> usize {
    usize::try_from(value.max(1)).unwrap_or(usize::MAX)
}

fn i64_to_usize_saturating(value: i64) -> usize {
    if value.is_negative() {
        0
    } else {
        usize::try_from(value).unwrap_or(usize::MAX)
    }
}

fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn i64_to_i32_saturating(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

pub(crate) async fn build_move_media_fanout_plan(
    media_service: &MediaService,
    room_id: &RoomId,
    request: &CoreMoveMediaRequest,
) -> Result<MoveMediaFanoutPlan, ApiError> {
    let original_media = if request.all_from_scope {
        match request.source_playlist_id.as_ref() {
            Some(playlist_id) => media_service
                .get_room_playlist_media(room_id, playlist_id)
                .await
                .map_err(ApiError::from)?,
            None => media_service
                .get_room_root_media(room_id)
                .await
                .map_err(ApiError::from)?,
        }
    } else {
        media_service
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

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn map_sort_direction(sort_direction: i32) -> CoreSortDirection {
    match crate::proto::client::SortDirection::try_from(sort_direction) {
        Ok(crate::proto::client::SortDirection::Desc) => CoreSortDirection::Desc,
        _ => CoreSortDirection::Asc,
    }
}

fn map_playlist_sort_from_media_sort(sort_by: i32) -> CorePlaylistListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CorePlaylistListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CorePlaylistListSortBy::CreatedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CorePlaylistListSortBy::UpdatedAt,
        _ => CorePlaylistListSortBy::Position,
    }
}

fn map_media_sort(sort_by: i32) -> CoreMediaListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CoreMediaListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CoreMediaListSortBy::AddedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CoreMediaListSortBy::UpdatedAt,
        crate::proto::client::MediaListSortBy::SourceProvider => {
            CoreMediaListSortBy::SourceProvider
        }
        crate::proto::client::MediaListSortBy::ProviderInstanceName => {
            CoreMediaListSortBy::ProviderInstanceName
        }
        _ => CoreMediaListSortBy::Position,
    }
}

fn map_availability_filter(filter: i32) -> Option<bool> {
    match crate::proto::client::ResourceAvailabilityFilter::try_from(filter)
        .unwrap_or(crate::proto::client::ResourceAvailabilityFilter::All)
    {
        crate::proto::client::ResourceAvailabilityFilter::All => None,
        crate::proto::client::ResourceAvailabilityFilter::Available => Some(true),
        crate::proto::client::ResourceAvailabilityFilter::Unavailable => Some(false),
    }
}

pub(crate) fn validate_dynamic_playlist_query_support(
    playlist: &Playlist,
    req: &crate::proto::client::ListPlaylistItemsRequest,
) -> Result<bool, ApiError> {
    if let Some(source_provider) = normalize_non_empty_filter(&req.source_provider) {
        if playlist.source_provider.as_deref() != Some(source_provider.as_str()) {
            return Ok(false);
        }
    }
    if let Some(provider_instance_name) = normalize_non_empty_filter(&req.provider_instance_name) {
        if playlist.provider_instance_name.as_deref() != Some(provider_instance_name.as_str()) {
            return Ok(false);
        }
    }

    let sort_by = crate::proto::client::MediaListSortBy::try_from(req.sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position);
    let sort_direction = crate::proto::client::SortDirection::try_from(req.sort_direction)
        .unwrap_or(crate::proto::client::SortDirection::Asc);
    let allows_default_sort = matches!(
        sort_by,
        crate::proto::client::MediaListSortBy::Position
            | crate::proto::client::MediaListSortBy::Unspecified
    ) && matches!(
        sort_direction,
        crate::proto::client::SortDirection::Asc | crate::proto::client::SortDirection::Unspecified
    );
    if !allows_default_sort {
        return Err(ApiError::InvalidInput(
            "dynamic playlist browsing does not support custom sorting yet".to_string(),
        ));
    }

    Ok(true)
}

pub(crate) fn build_move_media_request(
    req: crate::proto::client::MoveMediaRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<CoreMoveMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::MoveMediaRequest {
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

pub(crate) fn build_add_media_request(
    req: crate::proto::client::AddMediaRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<CoreAddMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::AddMediaRequest {
        playlist_id,
        source_provider,
        provider_instance_name,
        source_config,
        name,
    } = req;

    let playlist_id = playlist_id
        .map(|id| crate::impls::proto_validated_playlist_id(id, public_id_codec))
        .transpose()?;

    let source_config: serde_json::Value = if source_config.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&source_config)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?
    };

    let name = if name.is_empty() {
        DEFAULT_MEDIA_TITLE.to_string()
    } else {
        crate::impls::validation::validate_media_name(&name)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media name: {e}")))?
    };

    let provider_instance_name = normalize_non_empty_filter(&provider_instance_name);

    Ok(CoreAddMediaRequest {
        playlist_id,
        name,
        source_provider,
        provider_instance_name,
        source_config,
    })
}

pub(crate) fn build_delete_entries_request(
    req: crate::proto::client::DeleteEntriesRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<(CoreDeleteEntriesRequest, Vec<String>, Vec<String>), ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::DeleteEntriesRequest {
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
    req: crate::proto::client::AddMediaBatchRequest,
    public_id_codec: &crate::PublicIdCodec,
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

pub(crate) fn build_delete_media_request(
    req: crate::proto::client::DeleteMediaRequest,
) -> Result<crate::proto::client::DeleteEntriesRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(crate::proto::client::DeleteEntriesRequest {
        playlist_ids: Vec::new(),
        media_ids: vec![req.media_id],
        force: req.force,
    })
}

pub(crate) fn build_edit_media_request(
    req: crate::proto::client::EditMediaRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::service::media::EditMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    let name = if req.name.is_empty() {
        None
    } else {
        Some(
            crate::impls::validation::validate_media_name(&req.name)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media name: {e}")))?,
        )
    };

    Ok(synctv_core::service::media::EditMediaRequest {
        media_id: crate::impls::proto_validated_media_id(req.media_id, public_id_codec)?,
        name,
    })
}

impl ClientApiImpl {
    pub async fn add_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_add_media_request(req, &self.public_id_codec)?;
        let playlist_id = service_req.playlist_id;

        // Check total playlist size limit before adding
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            self.room_service
                .media_service()
                .count_room_playlist_media(&rid, playlist_id)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
        };
        if existing_count >= Self::MAX_PLAYLIST_SIZE {
            return Err(ApiError::InvalidInput(format!(
                "Playlist has reached maximum size of {} items",
                Self::MAX_PLAYLIST_SIZE
            )));
        }

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
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
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        Ok(crate::proto::client::AddMediaResponse {
            media: Some(media_to_proto_for_viewer(
                &media,
                true,
                Some(uid),
                &self.public_id_codec,
            )),
        })
    }

    pub async fn delete_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::DeleteMediaRequest,
    ) -> Result<crate::proto::client::DeleteMediaResponse, ApiError> {
        self.delete_entries(user_id, room_id, build_delete_media_request(req)?)
            .await?;

        Ok(crate::proto::client::DeleteMediaResponse { success: true })
    }

    /// Delete a mixed set of playlist and media entries.
    pub async fn delete_entries(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::DeleteEntriesRequest,
    ) -> Result<crate::proto::client::DeleteEntriesResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let (service_req, _explicit_media_ids, _explicit_playlist_ids) =
            build_delete_entries_request(req, &self.public_id_codec)?;
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let prepared_outbox_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            uid,
            username.clone(),
        );
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

        Ok(crate::proto::client::DeleteEntriesResponse {
            deleted_playlists: usize_to_i32_saturating(result.deleted_playlists),
            deleted_media: usize_to_i32_saturating(result.deleted_media),
        })
    }

    /// Edit media metadata
    pub async fn edit_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::EditMediaRequest,
    ) -> Result<crate::proto::client::EditMediaResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_edit_media_request(req, &self.public_id_codec)?;

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
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
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        // Invalidate room cache on other replicas so they see updated metadata
        self.room_cache_fanout.publish_invalidation(&rid);

        Ok(crate::proto::client::EditMediaResponse {
            media: Some(media_to_proto_for_viewer(
                &media,
                true,
                Some(uid),
                &self.public_id_codec,
            )),
        })
    }

    /// Clear all media directly under the room root
    pub async fn clear_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::ClearPlaylistResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Check permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::CLEAR_MEDIA_RESOURCES,
            )
            .await
            .map_err(ApiError::from)?;

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let prepared_outbox_fanout = self
            .media_fanout
            .prepare_removed_batch_outbox_fanout(rid, uid, username);
        let result = self
            .room_service
            .clear_playlist_with_outbox(rid, uid, Some(prepared_outbox_fanout.outbox_factory()))
            .await
            .map_err(ApiError::from)?;

        // Broadcast a single MediaRemovedBatch event instead of N individual
        // events. The distributed copy is written transactionally by core.
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

        Ok(crate::proto::client::ClearPlaylistResponse {
            success: true,
            deleted_count: i64_to_i32_saturating(result.deleted_count),
        })
    }

    /// Maximum total media items allowed in a single room's playlist.
    ///
    /// Prevents unbounded playlist growth which could degrade database
    /// performance and client rendering.
    pub const MAX_PLAYLIST_SIZE: usize = 1000;

    /// Add multiple media items in a batch (atomic - all succeed or all fail)
    pub async fn add_media_batch(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::AddMediaBatchRequest,
    ) -> Result<crate::proto::client::AddMediaBatchResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let AddMediaBatchBuildResult { items, playlist_id } =
            build_add_media_batch_request(req, &self.public_id_codec)?;
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            self.room_service
                .media_service()
                .count_room_playlist_media(&rid, playlist_id)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
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

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
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

        let results = media_list
            .into_iter()
            .map(|media| crate::proto::client::AddMediaResponse {
                media: Some(media_to_proto_for_viewer(
                    &media,
                    true,
                    Some(uid),
                    &self.public_id_codec,
                )),
            })
            .collect();

        Ok(crate::proto::client::AddMediaBatchResponse { results })
    }

    pub async fn move_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::MoveMediaRequest,
    ) -> Result<crate::proto::client::MoveMediaResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_move_media_request(req, &self.public_id_codec)?;

        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::REORDER_MEDIA_RESOURCES,
            )
            .await
            .map_err(Self::map_room_access_error)?;
        let media_fanout_plan =
            build_move_media_fanout_plan(self.room_service.media_service(), &rid, &service_req)
                .await?;

        let actor_username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|user| user.username)
            .unwrap_or_default();
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

        Ok(crate::proto::client::MoveMediaResponse {
            moved_count: usize_to_i32_saturating(media.len()),
            media: media_list_to_proto_with_availability(&media, |media| {
                media_to_proto_for_viewer(media, true, Some(uid), &self.public_id_codec)
            }),
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
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.list_playlist_items_for_actor(&actor, req).await
    }

    pub async fn list_playlist_items_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.list_playlist_items_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn list_playlist_items_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, PermissionBits::VIEW_MEDIA_RESOURCES)
            .await?;
        let rid = actor.room_id();
        let viewer_id = actor.user_id();
        let Some(playlist_id) = (if req.playlist_id.is_empty() {
            None
        } else {
            Some(crate::impls::proto_validated_playlist_id(
                req.playlist_id.clone(),
                &self.public_id_codec,
            )?)
        }) else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing the room root".to_string(),
                ));
            }
            let playlist_query = CorePlaylistListQuery {
                pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability: map_availability_filter(req.availability),
                sort_by: map_playlist_sort_from_media_sort(req.sort_by),
                sort_direction: map_sort_direction(req.sort_direction),
            };
            let media_query = CoreMediaListQuery {
                pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                availability: map_availability_filter(req.availability),
                sort_by: map_media_sort(req.sort_by),
                sort_direction: map_sort_direction(req.sort_direction),
            };
            let folder_count = self
                .room_service
                .count_client_playlists(&rid, None, &playlist_query)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?;
            let file_count = self
                .room_service
                .count_client_media(&rid, None, &media_query)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?;
            let total = folder_count + file_count;
            let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100);
            let skip = (page_i32_to_usize(req.page) - 1) * page_size;
            let (playlists, media) = if skip < folder_count {
                let playlists = self
                    .room_service
                    .list_client_playlists(
                        &rid,
                        None,
                        &playlist_query,
                        usize_to_i64_saturating(page_size),
                        usize_to_i64_saturating(skip),
                    )
                    .await
                    .map_err(ApiError::from)?;
                let remaining = page_size.saturating_sub(playlists.len());
                let media = if remaining > 0 {
                    self.room_service
                        .list_client_media(
                            &rid,
                            None,
                            &media_query,
                            usize_to_i64_saturating(remaining),
                            0,
                        )
                        .await
                        .map_err(ApiError::from)?
                } else {
                    Vec::new()
                };
                (playlists, media)
            } else {
                let media_skip = skip - folder_count;
                let media = self
                    .room_service
                    .list_client_media(
                        &rid,
                        None,
                        &media_query,
                        usize_to_i64_saturating(page_size),
                        usize_to_i64_saturating(media_skip),
                    )
                    .await
                    .map_err(ApiError::from)?;
                (Vec::new(), media)
            };
            let folder_ids: Vec<synctv_core::models::PlaylistId> =
                playlists.iter().map(|pl| pl.playlist.id).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&folder_ids)
                .await
                .unwrap_or_default();
            let proto_playlists = playlist_list_to_proto(&playlists, |entry| {
                let item_count =
                    i64_to_i32_saturating(counts.get(&entry.playlist.id).copied().unwrap_or(0));
                playlist_to_proto_for_viewer(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                    viewer_id,
                    &self.public_id_codec,
                )
            });
            let proto_media = media_list_to_proto_with_availability(&media, |entry| {
                media_to_proto_for_viewer(
                    &entry.media,
                    entry.is_available,
                    viewer_id,
                    &self.public_id_codec,
                )
            });

            return Ok(finalize_playlist_items_response_version(
                crate::proto::client::ListPlaylistItemsResponse {
                    playlists: proto_playlists,
                    media: proto_media,
                    total: usize_to_i32_saturating(total),
                    folder_count: usize_to_i32_saturating(folder_count),
                    file_count: usize_to_i32_saturating(file_count),
                    dynamic_items: Vec::new(),
                    current_path: Vec::new(),
                    version: String::new(),
                },
            ));
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
        let mut current_path: Vec<crate::proto::client::PlaylistBrowsePathNode> = static_path
            .iter()
            .map(|playlist| playlist_path_node_to_proto(playlist, &self.public_id_codec))
            .collect();

        if playlist.is_dynamic() {
            let Some(uid) = actor.user_id() else {
                return Err(ApiError::Authorization(
                    "Guests cannot browse dynamic provider playlists".to_string(),
                ));
            };
            self.room_service
                .ensure_client_usable_playlist(&playlist)
                .await
                .map_err(ApiError::from)?;
            if !validate_dynamic_playlist_query_support(&playlist, &req)? {
                return Ok(finalize_playlist_items_response_version(
                    crate::proto::client::ListPlaylistItemsResponse {
                        playlists: Vec::new(),
                        media: Vec::new(),
                        total: 0,
                        folder_count: 0,
                        file_count: 0,
                        dynamic_items: Vec::new(),
                        current_path,
                        version: String::new(),
                    },
                ));
            }

            let page = page_i32_to_usize(req.page);
            let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100);
            let items = self
                .room_service
                .media_service()
                .list_dynamic_playlist_items(
                    rid,
                    uid,
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                    DynamicListQuery {
                        page,
                        page_size,
                        search: normalize_non_empty_filter(&req.search),
                        refresh: req.refresh,
                    },
                )
                .await
                .map_err(ApiError::from)?;

            // Convert provider DirectoryItem to proto PlaylistItem
            let dynamic_items: Vec<_> = items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::ItemType;
                    let item_type = match item.item_type {
                        ItemType::Playlist => crate::proto::client::ItemType::Playlist as i32,
                        ItemType::Media => crate::proto::client::ItemType::Media as i32,
                    };
                    let thumbnail = match item.thumbnail {
                        Some(thumbnail) => Some(
                            crate::http::providers::emby::sign_emby_thumbnail_url(
                                &thumbnail,
                                &self
                                    .public_id_codec
                                    .encode_room_id(rid)
                                    .expect("positive room id must encode as public ID"),
                                &self
                                    .public_id_codec
                                    .encode_user_id(uid)
                                    .expect("positive user id must encode as public ID"),
                                self.signing_key.as_deref(),
                            )
                            .map_err(ApiError::Internal)?,
                        ),
                        None => None,
                    };

                    Ok(crate::proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        target: item.target,
                        size: item.size.map(u64_to_i64_saturating),
                        thumbnail: Some(thumbnail.unwrap_or_default()),
                        modified_at: Some(item.modified_at.unwrap_or(0)),
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;

            let browse_path = self
                .room_service
                .media_service()
                .get_dynamic_playlist_browse_path(
                    rid,
                    uid,
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                )
                .await
                .map_err(ApiError::from)?;
            current_path.extend(browse_path.into_iter().map(|segment| {
                crate::proto::client::PlaylistBrowsePathNode {
                    playlist_id: String::new(),
                    name: segment.name,
                    target: segment.target,
                }
            }));

            // Dynamic playlists don't provide a reliable total count since the
            // provider may paginate server-side.  Use -1 to signal "unknown total"
            // so the client knows to use has_more / next-page semantics.
            let total: i32 = -1;

            return Ok(finalize_playlist_items_response_version(
                crate::proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total,
                    folder_count: 0,
                    file_count: 0,
                    dynamic_items,
                    current_path,
                    version: String::new(),
                },
            ));
        }

        if !req.target.is_empty() {
            return Err(ApiError::InvalidInput(
                "target must be empty when browsing a static playlist".to_string(),
            ));
        }

        let playlist_query = CorePlaylistListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability: map_availability_filter(req.availability),
            sort_by: map_playlist_sort_from_media_sort(req.sort_by),
            sort_direction: map_sort_direction(req.sort_direction),
        };
        let media_query = CoreMediaListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            availability: map_availability_filter(req.availability),
            sort_by: map_media_sort(req.sort_by),
            sort_direction: map_sort_direction(req.sort_direction),
        };
        let folder_count = self
            .room_service
            .count_client_playlists(&rid, Some(&playlist_id), &playlist_query)
            .await
            .map_err(ApiError::from)
            .map(i64_to_usize_saturating)?;
        let file_count = self
            .room_service
            .count_client_media(&rid, Some(&playlist_id), &media_query)
            .await
            .map_err(ApiError::from)
            .map(i64_to_usize_saturating)?;
        let total = folder_count + file_count;
        let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100);
        let skip = (page_i32_to_usize(req.page) - 1) * page_size;
        let (playlists, media) = if skip < folder_count {
            let playlists = self
                .room_service
                .list_client_playlists(
                    &rid,
                    Some(&playlist_id),
                    &playlist_query,
                    usize_to_i64_saturating(page_size),
                    usize_to_i64_saturating(skip),
                )
                .await
                .map_err(ApiError::from)?;
            let remaining = page_size.saturating_sub(playlists.len());
            let media = if remaining > 0 {
                self.room_service
                    .list_client_media(
                        &rid,
                        Some(&playlist_id),
                        &media_query,
                        usize_to_i64_saturating(remaining),
                        0,
                    )
                    .await
                    .map_err(ApiError::from)?
            } else {
                Vec::new()
            };
            (playlists, media)
        } else {
            let media_skip = skip - folder_count;
            let media = self
                .room_service
                .list_client_media(
                    &rid,
                    Some(&playlist_id),
                    &media_query,
                    usize_to_i64_saturating(page_size),
                    usize_to_i64_saturating(media_skip),
                )
                .await
                .map_err(ApiError::from)?;
            (Vec::new(), media)
        };
        let folder_ids: Vec<synctv_core::models::PlaylistId> =
            playlists.iter().map(|pl| pl.playlist.id).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&folder_ids)
            .await
            .unwrap_or_default();
        let proto_playlists = playlist_list_to_proto(&playlists, |entry| {
            let item_count =
                i64_to_i32_saturating(counts.get(&entry.playlist.id).copied().unwrap_or(0));
            playlist_to_proto_for_viewer(
                &entry.playlist,
                item_count,
                entry.is_available,
                viewer_id,
                &self.public_id_codec,
            )
        });
        let proto_media = media_list_to_proto_with_availability(&media, |entry| {
            media_to_proto_for_viewer(
                &entry.media,
                entry.is_available,
                viewer_id,
                &self.public_id_codec,
            )
        });

        Ok(finalize_playlist_items_response_version(
            crate::proto::client::ListPlaylistItemsResponse {
                playlists: proto_playlists,
                media: proto_media,
                total: usize_to_i32_saturating(total),
                folder_count: usize_to_i32_saturating(folder_count),
                file_count: usize_to_i32_saturating(file_count),
                dynamic_items: Vec::new(),
                current_path,
                version: String::new(),
            },
        ))
    }

    /// Get a single media record from database
    pub async fn get_media(
        &self,
        user_id: &UserId,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_media_for_actor(&actor, media_id).await
    }

    pub async fn get_media_as_guest(
        &self,
        access: &GuestRoomAccess,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        self.get_media_for_actor(&RoomActor::Guest(access.clone()), media_id)
            .await
    }

    pub async fn get_media_for_actor(
        &self,
        actor: &RoomActor,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        let rid = actor.room_id();
        let mid = crate::impls::parse_media_id_param(media_id, "media_id", &self.public_id_codec)?;
        self.require_room_permission(actor, PermissionBits::VIEW_MEDIA_RESOURCES)
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

        Ok(media_to_proto_for_viewer(
            &media,
            availability.is_available(),
            actor.user_id(),
            &self.public_id_codec,
        ))
    }
}

#[async_trait::async_trait]
impl crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService for ClientApiImpl {
    async fn get_playlist_items_snapshot(
        &self,
        actor: &crate::impls::client::RoomActor,
        req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, crate::impls::ApiError> {
        self.list_playlist_items_for_actor(actor, req.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_add_media_batch_request, build_add_media_request, build_delete_entries_request,
        build_delete_media_request, build_edit_media_request, build_move_media_request,
        compute_playlist_items_response_version, validate_dynamic_playlist_query_support,
        DEFAULT_MEDIA_TITLE,
    };
    use chrono::Utc;
    use serde_json::json;
    use synctv_core::models::{MediaId, Playlist, PlaylistId, RoomId, UserId};

    fn make_playlist(
        name: &str,
        source_provider: Option<&str>,
        provider_instance_name: Option<&str>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: name.to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: source_provider.map(str::to_string),
            source_config: Some(json!({})),
            provider_instance_name: provider_instance_name.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_playlist_items_response_version_changes_when_only_thumbnail_url_changes() {
        let make_response = |thumbnail: &str| crate::proto::client::ListPlaylistItemsResponse {
            playlists: Vec::new(),
            media: Vec::new(),
            total: 1,
            folder_count: 0,
            file_count: 1,
            dynamic_items: vec![crate::proto::client::PlaylistItem {
                name: "Episode 1".to_string(),
                item_type: crate::proto::client::ItemType::Media as i32,
                target: br#"{"path":"/tv/episode-1"}"#.to_vec(),
                size: Some(123),
                thumbnail: Some(thumbnail.to_string()),
                modified_at: Some(456),
            }],
            current_path: Vec::new(),
            version: String::new(),
        };

        let original = compute_playlist_items_response_version(&make_response(
            "https://cdn.example.com/thumb-a.jpg",
        ));
        let changed = compute_playlist_items_response_version(&make_response(
            "https://cdn.example.com/thumb-b.jpg",
        ));

        assert_ne!(
            original, changed,
            "thumbnail-only changes must invalidate playlist item snapshots"
        );
    }

    #[test]
    fn test_build_add_media_request_requires_source_provider() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_add_media_request(
            crate::proto::client::AddMediaRequest {
                playlist_id: None,
                source_provider: String::new(),
                provider_instance_name: String::new(),
                source_config: br#"{"path":"/tv"}"#.to_vec(),
                name: String::new(),
            },
            &codec,
        )
        .unwrap_err();

        assert!(err.to_string().contains("source_provider"));
    }

    #[test]
    fn test_build_add_media_request_parses_dynamic_payload() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let playlist_id = PlaylistId::expect_positive(123);
        let request = build_add_media_request(
            crate::proto::client::AddMediaRequest {
                playlist_id: Some(codec.encode_playlist_id(playlist_id).unwrap()),
                source_provider: "alist".into(),
                provider_instance_name: "alist-main".into(),
                source_config: br#"{"path":"/tv"}"#.to_vec(),
                name: "Episode 1".into(),
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.playlist_id, Some(playlist_id));
        assert_eq!(request.name, "Episode 1");
        assert_eq!(request.source_provider, "alist");
        assert_eq!(
            request.provider_instance_name.as_deref(),
            Some("alist-main")
        );
        assert_eq!(request.source_config, serde_json::json!({"path":"/tv"}));
    }

    #[test]
    fn test_build_add_media_request_maps_empty_provider_instance_to_none() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let request = build_add_media_request(
            crate::proto::client::AddMediaRequest {
                playlist_id: None,
                source_provider: "direct_url".into(),
                provider_instance_name: String::new(),
                source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
                name: "Example".into(),
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.source_provider, "direct_url");
        assert!(request.provider_instance_name.is_none());
    }

    #[test]
    fn test_build_add_media_request_does_not_infer_title_from_source_config() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let request = build_add_media_request(
            crate::proto::client::AddMediaRequest {
                playlist_id: None,
                source_provider: "alist".into(),
                provider_instance_name: "alist-main".into(),
                source_config: br#"{"url":"https://example.com/video.mp4","path":"/tv"}"#.to_vec(),
                name: String::new(),
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.name, DEFAULT_MEDIA_TITLE);
    }

    #[test]
    fn test_build_add_media_batch_request_rejects_invalid_nested_playlist_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_add_media_batch_request(
            crate::proto::client::AddMediaBatchRequest {
                items: vec![crate::proto::client::AddMediaRequest {
                    playlist_id: Some("bad-playlist".into()),
                    source_provider: "alist".into(),
                    provider_instance_name: "alist-main".into(),
                    source_config: br#"{"path":"/tv"}"#.to_vec(),
                    name: "Episode 1".into(),
                }],
            },
            &codec,
        )
        .unwrap_err();

        assert!(err.to_string().contains("playlist_id"));
    }

    #[test]
    fn test_build_add_media_batch_request_reuses_single_item_builder_semantics() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let playlist_id = PlaylistId::expect_positive(123);
        let result = build_add_media_batch_request(
            crate::proto::client::AddMediaBatchRequest {
                items: vec![crate::proto::client::AddMediaRequest {
                    playlist_id: Some(codec.encode_playlist_id(playlist_id).unwrap()),
                    source_provider: "alist".into(),
                    provider_instance_name: "alist-main".into(),
                    source_config: br#"{"url":"https://example.com/video.mp4","path":"/tv"}"#
                        .to_vec(),
                    name: String::new(),
                }],
            },
            &codec,
        )
        .unwrap();

        assert_eq!(result.playlist_id, Some(playlist_id));
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, DEFAULT_MEDIA_TITLE);
        assert_eq!(
            result.items[0].source_config,
            serde_json::json!({"url":"https://example.com/video.mp4","path":"/tv"})
        );
    }

    #[test]
    fn test_build_delete_entries_request_rejects_empty_target_set() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_delete_entries_request(
            crate::proto::client::DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: Vec::new(),
                force: false,
            },
            &codec,
        )
        .unwrap_err();

        assert!(err.to_string().contains("delete_entries"));
    }

    #[test]
    fn test_build_delete_entries_request_parses_ids() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let playlist_id = PlaylistId::expect_positive(123);
        let media_id = MediaId::expect_positive(456);
        let playlist_public_id = codec.encode_playlist_id(playlist_id).unwrap();
        let media_public_id = codec.encode_media_id(media_id).unwrap();
        let (request, media_id_strings, playlist_id_strings) = build_delete_entries_request(
            crate::proto::client::DeleteEntriesRequest {
                playlist_ids: vec![playlist_public_id.clone()],
                media_ids: vec![media_public_id.clone()],
                force: true,
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.playlist_ids.len(), 1);
        assert_eq!(request.playlist_ids[0], playlist_id);
        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0], media_id);
        assert!(request.force);
        assert_eq!(media_id_strings, vec![media_public_id]);
        assert_eq!(playlist_id_strings, vec![playlist_public_id]);
    }

    #[test]
    fn test_build_delete_media_request_rejects_invalid_media_id() {
        let err = build_delete_media_request(crate::proto::client::DeleteMediaRequest {
            media_id: "bad-media".to_string(),
            force: false,
        })
        .unwrap_err();

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_delete_media_request_maps_to_delete_entries_request() {
        let media_id = crate::PublicIdCodec::default_for_tests()
            .encode_media_id(MediaId::expect_positive(123))
            .unwrap();
        let request = build_delete_media_request(crate::proto::client::DeleteMediaRequest {
            media_id: media_id.clone(),
            force: true,
        })
        .unwrap();

        assert!(request.playlist_ids.is_empty());
        assert_eq!(request.media_ids, vec![media_id]);
        assert!(request.force);
    }

    #[test]
    fn test_build_edit_media_request_rejects_invalid_media_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_edit_media_request(
            crate::proto::client::EditMediaRequest {
                media_id: "bad-media".to_string(),
                name: "Episode 1".to_string(),
            },
            &codec,
        )
        .unwrap_err();

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_edit_media_request_parses_title_and_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let media_id = MediaId::expect_positive(123);
        let request = build_edit_media_request(
            crate::proto::client::EditMediaRequest {
                media_id: codec.encode_media_id(media_id).unwrap(),
                name: "Episode 1".to_string(),
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.media_id, media_id);
        assert_eq!(request.name.as_deref(), Some("Episode 1"));
    }

    #[test]
    fn test_validate_dynamic_playlist_query_support_allows_search() {
        let playlist = make_playlist("Dynamic Folder", Some("alist"), Some("alist-main"));
        let supported = validate_dynamic_playlist_query_support(
            &playlist,
            &crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist.id.to_string(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: "alpha".to_string(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
                availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )
        .expect("dynamic playlist search should be supported");

        assert!(supported);
    }

    #[test]
    fn test_build_move_media_request_rejects_invalid_proto_payload() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let err = build_move_media_request(
            crate::proto::client::MoveMediaRequest {
                media_ids: Vec::new(),
                source_playlist_id: Some("playlist-1".into()),
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some("media-before".into()),
                after_media_id: Some("media-after".into()),
            },
            &codec,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("source_playlist_id")
                || err.to_string().contains("before_media_id")
        );
    }

    #[test]
    fn test_build_move_media_request_parses_ids() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let media_id = MediaId::expect_positive(123);
        let playlist_id = PlaylistId::expect_positive(456);
        let after_media_id = MediaId::expect_positive(789);
        let request = build_move_media_request(
            crate::proto::client::MoveMediaRequest {
                media_ids: vec![codec.encode_media_id(media_id).unwrap()],
                source_playlist_id: None,
                target_playlist_id: Some(codec.encode_playlist_id(playlist_id).unwrap()),
                all_from_scope: false,
                before_media_id: None,
                after_media_id: Some(codec.encode_media_id(after_media_id).unwrap()),
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0], media_id);
        assert_eq!(request.target_playlist_id, Some(playlist_id));
        assert_eq!(request.after_media_id, Some(after_media_id));
    }
}
