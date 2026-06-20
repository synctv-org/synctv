use synctv_core::{
    models::{ReviewRequestId, RoomId, UserId},
    service::{
        BanRecordListQuery, BanRecordTargetType, RoomCreationReviewListQuery,
        RoomJoinReviewListQuery, UserRegistrationReviewListQuery,
    },
};

use super::{
    ban_row_to_proto, i64_to_i32_api, normalize_non_empty_filter, pagination_limit_offset_i64,
    proto_review_status_filter, room_creation_review_row_to_proto,
    room_join_review_row_to_proto, user_registration_review_row_to_proto, AdminApiImpl, ApiError,
    RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};

impl AdminApiImpl {
    async fn load_user_registration_review(
        &self,
        request_id: UserId,
    ) -> Result<synctv_proto::admin::UserRegistrationReview, ApiError> {
        let row = self
            .review_service
            .load_user_registration(request_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))?;
        user_registration_review_row_to_proto(&row, &self.public_id_codec)
    }

    async fn load_room_creation_review(
        &self,
        request_id: RoomId,
    ) -> Result<synctv_proto::admin::RoomCreationReview, ApiError> {
        let row = self
            .review_service
            .load_room_creation(request_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))?;
        room_creation_review_row_to_proto(&row, &self.public_id_codec)
    }

    async fn load_room_join_review(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<synctv_proto::admin::RoomJoinReview, ApiError> {
        let row = self
            .review_service
            .load_room_join(request_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))?;
        room_join_review_row_to_proto(&row, &self.public_id_codec)
    }

    async fn load_join_review_target(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<(RoomId, UserId), ApiError> {
        self.review_service
            .load_room_join_target(request_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))
    }

    pub async fn list_user_registration_reviews(
        &self,
        req: synctv_proto::admin::ListUserRegistrationReviewsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListUserRegistrationReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let (limit, offset) =
            pagination_limit_offset_i64(req.page, req.page_size, "user registration review")?;
        let page = self
            .review_service
            .list_user_registrations(&UserRegistrationReviewListQuery {
                status: proto_review_status_filter(req.status)?,
                search: Some(req.search.clone()).filter(|search| !search.is_empty()),
                limit,
                offset,
            })
            .await?;
        let reviews = page
            .rows
            .iter()
            .map(|row| user_registration_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::admin::ListUserRegistrationReviewsResponse {
            reviews,
            total: i64_to_i32_api(page.total, "user registration review total")?,
        })
    }

    pub async fn approve_user_registration_review(
        &self,
        req: synctv_proto::admin::ApproveUserRegistrationReviewRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::ApproveUserRegistrationReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_request_id =
            crate::impls::proto_validated_user_id(req.request_id, &self.public_id_codec)?;
        let user = self
            .approve_user_registration_request(user_request_id, admin_user_id, ctx)
            .await?;
        Ok(synctv_proto::admin::ApproveUserRegistrationReviewResponse {
            review: Some(self.load_user_registration_review(user_request_id).await?),
            user: Some(user),
        })
    }

    pub async fn reject_user_registration_review(
        &self,
        req: synctv_proto::admin::RejectUserRegistrationReviewRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::RejectUserRegistrationReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let user_request_id =
            crate::impls::proto_validated_user_id(req.request_id, &self.public_id_codec)?;
        let reviewed_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id);
        self.user_service
            .reject_registration_request(&user_request_id, reviewed_by, &req.reason)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::admin::RejectUserRegistrationReviewResponse {
            review: Some(self.load_user_registration_review(user_request_id).await?),
            success: true,
        })
    }

    pub async fn list_room_creation_reviews(
        &self,
        req: synctv_proto::admin::ListRoomCreationReviewsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListRoomCreationReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let (limit, offset) =
            pagination_limit_offset_i64(req.page, req.page_size, "room creation review")?;
        let requested_by_filter = crate::impls::parse_optional_id_param(
            &req.requested_by,
            "requested_by",
            &self.public_id_codec,
        )?;

        let page = self
            .review_service
            .list_room_creations(&RoomCreationReviewListQuery {
                status: proto_review_status_filter(req.status)?,
                requested_by: requested_by_filter,
                search: Some(req.search.clone()).filter(|search| !search.is_empty()),
                limit,
                offset,
            })
            .await?;
        let reviews = page
            .rows
            .iter()
            .map(|row| room_creation_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::admin::ListRoomCreationReviewsResponse {
            reviews,
            total: i64_to_i32_api(page.total, "room creation review total")?,
        })
    }

    pub async fn approve_room_creation_review(
        &self,
        req: synctv_proto::admin::ApproveRoomCreationReviewRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::ApproveRoomCreationReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_request_id =
            crate::impls::proto_validated_room_id(req.request_id, &self.public_id_codec)?;
        let room = self
            .approve_room_creation_request(room_request_id, admin_user_id, ctx)
            .await?;
        Ok(synctv_proto::admin::ApproveRoomCreationReviewResponse {
            review: Some(self.load_room_creation_review(room_request_id).await?),
            room: Some(room),
        })
    }

    pub async fn reject_room_creation_review(
        &self,
        req: synctv_proto::admin::RejectRoomCreationReviewRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::RejectRoomCreationReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let room_request_id =
            crate::impls::proto_validated_room_id(req.request_id, &self.public_id_codec)?;
        let reviewed_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id);
        self.room_service
            .reject_room(
                room_request_id,
                reviewed_by,
                normalize_non_empty_filter(&req.reason),
            )
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::RejectRoomCreationReviewResponse {
            review: Some(self.load_room_creation_review(room_request_id).await?),
            success: true,
        })
    }

    pub async fn list_room_join_reviews(
        &self,
        req: synctv_proto::admin::ListRoomJoinReviewsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListRoomJoinReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let (limit, offset) =
            pagination_limit_offset_i64(req.page, req.page_size, "room join review")?;
        let room_id_filter = crate::impls::parse_optional_id_param(
            &req.room_id,
            "room_id",
            &self.public_id_codec,
        )?;
        let user_id_filter = crate::impls::parse_optional_id_param(
            &req.user_id,
            "user_id",
            &self.public_id_codec,
        )?;

        let page = self
            .review_service
            .list_room_joins(&RoomJoinReviewListQuery {
                status: proto_review_status_filter(req.status)?,
                room_id: room_id_filter,
                user_id: user_id_filter,
                search: None,
                limit,
                offset,
            })
            .await?;
        let reviews = page
            .rows
            .iter()
            .map(|row| room_join_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::admin::ListRoomJoinReviewsResponse {
            reviews,
            total: i64_to_i32_api(page.total, "room join review total")?,
        })
    }

    pub async fn approve_room_join_review(
        &self,
        req: synctv_proto::admin::ApproveRoomJoinReviewRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::ApproveRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let (room_id, _) = self.load_join_review_target(request_id).await?;
        let room_id_str = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::InvalidInput)?;
        let member = self
            .approve_room_join_request(&room_id_str, request_id, admin_user_id, ctx)
            .await?;
        Ok(synctv_proto::admin::ApproveRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(request_id).await?),
            member: Some(member),
        })
    }

    pub async fn reject_room_join_review(
        &self,
        req: synctv_proto::admin::RejectRoomJoinReviewRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::RejectRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let (room_id, _) = self.load_join_review_target(request_id).await?;
        let room_id_str = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::InvalidInput)?;
        self.reject_room_join_request(
            &room_id_str,
            request_id,
            req.reason.as_str(),
            admin_user_id,
            ctx,
        )
        .await?;
        Ok(synctv_proto::admin::RejectRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(request_id).await?),
            success: true,
        })
    }

    pub async fn list_ban_records(
        &self,
        req: synctv_proto::admin::ListBanRecordsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListBanRecordsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let (limit, offset) = pagination_limit_offset_i64(req.page, req.page_size, "ban record")?;

        let user_id_filter = crate::impls::parse_optional_id_param(
            &req.user_id,
            "user_id",
            &self.public_id_codec,
        )?;
        let room_id_filter = crate::impls::parse_optional_id_param(
            &req.room_id,
            "room_id",
            &self.public_id_codec,
        )?;

        let page = self
            .ban_record_service
            .list(&BanRecordListQuery {
                target_type: match synctv_proto::admin::BanTargetType::try_from(req.target_type) {
                    Ok(synctv_proto::admin::BanTargetType::User) => Some(BanRecordTargetType::User),
                    Ok(synctv_proto::admin::BanTargetType::Room) => Some(BanRecordTargetType::Room),
                    _ => None,
                },
                active: req.active,
                user_id: user_id_filter,
                room_id: room_id_filter,
                limit,
                offset,
            })
            .await?;

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

    async fn approve_user_registration_request(
        &self,
        request_id: UserId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        self.require_admin_actor(admin_user_id).await?;
        let persisted_reviewed_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id);
        let updated = self
            .user_service
            .approve_registration_request(&request_id, persisted_reviewed_by)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserApproved,
            synctv_core::models::AuditTargetType::User,
            Some(updated.id.to_string()),
            serde_json::json!({
                "request_id": request_id.to_string(),
                "target_user_id": updated.id.to_string(),
                "target_username": updated.username,
                "previous_review_status": "pending",
                "new_review_status": "approved",
            }),
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }
}
