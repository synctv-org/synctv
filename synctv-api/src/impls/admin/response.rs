#[cfg(test)]
use synctv_core::models::RoomId;
use synctv_core::models::{UserId, UserStatus};
use synctv_core::service::UserService;

use super::{
    try_admin_room_member_to_proto_with_settings, try_admin_room_to_proto, try_admin_user_to_proto,
    AdminApiImpl, ApiError,
};

pub(in crate::impls::admin) async fn load_creator_status_map(
    user_service: &UserService,
    creator_ids: &[UserId],
) -> Result<std::collections::HashMap<UserId, UserStatus>, ApiError> {
    let users = user_service
        .get_users_by_ids(creator_ids)
        .await
        .map_err(ApiError::from)?;
    Ok(users
        .into_iter()
        .map(|user| (user.id, user.status))
        .collect())
}

pub(in crate::impls::admin) fn room_creator_status_from_map(
    statuses: &std::collections::HashMap<UserId, UserStatus>,
    room: &synctv_core::models::Room,
) -> Result<UserStatus, ApiError> {
    statuses.get(&room.created_by).copied().ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing creator status for admin room {} creator {}",
            room.id, room.created_by
        ))
    })
}

pub(in crate::impls::admin) async fn load_room_creator_status(
    user_service: &UserService,
    room: &synctv_core::models::Room,
) -> Result<UserStatus, ApiError> {
    match user_service.get_user(&room.created_by).await {
        Ok(user) => Ok(user.status),
        Err(synctv_core::Error::NotFound(_)) => Ok(UserStatus::Banned),
        Err(error) => Err(ApiError::from(error)),
    }
}

impl AdminApiImpl {
    pub(in crate::impls::admin) async fn admin_user_to_proto_with_email(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        let email = self
            .user_service
            .get_email(&user.id)
            .await
            .map_err(ApiError::from)?;
        try_admin_user_to_proto(user, email.as_deref(), &self.public_id_codec)
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
    ) -> Result<synctv_proto::admin::AdminRoom, ApiError> {
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
        let creator_username = self
            .user_service
            .get_usernames(std::slice::from_ref(&room.created_by))
            .await
            .map_err(ApiError::from)?
            .into_values()
            .next();
        let creator_status = load_room_creator_status(&self.user_service, room).await?;
        let member_count = self
            .room_service
            .get_member_count(&room.id)
            .await
            .map(Some)
            .map_err(ApiError::from)?;

        try_admin_room_to_proto(
            room,
            Some(settings),
            member_count,
            creator_username.as_deref(),
            creator_status,
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
