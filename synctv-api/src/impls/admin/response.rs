use futures::{stream, StreamExt as _, TryStreamExt as _};
#[cfg(test)]
use synctv_core::models::RoomId;
use synctv_core::models::UserId;
use synctv_core::service::FileStorageService;

use super::{
    try_admin_room_member_to_proto_with_settings, try_admin_user_to_proto,
    try_managed_room_to_proto, AdminApiImpl, ApiError,
};

const ADMIN_ROOM_ASSET_CONCURRENCY: usize = 16;
const ADMIN_USER_LOAD_CONCURRENCY: usize = 16;

pub(in crate::impls::admin) type AdminRoomCover = (
    synctv_core::models::StoredFileReference,
    crate::impls::stored_files::StoredFileObjectAccess,
);

fn stored_file_reference_rendered_url(
    storage: &dyn FileStorageService,
    file_reference: &synctv_core::models::StoredFileReference,
    object_kind: synctv_core::models::FileObjectKind,
) -> Result<Option<String>, ApiError> {
    Ok(
        crate::impls::stored_files::stored_file_reference_access_for_kind(
            storage,
            file_reference,
            object_kind,
        )?
        .and_then(|access| crate::impls::stored_files::stored_file_object_access_url(&access)),
    )
}

impl AdminApiImpl {
    pub(in crate::impls::admin) async fn load_creator_user_map(
        &self,
        creator_ids: &[UserId],
    ) -> Result<std::collections::HashMap<UserId, synctv_core::models::User>, ApiError> {
        let users = self
            .user_service
            .get_users_by_ids(creator_ids)
            .await
            .map_err(ApiError::from)?;
        Ok(users.into_iter().map(|user| (user.id, user)).collect())
    }

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
        let file_reference = self
            .user_service
            .get_stored_file_reference(reference_id)
            .await
            .map_err(ApiError::from)?;
        let Some(file_reference) = file_reference else {
            return Ok(None);
        };

        stored_file_reference_rendered_url(
            storage.as_ref(),
            &file_reference,
            synctv_core::service::user_avatar_upload_policy().object_kind,
        )
    }

    pub(in crate::impls::admin) async fn room_cover_for_admin(
        &self,
        room: &synctv_core::models::Room,
    ) -> Result<Option<AdminRoomCover>, ApiError> {
        let Some(reference_id) = room.cover_file_reference_id else {
            return Ok(None);
        };
        let Some(storage) = self.room_service.file_storage_service() else {
            return Ok(None);
        };
        let file_reference = self
            .room_service
            .get_stored_file_reference(reference_id)
            .await
            .map_err(ApiError::from)?;
        let Some(file_reference) = file_reference else {
            return Ok(None);
        };
        let access = crate::impls::stored_files::stored_file_reference_access_for_kind(
            storage.as_ref(),
            &file_reference,
            synctv_core::service::room_cover_upload_policy().object_kind,
        )?;
        Ok(access.map(|access| (file_reference, access)))
    }

    pub(in crate::impls::admin) async fn load_admin_room_list_assets(
        &self,
        rooms: &[synctv_core::models::Room],
        creators: &std::collections::HashMap<UserId, synctv_core::models::User>,
    ) -> Result<
        (
            std::collections::HashMap<UserId, Option<String>>,
            std::collections::HashMap<synctv_core::models::RoomId, Option<AdminRoomCover>>,
        ),
        ApiError,
    > {
        let avatar_urls = stream::iter(creators.keys().copied())
            .map(|creator_id| async move {
                let creator = &creators[&creator_id];
                self.creator_avatar_url(creator)
                    .await
                    .map(|url| (creator_id, url))
            })
            .buffered(ADMIN_ROOM_ASSET_CONCURRENCY)
            .try_collect::<std::collections::HashMap<_, _>>();
        let room_covers = stream::iter(0..rooms.len())
            .map(|index| async move {
                let room = &rooms[index];
                self.room_cover_for_admin(room)
                    .await
                    .map(|cover| (room.id, cover))
            })
            .buffered(ADMIN_ROOM_ASSET_CONCURRENCY)
            .try_collect::<std::collections::HashMap<_, _>>();

        tokio::try_join!(avatar_urls, room_covers)
    }

    pub(in crate::impls::admin) async fn admin_user_to_proto_with_email(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        let (email, presence) = tokio::join!(
            self.user_service.get_email(&user.id),
            self.presence_service.user_stats(user.id),
        );
        let email = email.map_err(ApiError::from)?;
        let presence = presence.map_err(ApiError::from)?;
        try_admin_user_to_proto(
            user,
            email.as_deref(),
            Some(&presence),
            &self.public_id_codec,
        )
    }

    pub(in crate::impls::admin) async fn admin_users_to_proto_with_email(
        &self,
        users: &[synctv_core::models::User],
    ) -> Result<Vec<synctv_proto::admin::AdminUser>, ApiError> {
        stream::iter(0..users.len())
            .map(|index| async move { self.admin_user_to_proto_with_email(&users[index]).await })
            .buffered(ADMIN_USER_LOAD_CONCURRENCY)
            .try_collect()
            .await
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
        let ((creator, creator_avatar_url), member_count, presence, cover) = tokio::try_join!(
            async {
                let creator = self
                    .user_service
                    .get_user(&room.created_by)
                    .await
                    .map_err(ApiError::from)?;
                let creator_avatar_url = self.creator_avatar_url(&creator).await?;
                Ok::<_, ApiError>((creator, creator_avatar_url))
            },
            async {
                self.room_service
                    .get_member_count(&room.id)
                    .await
                    .map(Some)
                    .map_err(ApiError::from)
            },
            async {
                self.presence_service
                    .room_stats(room.id)
                    .await
                    .map_err(ApiError::from)
            },
            self.room_cover_for_admin(room),
        )?;

        try_managed_room_to_proto(
            room,
            Some(settings),
            member_count,
            Some(creator.username.as_str()),
            creator.status,
            creator_avatar_url.as_deref(),
            cover.as_ref().map(|(reference, _)| reference),
            cover.as_ref().map(|(_, access)| access),
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
