use rayon::prelude::*;
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::service::StreamKickAuditRequest;

use super::{
    compare_active_streams, live_streaming_unavailable_error, normalize_non_empty_filter,
    paginate_vec, usize_to_i32_api, AdminApiImpl, ApiError, RequestContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveStreamListSortBy {
    StartedAt,
    RoomId,
    MediaId,
    UserId,
    NodeId,
}

impl AdminApiImpl {
    pub async fn list_active_streams(
        &self,
        req: synctv_proto::admin::ListActiveStreamsRequest,
    ) -> Result<synctv_proto::admin::ListActiveStreamsResponse, ApiError> {
        // API impl is the protobuf/public-id boundary for admin operations.
        // Core-facing logic below uses typed ids and normalized values.
        crate::impls::validate_proto_request(&req)?;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let active_publishers =
            infrastructure
                .list_active_generations()
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("Failed to list active streams: {error}"))
                })?;
        let room_id = normalize_non_empty_filter(&req.room_id)
            .map(|room_id| crate::impls::proto_validated_room_id(room_id, &self.public_id_codec))
            .transpose()?
            .map(|room_id| room_id.to_string());
        let user_filter = normalize_non_empty_filter(&req.user_id)
            .map(|user_id| crate::impls::proto_validated_user_id(user_id, &self.public_id_codec))
            .transpose()?
            .map(|user_id| user_id.to_string());
        let node_filter = normalize_non_empty_filter(&req.node_id);
        let search =
            normalize_non_empty_filter(&req.search).map(|value| value.to_ascii_lowercase());
        let sort_by = super::proto_admin_active_stream_list_sort_by(req.sort_by)?;
        let sort_direction = super::proto_admin_active_stream_sort_direction(req.sort_direction)?;

        let mut streams = Vec::new();
        for active_publisher in active_publishers {
            if let Some(filter_room) = room_id.as_deref() {
                if active_publisher.room_id != filter_room {
                    continue;
                }
            }
            if let Some(filter_user) = user_filter.as_deref() {
                if active_publisher.generation.user_id != filter_user {
                    continue;
                }
            }

            let user_id = if active_publisher.generation.user_id.is_empty() {
                String::new()
            } else {
                self.public_id_codec
                    .encode_user_id(
                        active_publisher
                            .generation
                            .user_id
                            .parse::<UserId>()
                            .map_err(|error| {
                                ApiError::Internal(format!(
                                    "Invalid active publisher user id: {error}"
                                ))
                            })?,
                    )
                    .map_err(ApiError::Internal)?
            };

            let stream = synctv_proto::admin::ActiveStreamInfo {
                room_id: self
                    .public_id_codec
                    .encode_room_id(active_publisher.room_id.parse::<RoomId>().map_err(
                        |error| {
                            ApiError::Internal(format!("Invalid active publisher room id: {error}"))
                        },
                    )?)
                    .map_err(ApiError::Internal)?,
                media_id: self
                    .public_id_codec
                    .encode_media_id(active_publisher.media_id.parse::<MediaId>().map_err(
                        |error| {
                            ApiError::Internal(format!(
                                "Invalid active publisher media id: {error}"
                            ))
                        },
                    )?)
                    .map_err(ApiError::Internal)?,
                user_id,
                node_id: active_publisher.generation.node_id,
                started_at: active_publisher.generation.started_at.timestamp(),
            };

            if let Some(filter_node) = node_filter.as_deref() {
                if stream.node_id != filter_node {
                    continue;
                }
            }
            if let Some(search) = &search {
                let haystack = format!(
                    "{}\n{}\n{}\n{}",
                    stream.room_id.to_ascii_lowercase(),
                    stream.media_id.to_ascii_lowercase(),
                    stream.user_id.to_ascii_lowercase(),
                    stream.node_id.to_ascii_lowercase(),
                );
                if !haystack.contains(search) {
                    continue;
                }
            }

            streams.push(stream);
        }

        let total = usize_to_i32_api(streams.len(), "active stream count")?;

        streams.par_sort_by(|left, right| {
            compare_active_streams(left, right, sort_by, sort_direction)
        });
        let streams = paginate_vec(streams, req.page, req.page_size)?;

        Ok(synctv_proto::admin::ListActiveStreamsResponse { streams, total })
    }

    pub async fn kick_stream(
        &self,
        req: synctv_proto::admin::KickStreamRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<(), ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let media_id = crate::impls::proto_validated_media_id(req.media_id, &self.public_id_codec)?;
        let reason = req.reason;
        let room_id_key = room_id.to_string();
        let media_id_key = media_id.to_string();
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        tracing::info!(
            room_id = %room_id,
            media_id = %media_id,
            reason = %reason,
            admin_user_id = %admin_user_id,
            "Admin kicking stream"
        );

        if !infrastructure
            .is_stream_active(&room_id_key, &media_id_key)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
        {
            return Err(ApiError::NotFound("Active stream not found".to_string()));
        }

        self.realtime_lifecycle
            .kick_stream(&room_id, &media_id, &reason)
            .await
            .map_err(|error| crate::impls::map_livestream_stream_error(&error))?;

        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        if let Err(e) = self
            .audit_service
            .log_stream_kicked(StreamKickAuditRequest {
                actor_id: admin_user_id.to_string(),
                actor_username: admin_username.clone(),
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
                reason: if reason.is_empty() {
                    None
                } else {
                    Some(reason)
                },
                ip_address: ctx.ip_address.clone(),
                user_agent: ctx.user_agent.clone(),
            })
            .await
        {
            tracing::error!(
                error = %e,
                admin_user_id = %admin_user_id,
                admin_username = %admin_username,
                room_id = %room_id,
                media_id = %media_id,
                "AUDIT LOG FAILURE: failed to record stream kick. Manual review required."
            );
        }

        Ok(())
    }

    pub async fn create_publish_key_for_actor(
        &self,
        room_id: &str,
        request: synctv_proto::client::CreateRoomPublishKeyRequest,
        actor_user_id: &UserId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::client::CreateRoomPublishKeyResponse, ApiError> {
        crate::impls::validate_proto_request(&request)?;
        let options = crate::impls::client::stream::publish_key_options(&request)?;
        let public_media_id = request.media_id.clone();
        let room_id_value =
            crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let media_id_value =
            crate::impls::proto_validated_media_id(request.media_id, &self.public_id_codec)?;

        let (media, room) = tokio::join!(
            self.room_service
                .media_service()
                .get_room_media(&room_id_value, &media_id_value),
            self.room_service.get_room(&room_id_value),
        );
        media
            .map_err(|error| ApiError::Internal(format!("Failed to load media: {error}")))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id_value} not found")))?;
        let room = room.map_err(ApiError::from)?;
        crate::impls::client::stream::ensure_room_accepts_live_publish(&room)?;

        let publish_key_service = self.publish_key_service.as_deref().ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "Publish key service is not available on this server.".to_string(),
            )
        })?;
        let response = crate::impls::client::stream::issue_room_publish_key(
            publish_key_service,
            &self.runtime_settings,
            &self.public_id_codec,
            room_id_value,
            media_id_value,
            actor_user_id,
            options,
        )?;

        tracing::info!(
            room_id,
            media_id = public_media_id,
            actor_user_id = %actor_user_id,
            admin_user_id = %admin_user_id,
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin created publish key for actor user"
        );

        Ok(response)
    }

    pub async fn get_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<synctv_proto::client::GetRoomStreamInfoResponse, ApiError> {
        let request = synctv_proto::client::GetRoomStreamInfoRequest {
            media_id: media_id.to_string(),
        };
        crate::impls::validate_proto_request(&request)?;
        let room_id = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let media_id =
            crate::impls::proto_validated_media_id(request.media_id, &self.public_id_codec)?;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(crate::impls::client::stream::live_streaming_unavailable_error)?;

        crate::impls::client::stream::fetch_stream_info(
            infrastructure,
            &self.public_id_codec,
            &room_id.to_string(),
            &media_id.to_string(),
        )
        .await
    }

    pub async fn list_room_streams(
        &self,
        room_id: &str,
        req: synctv_proto::client::ListRoomStreamsRequest,
    ) -> Result<synctv_proto::client::ListRoomStreamsResponse, ApiError> {
        let req = crate::impls::client::stream::build_room_streams_request(req)?;
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        if self.live_streaming_infrastructure.is_none() {
            return Err(crate::impls::client::stream::live_streaming_unavailable_error());
        }

        let media_ids = self
            .realtime_lifecycle
            .active_room_stream_media_ids(&rid)
            .await;

        crate::impls::client::stream::build_room_streams_response(
            media_ids,
            &req,
            &self.public_id_codec,
        )
    }
}
