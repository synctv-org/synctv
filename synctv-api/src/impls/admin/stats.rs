use super::{i64_to_i32_api, usize_to_i32_api, AdminApiImpl, ApiError};

impl AdminApiImpl {
    pub async fn get_system_stats(
        &self,
        _req: synctv_proto::admin::GetSystemStatsRequest,
    ) -> Result<synctv_proto::admin::GetSystemStatsResponse, ApiError> {
        let stats_pagination = synctv_core::models::PageParams::new(Some(1), Some(1));
        let query_all = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let query_active = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Active),
            ..Default::default()
        };
        let query_banned = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Banned),
            ..Default::default()
        };
        let room_query_all = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let room_query_active = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::RoomStatus::Active),
            ..Default::default()
        };
        let room_query_banned = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            is_banned: Some(true),
            ..Default::default()
        };

        let (
            total_users_res,
            active_users_res,
            banned_users_res,
            total_rooms_res,
            active_rooms_res,
            banned_rooms_res,
            provider_count_res,
            total_media_res,
            presence_res,
        ) = tokio::join!(
            self.user_service.list_users(&query_all),
            self.user_service.list_users(&query_active),
            self.user_service.list_users(&query_banned),
            self.room_service.list_rooms(&room_query_all),
            self.room_service.list_rooms(&room_query_active),
            self.room_service.list_rooms(&room_query_banned),
            self.provider_instance_manager.get_all_instances(),
            self.room_service.media_service().count_all_media(),
            self.presence_service.overview(),
        );

        let (_, total_users) = total_users_res.map_err(ApiError::from)?;
        let (_, active_users) = active_users_res.map_err(ApiError::from)?;
        let (_, banned_users) = banned_users_res.map_err(ApiError::from)?;
        let (_, total_rooms) = total_rooms_res.map_err(ApiError::from)?;
        let (_, active_rooms) = active_rooms_res.map_err(ApiError::from)?;
        let (_, banned_rooms) = banned_rooms_res.map_err(ApiError::from)?;
        let provider_count = usize_to_i32_api(
            provider_count_res.map_err(ApiError::from)?.len(),
            "provider instance count",
        )?;
        let total_media = i64_to_i32_api(total_media_res.map_err(ApiError::from)?, "media total")?;
        let presence = crate::impls::client::convert::presence_overview_to_proto(
            &presence_res.map_err(ApiError::from)?,
        )?;

        Ok(synctv_proto::admin::GetSystemStatsResponse {
            total_users: i64_to_i32_api(total_users, "user total")?,
            active_users: i64_to_i32_api(active_users, "active user total")?,
            banned_users: i64_to_i32_api(banned_users, "banned user total")?,
            total_rooms: i64_to_i32_api(total_rooms, "room total")?,
            active_rooms: i64_to_i32_api(active_rooms, "active room total")?,
            banned_rooms: i64_to_i32_api(banned_rooms, "banned room total")?,
            total_media,
            provider_instances: provider_count,
            additional_stats: vec![],
            presence: Some(presence),
        })
    }
}
