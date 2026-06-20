use synctv_core::{
    models::{AuditAction, AuditTargetType, ContentReportStatus, ContentReportTargetType, UserId},
    service::{ContentReportListQuery, ContentReportListScope},
};

use super::{
    content_report_row_to_proto, i64_to_i32_api, pagination_limit_offset_i64, AdminApiImpl,
    ApiError, RequestContext,
};

impl AdminApiImpl {
    pub async fn list_content_reports(
        &self,
        req: synctv_proto::admin::ListContentReportsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListContentReportsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let (limit, offset) =
            pagination_limit_offset_i64(req.page, req.page_size, "content report")?;

        let reporter_user_id = crate::impls::parse_optional_id_param(
            &req.reporter_user_id,
            "reporter_user_id",
            &self.public_id_codec,
        )?;
        let room_id = crate::impls::parse_optional_id_param(
            &req.room_id,
            "room_id",
            &self.public_id_codec,
        )?;
        let target_room_id = crate::impls::parse_optional_id_param(
            &req.target_room_id,
            "target_room_id",
            &self.public_id_codec,
        )?;
        let target_user_id = crate::impls::parse_optional_id_param(
            &req.target_user_id,
            "target_user_id",
            &self.public_id_codec,
        )?;
        let target_member_room_id = crate::impls::parse_optional_id_param(
            &req.target_member_room_id,
            "target_member_room_id",
            &self.public_id_codec,
        )?;
        let target_member_user_id = crate::impls::parse_optional_id_param(
            &req.target_member_user_id,
            "target_member_user_id",
            &self.public_id_codec,
        )?;
        let target_chat_message_id = if req.target_chat_message_id <= 0 {
            None
        } else {
            Some(req.target_chat_message_id)
        };

        let page = self
            .content_report_service
            .list_reports(ContentReportListQuery {
                status: report_status_from_proto(req.status)?,
                target_type: report_target_type_from_proto(req.target_type)?,
                reporter_user_id,
                room_id,
                target_room_id,
                target_user_id,
                target_member_room_id,
                target_member_user_id,
                target_chat_message_id,
                scope: report_scope_from_proto(req.scope)?,
                search: req.search,
                limit,
                offset,
            })
            .await?;

        let reports = page
            .rows
            .iter()
            .map(|row| content_report_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::admin::ListContentReportsResponse {
            reports,
            total: i64_to_i32_api(page.total, "content report total")?,
        })
    }

    pub async fn get_content_report(
        &self,
        req: synctv_proto::admin::GetContentReportRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::GetContentReportResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let report_id = self
            .public_id_codec
            .decode_content_report_id(&req.report_id)
            .map_err(ApiError::InvalidInput)?;
        let report = self.content_report_service.get_report(report_id).await?;
        Ok(synctv_proto::admin::GetContentReportResponse {
            report: Some(content_report_row_to_proto(&report, &self.public_id_codec)?),
        })
    }

    pub async fn update_content_report_status(
        &self,
        req: synctv_proto::admin::UpdateContentReportStatusRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateContentReportStatusResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let report_id = self
            .public_id_codec
            .decode_content_report_id(&req.report_id)
            .map_err(ApiError::InvalidInput)?;
        let status = required_report_status_from_proto(req.status)?;
        let report = self
            .content_report_service
            .update_status(report_id, status, *admin_user_id, req.resolution_note)
            .await?;

        self.log_admin_action(
            admin_user_id,
            AuditAction::ContentReportStatusUpdated,
            AuditTargetType::ContentReport,
            Some(report.id.to_string()),
            serde_json::json!({
                "report_id": report.id.to_string(),
                "status": report.status.as_str(),
                "target_type": report.target_type.as_str(),
                "reporter_user_id": report.reporter_user_id.to_string(),
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::UpdateContentReportStatusResponse {
            report: Some(content_report_row_to_proto(&report, &self.public_id_codec)?),
            success: true,
        })
    }
}

fn report_target_type_from_proto(value: i32) -> Result<Option<ContentReportTargetType>, ApiError> {
    match synctv_proto::admin::ContentReportTargetType::try_from(value) {
        Ok(synctv_proto::admin::ContentReportTargetType::Unspecified) => Ok(None),
        Ok(synctv_proto::admin::ContentReportTargetType::Room) => {
            Ok(Some(ContentReportTargetType::Room))
        }
        Ok(synctv_proto::admin::ContentReportTargetType::User) => {
            Ok(Some(ContentReportTargetType::User))
        }
        Ok(synctv_proto::admin::ContentReportTargetType::RoomMember) => {
            Ok(Some(ContentReportTargetType::RoomMember))
        }
        Ok(synctv_proto::admin::ContentReportTargetType::ChatMessage) => {
            Ok(Some(ContentReportTargetType::ChatMessage))
        }
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid content report target type".to_string(),
        )),
    }
}

fn report_scope_from_proto(value: i32) -> Result<ContentReportListScope, ApiError> {
    match synctv_proto::admin::ContentReportScope::try_from(value) {
        Ok(
            synctv_proto::admin::ContentReportScope::Unspecified
            | synctv_proto::admin::ContentReportScope::AnyRelated,
        ) => Ok(ContentReportListScope::AnyRelated),
        Ok(synctv_proto::admin::ContentReportScope::RoomContext) => {
            Ok(ContentReportListScope::RoomContext)
        }
        Ok(synctv_proto::admin::ContentReportScope::TargetRoom) => {
            Ok(ContentReportListScope::TargetRoom)
        }
        Ok(synctv_proto::admin::ContentReportScope::TargetUser) => {
            Ok(ContentReportListScope::TargetUser)
        }
        Ok(synctv_proto::admin::ContentReportScope::TargetMember) => {
            Ok(ContentReportListScope::TargetMember)
        }
        Ok(synctv_proto::admin::ContentReportScope::TargetChatMessage) => {
            Ok(ContentReportListScope::TargetChatMessage)
        }
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid content report scope".to_string(),
        )),
    }
}

fn report_status_from_proto(value: i32) -> Result<Option<ContentReportStatus>, ApiError> {
    match synctv_proto::admin::ContentReportStatus::try_from(value) {
        Ok(synctv_proto::admin::ContentReportStatus::Unspecified) => Ok(None),
        Ok(synctv_proto::admin::ContentReportStatus::Open) => Ok(Some(ContentReportStatus::Open)),
        Ok(synctv_proto::admin::ContentReportStatus::Reviewing) => {
            Ok(Some(ContentReportStatus::Reviewing))
        }
        Ok(synctv_proto::admin::ContentReportStatus::Resolved) => {
            Ok(Some(ContentReportStatus::Resolved))
        }
        Ok(synctv_proto::admin::ContentReportStatus::Dismissed) => {
            Ok(Some(ContentReportStatus::Dismissed))
        }
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid content report status".to_string(),
        )),
    }
}

fn required_report_status_from_proto(value: i32) -> Result<ContentReportStatus, ApiError> {
    report_status_from_proto(value)?
        .ok_or_else(|| ApiError::InvalidInput("Content report status is required".to_string()))
}
