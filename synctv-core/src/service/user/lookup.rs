use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    cache::{KeyBuilder, UsernameCache},
    models::{StoredFileReference, User, UserId, UserLifecycleMetadata},
    repository::FileStorageRepository,
    service::{file_storage::FileStorageService, TokenBlacklistStore, UserService},
    Error, Result,
};

impl UserService {
    pub async fn get_user(&self, user_id: &UserId) -> Result<User> {
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    /// Load an account for administrative lifecycle operations, including a
    /// soft-deleted account that remains inside its recovery window.
    pub async fn get_user_for_admin(&self, user_id: &UserId) -> Result<User> {
        self.repository
            .get_by_id_including_deleted(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    pub async fn get_user_lifecycle_metadata(
        &self,
        user_id: &UserId,
    ) -> Result<UserLifecycleMetadata> {
        self.repository
            .get_lifecycle_metadata(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User lifecycle metadata not found".to_string()))
    }

    pub async fn get_user_lifecycle_metadata_by_ids_eventually_consistent(
        &self,
        user_ids: &[UserId],
    ) -> Result<Vec<UserLifecycleMetadata>> {
        self.repository
            .get_lifecycle_metadata_by_ids_eventually_consistent(user_ids)
            .await
    }

    pub async fn list_users(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository.list(query).await
    }

    pub async fn list_users_eventually_consistent(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository.list_eventually_consistent(query).await
    }

    pub async fn list_admins(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository.list_admins(query).await
    }

    pub async fn list_admins_eventually_consistent(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository
            .list_admins_eventually_consistent(query)
            .await
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.repository.pool()
    }

    #[must_use]
    pub fn eventually_consistent_pool(&self) -> &PgPool {
        self.repository.eventually_consistent_pool()
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn FileStorageService>> {
        self.file_storage_service.as_ref()
    }

    pub async fn get_stored_file_reference(
        &self,
        file_reference_id: i64,
    ) -> Result<Option<StoredFileReference>> {
        FileStorageRepository::new(self.repository.pool().clone())
            .get_reference_by_id(file_reference_id)
            .await
    }

    pub fn access_token_duration_seconds(&self) -> Result<i64> {
        self.jwt_service.access_token_duration_seconds()
    }

    #[must_use]
    pub const fn username_cache(&self) -> &UsernameCache {
        &self.username_cache
    }

    #[must_use]
    pub fn token_blacklist_store(&self) -> Arc<dyn TokenBlacklistStore> {
        Arc::clone(&self.token_blacklist)
    }

    #[must_use]
    pub const fn key_builder(&self) -> &KeyBuilder {
        &self.key_builder
    }

    pub async fn health_check(&self) -> Result<()> {
        sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
            .fetch_one(self.pool())
            .await?;

        Ok(())
    }
}
