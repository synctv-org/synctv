use std::sync::Arc;

use crate::{
    cache::{CacheDomain, CacheInvalidationRuntime, MemberPermissionKey},
    models::{RoomId, UserId},
    Result,
};

use super::{PermissionCacheFence, PermissionService, PermissionWriteFence};

impl PermissionService {
    pub(crate) async fn invalidate_cache_local_only(&self, room_id: &RoomId, user_id: &UserId) {
        let cache_key = MemberPermissionKey::new(*room_id, *user_id);
        if let Err(error) = self.member_permission_cache.invalidate(&cache_key).await {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                error = %error,
                "Failed to invalidate member permission source cache"
            );
        }
    }

    pub(crate) async fn invalidate_room_cache_local_only(&self, room_id: &RoomId) {
        self.member_permission_cache.clear().await;
        if let Err(error) = self.room_settings_cache.invalidate(room_id).await {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to invalidate permission room settings source cache"
            );
        }
    }

    pub(crate) async fn clear_cache_local_only(&self) {
        self.member_permission_cache.clear().await;
        self.room_settings_cache.clear().await;
    }

    pub(crate) fn invalidation_service(&self) -> Option<Arc<dyn CacheInvalidationRuntime>> {
        self.invalidation_service.service.read().clone()
    }

    pub(crate) fn permission_domain(room_id: &RoomId, user_id: &UserId) -> CacheDomain {
        CacheDomain::Permission {
            room_id: *room_id,
            user_id: *user_id,
        }
    }

    pub(crate) fn room_settings_domain(room_id: &RoomId) -> CacheDomain {
        CacheDomain::RoomSettings { room_id: *room_id }
    }

    pub(super) async fn current_permission_cache_fence(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<PermissionCacheFence>> {
        if !self.consistency.is_authoritative() {
            return Ok(None);
        }

        let user_domain = Self::permission_domain(room_id, user_id);
        let room_settings_domain = Self::room_settings_domain(room_id);
        let versions = self
            .consistency
            .current_versions(&[user_domain.clone(), room_settings_domain.clone()])
            .await?;
        let Some(user_version) = versions.first().and_then(|version| *version) else {
            return Ok(None);
        };
        let Some(room_settings_version) = versions.get(1).and_then(|version| *version) else {
            return Ok(None);
        };

        Ok(Some(PermissionCacheFence {
            user_version,
            room_settings_version,
            user_fence_key: self.consistency.fence_key(&user_domain),
            room_settings_fence_key: self.consistency.fence_key(&room_settings_domain),
        }))
    }

    pub(crate) async fn begin_permission_write(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        db_version: i64,
    ) -> Result<PermissionWriteFence> {
        let domain = Self::permission_domain(room_id, user_id);
        let reservation = self
            .consistency
            .begin_observed_write(&domain, db_version)
            .await?;
        let version = reservation
            .as_ref()
            .map_or(0, |reservation| reservation.version);
        Ok(PermissionWriteFence {
            domain,
            reservation,
            version,
        })
    }

    pub(crate) async fn commit_permission_write(
        &self,
        fence: &PermissionWriteFence,
        version: i64,
    ) -> Result<()> {
        self.consistency
            .commit_reserved_write(&fence.domain, fence.reservation.as_ref(), version)
            .await?;
        Ok(())
    }

    pub(crate) async fn abort_permission_write(&self, fence: &PermissionWriteFence) {
        self.consistency
            .abort_reserved_write(&fence.domain, fence.reservation.as_ref())
            .await;
    }

    pub(crate) async fn advance_permission_fence_to_current_member_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<i64> {
        if !self.consistency.is_authoritative() {
            return Ok(0);
        }

        let member_repo = self.member_repo()?;
        if let Some(member) = member_repo.get(room_id, user_id).await? {
            self.consistency
                .set_version_at_least(&Self::permission_domain(room_id, user_id), member.version)
                .await
        } else {
            let version = member_repo.lifecycle_version(room_id, user_id).await?;
            self.consistency
                .set_version_at_least(&Self::permission_domain(room_id, user_id), version)
                .await
        }
    }

    pub(crate) async fn seed_permission_fence_to_member_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        member_version: i64,
    ) -> Result<i64> {
        if !self.consistency.is_authoritative() {
            return Ok(0);
        }

        self.consistency
            .set_version_at_least(&Self::permission_domain(room_id, user_id), member_version)
            .await
    }

    pub(crate) async fn seed_permission_fences_after_strong_read(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        member_version: i64,
        settings_version: i64,
    ) -> Result<()> {
        if !self.consistency.is_authoritative() {
            return Ok(());
        }

        self.consistency
            .set_version_at_least(&Self::permission_domain(room_id, user_id), member_version)
            .await?;
        self.consistency
            .set_version_at_least(&Self::room_settings_domain(room_id), settings_version)
            .await?;
        Ok(())
    }
}
