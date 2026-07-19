use synctv_core::service::BanRecordListQuery;

use super::{ban_row_to_proto, i64_to_i32_api, AdminApiImpl, ApiError};

fn ban_records_query(
    api: &AdminApiImpl,
    req: &synctv_proto::admin::ListBanRecordsRequest,
) -> Result<BanRecordListQuery, ApiError> {
    let (limit, offset) =
        super::pagination_limit_offset_i64(req.page, req.page_size, "ban record")?;
    Ok(BanRecordListQuery {
        target_type: super::ban_record_target_type_from_proto(req.target_type)?,
        active: req.active,
        user_id: crate::impls::parse_optional_id_param(
            &req.user_id,
            "user_id",
            &api.public_id_codec,
        )?,
        room_id: crate::impls::parse_optional_id_param(
            &req.room_id,
            "room_id",
            &api.public_id_codec,
        )?,
        limit,
        offset,
    })
}

impl AdminApiImpl {
    pub async fn list_ban_records(
        &self,
        req: synctv_proto::admin::ListBanRecordsRequest,
        admin_user_id: &synctv_core::models::UserId,
    ) -> Result<synctv_proto::admin::ListBanRecordsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let query = ban_records_query(self, &req)?;
        let page = self.ban_record_service.list(&query).await?;

        let bans = page
            .rows
            .iter()
            .map(|row| ban_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::admin::ListBanRecordsResponse {
            bans,
            total: i64_to_i32_api(page.total, "ban record total")?,
        })
    }
}
