#[cfg(test)]
use synctv_core::models::RoomId;
use synctv_core::models::UserId;
use synctv_core::service::UserService;

use super::{
    try_admin_room_member_to_proto_with_settings, try_admin_user_to_proto,
    try_managed_room_to_proto, AdminApiImpl, ApiError,
};

pub(in crate::impls::admin) async fn load_creator_user_map(
    user_service: &UserService,
    creator_ids: &[UserId],
) -> Result<std::collections::HashMap<UserId, synctv_core::models::User>, ApiError> {
    let users = user_service
        .get_users_by_ids(creator_ids)
        .await
        .map_err(ApiError::from)?;
    Ok(users.into_iter().map(|user| (user.id, user)).collect())
}

impl AdminApiImpl {
    pub(in crate::impls::admin) async fn creator_avatar_url(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<Option<String>, ApiError> {
        let Some(reference_id) = user.avatar_file_reference_id else {
            return Ok(None);
        };
        let Some(storage) = self.user_service.file_storage_service() else {
            return Ok(None);
        };
        let file_reference =
            synctv_core::repository::FileStorageRepository::new(self.user_service.pool().clone())
                .get_reference_by_id(reference_id)
                .await
                .map_err(ApiError::from)?;
        let Some(file_reference) = file_reference else {
            return Ok(None);
        };

        storage
            .object_url(
                &file_reference.storage_backend,
                &file_reference.object_key,
                &synctv_core::service::user_avatar_upload_policy().database_object_route_prefix,
            )
            .map_err(ApiError::from)
    }

    pub(in crate::impls::admin) async fn room_cover_for_admin(
        &self,
        room: &synctv_core::models::Room,
    ) -> Result<Option<(synctv_core::models::StoredFileReference, String)>, ApiError> {
        let Some(reference_id) = room.cover_file_reference_id else {
            return Ok(None);
        };
        let Some(storage) = self.room_service.file_storage_service() else {
            return Ok(None);
        };
        let file_reference =
            synctv_core::repository::FileStorageRepository::new(self.room_service.pool().clone())
                .get_reference_by_id(reference_id)
                .await
                .map_err(ApiError::from)?;
        let Some(file_reference) = file_reference else {
            return Ok(None);
        };
        let url = storage
            .object_url(
                &file_reference.storage_backend,
                &file_reference.object_key,
                &synctv_core::service::room_cover_upload_policy().database_object_route_prefix,
            )
            .map_err(ApiError::from)?;
        Ok(url.map(|url| (file_reference, url)))
    }

    pub(in crate::impls::admin) async fn admin_user_to_proto_with_email(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        let email = self
            .user_service
            .get_email(&user.id)
            .await
            .map_err(ApiError::from)?;
        let presence = self
            .presence_service
            .user_stats(user.id)
            .await
            .map_err(ApiError::from)?;
        try_admin_user_to_proto(
            user,
            email.as_deref(),
            Some(&presence),
            &self.public_id_codec,
        )
    }

    pub(in crate::impls::admin) async fn admin_room_member_to_proto(
        &self,
        member: &synctv_core::models::RoomMemberWithUser,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        let room_settings = self
            .room_service
            .get_room_settings(&member.room_id)
            .await
            .map_err(ApiError::from)?;
        try_admin_room_member_to_proto_with_settings(
            member,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        )
    }

    pub(in crate::impls::admin) async fn load_admin_room_proto(
        &self,
        room: &synctv_core::models::Room,
        settings: Option<&synctv_core::models::RoomSettings>,
    ) -> Result<synctv_proto::admin::Room, ApiError> {
        let loaded_settings;
        let settings = if let Some(settings) = settings {
            settings
        } else {
            loaded_settings = self
                .room_service
                .get_room_settings(&room.id)
                .await
                .map_err(ApiError::from)?;
            &loaded_settings
        };
        let creator = self
            .user_service
            .get_user(&room.created_by)
            .await
            .map_err(ApiError::from)?;
        let creator_avatar_url = self.creator_avatar_url(&creator).await?;
        let member_count = self
            .room_service
            .get_member_count(&room.id)
            .await
            .map(Some)
            .map_err(ApiError::from)?;
        let presence = self
            .presence_service
            .room_stats(room.id)
            .await
            .map_err(ApiError::from)?;
        let cover = self.room_cover_for_admin(room).await?;

        try_managed_room_to_proto(
            room,
            Some(settings),
            member_count,
            Some(creator.username.as_str()),
            creator.status,
            creator_avatar_url.as_deref(),
            cover.as_ref().map(|(reference, _)| reference),
            cover.as_ref().map(|(_, url)| url.as_str()),
            Some(&presence),
            &self.public_id_codec,
        )
    }
}

#[cfg(test)]
pub(in crate::impls::admin) async fn active_room_stream_media_ids_for_infra(
    live_streaming_infrastructure: Option<
        &std::sync::Arc<synctv_livestream::LiveStreamingInfrastructure>,
    >,
    room_id: &RoomId,
) -> Vec<synctv_core::models::MediaId> {
    let connection_service: std::sync::Arc<dyn synctv_realtime::sync::ConnectionRuntime> =
        std::sync::Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        ));
    let realtime_lifecycle = crate::realtime_lifecycle::default_realtime_lifecycle_service(
        connection_service,
        live_streaming_infrastructure.cloned(),
        crate::realtime_fanout::disabled_realtime_fanout_service(),
    );
    realtime_lifecycle
        .active_room_stream_media_ids(room_id)
        .await
}
