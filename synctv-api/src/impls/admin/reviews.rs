use synctv_core::models::{AuditDetails, ReviewRequestId, RoomId, UserId};

use super::{
    i64_to_i32_api, normalize_non_empty_filter, room_creation_review_row_to_proto,
    room_join_review_row_to_proto, user_registration_review_row_to_proto, AdminApiImpl, ApiError,
    RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};

fn user_registration_reviews_query(
    req: &synctv_proto::admin::ListUserRegistrationReviewsRequest,
) -> Result<synctv_core::service::UserRegistrationReviewListQuery, ApiError> {
    let (limit, offset) =
        super::pagination_limit_offset_i64(req.page, req.page_size, "user registration review")?;
    Ok(synctv_core::service::UserRegistrationReviewListQuery {
        status: super::proto_review_status_filter(req.status)?,
        search: normalize_non_empty_filter(&req.search),
        limit,
        offset,
    })
}

fn room_creation_reviews_query(
    req: &synctv_proto::admin::ListRoomCreationReviewsRequest,
    api: &AdminApiImpl,
) -> Result<synctv_core::service::RoomCreationReviewListQuery, ApiError> {
    let (limit, offset) =
        super::pagination_limit_offset_i64(req.page, req.page_size, "room creation review")?;
    Ok(synctv_core::service::RoomCreationReviewListQuery {
        status: super::proto_review_status_filter(req.status)?,
        requested_by: crate::impls::parse_optional_id_param(
            &req.requested_by,
            "requested_by",
            &api.public_id_codec,
        )?,
        search: normalize_non_empty_filter(&req.search),
        limit,
        offset,
    })
}

fn room_join_reviews_query(
    req: &synctv_proto::admin::ListRoomJoinReviewsRequest,
    api: &AdminApiImpl,
) -> Result<synctv_core::service::RoomJoinReviewListQuery, ApiError> {
    let (limit, offset) =
        super::pagination_limit_offset_i64(req.page, req.page_size, "room join review")?;
    Ok(synctv_core::service::RoomJoinReviewListQuery {
        status: super::proto_review_status_filter(req.status)?,
        room_id: crate::impls::parse_optional_id_param(
            &req.room_id,
            "room_id",
            &api.public_id_codec,
        )?,
        user_id: crate::impls::parse_optional_id_param(
            &req.user_id,
            "user_id",
            &api.public_id_codec,
        )?,
        search: None,
        limit,
        offset,
    })
}

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
        let query = user_registration_reviews_query(&req)?;
        let page = self.review_service.list_user_registrations(&query).await?;
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
    ) -> Result<synctv_proto::admin::UserRegistrationReview, ApiError> {
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

        self.load_user_registration_review(user_request_id).await
    }

    pub async fn list_room_creation_reviews(
        &self,
        req: synctv_proto::admin::ListRoomCreationReviewsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListRoomCreationReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let query = room_creation_reviews_query(&req, self)?;
        let page = self.review_service.list_room_creations(&query).await?;
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
    ) -> Result<synctv_proto::admin::RoomCreationReview, ApiError> {
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
        self.load_room_creation_review(room_request_id).await
    }

    pub async fn list_room_join_reviews(
        &self,
        req: synctv_proto::admin::ListRoomJoinReviewsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ListRoomJoinReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let query = room_join_reviews_query(&req, self)?;
        let page = self.review_service.list_room_joins(&query).await?;
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
        let member = self
            .approve_room_join_request(room_id, request_id, admin_user_id, ctx)
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
    ) -> Result<synctv_proto::admin::RoomJoinReview, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_admin_actor(admin_user_id).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let (room_id, _) = self.load_join_review_target(request_id).await?;
        self.reject_room_join_request(room_id, request_id, req.reason.as_str(), admin_user_id, ctx)
            .await?;
        self.load_room_join_review(request_id).await
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
            AuditDetails {
                request_id: Some(request_id.to_string()),
                target_user_id: Some(updated.id.to_string()),
                target_username: Some(updated.username.clone()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("approved".to_string()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }
}
