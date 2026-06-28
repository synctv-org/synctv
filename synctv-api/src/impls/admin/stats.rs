use super::{i64_to_i32_api, usize_to_i32_api, AdminApiImpl, ApiError};

impl AdminApiImpl {
    pub async fn get_system_stats(
        &self,
        _req: synctv_proto::admin::GetSystemStatsRequest,
    ) -> Result<synctv_proto::admin::GetSystemStatsResponse, ApiError> {
        // Fetch all stats in parallel: one optimized database query + other services
        let (stats_res, provider_count_res, total_media_res, presence_res) = tokio::join!(
            self.system_stats_service.get_system_stats(),
            self.provider_instance_manager.get_all_instances(),
            self.room_service.media_service().count_all_media(),
            self.presence_service.overview(),
        );

        let stats = stats_res.map_err(ApiError::from)?;
        let provider_count = usize_to_i32_api(
            provider_count_res.map_err(ApiError::from)?.len(),
            "provider instance count",
        )?;
        let total_media = i64_to_i32_api(total_media_res.map_err(ApiError::from)?, "media total")?;
        let presence = crate::impls::client::convert::presence_overview_to_proto(
            &presence_res.map_err(ApiError::from)?,
        )?;

        Ok(synctv_proto::admin::GetSystemStatsResponse {
            total_users: i64_to_i32_api(stats.total_users, "user total")?,
            active_users: i64_to_i32_api(stats.active_users, "active user total")?,
            banned_users: i64_to_i32_api(stats.banned_users, "banned user total")?,
            total_rooms: i64_to_i32_api(stats.total_rooms, "room total")?,
            active_rooms: i64_to_i32_api(stats.active_rooms, "active room total")?,
            banned_rooms: i64_to_i32_api(stats.banned_rooms, "banned room total")?,
            total_media,
            provider_instances: provider_count,
            additional_stats: Some(synctv_proto::admin::SystemAdditionalStats {
                active_streams: 0,
                open_reports: 0,
            }),
            presence: Some(presence),
        })
    }
}
