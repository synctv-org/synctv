//! Room operations: list, create, get, join, leave, delete, settings, chat, hot rooms, public settings

use crate::impls::ApiError;
use std::collections::HashMap;
use synctv_core::models::{
    ChatMentionInput, ChatMessageEvent, ChatMessageType, ChatMessageWithAttachments, ChatPinEvent,
    ChatPlaybackMessagesQuery, CreateChatAttachmentUploadSession, MarkChatRead, PageParams,
    RoomListQuery, RoomListSortBy, RoomStatus, SendChatMessage, SetChatReaction, SortDirection,
    StoreFileUploadResult, UserId,
};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::ClientResourceAvailability;

use super::convert::{
    apply_room_settings_patch_from_proto, chat_message_selection_from_proto_values,
    chat_metadata_from_proto, file_metadata_from_proto, member_status_to_proto,
    provider_target_from_proto, resource_availability_enum_to_proto, room_role_to_proto,
    room_settings_from_proto, room_settings_to_proto, try_members_to_proto,
    try_playback_state_to_proto,
};
use super::media::{
    complete_upload_response_fields, complete_upload_session_request,
    prepare_delete_entries_outbox_fanout, proto_file_range_request, proto_file_upload_range,
    proto_upload_manifest_parts, required_file_upload_reference, room_cover_object_to_proto,
    room_cover_upload_create_result_to_proto, uploaded_parts_response_fields,
    PrepareDeleteEntriesOutboxFanout,
};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};

mod support;
use support::*;
pub(crate) use support::{
    build_search_chat_messages_query, parse_optional_room_category_id,
    parse_proto_chat_attachments, parse_room_label_ids,
};
#[cfg(test)]
pub(crate) use support::{chat_reaction_count, chat_reaction_summary_to_proto};

fn required_room_availability(
    availability_map: &HashMap<synctv_core::models::RoomId, ClientResourceAvailability>,
    room_id: &synctv_core::models::RoomId,
) -> Result<ClientResourceAvailability, ApiError> {
    availability_map.get(room_id).copied().ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing client availability for room {room_id} in batch response"
        ))
    })
}

fn build_server_time_response(
    req: synctv_proto::client::GetServerTimeRequest,
    clock: &dyn synctv_core::Clock,
) -> synctv_proto::client::GetServerTimeResponse {
    let server_received_at_nanos = clock.now_nanos();
    let server_sent_at_nanos = clock.now_nanos().max(server_received_at_nanos);

    synctv_proto::client::GetServerTimeResponse {
        client_sent_at_nanos: req.client_sent_at_nanos,
        server_received_at_nanos,
        server_sent_at_nanos,
    }
}

impl ClientApiImpl {
    async fn load_room_creator_public_views(
        &self,
        rooms: &[synctv_core::models::Room],
    ) -> Result<HashMap<UserId, synctv_proto::client::UserPublicView>, ApiError> {
        let creator_ids: Vec<UserId> = rooms
            .iter()
            .map(|room| room.created_by)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let creators = self
            .user_service
            .get_users_by_ids(&creator_ids)
            .await
            .map_err(ApiError::from)?;

        // Batch load all avatars in parallel
        let creator_views = self
            .batch_user_public_views_with_loaded_avatars(&creators)
            .await?;

        // Build map from user ID to public view
        let mut map = HashMap::with_capacity(creator_views.len());
        for (i, creator) in creators.iter().enumerate() {
            map.insert(creator.id, creator_views[i].clone());
        }

        Ok(map)
    }

    fn required_creator_public_view(
        map: &HashMap<UserId, synctv_proto::client::UserPublicView>,
        room: &synctv_core::models::Room,
    ) -> Result<synctv_proto::client::UserPublicView, ApiError> {
        map.get(&room.created_by).cloned().ok_or_else(|| {
            ApiError::Internal(format!(
                "Missing creator public view for room {} creator {}",
                room.id, room.created_by
            ))
        })
    }

    async fn load_room_member_count(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Result<Option<i32>, ApiError> {
        self.room_service
            .get_member_count(room_id)
            .await
            .map(Some)
            .map_err(ApiError::from)
    }

    async fn load_room_playback_state_proto(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Result<synctv_proto::client::PlaybackState, ApiError> {
        let state = self
            .room_service
            .get_playback_state(room_id)
            .await
            .map_err(ApiError::from)?;
        try_playback_state_to_proto(&state, &self.public_id_codec)
    }

    async fn user_username_for_event(&self, user_id: &UserId) -> Result<String, ApiError> {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
            .map_err(ApiError::from)
    }

    /// Get the currently playing media for a room.
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_playing_media(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<Option<synctv_proto::client::Media>, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Check membership before returning playing media
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let media = self
            .room_service
            .get_playing_media(&rid)
            .await
            .map_err(ApiError::from)?;
        match media {
            Some(media) => Ok(Some(
                self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(uid))
                    .await?,
            )),
            None => Ok(None),
        }
    }

    pub async fn list_rooms(
        &self,
        req: synctv_proto::client::ListRoomsRequest,
    ) -> Result<synctv_proto::client::ListRoomsResponse, ApiError> {
        let query = build_public_room_list_query(req, &self.public_id_codec)?;
        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;
        let availability_map = self
            .room_service
            .room_availability_batch(&rooms)
            .await
            .map_err(ApiError::from)?;

        let room_ids: Vec<synctv_core::models::RoomId> = rooms.iter().map(|room| room.id).collect();
        let room_id_refs: Vec<&synctv_core::models::RoomId> = room_ids.iter().collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&room_id_refs)
            .await
            .map_err(ApiError::from)?;
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_stats = self
            .presence_service
            .room_stats_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_by_room: HashMap<synctv_core::models::RoomId, _> = presence_stats
            .iter()
            .map(|stats| (stats.room_id, stats))
            .collect();
        let creator_views = self.load_room_creator_public_views(&rooms).await?;

        let mut room_list = Vec::with_capacity(rooms.len());
        for r in &rooms {
            let member_count = crate::impls::room_member_count_or_zero(&member_counts, &r.id);
            let availability = required_room_availability(&availability_map, &r.id)?;
            let settings = required_room_settings(&room_settings_map, &r.id)?;
            room_list.push(
                self.room_to_proto_with_availability_presence_and_loaded_cover(
                    r,
                    Some(settings),
                    Some(member_count),
                    availability,
                    presence_by_room.get(&r.id).copied(),
                    Some(Self::required_creator_public_view(&creator_views, r)?),
                )
                .await?,
            );
        }

        Ok(synctv_proto::client::ListRoomsResponse {
            rooms: room_list,
            total: i64_to_i32_api(total, "room total")?,
        })
    }

    pub async fn list_my_rooms(
        &self,
        user_id: &UserId,
        req: synctv_proto::client::ListMyRoomsRequest,
    ) -> Result<synctv_proto::client::ListMyRoomsResponse, ApiError> {
        let uid = *user_id;
        let query = build_my_room_list_query(req)?;
        let (rooms, total) = self
            .room_service
            .list_accessible_joined_rooms_with_query(&uid, &query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch room settings for full permission calculation.
        let room_ids: Vec<synctv_core::models::RoomId> =
            rooms.iter().map(|(room, _, _, _)| room.id).collect();
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_stats = self
            .presence_service
            .room_stats_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_by_room: HashMap<synctv_core::models::RoomId, _> = presence_stats
            .iter()
            .map(|stats| (stats.room_id, stats))
            .collect();
        let room_models: Vec<synctv_core::models::Room> =
            rooms.iter().map(|(room, _, _, _)| room.clone()).collect();
        let creator_views = self.load_room_creator_public_views(&room_models).await?;

        let mut room_list = Vec::with_capacity(rooms.len());
        for (room, role, _status, member_count) in &rooms {
            // Use the full permission calculation instead of role.permissions(),
            // which only gives role-level defaults. calculate_role_default_permissions applies:
            //   1. Global default permissions (from RuntimeSettingsStore)
            //   2. Room-level overrides (room_added / room_removed)
            let settings = required_room_settings(&room_settings_map, &room.id)?;
            let permissions = self
                .room_service
                .permission_service()
                .calculate_role_default_permissions(role, settings)
                .0;
            let relation = if room.created_by == uid {
                synctv_proto::client::MyRoomRelation::Created as i32
            } else {
                synctv_proto::client::MyRoomRelation::Participating as i32
            };
            room_list.push(synctv_proto::client::MyRoom {
                room: Some(
                    self.room_to_proto_with_availability_presence_and_loaded_cover(
                        room,
                        Some(settings),
                        Some(*member_count),
                        synctv_core::service::ClientResourceAvailability::Available,
                        presence_by_room.get(&room.id).copied(),
                        Some(Self::required_creator_public_view(&creator_views, room)?),
                    )
                    .await?,
                ),
                permissions,
                role: room_role_to_proto(*role),
                relation,
            });
        }

        Ok(synctv_proto::client::ListMyRoomsResponse {
            rooms: room_list,
            total: i64_to_i32_api(total, "my room total")?,
        })
    }

    pub async fn create_room(
        &self,
        user_id: &UserId,
        mut req: synctv_proto::client::CreateRoomRequest,
    ) -> Result<synctv_proto::client::Room, ApiError> {
        // Validate and sanitize room name
        req.name = crate::impls::validate_room_name_input(&req.name)?;

        // Validate and sanitize room description against ROOM_DESCRIPTION_MAX
        if !req.description.is_empty() {
            req.description = crate::impls::validate_room_description_input(&req.description)?;
        }

        crate::impls::validate_proto_request(&req)?;

        let uid = *user_id;

        let settings = req
            .settings
            .map(|settings| room_settings_from_proto(Some(settings)))
            .transpose()?;
        let password = if req.password.is_empty() {
            None
        } else {
            validate_room_password_for_set(&req.password)?;
            Some(req.password)
        };
        let category_id = parse_optional_room_category_id(&req.category_id, &self.public_id_codec)?;
        let label_ids = parse_room_label_ids(&req.label_ids, &self.public_id_codec)?;

        let response_settings =
            crate::impls::client::convert::normalize_created_room_settings(settings.as_ref());
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_created_outbox_fanout(uid);
        let (room, _member) = self
            .room_service
            .create_room_with_taxonomy_outbox(
                synctv_core::service::CreateRoomWithTaxonomyRequest {
                    name: req.name,
                    description: req.description,
                    created_by: uid,
                    password,
                    settings,
                    category_id,
                    label_ids,
                },
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        self.room_to_proto_basic_with_loaded_cover(
            &room,
            Some(&response_settings),
            self.load_room_member_count(&room.id).await?,
        )
        .await
    }

    pub async fn list_room_categories(
        &self,
        req: synctv_proto::client::ListRoomCategoriesRequest,
    ) -> Result<synctv_proto::client::ListRoomCategoriesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let categories = self
            .room_service
            .list_room_categories(!req.include_disabled)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::client::ListRoomCategoriesResponse {
            categories: categories
                .iter()
                .map(|category| {
                    crate::impls::client::convert::room_category_to_proto(
                        category,
                        &self.public_id_codec,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub async fn list_room_labels(
        &self,
        req: synctv_proto::client::ListRoomLabelsRequest,
    ) -> Result<synctv_proto::client::ListRoomLabelsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let category_id = parse_optional_room_category_id(&req.category_id, &self.public_id_codec)?;
        let labels = self
            .room_service
            .list_room_labels(!req.include_disabled, category_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::client::ListRoomLabelsResponse {
            labels: labels
                .iter()
                .map(|label| {
                    crate::impls::client::convert::room_label_to_proto(label, &self.public_id_codec)
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub async fn get_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_for_actor(&actor).await
    }

    pub async fn get_room_for_actor(
        &self,
        actor: &RoomActor,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        let rid = actor.room_id();
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        let playback_state = self.load_room_playback_state_proto(&rid).await?;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let presence = self
            .presence_service
            .room_stats(rid)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::client::GetRoomResponse {
            room: Some(
                self.room_to_proto_with_availability_presence_and_loaded_cover(
                    &room,
                    Some(&settings),
                    self.load_room_member_count(&rid).await?,
                    synctv_core::service::ClientResourceAvailability::Available,
                    Some(&presence),
                    None,
                )
                .await?,
            ),
            playback_state: Some(playback_state),
        })
    }

    pub async fn get_room_as_guest(
        &self,
        access: &GuestRoomAccess,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        self.get_room_for_actor(&RoomActor::Guest(access.clone()))
            .await
    }

    async fn room_response_after_room_update(
        &self,
        room: &synctv_core::models::Room,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        let rid = room.id;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::client::GetRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    room,
                    Some(&settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            playback_state: Some(self.load_room_playback_state_proto(&rid).await?),
        })
    }

    pub async fn create_room_cover_upload_session(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::CreateRoomCoverUploadSessionRequest,
    ) -> Result<synctv_proto::client::CreateRoomCoverUploadSessionResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let session = self
            .room_service
            .create_room_cover_upload_session(
                rid,
                *user_id,
                synctv_core::service::CreateRoomCoverUploadSession {
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
        room_cover_upload_create_result_to_proto(session)
    }

    pub async fn upload_room_cover_object(
        &self,
        req: synctv_proto::client::UploadRoomCoverObjectRequest,
    ) -> Result<synctv_proto::client::UploadRoomCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .store_room_cover_upload_object(
                &req.encoded_object_key,
                &req.token,
                req.content_type.as_deref(),
                proto_file_upload_range(req.content_range),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) = uploaded_parts_response_fields(&blob);
        Ok(synctv_proto::client::UploadRoomCoverObjectResponse {
            object: match blob {
                StoreFileUploadResult::Complete(blob) => Some(room_cover_object_to_proto(&blob)),
                StoreFileUploadResult::PartAccepted { .. } => None,
            },
            complete,
            uploaded_size_bytes,
            uploaded_parts,
        })
    }

    pub async fn complete_room_cover_upload_session(
        &self,
        req: synctv_proto::client::CompleteRoomCoverUploadSessionRequest,
    ) -> Result<synctv_proto::client::CompleteRoomCoverUploadSessionResponse, ApiError> {
        let result = self
            .room_service
            .complete_room_cover_upload_session(complete_upload_session_request(
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
            synctv_proto::client::CompleteRoomCoverUploadSessionResponse {
                object: result.object.as_ref().map(room_cover_object_to_proto),
                complete,
                uploaded_size_bytes,
                uploaded_parts,
            },
        )
    }

    pub async fn get_room_cover_object(
        &self,
        req: synctv_proto::client::GetRoomCoverObjectRequest,
    ) -> Result<synctv_core::models::FileObjectDownload, ApiError> {
        self.room_service
            .get_room_cover_object_stream(
                &req.encoded_object_key,
                &req.token,
                proto_file_range_request(req.range),
            )
            .await
            .map_err(ApiError::from)
    }

    pub async fn update_room_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateRoomCoverRequest,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let cover = required_file_upload_reference(req.cover_reference, "cover_reference")?;
        let room = self
            .room_service
            .update_room_cover(rid, *user_id, cover)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.room_response_after_room_update(&room).await
    }

    pub async fn clear_room_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ClearRoomCoverRequest,
    ) -> Result<synctv_proto::client::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let room = self
            .room_service
            .clear_room_cover(rid, *user_id)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.room_response_after_room_update(&room).await
    }

    pub async fn join_room(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::JoinRoomRequest,
        client_ip: Option<&str>,
    ) -> Result<synctv_proto::client::JoinRoomResponse, ApiError> {
        Box::pin(self.join_room_with_control(user_id, room_id, req, client_ip, None)).await
    }

    pub async fn join_room_with_control(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::JoinRoomRequest,
        client_ip: Option<&str>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::JoinRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let remark_name = crate::impls::normalize_member_remark_name(&req.remark_name);
        let display_tag = crate::impls::normalize_member_display_tag(&req.display_tag);
        let password = if req.password.is_empty() {
            None
        } else {
            validate_room_password_for_verify(&req.password)?;
            Some(req.password)
        };

        let password_enabled = self
            .room_service
            .is_room_password_enabled(&rid)
            .await
            .map_err(ApiError::from)?;

        if password_enabled {
            let password = password.as_ref().ok_or_else(|| {
                ApiError::Authorization("Forbidden: Password required".to_string())
            })?;
            let parsed_client_ip = parse_optional_client_ip(client_ip)?;
            if !self
                .room_service
                .check_room_password_with_rate_limit_with_control(
                    &rid,
                    password,
                    parsed_client_ip,
                    request_control,
                )
                .await
                .map_err(ApiError::from)?
            {
                return Err(ApiError::Authorization(
                    "Forbidden: Invalid password".to_string(),
                ));
            }
        }

        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        let target_presence = self
            .presence_service
            .user_room_stats_fresh(uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
        let (_room, member, members) = self
            .room_service
            .join_room_with_outbox(
                rid,
                uid,
                password,
                remark_name,
                display_tag,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        // Get updated room and playback state
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback_state = self.load_room_playback_state_proto(&rid).await?;

        let proto_members = try_members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        )?;

        let requires_approval = proto_members.is_empty();
        Ok(synctv_proto::client::JoinRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&room_settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            members: proto_members,
            playback_state: Some(playback_state),
            membership_status: member_status_to_proto(member.status),
            requires_approval,
        })
    }

    pub async fn start_room_password_login_with_control(
        &self,
        user_id: &UserId,
        req: synctv_proto::client::StartRoomPasswordLoginRequest,
        client_ip: Option<&str>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::StartRoomPasswordLoginResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(&req.room_id)?;
        let parsed_client_ip = parse_optional_client_ip(client_ip)?;
        let challenge = self
            .room_service
            .start_room_opaque_password_login_with_control(
                &rid,
                &uid,
                req.credential_request,
                parsed_client_ip,
                request_control,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::client::StartRoomPasswordLoginResponse {
            session_id: challenge.session_id,
            credential_response: challenge.credential_response,
        })
    }

    pub async fn finish_room_password_login_with_control(
        &self,
        user_id: &UserId,
        expected_room_id: Option<&str>,
        req: synctv_proto::client::FinishRoomPasswordLoginRequest,
        client_ip: Option<&str>,
    ) -> Result<synctv_proto::client::JoinRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let expected_room_id = expected_room_id
            .map(|room_id| self.parse_room_id(room_id))
            .transpose()?;
        let parsed_client_ip = parse_optional_client_ip(client_ip)?;
        let target_presence = if let Some(room_id) = expected_room_id {
            Some(
                self.presence_service
                    .user_room_stats_fresh(uid, room_id)
                    .await
                    .map_err(ApiError::from)?,
            )
        } else {
            None
        };
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence
                    .as_ref()
                    .is_some_and(|presence| presence.is_online),
                target_presence
                    .as_ref()
                    .map_or(0, |presence| presence.connection_count),
            );
        let (room, member, members) = Box::pin(
            self.room_service
                .finish_room_opaque_password_login_with_outbox(
                    expected_room_id.as_ref(),
                    &req.session_id,
                    &uid,
                    req.credential_finalization,
                    parsed_client_ip,
                    Some(prepared_membership_fanout.outbox_factory()),
                ),
        )
        .await
        .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let rid = room.id;
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback_state = self.load_room_playback_state_proto(&rid).await?;
        let proto_members = try_members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        )?;
        let requires_approval = proto_members.is_empty();
        Ok(synctv_proto::client::JoinRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&room_settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            members: proto_members,
            playback_state: Some(playback_state),
            membership_status: member_status_to_proto(member.status),
            requires_approval,
        })
    }

    pub async fn create_websocket_ticket_with_control(
        &self,
        user_id: &UserId,
        password_version: i32,
        req: synctv_proto::client::CreateWebSocketTicketRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::CreateWebSocketTicketResponse, ApiError> {
        let room_id = build_create_websocket_ticket_request(&req, &self.public_id_codec)?;
        let requested_room_id = req.room_id;
        let ws_ticket_service = &self.ws_ticket_service;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|err| match err {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound(format!("Room {requested_room_id} not found"))
                }
                other => ApiError::from(other),
            })?;

        if room.is_banned {
            return Err(ApiError::Authorization("Room is banned".to_string()));
        }

        let is_member = self
            .room_service
            .member_service()
            .is_member(&room_id, user_id)
            .await
            .map_err(ApiError::from)?;

        if !is_member {
            return Err(ApiError::Authorization(
                "Not a member of this room. Join the room first.".to_string(),
            ));
        }

        let ticket = ws_ticket_service
            .create_ticket_with_control(user_id, &room_id, password_version, request_control)
            .await
            .map_err(ApiError::from)?;

        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::from)?;

        Ok(synctv_proto::client::CreateWebSocketTicketResponse {
            ticket,
            room_id: public_room_id.clone(),
            expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
            usage: format!("Use in WebSocket URL: ws://host/ws/rooms/{public_room_id}?ticket=xxx"),
        })
    }

    pub async fn create_websocket_ticket_for_actor_with_control(
        &self,
        actor: RoomActor,
        req: synctv_proto::client::CreateWebSocketTicketRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::client::CreateWebSocketTicketResponse, ApiError> {
        let room_id = build_create_websocket_ticket_request(&req, &self.public_id_codec)?;
        let requested_room_id = req.room_id;
        if actor.room_id() != room_id {
            return Err(ApiError::Authorization(
                "Cannot create a WebSocket ticket for a different room".to_string(),
            ));
        }

        let ws_ticket_service = &self.ws_ticket_service;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|err| match err {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound(format!("Room {requested_room_id} not found"))
                }
                other => ApiError::from(other),
            })?;

        if room.is_banned {
            return Err(ApiError::Authorization("Room is banned".to_string()));
        }

        let ticket = match actor {
            RoomActor::User { user_id, .. } => {
                let password_version = self
                    .user_service
                    .get_password_credential_state(&user_id)
                    .await
                    .map_err(ApiError::from)?
                    .version;
                ws_ticket_service
                    .create_ticket_with_control(
                        &user_id,
                        &room_id,
                        password_version,
                        request_control,
                    )
                    .await
                    .map_err(ApiError::from)?
            }
            RoomActor::Guest(access) => ws_ticket_service
                .create_guest_ticket_with_control(
                    synctv_core::service::CreateGuestTicketRequest {
                        room_id,
                        guest_id: access.guest_id,
                        display_name: access.display_name,
                        session_id: access.session_id,
                        token_jti: access.token_jti,
                        room_guest_version: access.room_guest_version,
                        permissions: access.permissions,
                    },
                    request_control,
                )
                .await
                .map_err(ApiError::from)?,
        };

        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::Internal)?;

        Ok(synctv_proto::client::CreateWebSocketTicketResponse {
            ticket,
            room_id: public_room_id.clone(),
            expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
            usage: format!("Use in WebSocket URL: ws://host/ws/rooms/{public_room_id}?ticket=xxx"),
        })
    }

    pub async fn leave_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<synctv_proto::client::LeaveRoomResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_user_left_outbox_fanout();
        let username = self.user_username_for_event(&uid).await?;
        let prepared_cleanup_fanout =
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
        self.room_service
            .leave_room_with_outbox(
                rid,
                uid,
                Some(prepared_membership_fanout.outbox_factory()),
                Some(prepared_cleanup_fanout.member_cleanup_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the user's room-scoped connections and any local
        // publishers tied to the room they just left.
        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &uid)
            .await;

        prepared_membership_fanout.publish_after_outbox_commit();
        prepared_cleanup_fanout.publish_after_outbox_commit();

        Ok(synctv_proto::client::LeaveRoomResponse { success: true })
    }

    pub async fn delete_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<synctv_proto::client::DeleteRoomResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_deleted_outbox_fanout(&rid, &uid)?;

        // 1. Delete the DB record first. If this fails, no realtime event is
        //    published and no connections are dropped -- the room remains intact.
        self.room_service
            .delete_room_with_outbox(rid, uid, Some(prepared_outbox_fanout.cloned_outbox_event()))
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        // Force disconnect room members and any active publishers tied to this room.
        self.realtime_lifecycle
            .disconnect_room(&rid, "room_deleted")
            .await;

        Ok(synctv_proto::client::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateRoomSettingsRequest,
    ) -> Result<synctv_proto::client::Room, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        let current_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings = validate_update_room_settings_request(&req, current_settings)?;
        let username = self.user_username_for_event(&uid).await?;
        let prepared_settings_fanout = self
            .room_settings_fanout
            .prepare_settings_changed(&rid, &uid, &username)?;
        let snapshot = self
            .room_service
            .set_settings_with_outbox(
                rid,
                uid,
                settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        let prepared_settings_fanout = prepared_settings_fanout
            .with_settings_and_version(&snapshot.settings, snapshot.version)
            .map_err(ApiError::from)?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(prepared_settings_fanout);
        self.room_cache_fanout.publish_invalidation(&rid);

        // Get updated room
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        self.room_to_proto_basic_with_loaded_cover(
            &room,
            Some(&snapshot.settings),
            self.load_room_member_count(&rid).await?,
        )
        .await
    }

    pub async fn start_room_password_registration(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::StartRoomPasswordRegistrationRequest,
    ) -> Result<synctv_proto::client::StartRoomPasswordRegistrationResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let challenge = self
            .room_service
            .start_room_opaque_password_registration(&rid, &uid, req.registration_request)
            .await
            .map_err(ApiError::from)?;
        Ok(
            synctv_proto::client::StartRoomPasswordRegistrationResponse {
                session_id: challenge.session_id,
                registration_response: challenge.registration_response,
            },
        )
    }

    pub async fn finish_room_password_registration(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::FinishRoomPasswordRegistrationRequest,
    ) -> Result<synctv_proto::client::SetRoomPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let username = self.user_username_for_event(&uid).await?;
        let state = self
            .room_service
            .finish_room_opaque_password_registration(
                &rid,
                &req.session_id,
                &uid,
                req.registration_upload,
            )
            .await
            .map_err(ApiError::from)?;

        self.room_cache_fanout.publish_invalidation(&state.room_id);
        tracing::debug!(
            room_id = %state.room_id,
            user_id = %uid,
            username = %username,
            password_enabled = state.enabled,
            password_version = state.version,
            "Room password updated"
        );

        Ok(synctv_proto::client::SetRoomPasswordResponse { success: true })
    }

    pub async fn clear_room_password(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ClearRoomPasswordRequest,
    ) -> Result<synctv_proto::client::SetRoomPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await
            .map_err(ApiError::from)?;
        let state = self
            .room_service
            .update_room_password_as(&rid, Some(&uid), None)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        tracing::debug!(
            room_id = %rid,
            user_id = %uid,
            password_enabled = state.enabled,
            password_version = state.version,
            "Room password cleared"
        );
        Ok(synctv_proto::client::SetRoomPasswordResponse { success: true })
    }

    /// Get room settings
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<synctv_proto::client::GetRoomSettingsResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_settings_for_actor(&actor).await
    }

    pub async fn get_room_settings_for_actor(
        &self,
        actor: &RoomActor,
    ) -> Result<synctv_proto::client::GetRoomSettingsResponse, ApiError> {
        let rid = actor.room_id();
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::client::GetRoomSettingsResponse {
            settings: Some(room_settings_to_proto(&settings)),
            version,
        })
    }

    pub async fn get_room_settings_as_guest(
        &self,
        access: &GuestRoomAccess,
    ) -> Result<synctv_proto::client::GetRoomSettingsResponse, ApiError> {
        self.get_room_settings_for_actor(&RoomActor::Guest(access.clone()))
            .await
    }

    /// Reset room settings to defaults
    pub async fn reset_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<synctv_proto::client::RoomSettings, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let username = self.user_username_for_event(&uid).await?;
        let prepared_settings_fanout = self
            .room_settings_fanout
            .prepare_settings_changed(&rid, &uid, &username)?;
        let snapshot = self
            .room_service
            .reset_room_settings_with_outbox(
                &rid,
                &uid,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout
                    .with_settings_and_version(&snapshot.settings, snapshot.version)?,
            );
        self.room_cache_fanout.publish_invalidation(&rid);

        Ok(room_settings_to_proto(&snapshot.settings))
    }

    pub async fn transfer_room_ownership(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::TransferRoomOwnershipRequest,
    ) -> Result<synctv_proto::client::Room, ApiError> {
        let current_owner_id = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let new_owner_id = build_transfer_room_ownership_request(req, &self.public_id_codec)?;

        let target_presence = self
            .presence_service
            .user_room_stats_fresh(current_owner_id, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
        let room = self
            .room_service
            .transfer_room_ownership_with_outbox(
                rid,
                current_owner_id,
                new_owner_id,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(Self::map_room_access_error)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&rid);
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        self.room_to_proto_basic_with_loaded_cover(
            &room,
            Some(&settings),
            self.load_room_member_count(&rid).await?,
        )
        .await
    }

    /// Get public settings
    pub fn get_public_settings(
        &self,
    ) -> Result<synctv_proto::client::GetPublicSettingsResponse, ApiError> {
        let reg = self
            .runtime_settings_store
            .as_ref()
            .ok_or_else(runtime_settings_store_unavailable_error)?;

        let s = reg.to_public_settings().map_err(ApiError::from)?;
        Ok(synctv_proto::client::GetPublicSettingsResponse {
            room_creation_enabled: s.room_creation_enabled,
            max_rooms_per_user: s.max_rooms_per_user,
            default_max_members: s.default_max_members,
            max_pinned_chat_messages_per_room: s.max_pinned_chat_messages_per_room,
            room_creation_approval_required: s.approval_required,
            room_password_policy: s.room_password_policy.to_string(),
            enable_password_signup: s.enable_password_signup,
            password_signup_need_review: s.password_signup_need_review,
            enable_email_signup: s.enable_email_signup,
            email_signup_need_review: s.email_signup_need_review,
            enable_email: s.enable_email && self.email_api.is_some(),
            enable_webauthn: self.passkey_service.is_some(),
            enable_webauthn_signup: s.enable_webauthn_signup,
            webauthn_signup_need_review: s.webauthn_signup_need_review,
            enable_guest: s.enable_guest,
            movie_proxy: s.movie_proxy,
            live_proxy: s.live_proxy,
            ts_disguised_as_png: s.ts_disguised_as_png,
            custom_publish_host: s.custom_publish_host,
            email_whitelist_enabled: s.email_whitelist_enabled,
            email_whitelist_domains: s.email_whitelist_domains,
        })
    }

    pub async fn get_server_info(
        &self,
    ) -> Result<synctv_proto::client::GetServerInfoResponse, ApiError> {
        let reg = self
            .runtime_settings_store
            .as_ref()
            .ok_or_else(runtime_settings_store_unavailable_error)?;
        let server_id = reg
            .get_or_initialize_server_id()
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::client::GetServerInfoResponse {
            server_id,
            server_name: self.runtime_settings.webrtc.rp_name.clone(),
        })
    }

    pub fn get_server_time(
        &self,
        req: synctv_proto::client::GetServerTimeRequest,
    ) -> synctv_proto::client::GetServerTimeResponse {
        build_server_time_response(req, self.clock.as_ref())
    }

    /// Check if a room exists and whether it requires a password (public endpoint).
    ///
    /// Only returns whether the room requires a password -- the room name is
    /// intentionally omitted to avoid leaking room metadata to unauthenticated
    /// users (room enumeration / information disclosure).
    pub async fn check_room(
        &self,
        req: synctv_proto::client::CheckRoomRequest,
    ) -> Result<synctv_proto::client::CheckRoomResponse, ApiError> {
        let rid = build_check_room_request(req, &self.public_id_codec)?;

        match self.room_service.get_room(&rid).await {
            Ok(room) => {
                let password_enabled = self
                    .room_service
                    .is_room_password_enabled(&rid)
                    .await
                    .map_err(ApiError::from)?;
                let availability = self
                    .room_service
                    .room_availability(&room)
                    .await
                    .map_err(ApiError::from)?;
                Ok(synctv_proto::client::CheckRoomResponse {
                    exists: true,
                    requires_password: password_enabled,
                    name: String::new(),
                    availability: resource_availability_enum_to_proto(availability),
                })
            }
            Err(synctv_core::Error::NotFound(_)) => Ok(synctv_proto::client::CheckRoomResponse {
                exists: false,
                requires_password: false,
                name: String::new(),
                availability: synctv_proto::client::ResourceAvailability::Unspecified as i32,
            }),
            Err(error) => Err(ApiError::from(error)),
        }
    }

    pub async fn get_hot_rooms(
        &self,
        req: synctv_proto::client::GetHotRoomsRequest,
    ) -> Result<synctv_proto::client::GetHotRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let limit = if req.limit == 0 {
            DEFAULT_HOT_ROOM_LIMIT
        } else {
            i64::from(req.limit)
        };
        let limit_usize =
            positive_i64_to_usize(limit, DEFAULT_HOT_ROOM_LIMIT_USIZE, "hot room limit")?;

        let room_online_counts = self
            .presence_service
            .hot_room_stats()
            .await
            .map_err(ApiError::from)?;
        let room_ids: Vec<synctv_core::models::RoomId> = room_online_counts
            .iter()
            .map(|stats| stats.room_id)
            .collect();
        let rooms = self
            .room_service
            .list_active_unbanned_rooms_by_ids(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let mut online_by_room: HashMap<synctv_core::models::RoomId, usize> = room_online_counts
            .into_iter()
            .map(|stats| (stats.room_id, stats.online_user_count))
            .collect();
        let mut top_rooms = Vec::with_capacity(rooms.len());
        for room in rooms {
            let count = online_by_room.remove(&room.id).ok_or_else(|| {
                ApiError::Internal(format!("Missing online count for hot room {}", room.id))
            })?;
            if count > 0 {
                top_rooms.push((room, usize_to_i32_api(count, "hot room online count")?));
            }
        }
        top_rooms.sort_by_key(|(room, count)| (std::cmp::Reverse(*count), room.id));
        top_rooms.truncate(limit_usize);
        if top_rooms.len() < limit_usize {
            let recent_limit =
                u32::try_from(limit_usize.saturating_add(top_rooms.len())).map_err(|_| {
                    ApiError::InvalidInput("hot room limit exceeds u32::MAX".to_string())
                })?;
            let (recent_rooms, _) = self
                .room_service
                .list_rooms(&RoomListQuery {
                    pagination: PageParams::new(Some(1), Some(recent_limit)),
                    status: Some(RoomStatus::Active),
                    search: None,
                    is_banned: Some(false),
                    creator_id: None,
                    category_id: None,
                    label_ids: Vec::new(),
                    sort_by: RoomListSortBy::LastActivityAt,
                    sort_direction: SortDirection::Desc,
                })
                .await
                .map_err(ApiError::from)?;
            let mut selected_ids: std::collections::HashSet<synctv_core::models::RoomId> =
                top_rooms.iter().map(|(room, _)| room.id).collect();
            for room in recent_rooms {
                if top_rooms.len() >= limit_usize {
                    break;
                }
                if selected_ids.insert(room.id) {
                    top_rooms.push((room, 0));
                }
            }
        }

        let selected_rooms: Vec<synctv_core::models::Room> =
            top_rooms.iter().map(|(room, _)| room.clone()).collect();
        let availability_map = self
            .room_service
            .room_availability_batch(&selected_rooms)
            .await
            .map_err(ApiError::from)?;

        let room_ids: Vec<synctv_core::models::RoomId> =
            top_rooms.iter().map(|(room, _)| room.id).collect();
        let top_room_id_refs: Vec<&synctv_core::models::RoomId> = room_ids.iter().collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&top_room_id_refs)
            .await
            .map_err(ApiError::from)?;

        let settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let selected_presence_stats = self
            .presence_service
            .room_stats_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let selected_presence_by_room: HashMap<synctv_core::models::RoomId, _> =
            selected_presence_stats
                .iter()
                .map(|stats| (stats.room_id, stats))
                .collect();
        let creator_views = self.load_room_creator_public_views(&selected_rooms).await?;

        let mut hot_rooms = Vec::with_capacity(top_rooms.len());
        for (room, online_count) in top_rooms {
            let total_members = crate::impls::room_member_count_or_zero(&member_counts, &room.id);
            let settings = required_room_settings(&settings_map, &room.id)?;
            let availability = required_room_availability(&availability_map, &room.id)?;

            hot_rooms.push(synctv_proto::client::RoomWithStats {
                room: Some(
                    self.room_to_proto_with_availability_presence_and_loaded_cover(
                        &room,
                        Some(settings),
                        Some(total_members),
                        availability,
                        selected_presence_by_room.get(&room.id).copied(),
                        Some(Self::required_creator_public_view(&creator_views, &room)?),
                    )
                    .await?,
                ),
                online_count,
                total_members,
            });
        }

        Ok(synctv_proto::client::GetHotRoomsResponse { rooms: hot_rooms })
    }

    pub async fn get_chat_history(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::GetChatHistoryRequest,
    ) -> Result<synctv_proto::client::GetChatHistoryResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_chat_history_for_actor(&actor, req).await
    }

    async fn chat_messages_to_proto(
        &self,
        messages: Vec<ChatMessageWithAttachments>,
    ) -> Result<Vec<synctv_proto::client::ChatMessageReceive>, ApiError> {
        let user_ids: Vec<synctv_core::models::UserId> = messages
            .iter()
            .filter_map(|message| message.message.user_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let username_map: std::collections::HashMap<synctv_core::models::UserId, String> = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .map_err(ApiError::from)?;

        messages
            .into_iter()
            .map(|message| {
                let (user_id_str, username) = match &message.message.user_id {
                    Some(uid) => {
                        let uid_str =
                            self.public_id_codec.encode_user_id(*uid).map_err(|error| {
                                ApiError::Internal(format!(
                                    "Failed to encode chat message user id: {error}"
                                ))
                            })?;
                        let name = username_map.get(uid).cloned().ok_or_else(|| {
                            ApiError::NotFound("Chat message author not found".to_string())
                        })?;
                        (uid_str, name)
                    }
                    None => (String::new(), "[deleted]".to_string()),
                };

                let mut proto = chat_message_to_proto(self, &message, username)?;
                proto.user_id = user_id_str;
                Ok(proto)
            })
            .collect::<Result<Vec<_>, ApiError>>()
    }

    async fn get_chat_history_for_room_id(
        &self,
        rid: &synctv_core::models::RoomId,
        viewer_user_id: Option<UserId>,
        req: synctv_proto::client::GetChatHistoryRequest,
    ) -> Result<synctv_proto::client::GetChatHistoryResponse, ApiError> {
        let (limit, cursor, selection) = build_get_chat_history_request(&req)?;
        let cursor = cursor
            .map(|(created_at, id)| synctv_core::models::ChatHistoryCursor { created_at, id });
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let page = chat_service
            .get_history_page_with_attachments_for_viewer(
                rid,
                cursor,
                limit,
                true,
                viewer_user_id.as_ref(),
                &selection,
            )
            .await
            .map_err(ApiError::from)?;
        let next_cursor_str = page.next_cursor.map(|cursor| {
            format!(
                "{}|{}",
                synctv_common::time::format_datetime_rfc3339(cursor.created_at),
                cursor.id
            )
        });

        let proto_messages = self.chat_messages_to_proto(page.messages).await?;

        Ok(synctv_proto::client::GetChatHistoryResponse {
            messages: proto_messages,
            next_cursor: next_cursor_str.unwrap_or_default(),
            event_cursor: Some(synctv_proto::client::EventCursor {
                event_id: page.event_cursor.event_id,
                sequence: page.event_cursor.sequence,
            }),
        })
    }

    async fn search_chat_messages_for_room_id(
        &self,
        rid: &synctv_core::models::RoomId,
        viewer_user_id: Option<UserId>,
        req: synctv_proto::client::SearchChatMessagesRequest,
    ) -> Result<synctv_proto::client::SearchChatMessagesResponse, ApiError> {
        let query = build_search_chat_messages_query(*rid, &req, &self.public_id_codec)?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let page = chat_service
            .search_messages_with_attachments_for_viewer(query, viewer_user_id.as_ref())
            .await
            .map_err(ApiError::from)?;
        let next_cursor = page
            .next_cursor
            .map(|cursor| {
                format!(
                    "{}|{}",
                    synctv_common::time::format_datetime_rfc3339(cursor.created_at),
                    cursor.id
                )
            })
            .unwrap_or_default();
        let messages = self.chat_messages_to_proto(page.messages).await?;

        Ok(synctv_proto::client::SearchChatMessagesResponse {
            messages,
            next_cursor,
            event_cursor: Some(synctv_proto::client::EventCursor {
                event_id: page.event_cursor.event_id,
                sequence: page.event_cursor.sequence,
            }),
        })
    }

    pub async fn send_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::SendChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let attachments = parse_proto_chat_attachments(&req.attachments)?;
        let playback_state = self
            .room_service
            .playback_service()
            .get_state(&room_id)
            .await
            .map_err(ApiError::from)?;
        let metadata = crate::impls::messaging::chat_metadata_for_send(
            chat_metadata_from_proto(req.metadata.as_ref())?,
            &req.display_position,
            &req.display_color,
            Some(&playback_state),
        )
        .map_err(ApiError::InvalidInput)?;
        let outcome = chat_service
            .send_message_event_outcome(SendChatMessage {
                room_id,
                user_id,
                client_message_id: optional_trimmed_string(&req.client_message_id),
                content: req.content,
                message_type: ChatMessageType::User,
                reply_to_message_id: if req.reply_to_message_id.trim().is_empty() {
                    None
                } else {
                    Some(parse_chat_message_id(&req.reply_to_message_id)?)
                },
                metadata,
                attachments,
                mentions: self.parse_proto_chat_mentions(req.mentions)?,
            })
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
            if let Some(pin_event) = &outcome.pin_event {
                self.broadcast_chat_pin_event(pin_event);
            }
        }
        Ok(synctv_proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await?),
        })
    }

    pub async fn create_chat_attachment_upload_session_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::CreateChatAttachmentUploadSessionRequest,
    ) -> Result<synctv_proto::client::CreateChatAttachmentUploadSessionResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let session = chat_service
            .create_attachment_upload_session(CreateChatAttachmentUploadSession {
                room_id: actor.room_id(),
                user_id,
                client_attachment_id: optional_trimmed_string(&req.client_attachment_id),
                filename: optional_trimmed_string(&req.filename),
                mime_type: req.mime_type,
                size_bytes: req.size_bytes,
                width: (req.width > 0).then_some(req.width),
                height: (req.height > 0).then_some(req.height),
                duration_seconds: (req.duration_seconds > 0).then_some(req.duration_seconds),
                bitrate_bps: (req.bitrate_bps > 0).then_some(req.bitrate_bps),
                parts: proto_upload_manifest_parts(req.parts),
                metadata: file_metadata_from_proto(req.metadata.as_ref())?,
            })
            .await
            .map_err(ApiError::from)?;
        chat_attachment_upload_create_result_to_proto(session)
    }

    pub async fn upload_chat_attachment_object(
        &self,
        req: synctv_proto::client::UploadChatAttachmentObjectRequest,
    ) -> Result<synctv_proto::client::UploadChatAttachmentObjectResponse, ApiError> {
        if !req.room_id.trim().is_empty() {
            let _room_id = self.parse_room_id(&req.room_id)?;
        }
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let blob = chat_service
            .store_attachment_upload_object(
                &req.encoded_object_key,
                &req.token,
                req.content_type.as_deref(),
                proto_file_upload_range(req.content_range),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        let (complete, uploaded_size_bytes, uploaded_parts) = uploaded_parts_response_fields(&blob);
        Ok(synctv_proto::client::UploadChatAttachmentObjectResponse {
            object: match blob {
                StoreFileUploadResult::Complete(blob) => {
                    Some(chat_attachment_object_to_proto(&req.room_id, &blob))
                }
                StoreFileUploadResult::PartAccepted { .. } => None,
            },
            complete,
            uploaded_size_bytes,
            uploaded_parts,
        })
    }

    pub async fn complete_chat_attachment_upload_session(
        &self,
        req: synctv_proto::client::CompleteChatAttachmentUploadSessionRequest,
    ) -> Result<synctv_proto::client::CompleteChatAttachmentUploadSessionResponse, ApiError> {
        let room_id = req.room_id.clone();
        if !room_id.trim().is_empty() {
            let _room_id = self.parse_room_id(&room_id)?;
        }
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let result = chat_service
            .complete_attachment_upload_session(complete_upload_session_request(
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
            synctv_proto::client::CompleteChatAttachmentUploadSessionResponse {
                object: result
                    .object
                    .as_ref()
                    .map(|blob| chat_attachment_object_to_proto(&room_id, blob)),
                complete,
                uploaded_size_bytes,
                uploaded_parts,
            },
        )
    }

    pub async fn get_chat_attachment_object(
        &self,
        req: synctv_proto::client::GetChatAttachmentObjectRequest,
    ) -> Result<synctv_core::models::FileObjectDownload, ApiError> {
        if !req.room_id.trim().is_empty() {
            let _room_id = self.parse_room_id(&req.room_id)?;
        }
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        chat_service
            .get_attachment_object_stream(
                &req.encoded_object_key,
                &req.token,
                proto_file_range_request(req.range),
            )
            .await
            .map_err(ApiError::from)
    }

    pub async fn edit_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::EditChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .edit_message_outcome(edit_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
            if let Some(pin_event) = &outcome.pin_event {
                self.broadcast_chat_pin_event(pin_event);
            }
        }
        Ok(synctv_proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await?),
        })
    }

    pub async fn delete_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::DeleteChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .delete_message_event_outcome(delete_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                &req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
            if let Some(pin_event) = &outcome.pin_event {
                self.broadcast_chat_pin_event(pin_event);
            }
        }
        Ok(synctv_proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await?),
        })
    }

    pub async fn list_pinned_chat_messages_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::ListPinnedChatMessagesRequest,
    ) -> Result<synctv_proto::client::ListPinnedChatMessagesResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let limit = if req.limit <= 0 {
            20
        } else {
            req.limit.min(100)
        };
        let pinned = chat_service
            .list_pinned_messages_for_authorized_viewer(
                &actor.room_id(),
                actor.user_id().as_ref(),
                limit,
            )
            .await
            .map_err(ApiError::from)?;
        let mut messages = Vec::with_capacity(pinned.len());
        for message in pinned {
            messages.push(chat_pinned_message_to_proto(self, message).await?);
        }
        Ok(synctv_proto::client::ListPinnedChatMessagesResponse { messages })
    }

    pub async fn pin_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::PinChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatPinEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .pin_message_event_outcome(pin_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                &req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_pin_event(&outcome.event);
        }
        Ok(synctv_proto::client::ChatPinEventResponse {
            event: Some(chat_pin_event_to_proto(self, outcome.event).await?),
        })
    }

    pub async fn unpin_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::UnpinChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatPinEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .unpin_message_event_outcome(unpin_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                &req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_pin_event(&outcome.event);
        }
        Ok(synctv_proto::client::ChatPinEventResponse {
            event: Some(chat_pin_event_to_proto(self, outcome.event).await?),
        })
    }

    pub async fn set_chat_reaction_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::SetChatReactionRequest,
    ) -> Result<synctv_proto::client::ChatMessageEvent, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: actor.room_id(),
                message_id: parse_chat_message_id(&req.message_id)?,
                user_id,
                reaction_key: req.reaction_key,
                enabled: req.enabled,
            })
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
            if let Some(pin_event) = &outcome.pin_event {
                self.broadcast_chat_pin_event(pin_event);
            }
        }
        chat_event_to_proto(self, outcome.event).await
    }

    pub async fn list_chat_reaction_users_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::ListChatReactionUsersRequest,
    ) -> Result<synctv_proto::client::ListChatReactionUsersResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let (limit, cursor) = build_list_chat_reaction_users_request(&req, &self.public_id_codec)?;
        let cursor =
            cursor.map(
                |(reacted_at, user_id)| synctv_core::models::ChatReactionUsersCursor {
                    reacted_at,
                    user_id,
                },
            );
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let page = chat_service
            .list_reaction_users(
                &actor.room_id(),
                parse_chat_message_id(&req.message_id)?,
                &user_id,
                &req.reaction_key,
                cursor,
                limit,
            )
            .await
            .map_err(ApiError::from)?;
        let user_ids = page
            .users
            .iter()
            .map(|reaction_user| reaction_user.user_id)
            .collect::<Vec<_>>();
        let username_map = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .map_err(ApiError::from)?;
        let users = page
            .users
            .into_iter()
            .map(|reaction_user| {
                let user_id = self
                    .public_id_codec
                    .encode_user_id(reaction_user.user_id)
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "Failed to encode chat reaction user id: {error}"
                        ))
                    })?;
                let username = username_map
                    .get(&reaction_user.user_id)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::NotFound("Chat reaction user not found".to_string())
                    })?;
                Ok(synctv_proto::client::ChatReactionUser {
                    user_id,
                    username,
                    reacted_at: reaction_user.reacted_at.timestamp(),
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let next_cursor = page
            .next_cursor
            .map(|cursor| {
                let user_id = self
                    .public_id_codec
                    .encode_user_id(cursor.user_id)
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "Failed to encode chat reaction cursor user id: {error}"
                        ))
                    })?;
                Ok::<String, ApiError>(format!(
                    "{}|{}",
                    synctv_common::time::format_datetime_rfc3339(cursor.reacted_at),
                    user_id
                ))
            })
            .transpose()?;

        Ok(synctv_proto::client::ListChatReactionUsersResponse {
            users,
            next_cursor: next_cursor.unwrap_or_default(),
            total: page.total,
        })
    }

    pub async fn mark_chat_read_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::MarkChatReadRequest,
    ) -> Result<synctv_proto::client::ChatReadStateResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let state = chat_service
            .mark_read(MarkChatRead {
                room_id: actor.room_id(),
                user_id,
                message_id: parse_chat_message_id(&req.message_id)?,
            })
            .await
            .map_err(ApiError::from)?;
        chat_read_state_to_proto(self, state)
    }

    pub async fn get_chat_read_state_for_actor(
        &self,
        actor: &RoomActor,
        _req: synctv_proto::client::GetChatReadStateRequest,
    ) -> Result<synctv_proto::client::ChatReadStateResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let state = chat_service
            .get_read_state(&actor.room_id(), &user_id)
            .await
            .map_err(ApiError::from)?;
        chat_read_state_to_proto(self, state)
    }

    pub async fn get_chat_message_read_receipts_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetChatMessageReadReceiptsRequest,
    ) -> Result<synctv_proto::client::GetChatMessageReadReceiptsResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let page = chat_service
            .get_message_read_receipts(
                &actor.room_id(),
                &user_id,
                parse_chat_message_id(&req.message_id)?,
                req.page,
                req.page_size,
            )
            .await
            .map_err(ApiError::from)?;
        chat_message_read_receipts_to_proto(self, page).await
    }

    fn broadcast_chat_event(&self, event: &ChatMessageEvent) {
        self.chat_event_dispatcher.dispatch(event);
    }

    fn broadcast_chat_pin_event(&self, event: &ChatPinEvent) {
        self.chat_event_dispatcher.dispatch_pin(event);
    }

    pub async fn get_chat_history_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: synctv_proto::client::GetChatHistoryRequest,
    ) -> Result<synctv_proto::client::GetChatHistoryResponse, ApiError> {
        self.get_chat_history_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn get_chat_history_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetChatHistoryRequest,
    ) -> Result<synctv_proto::client::GetChatHistoryResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        self.get_chat_history_for_room_id(&actor.room_id(), actor.user_id(), req)
            .await
    }

    pub async fn search_chat_messages_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::SearchChatMessagesRequest,
    ) -> Result<synctv_proto::client::SearchChatMessagesResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        self.search_chat_messages_for_room_id(&actor.room_id(), actor.user_id(), req)
            .await
    }

    pub async fn get_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetChatMessageRequest,
    ) -> Result<synctv_proto::client::ChatMessageReceive, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let message = chat_service
            .get_message_with_attachments_for_viewer(
                &actor.room_id(),
                parse_chat_message_id(&req.message_id)?,
                req.include_deleted,
                actor.user_id().as_ref(),
            )
            .await
            .map_err(ApiError::from)?;
        let username = username_for_chat_message(self, &message.message).await?;
        chat_message_to_proto(self, &message, username)
    }

    pub async fn get_chat_message_context_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetChatMessageContextRequest,
    ) -> Result<synctv_proto::client::GetChatMessageContextResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let context = chat_service
            .get_message_context_for_viewer(
                &actor.room_id(),
                parse_chat_message_id(&req.message_id)?,
                positive_i32(req.before_limit, 20).min(50),
                positive_i32(req.after_limit, 20).min(50),
                req.include_deleted,
                actor.user_id().as_ref(),
            )
            .await
            .map_err(ApiError::from)?;
        let before = self.chat_messages_to_proto(context.before).await?;
        let username = username_for_chat_message(self, &context.anchor.message).await?;
        let message = chat_message_to_proto(self, &context.anchor, username)?;
        let after = self.chat_messages_to_proto(context.after).await?;
        Ok(synctv_proto::client::GetChatMessageContextResponse {
            before,
            message: Some(message),
            after,
        })
    }

    pub async fn get_chat_playback_messages_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetChatPlaybackMessagesRequest,
    ) -> Result<synctv_proto::client::GetChatPlaybackMessagesResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let media_id = optional_trimmed_string(&req.playback_media_id)
            .map(|id| crate::impls::proto_validated_media_id(id, &self.public_id_codec))
            .transpose()?;
        let playlist_id = optional_trimmed_string(&req.playback_playlist_id)
            .map(|id| crate::impls::proto_validated_playlist_id(id, &self.public_id_codec))
            .transpose()?;
        let target = provider_target_from_proto(req.playback_target)?;
        let position_seconds = required_playback_position_seconds(req.position_seconds)?;
        let before_seconds =
            optional_positive_window_seconds(req.before_seconds, 0.0, "before_seconds")?;
        let after_seconds =
            optional_positive_window_seconds(req.after_seconds, 30.0, "after_seconds")?;
        let limit = optional_positive_limit(req.limit, 200, 500, "limit")?;
        let selection = chat_message_selection_from_proto_values(&req.include_message_types)?;
        let messages = chat_service
            .get_playback_messages_with_attachments_for_viewer(
                ChatPlaybackMessagesQuery {
                    room_id: actor.room_id(),
                    media_id,
                    playlist_id,
                    target,
                    selection,
                    position_seconds,
                    before_seconds,
                    after_seconds,
                    limit,
                    include_deleted: req.include_deleted,
                },
                actor.user_id().as_ref(),
            )
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::client::GetChatPlaybackMessagesResponse {
            messages: self.chat_messages_to_proto(messages).await?,
        })
    }

    pub(crate) fn parse_proto_chat_mentions(
        &self,
        mentions: Vec<synctv_proto::client::ChatMentionInput>,
    ) -> Result<Vec<ChatMentionInput>, ApiError> {
        mentions
            .into_iter()
            .map(|mention| {
                let user_id = crate::impls::parse_user_id_param(
                    &mention.user_id,
                    "mention.user_id",
                    &self.public_id_codec,
                )?;
                Ok(ChatMentionInput {
                    user_id,
                    start: mention.start,
                    length: mention.length,
                })
            })
            .collect()
    }
}

pub(crate) fn validate_update_room_settings_request(
    req: &synctv_proto::client::UpdateRoomSettingsRequest,
    current: synctv_core::models::RoomSettings,
) -> Result<synctv_core::models::RoomSettings, ApiError> {
    apply_room_settings_patch_from_proto(current, *req)
}

#[cfg(test)]
mod tests {
    use super::{
        build_check_room_request, build_create_websocket_ticket_request,
        build_get_chat_history_request, build_my_room_list_query, build_public_room_list_query,
        build_transfer_room_ownership_request, chat_pin_event_to_proto,
        delete_chat_message_request_to_core, edit_chat_message_request_to_core,
        optional_positive_limit, optional_positive_window_seconds, optional_trimmed_string,
        parse_proto_chat_attachments, required_playback_position_seconds,
        required_room_availability, runtime_settings_store_unavailable_error,
    };
    use crate::impls::ErrorKind;
    use std::collections::HashMap;
    use synctv_core::service::ClientResourceAvailability;
    use synctv_core::{
        models::{
            ChatMessage, ChatMessagePin, ChatMessageWithAttachments, ChatPinEvent,
            ChatPinEventKind, RoomId, SignupMethod, User,
        },
        repository::UserRepository,
        service::RoomService,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn api_err<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<crate::impls::ApiError> {
        match result {
            Ok(_) => Err(test_error("expected API error result")),
            Err(error) => Ok(error),
        }
    }

    fn assert_invalid_argument_contains(
        error: &crate::impls::ApiError,
        expected: &str,
    ) -> TestResult {
        if !error.is_invalid_argument() {
            return Err(test_error(format!("expected invalid input, got {error:?}")));
        }
        let message = error.message();
        assert!(message.contains(expected), "{message}");
        Ok(())
    }

    fn codec_ok<T>(result: Result<T, String>) -> TestResult<T> {
        result.map_err(test_error)
    }

    fn test_client_api(
        pool: sqlx::PgPool,
        user_service: std::sync::Arc<synctv_core::service::UserService>,
    ) -> super::ClientApiImpl {
        let room_service = std::sync::Arc::new(
            RoomService::new_for_tests(pool, (*user_service).clone())
                .expect("room service should build"),
        );
        super::ClientApiImpl::new_with_runtime(
            crate::impls::ClientApiOptions {
                read_pool: None,
                user_service,
                room_service,
                chat_service: None,
                connection_service: std::sync::Arc::new(
                    synctv_realtime::sync::ConnectionManager::default(),
                ),
                runtime_settings: std::sync::Arc::new(crate::ApiRuntimeSettings::default()),
                publish_key_service: None,
                jwt_service: synctv_core_testing::create_test_jwt_service(),
                live_streaming_infrastructure: None,
                runtime_settings_store: None,
                provider_stores: std::sync::Arc::new(
                    synctv_core::provider::ProviderStoreRegistry::local_only("test:chat-pin-api:"),
                ),
                public_id_codec: std::sync::Arc::new(synctv_adapter::PublicIdCodec::plain()),
                email_api: None,
                passkey_service: None,
            },
            crate::test_support::client_api_runtime(),
        )
    }

    #[test]
    fn build_public_room_list_query_maps_sorting_and_defaults() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let query = api_ok(build_public_room_list_query(
            synctv_proto::client::ListRoomsRequest {
                page: 0,
                page_size: 0,
                search: "alpha".to_string(),
                sort_by: synctv_proto::client::RoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                category_id: String::new(),
                label_ids: Vec::new(),
            },
            &public_id_codec,
        ))?;

        assert_eq!(query.pagination.page, 1);
        assert_eq!(query.pagination.page_size, 20);
        assert_eq!(query.search.as_deref(), Some("alpha"));
        assert_eq!(query.status, Some(synctv_core::models::RoomStatus::Active));
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.sort_by, synctv_core::models::RoomListSortBy::Name);
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Asc
        );
        Ok(())
    }

    #[test]
    fn build_my_room_list_query_maps_filters_sorting_and_defaults() -> TestResult {
        let query = api_ok(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 0,
                page_size: 0,
                search: "alpha".to_string(),
                status: synctv_proto::common::RoomStatus::Closed as i32,
                is_banned: Some(false),
                relation: synctv_proto::client::MyRoomRelation::Participating as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        ))?;

        assert_eq!(query.pagination.page, 1);
        assert_eq!(query.pagination.page_size, 20);
        assert_eq!(query.search.as_deref(), Some("alpha"));
        assert_eq!(query.status, Some(synctv_core::models::RoomStatus::Closed));
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(
            query.relation,
            synctv_core::models::MyRoomRelation::Participating
        );
        assert_eq!(query.sort_by, synctv_core::models::MyRoomListSortBy::Name);
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Asc
        );
        Ok(())
    }

    #[test]
    fn build_my_room_list_query_defaults_relation_to_all() -> TestResult {
        let query = api_ok(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Unspecified as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
            },
        ))?;

        assert_eq!(query.relation, synctv_core::models::MyRoomRelation::All);
        assert_eq!(
            query.sort_by,
            synctv_core::models::MyRoomListSortBy::JoinedAt
        );
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Desc
        );
        Ok(())
    }

    #[test]
    fn build_my_room_list_query_rejects_unknown_room_status() -> TestResult {
        let error = api_err(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: 99,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Unspecified as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
            },
        ))?;

        assert_invalid_argument_contains(&error, "status")?;
        Ok(())
    }

    #[test]
    fn room_list_query_builders_reject_unknown_sort_and_relation_enums() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let public_room_error = api_err(build_public_room_list_query(
            synctv_proto::client::ListRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                sort_by: 99,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
                category_id: String::new(),
                label_ids: Vec::new(),
            },
            &public_id_codec,
        ))?;
        assert_invalid_argument_contains(&public_room_error, "sort_by")?;

        let my_room_relation_error = api_err(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: None,
                relation: 99,
                sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
            },
        ))?;
        assert_invalid_argument_contains(&my_room_relation_error, "relation")?;

        let my_room_sort_error = api_err(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Unspecified as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
                sort_direction: 99,
            },
        ))?;
        assert_invalid_argument_contains(&my_room_sort_error, "sort_direction")?;
        Ok(())
    }

    #[test]
    fn build_my_room_list_query_rejects_too_long_search() -> TestResult {
        let error = api_err(build_my_room_list_query(
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: "a".repeat(101),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Unspecified as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Unspecified as i32,
                sort_direction: synctv_proto::client::SortDirection::Unspecified as i32,
            },
        ))?;

        assert_invalid_argument_contains(&error, "search")?;
        Ok(())
    }

    #[test]
    fn build_public_room_list_query_rejects_invalid_proto_request() -> TestResult {
        let public_id_codec = synctv_adapter::PublicIdCodec::plain();
        let error = api_err(build_public_room_list_query(
            synctv_proto::client::ListRoomsRequest {
                page: -1,
                page_size: 101,
                search: "a".repeat(101),
                sort_by: 99,
                sort_direction: 99,
                category_id: String::new(),
                label_ids: Vec::new(),
            },
            &public_id_codec,
        ))?;

        assert!(error.is_invalid_argument(), "{error:?}");
        let message = error.message();
        assert!(message.contains("page"), "{message}");
        assert!(message.contains("page_size"), "{message}");
        assert!(message.contains("search"), "{message}");
        assert!(message.contains("sort_by"), "{message}");
        assert!(message.contains("sort_direction"), "{message}");
        Ok(())
    }

    #[test]
    fn build_transfer_room_ownership_request_rejects_invalid_new_owner_user_id() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let error = api_err(build_transfer_room_ownership_request(
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: "bad-id".to_string(),
            },
            &codec,
        ))?;

        assert_invalid_argument_contains(&error, "new_owner_user_id")?;
        Ok(())
    }

    #[test]
    fn build_check_room_request_rejects_invalid_room_id() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let error = api_err(build_check_room_request(
            synctv_proto::client::CheckRoomRequest {
                room_id: "bad-room".to_string(),
            },
            &codec,
        ))?;

        assert_invalid_argument_contains(&error, "room_id")?;
        Ok(())
    }

    #[test]
    fn build_create_websocket_ticket_request_rejects_invalid_room_id() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let error = api_err(build_create_websocket_ticket_request(
            &synctv_proto::client::CreateWebSocketTicketRequest {
                room_id: "bad-room".to_string(),
            },
            &codec,
        ))?;

        assert_invalid_argument_contains(&error, "room_id")?;
        Ok(())
    }

    #[test]
    fn build_create_websocket_ticket_request_parses_proto_validated_room_id() -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let room_id = synctv_core::models::RoomId::expect_positive(123);
        let room_public_id = codec_ok(codec.encode_room_id(room_id))?;
        let parsed = api_ok(build_create_websocket_ticket_request(
            &synctv_proto::client::CreateWebSocketTicketRequest {
                room_id: room_public_id,
            },
            &codec,
        ))?;

        assert_eq!(parsed, room_id);
        Ok(())
    }

    #[test]
    fn build_create_websocket_ticket_request_rejects_proto_valid_but_undecodable_room_id(
    ) -> TestResult {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let error = api_err(build_create_websocket_ticket_request(
            &synctv_proto::client::CreateWebSocketTicketRequest {
                room_id: "room_abc".to_string(),
            },
            &codec,
        ))?;

        assert_invalid_argument_contains(&error, "RoomId")?;
        Ok(())
    }

    #[test]
    fn build_get_chat_history_request_rejects_invalid_limit() -> TestResult {
        let error = api_err(build_get_chat_history_request(
            &synctv_proto::client::GetChatHistoryRequest {
                limit: 101,
                cursor: String::new(),
                include_message_types: Vec::new(),
            },
        ))?;

        assert_invalid_argument_contains(&error, "limit")?;
        Ok(())
    }

    #[test]
    fn hot_rooms_validation_rejects_out_of_range_limit() -> TestResult {
        let error = api_err(crate::impls::validate_proto_request(
            &synctv_proto::client::GetHotRoomsRequest { limit: 51 },
        ))?;

        assert_invalid_argument_contains(&error, "limit")?;
        Ok(())
    }

    #[test]
    fn hot_rooms_validation_allows_default_limit_sentinel() -> TestResult {
        api_ok(crate::impls::validate_proto_request(
            &synctv_proto::client::GetHotRoomsRequest { limit: 0 },
        ))?;
        Ok(())
    }

    #[test]
    fn required_room_availability_rejects_missing_batch_entry() -> TestResult {
        let room_id = synctv_core::models::RoomId::expect_positive(456);
        let map = HashMap::from([(room_id, ClientResourceAvailability::CreatorInactive)]);

        assert_eq!(
            api_ok(required_room_availability(&map, &room_id))?,
            ClientResourceAvailability::CreatorInactive
        );

        let missing_room_id = synctv_core::models::RoomId::expect_positive(457);
        let error = api_err(required_room_availability(&map, &missing_room_id))?;
        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("Missing client availability for room")
        ));
        Ok(())
    }

    #[test]
    fn build_get_chat_history_request_rejects_invalid_cursor() -> TestResult {
        let error = api_err(build_get_chat_history_request(
            &synctv_proto::client::GetChatHistoryRequest {
                limit: 50,
                cursor: "not-a-cursor".to_string(),
                include_message_types: Vec::new(),
            },
        ))?;

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("Invalid cursor format"), "{message}");
            }
            other => return Err(test_error(format!("expected invalid input, got {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn optional_trimmed_string_normalizes_idempotency_keys() {
        assert_eq!(
            optional_trimmed_string("  client-key  ").as_deref(),
            Some("client-key")
        );
        assert!(optional_trimmed_string(" \n\t ").is_none());
    }

    #[test]
    fn chat_playback_window_seconds_validate_explicit_values() -> TestResult {
        assert_eq!(
            api_ok(optional_positive_window_seconds(0.0, 30.0, "after_seconds"))?,
            30.0
        );
        assert_eq!(
            api_ok(optional_positive_window_seconds(
                12.5,
                30.0,
                "after_seconds"
            ))?,
            12.5
        );
        assert!(matches!(
            optional_positive_window_seconds(-1.0, 30.0, "after_seconds"),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("after_seconds")
        ));
        assert!(matches!(
            optional_positive_window_seconds(f64::NAN, 30.0, "after_seconds"),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("after_seconds")
        ));
        Ok(())
    }

    #[test]
    fn chat_playback_limit_validates_explicit_values() -> TestResult {
        assert_eq!(api_ok(optional_positive_limit(0, 200, 500, "limit"))?, 200);
        assert_eq!(api_ok(optional_positive_limit(50, 200, 500, "limit"))?, 50);
        assert!(matches!(
            optional_positive_limit(-1, 200, 500, "limit"),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("limit")
        ));
        assert!(matches!(
            optional_positive_limit(501, 200, 500, "limit"),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("limit")
        ));
        Ok(())
    }

    #[test]
    fn chat_playback_position_seconds_is_required_valid_value() -> TestResult {
        assert_eq!(api_ok(required_playback_position_seconds(42.5))?, 42.5);
        assert!(matches!(
            required_playback_position_seconds(-0.1),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("position_seconds")
        ));
        assert!(matches!(
            required_playback_position_seconds(f64::INFINITY),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("position_seconds")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn chat_pin_event_response_populates_message_username() -> TestResult {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service =
            std::sync::Arc::new(synctv_core_testing::create_test_user_service(pool.clone()));
        let user = UserRepository::new(pool.clone())
            .create(&User::new(
                "pin_response_author".to_string(),
                SignupMethod::Password,
            ))
            .await
            .map_err(|error| test_error(error.to_string()))?;
        let api = test_client_api(pool, user_service);
        let room_id = RoomId::expect_positive(7);
        let occurred_at = synctv_core::SystemClock.now();
        let mut message = ChatMessage::new(room_id, user.id, "pinned body".to_string());
        message.id = 42;
        message.created_at = occurred_at;
        let pin = ChatMessagePin {
            room_id,
            message_id: message.id,
            message_created_at: message.created_at,
            pinned_by: Some(user.id),
            pinned_by_username: Some(user.username.clone()),
            note: None,
            pinned_at: occurred_at,
        };
        let event = ChatPinEvent {
            event_id: "pin-response-event".to_string(),
            sequence: 5,
            room_id,
            actor_user_id: user.id,
            kind: ChatPinEventKind::Pinned,
            message: ChatMessageWithAttachments {
                message,
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions: Vec::new(),
                pin: Some(pin.clone()),
            },
            pin: Some(pin),
            occurred_at,
        };

        let proto = api_ok(chat_pin_event_to_proto(&api, event).await)?;
        let message = proto
            .message
            .ok_or_else(|| test_error("pin event response should contain message"))?;
        assert_eq!(message.username, "pin_response_author");
        Ok(())
    }

    #[test]
    fn chat_attachment_display_proto_hides_upload_token_metadata() -> TestResult {
        let metadata = synctv_core::models::FileMetadata {
            width: Some(640),
            height: Some(480),
            blurhash: Some("abc".to_string()),
            ..Default::default()
        };
        let attachment = synctv_core::models::NewStoredFile {
            filename: None,
            id: "attachment-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "rooms/1/chat/2/attachment-1".to_string(),
            object_access: None,
            url: Some("https://cdn.example.test/rooms/1/chat/2/attachment-1.webp".to_string()),
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(1024),
            width: Some(640),
            height: Some(480),
            metadata: metadata.clone(),
        };

        let proto = api_ok(super::new_chat_attachment_to_proto(&attachment))?;

        assert_eq!(proto.id, "attachment-1");
        assert_eq!(
            proto.url,
            "https://cdn.example.test/rooms/1/chat/2/attachment-1.webp"
        );
        let proto_metadata = proto.metadata.expect("metadata should be present");
        assert_eq!(proto_metadata.width, Some(640));
        assert_eq!(proto_metadata.height, Some(480));
        Ok(())
    }

    #[test]
    fn chat_attachment_upload_session_returns_submit_reference() -> TestResult {
        let attachment = synctv_core::models::NewStoredFile {
            filename: None,
            id: "attachment-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "rooms/1/chat/2/attachment-1".to_string(),
            object_access: None,
            url: None,
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(1024),
            width: Some(640),
            height: Some(480),
            metadata: synctv_core::models::FileMetadata {
                upload_token: Some("v1.payload.signature".to_string()),
                ..Default::default()
            },
        };

        let proto = api_ok(super::upload_session_chat_attachment_to_proto(&attachment))?;
        let parsed = api_ok(parse_proto_chat_attachments(std::slice::from_ref(&proto)))?;

        assert_eq!(proto.id, "attachment-1");
        assert_eq!(
            proto.kind,
            synctv_proto::client::ChatAttachmentReferenceKind::Upload as i32
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "attachment-1");
        assert!(matches!(
            parsed[0].kind,
            synctv_core::models::SubmittedFileReferenceKind::Upload
        ));
        Ok(())
    }

    #[test]
    fn chat_attachment_reuse_reference_parses_to_core() -> TestResult {
        let parsed = api_ok(parse_proto_chat_attachments(&[
            synctv_proto::client::ChatAttachmentReference {
                id: "reuse-token".to_string(),
                kind: synctv_proto::client::ChatAttachmentReferenceKind::Reuse as i32,
            },
        ]))?;

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "reuse-token");
        assert!(matches!(
            parsed[0].kind,
            synctv_core::models::SubmittedFileReferenceKind::Reuse
        ));
        Ok(())
    }

    #[test]
    fn chat_attachment_upload_session_requires_upload_metadata_when_upload_required() {
        let session = synctv_core::models::FileUploadSession {
            file: synctv_core::models::NewStoredFile {
                filename: None,
                id: "attachment-1".to_string(),
                storage_backend: "database".to_string(),
                object_key: "rooms/1/chat/2/attachment-1".to_string(),
                object_access: None,
                url: None,
                mime_type: Some("image/webp".to_string()),
                size_bytes: Some(1024),
                width: Some(640),
                height: Some(480),
                metadata: synctv_core::models::FileMetadata {
                    upload_token: Some("v1.payload.signature".to_string()),
                    ..Default::default()
                },
            },
            encoded_object_key: "encoded-attachment-1".to_string(),
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_object_access: None,
            upload_url: Some("https://upload.example.test/attachment-1".to_string()),
            upload_method: None,
            upload_headers: Default::default(),
            expires_at: Some(synctv_core::SystemClock.now()),
            max_size_bytes: 1024 * 1024,
            resumable: true,
            part_size_bytes: 4 * 1024 * 1024,
            uploaded_size_bytes: 0,
            uploaded_parts: Vec::new(),
            upload_id: None,
            part_urls: Vec::new(),
        };

        assert!(matches!(
            super::upload_session_to_proto(session),
            Err(crate::impls::ApiError::Internal(message)) if message.contains("upload_method")
        ));
    }

    #[test]
    fn chat_attachment_upload_session_accepts_multipart_upload_targets() {
        let session = synctv_core::models::FileUploadSession {
            file: synctv_core::models::NewStoredFile {
                filename: None,
                id: "attachment-1".to_string(),
                storage_backend: "s3".to_string(),
                object_key: "rooms/1/chat/2/attachment-1".to_string(),
                object_access: None,
                url: None,
                mime_type: Some("image/webp".to_string()),
                size_bytes: Some(1024),
                width: Some(640),
                height: Some(480),
                metadata: synctv_core::models::FileMetadata {
                    upload_token: Some("v1.payload.signature".to_string()),
                    ..Default::default()
                },
            },
            encoded_object_key: "encoded-attachment-1".to_string(),
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_object_access: None,
            upload_url: Some("https://upload.example.test/attachment-1".to_string()),
            upload_method: Some("PUT".to_string()),
            upload_headers: Default::default(),
            expires_at: Some(synctv_core::SystemClock.now()),
            max_size_bytes: 1024 * 1024,
            resumable: true,
            part_size_bytes: 4 * 1024 * 1024,
            uploaded_size_bytes: 0,
            uploaded_parts: Vec::new(),
            upload_id: Some("upload-id".to_string()),
            part_urls: vec![synctv_core::models::FileUploadPartUrl {
                part_number: 1,
                offset_bytes: 0,
                size_bytes: 1024,
                upload_url: "https://upload.example.test/part-1".to_string(),
                upload_method: "PUT".to_string(),
                upload_headers: Default::default(),
                expires_at: Some(synctv_core::SystemClock.now()),
            }],
        };

        let proto = api_ok(super::upload_session_to_proto(session))
            .expect("multipart upload session should convert");

        assert_eq!(
            proto.upload_url.as_deref(),
            Some("https://upload.example.test/attachment-1")
        );
        assert_eq!(proto.upload_method.as_deref(), Some("PUT"));
        assert_eq!(proto.part_urls.len(), 1);
        assert_eq!(proto.part_urls[0].upload_method, "PUT");
    }

    #[test]
    fn edit_chat_message_request_maps_client_operation_id() -> TestResult {
        let request = synctv_proto::client::EditChatMessageRequest {
            message_id: "42".to_string(),
            content: "hello".to_string(),
            expected_version: 7,
            metadata: None,
            client_operation_id: " edit-op-42 ".to_string(),
        };
        let core = api_ok(edit_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            request,
        ))?;

        assert_eq!(
            core.room_id,
            synctv_core::models::RoomId::expect_positive(9)
        );
        assert_eq!(
            core.user_id,
            synctv_core::models::UserId::expect_positive(11)
        );
        assert_eq!(core.message_id, 42);
        assert_eq!(core.client_operation_id.as_deref(), Some("edit-op-42"));
        assert_eq!(core.expected_version, Some(7));
        Ok(())
    }

    #[test]
    fn delete_chat_message_request_maps_client_operation_id() -> TestResult {
        let request = synctv_proto::client::DeleteChatMessageRequest {
            message_id: "42".to_string(),
            expected_version: 7,
            reason: " cleanup ".to_string(),
            client_operation_id: " delete-op-42 ".to_string(),
        };
        let core = api_ok(delete_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            &request,
        ))?;

        assert_eq!(
            core.room_id,
            synctv_core::models::RoomId::expect_positive(9)
        );
        assert_eq!(
            core.user_id,
            synctv_core::models::UserId::expect_positive(11)
        );
        assert_eq!(core.message_id, 42);
        assert_eq!(core.client_operation_id.as_deref(), Some("delete-op-42"));
        assert_eq!(core.reason.as_deref(), Some("cleanup"));
        assert_eq!(core.expected_version, Some(7));
        Ok(())
    }

    #[test]
    fn edit_chat_message_request_accepts_absent_expected_version() -> TestResult {
        let core = api_ok(edit_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            synctv_proto::client::EditChatMessageRequest {
                message_id: "42".to_string(),
                content: "hello".to_string(),
                expected_version: 0,
                metadata: None,
                client_operation_id: String::new(),
            },
        ))?;

        assert_eq!(core.expected_version, None);
        Ok(())
    }

    #[test]
    fn delete_chat_message_request_accepts_absent_expected_version() -> TestResult {
        let request = synctv_proto::client::DeleteChatMessageRequest {
            message_id: "42".to_string(),
            expected_version: 0,
            reason: String::new(),
            client_operation_id: String::new(),
        };
        let core = api_ok(delete_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            &request,
        ))?;

        assert_eq!(core.expected_version, None);
        Ok(())
    }

    #[test]
    fn chat_message_request_rejects_negative_expected_version() -> TestResult {
        let edit_error = api_err(edit_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            synctv_proto::client::EditChatMessageRequest {
                message_id: "42".to_string(),
                content: "hello".to_string(),
                expected_version: -1,
                metadata: None,
                client_operation_id: String::new(),
            },
        ))?;
        assert!(matches!(
            edit_error,
            crate::impls::ApiError::InvalidInput(message)
                if message.contains("expected_version")
        ));

        let delete_request = synctv_proto::client::DeleteChatMessageRequest {
            message_id: "42".to_string(),
            expected_version: -1,
            reason: String::new(),
            client_operation_id: String::new(),
        };
        let delete_error = api_err(delete_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            &delete_request,
        ))?;
        assert!(matches!(
            delete_error,
            crate::impls::ApiError::InvalidInput(message)
                if message.contains("expected_version")
        ));
        Ok(())
    }

    #[test]
    fn chat_reaction_summary_rejects_empty_key() -> TestResult {
        let reaction = synctv_core::models::ChatReactionSummary {
            key: " ".to_string(),
            count: 1,
            reacted_by_me: false,
        };

        let error = api_err(super::chat_reaction_summary_to_proto(&reaction))?;

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("reaction summary key is empty")
        ));
        Ok(())
    }

    #[test]
    fn chat_reaction_summary_rejects_negative_count() -> TestResult {
        let reaction = synctv_core::models::ChatReactionSummary {
            key: "like".to_string(),
            count: -1,
            reacted_by_me: true,
        };

        let error = api_err(super::chat_reaction_summary_to_proto(&reaction))?;

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("negative count")
        ));
        Ok(())
    }

    #[test]
    fn chat_reaction_count_rejects_overflow() -> TestResult {
        let reactions = vec![
            synctv_proto::client::ChatReactionSummary {
                key: "a".to_string(),
                count: i64::MAX,
                reacted_by_me: false,
            },
            synctv_proto::client::ChatReactionSummary {
                key: "b".to_string(),
                count: 1,
                reacted_by_me: false,
            },
        ];

        let error = api_err(super::chat_reaction_count(&reactions))?;

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("reaction count exceeds")
        ));
        Ok(())
    }

    #[test]
    fn get_public_settings_missing_registry_is_service_unavailable() {
        let err = runtime_settings_store_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Public settings are not available on this server."
        );
    }

    #[test]
    fn get_server_time_echoes_client_timestamp_and_sets_server_window() {
        let clock = synctv_core::SyncedClock::system();
        let before = clock.now_nanos();
        let response = super::build_server_time_response(
            synctv_proto::client::GetServerTimeRequest {
                client_sent_at_nanos: 1_700_000_000_123_456_789,
            },
            &clock,
        );
        let after = clock.now_nanos();

        assert_eq!(response.client_sent_at_nanos, 1_700_000_000_123_456_789);
        assert!(response.server_received_at_nanos >= before);
        assert!(response.server_received_at_nanos <= after);
        assert!(response.server_sent_at_nanos >= response.server_received_at_nanos);
        assert!(response.server_sent_at_nanos <= after);
    }

    #[test]
    fn room_password_set_validation_rejects_whitespace_only_password() -> TestResult {
        let err = api_err(super::validate_room_password_for_set("    "))?;
        assert!(matches!(err, crate::impls::ApiError::InvalidInput(_)));
        Ok(())
    }

    #[test]
    fn room_password_set_validation_counts_trimmed_password_length() -> TestResult {
        let err = api_err(super::validate_room_password_for_set(" abc "))?;
        assert!(matches!(err, crate::impls::ApiError::InvalidInput(_)));
        api_ok(super::validate_room_password_for_set(" abcd "))?;
        Ok(())
    }

    #[test]
    fn parse_optional_client_ip_accepts_valid_ip() -> TestResult {
        let parsed = api_ok(super::parse_optional_client_ip(Some("203.0.113.42")))?;
        let expected = "203.0.113.42".parse()?;
        assert_eq!(parsed, Some(expected));
        Ok(())
    }

    #[test]
    fn parse_optional_client_ip_rejects_invalid_ip() -> TestResult {
        let error = api_err(super::parse_optional_client_ip(Some("not-an-ip")))?;
        assert!(matches!(
            error,
            crate::impls::ApiError::InvalidInput(message) if message.contains("Invalid client IP address")
        ));
        Ok(())
    }
}
