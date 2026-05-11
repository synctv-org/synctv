use sqlx::PgPool;

use crate::models::{ReviewRequestId, RoomId, UserId};
use crate::repository::ReviewRepository;
use crate::Result;

pub use crate::repository::{
    ReviewPage, RoomCreationReviewListQuery, RoomCreationReviewRecord, RoomJoinReviewListQuery,
    RoomJoinReviewRecord, UserRegistrationReviewListQuery, UserRegistrationReviewRecord,
};

/// Transport-neutral review read service.
#[derive(Clone)]
pub struct ReviewService {
    repository: ReviewRepository,
}

impl ReviewService {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            repository: ReviewRepository::new(pool),
        }
    }

    pub async fn load_user_registration(
        &self,
        request_id: UserId,
    ) -> Result<Option<UserRegistrationReviewRecord>> {
        self.repository.load_user_registration(request_id).await
    }

    pub async fn list_user_registrations(
        &self,
        query: &UserRegistrationReviewListQuery,
    ) -> Result<ReviewPage<UserRegistrationReviewRecord>> {
        self.repository.list_user_registrations(query).await
    }

    pub async fn load_room_creation(
        &self,
        request_id: RoomId,
    ) -> Result<Option<RoomCreationReviewRecord>> {
        self.repository.load_room_creation(request_id).await
    }

    pub async fn list_room_creations(
        &self,
        query: &RoomCreationReviewListQuery,
    ) -> Result<ReviewPage<RoomCreationReviewRecord>> {
        self.repository.list_room_creations(query).await
    }

    pub async fn load_room_join(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        self.repository.load_room_join(request_id).await
    }

    pub async fn load_room_join_in_room(
        &self,
        request_id: ReviewRequestId,
        room_id: RoomId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        self.repository
            .load_room_join_in_room(request_id, room_id)
            .await
    }

    pub async fn load_room_join_target(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<(RoomId, UserId)>> {
        self.repository.load_room_join_target(request_id).await
    }

    pub async fn list_room_joins(
        &self,
        query: &RoomJoinReviewListQuery,
    ) -> Result<ReviewPage<RoomJoinReviewRecord>> {
        self.repository.list_room_joins(query).await
    }
}
