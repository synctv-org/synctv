use chrono::{DateTime, Utc};

use crate::{
    models::{BlockedUser, PageParams, UserId},
    Error, Result,
};

use super::UserService;

impl UserService {
    pub async fn block_user(
        &self,
        blocker_user_id: &UserId,
        blocked_user_id: &UserId,
    ) -> Result<DateTime<Utc>> {
        if blocker_user_id == blocked_user_id {
            return Err(Error::InvalidInput("Cannot block yourself".to_string()));
        }
        if self.repository.get_by_id(blocked_user_id).await?.is_none() {
            return Err(Error::NotFound("User not found".to_string()));
        }
        self.repository
            .block_user(blocker_user_id, blocked_user_id)
            .await
    }

    pub async fn unblock_user(
        &self,
        blocker_user_id: &UserId,
        blocked_user_id: &UserId,
    ) -> Result<bool> {
        self.repository
            .unblock_user(blocker_user_id, blocked_user_id)
            .await
    }

    pub async fn is_blocking(
        &self,
        blocker_user_id: &UserId,
        blocked_user_id: &UserId,
    ) -> Result<bool> {
        self.repository
            .is_blocking(blocker_user_id, blocked_user_id)
            .await
    }

    pub async fn blocked_user_ids(&self, blocker_user_id: &UserId) -> Result<Vec<UserId>> {
        self.repository.blocked_user_ids(blocker_user_id).await
    }

    pub async fn blocked_user_ids_eventually_consistent(
        &self,
        blocker_user_id: &UserId,
    ) -> Result<Vec<UserId>> {
        self.repository
            .blocked_user_ids_eventually_consistent(blocker_user_id)
            .await
    }

    pub async fn list_blocked_users(
        &self,
        blocker_user_id: &UserId,
        pagination: PageParams,
        search: Option<&str>,
    ) -> Result<(Vec<BlockedUser>, i64)> {
        self.repository
            .list_blocked_users(blocker_user_id, pagination, search)
            .await
    }
}
