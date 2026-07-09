use synctv_core::{
    models::{
        ContentReportAdminRow, ContentReportStatus, ContentReportTarget, ContentReportTargetType,
        CreateContentReport, RoomId, RoomPermission, UserId,
    },
    service::{ContentReportListQuery, ContentReportListScope},
};
use synctv_proto::client::{
    report_content_request, ContentReport, ContentReportStatus as ClientContentReportStatus,
    ContentReportTargetType as ClientContentReportTargetType, GetRoomContentReportRequest,
    ListRoomContentReportsRequest, ListRoomContentReportsResponse, ReportChatMessageTarget,
    ReportContentRequest, ReportContentResponse, ReportRoomMemberTarget, ReportRoomTarget,
    ReportUserTarget, UpdateRoomContentReportStatusRequest, UpdateRoomContentReportStatusResponse,
};

use super::{ClientApiImpl, RoomActor};
use crate::impls::client::convert::{
    content_report_metadata_from_proto, content_report_metadata_to_proto,
};
use crate::impls::ApiError;

impl ClientApiImpl {
    pub async fn report_content_for_actor(
        &self,
        actor: &RoomActor,
        req: ReportContentRequest,
    ) -> Result<ReportContentResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let reporter_user_id = actor.require_user_id()?;
        let target = self.report_target_from_proto(req.target)?;
        if let Some(target_room_id) = target.room_context() {
            self.room_service
                .check_membership(&target_room_id, &reporter_user_id)
                .await
                .map_err(Self::map_room_access_error)?;
        }
        let metadata = content_report_metadata_from_proto(req.metadata.as_ref())?;
        let report = self
            .content_report_service
            .create_report(CreateContentReport {
                reporter_user_id,
                target,
                reason_code: req.reason_code,
                reason: req.reason,
                metadata,
            })
            .await
            .map_err(ApiError::from)?;

        Ok(ReportContentResponse {
            report_id: self
                .public_id_codec
                .encode_content_report_id(report.id)
                .map_err(|err| ApiError::Internal(format!("failed to encode report id: {err}")))?,
            created_at: report.created_at.timestamp(),
        })
    }

    pub async fn list_room_content_reports_for_actor(
        &self,
        actor: &RoomActor,
        req: ListRoomContentReportsRequest,
    ) -> Result<ListRoomContentReportsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        self.require_room_report_admin(room_id, user_id).await?;
        let page = page_i32_to_usize(req.page)?;
        let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100)?;
        let offset = page
            .checked_sub(1)
            .and_then(|page_index| page_index.checked_mul(page_size))
            .ok_or_else(|| ApiError::InvalidInput("content report offset overflow".to_string()))?;
        let target_member_user_id = if req.target_member_user_id.trim().is_empty() {
            None
        } else {
            Some(crate::impls::parse_user_id_param(
                &req.target_member_user_id,
                "target_member_user_id",
                &self.public_id_codec,
            )?)
        };
        let target_chat_message_id = if req.target_chat_message_id <= 0 {
            None
        } else {
            Some(req.target_chat_message_id)
        };
        let page = self
            .content_report_service
            .list_reports(ContentReportListQuery {
                status: client_report_status_from_proto(req.status)?,
                target_type: client_report_target_type_from_proto(req.target_type)?,
                reporter_user_id: None,
                room_id: Some(room_id),
                target_room_id: None,
                target_user_id: None,
                target_member_room_id: None,
                target_member_user_id,
                target_chat_message_id,
                scope: ContentReportListScope::RoomContext,
                search: req.search,
                limit: usize_to_i64(page_size, "content report page size")?,
                offset: usize_to_i64(offset, "content report offset")?,
            })
            .await
            .map_err(ApiError::from)?;

        let reports = page
            .rows
            .iter()
            .map(|row| content_report_row_to_client_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ListRoomContentReportsResponse {
            reports,
            total: i64_to_i32(page.total, "content report total")?,
        })
    }

    pub async fn get_room_content_report_for_actor(
        &self,
        actor: &RoomActor,
        req: GetRoomContentReportRequest,
    ) -> Result<ContentReport, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        self.require_room_report_admin(room_id, user_id).await?;
        let row = self
            .load_room_scoped_report(room_id, &req.report_id)
            .await?;
        content_report_row_to_client_proto(&row, &self.public_id_codec)
    }

    pub async fn update_room_content_report_status_for_actor(
        &self,
        actor: &RoomActor,
        req: UpdateRoomContentReportStatusRequest,
    ) -> Result<UpdateRoomContentReportStatusResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        self.require_room_report_admin(room_id, user_id).await?;
        let current = self
            .load_room_scoped_report(room_id, &req.report_id)
            .await?;
        let report = self
            .content_report_service
            .update_status(
                current.id,
                required_client_report_status_from_proto(req.status)?,
                user_id,
                req.resolution_note,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(UpdateRoomContentReportStatusResponse {
            report: Some(content_report_row_to_client_proto(
                &report,
                &self.public_id_codec,
            )?),
            success: true,
        })
    }

    async fn require_room_report_admin(
        &self,
        room_id: RoomId,
        user_id: UserId,
    ) -> Result<(), ApiError> {
        self.room_service
            .check_permission(&room_id, &user_id, RoomPermission::SET_ROOM_SETTINGS)
            .await
            .map_err(Self::map_room_access_error)
    }

    async fn load_room_scoped_report(
        &self,
        room_id: RoomId,
        report_id: &str,
    ) -> Result<ContentReportAdminRow, ApiError> {
        let report_id = self
            .public_id_codec
            .decode_content_report_id(report_id)
            .map_err(ApiError::InvalidInput)?;
        let row = self
            .content_report_service
            .get_report(report_id)
            .await
            .map_err(ApiError::from)?;
        let belongs_to_room = match row.target_type {
            ContentReportTargetType::Room => row.target_room_id == Some(room_id),
            ContentReportTargetType::RoomMember => row.target_member_room_id == Some(room_id),
            ContentReportTargetType::ChatMessage => row.room_id == Some(room_id),
            ContentReportTargetType::User => false,
        };
        if !belongs_to_room {
            return Err(ApiError::NotFound("Content report not found".to_string()));
        }
        Ok(row)
    }

    fn report_target_from_proto(
        &self,
        target: Option<report_content_request::Target>,
    ) -> Result<ContentReportTarget, ApiError> {
        match target
            .ok_or_else(|| ApiError::InvalidInput("report target is required".to_string()))?
        {
            report_content_request::Target::Room(ReportRoomTarget { room_id }) => {
                let room_id =
                    crate::impls::parse_room_id_param(&room_id, "room_id", &self.public_id_codec)?;
                Ok(ContentReportTarget::Room { room_id })
            }
            report_content_request::Target::User(ReportUserTarget { user_id }) => {
                let user_id =
                    crate::impls::parse_user_id_param(&user_id, "user_id", &self.public_id_codec)?;
                Ok(ContentReportTarget::User { user_id })
            }
            report_content_request::Target::RoomMember(ReportRoomMemberTarget {
                room_id,
                user_id,
            }) => {
                let room_id =
                    crate::impls::parse_room_id_param(&room_id, "room_id", &self.public_id_codec)?;
                let user_id =
                    crate::impls::parse_user_id_param(&user_id, "user_id", &self.public_id_codec)?;
                Ok(ContentReportTarget::RoomMember { room_id, user_id })
            }
            report_content_request::Target::ChatMessage(ReportChatMessageTarget {
                room_id,
                message_id,
            }) => {
                let room_id =
                    crate::impls::parse_room_id_param(&room_id, "room_id", &self.public_id_codec)?;
                let message_id = message_id.trim().parse::<i64>().map_err(|error| {
                    ApiError::InvalidInput(format!("Invalid message_id: {error}"))
                })?;
                if message_id <= 0 {
                    return Err(ApiError::InvalidInput(
                        "Invalid message_id: expected a positive integer".to_string(),
                    ));
                }
                Ok(ContentReportTarget::ChatMessage {
                    room_id,
                    message_id,
                })
            }
        }
    }
}

fn page_i32_to_usize(value: i32) -> Result<usize, ApiError> {
    if value == 0 {
        return Ok(1);
    }
    usize::try_from(value)
        .ok()
        .filter(|value| *value >= 1)
        .ok_or_else(|| ApiError::InvalidInput("page must be at least 1".to_string()))
}

fn usize_to_i64(value: usize, field: &'static str) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::InvalidInput(format!("{field} exceeds i64")))
}

fn i64_to_i32(value: i64, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::InvalidInput(format!("{field} exceeds i32")))
}

fn client_report_target_type_from_proto(
    value: i32,
) -> Result<Option<ContentReportTargetType>, ApiError> {
    match ClientContentReportTargetType::try_from(value) {
        Ok(ClientContentReportTargetType::Unspecified) => Ok(None),
        Ok(ClientContentReportTargetType::Room) => Ok(Some(ContentReportTargetType::Room)),
        Ok(ClientContentReportTargetType::User) => Ok(Some(ContentReportTargetType::User)),
        Ok(ClientContentReportTargetType::RoomMember) => {
            Ok(Some(ContentReportTargetType::RoomMember))
        }
        Ok(ClientContentReportTargetType::ChatMessage) => {
            Ok(Some(ContentReportTargetType::ChatMessage))
        }
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid content report target type".to_string(),
        )),
    }
}

fn client_report_status_from_proto(value: i32) -> Result<Option<ContentReportStatus>, ApiError> {
    match ClientContentReportStatus::try_from(value) {
        Ok(ClientContentReportStatus::Unspecified) => Ok(None),
        Ok(ClientContentReportStatus::Open) => Ok(Some(ContentReportStatus::Open)),
        Ok(ClientContentReportStatus::Reviewing) => Ok(Some(ContentReportStatus::Reviewing)),
        Ok(ClientContentReportStatus::Resolved) => Ok(Some(ContentReportStatus::Resolved)),
        Ok(ClientContentReportStatus::Dismissed) => Ok(Some(ContentReportStatus::Dismissed)),
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid content report status".to_string(),
        )),
    }
}

fn required_client_report_status_from_proto(value: i32) -> Result<ContentReportStatus, ApiError> {
    client_report_status_from_proto(value)?
        .ok_or_else(|| ApiError::InvalidInput("Content report status is required".to_string()))
}

fn content_report_target_type_to_client_proto(value: ContentReportTargetType) -> i32 {
    match value {
        ContentReportTargetType::Room => ClientContentReportTargetType::Room as i32,
        ContentReportTargetType::User => ClientContentReportTargetType::User as i32,
        ContentReportTargetType::RoomMember => ClientContentReportTargetType::RoomMember as i32,
        ContentReportTargetType::ChatMessage => ClientContentReportTargetType::ChatMessage as i32,
    }
}

fn content_report_status_to_client_proto(value: ContentReportStatus) -> i32 {
    match value {
        ContentReportStatus::Open => ClientContentReportStatus::Open as i32,
        ContentReportStatus::Reviewing => ClientContentReportStatus::Reviewing as i32,
        ContentReportStatus::Resolved => ClientContentReportStatus::Resolved as i32,
        ContentReportStatus::Dismissed => ClientContentReportStatus::Dismissed as i32,
    }
}

fn content_report_row_to_client_proto(
    row: &ContentReportAdminRow,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<ContentReport, ApiError> {
    Ok(ContentReport {
        id: public_id_codec
            .encode_content_report_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        reporter_user_id: public_id_codec
            .encode_user_id(row.reporter_user_id)
            .map_err(ApiError::InvalidInput)?,
        reporter_username: row.reporter_username.clone(),
        room_id: encode_optional_room_id(public_id_codec, row.room_id)?,
        room_name: row.room_name.clone(),
        target_type: content_report_target_type_to_client_proto(row.target_type),
        target_room_id: encode_optional_room_id(public_id_codec, row.target_room_id)?,
        target_room_name: row.target_room_name.clone(),
        target_user_id: encode_optional_user_id(public_id_codec, row.target_user_id)?,
        target_username: row.target_username.clone(),
        target_member_room_id: encode_optional_room_id(public_id_codec, row.target_member_room_id)?,
        target_member_room_name: row.target_member_room_name.clone(),
        target_member_user_id: encode_optional_user_id(public_id_codec, row.target_member_user_id)?,
        target_member_username: row.target_member_username.clone(),
        target_chat_message_id: row.target_chat_message_id.unwrap_or_default(),
        target_chat_message_created_at: row
            .target_chat_message_created_at
            .map(|time| time.timestamp())
            .unwrap_or_default(),
        target_chat_message_preview: row.target_chat_message_preview.clone(),
        reason_code: row.reason_code.clone(),
        reason: row.reason.clone(),
        metadata: content_report_metadata_to_proto(row.metadata.as_ref())?,
        status: content_report_status_to_client_proto(row.status),
        reviewed_by: encode_optional_user_id(public_id_codec, row.reviewed_by)?,
        reviewed_by_username: row.reviewed_by_username.clone(),
        reviewed_at: row
            .reviewed_at
            .map(|time| time.timestamp())
            .unwrap_or_default(),
        resolution_note: row.resolution_note.clone(),
        created_at: row.created_at.timestamp(),
        updated_at: row.updated_at.timestamp(),
    })
}

fn encode_optional_room_id(
    public_id_codec: &synctv_adapter::PublicIdCodec,
    room_id: Option<RoomId>,
) -> Result<String, ApiError> {
    room_id
        .map(|id| {
            public_id_codec
                .encode_room_id(id)
                .map_err(ApiError::InvalidInput)
        })
        .transpose()
        .map(std::option::Option::unwrap_or_default)
}

fn encode_optional_user_id(
    public_id_codec: &synctv_adapter::PublicIdCodec,
    user_id: Option<UserId>,
) -> Result<String, ApiError> {
    user_id
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(ApiError::InvalidInput)
        })
        .transpose()
        .map(std::option::Option::unwrap_or_default)
}
