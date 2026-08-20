//! Member permission source cache (L1: Moka in-memory, L2: Redis).
//!
//! This cache stores only the permission source columns from `room_members`.
//! Effective permissions are computed at read time from this source, room
//! settings, and runtime global permission defaults.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{CacheKey, FenceReadResult, TieredCache, Versioned};
use crate::models::{RoomId, RoomMember, RoomRole, UserId};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberPermissionKey {
    pub room_id: RoomId,
    pub user_id: UserId,
}

impl MemberPermissionKey {
    #[must_use]
    pub const fn new(room_id: RoomId, user_id: UserId) -> Self {
        Self { room_id, user_id }
    }
}

impl fmt::Display for MemberPermissionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.room_id, self.user_id)
    }
}

impl CacheKey for MemberPermissionKey {
    fn cache_key(&self) -> String {
        format!("{}:{}", self.room_id, self.user_id)
    }

    fn try_from_id(id: &str) -> Result<Self> {
        let Some((room_id, user_id)) = id.split_once(':') else {
            return Err(crate::Error::InvalidInput(format!(
                "Invalid member permission cache key: {id}"
            )));
        };
        Ok(Self {
            room_id: room_id.parse().map_err(|_| {
                crate::Error::InvalidInput(format!(
                    "Invalid room id in member permission key: {id}"
                ))
            })?,
            user_id: user_id.parse().map_err(|_| {
                crate::Error::InvalidInput(format!(
                    "Invalid user id in member permission key: {id}"
                ))
            })?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachedMemberPermissionSource {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub role: RoomRole,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub version: i64,
}

impl From<&RoomMember> for CachedMemberPermissionSource {
    fn from(member: &RoomMember) -> Self {
        Self {
            room_id: member.room_id,
            user_id: member.user_id,
            role: member.role,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            version: member.version,
        }
    }
}

impl CachedMemberPermissionSource {
    #[must_use]
    pub fn to_room_member(&self) -> RoomMember {
        RoomMember {
            room_id: self.room_id,
            user_id: self.user_id,
            role: self.role,
            added_permissions: self.added_permissions,
            removed_permissions: self.removed_permissions,
            admin_added_permissions: self.admin_added_permissions,
            admin_removed_permissions: self.admin_removed_permissions,
            remark_name: String::new(),
            display_tag: String::new(),
            joined_at: crate::SystemClock.now(),
            version: self.version,
        }
    }
}

impl Versioned for CachedMemberPermissionSource {
    fn cache_version(&self) -> i64 {
        self.version
    }
}

#[derive(Clone)]
pub struct MemberPermissionCache {
    inner: TieredCache<MemberPermissionKey, CachedMemberPermissionSource>,
}

impl MemberPermissionCache {
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Self {
        let inner = TieredCache::new(
            l2,
            l1_max_capacity,
            l1_ttl_seconds,
            l2_ttl_seconds,
            key_prefix,
            "member_permission".to_string(),
        );
        Self { inner }
    }

    pub async fn get_l1(&self, key: &MemberPermissionKey) -> Option<CachedMemberPermissionSource> {
        self.inner.get_l1(key).await
    }

    pub async fn get_l2(
        &self,
        key: &MemberPermissionKey,
    ) -> Result<Option<CachedMemberPermissionSource>> {
        self.inner.get_l2(key).await
    }

    pub async fn get_by_fence_key(
        &self,
        key: &MemberPermissionKey,
        fence_key: &str,
    ) -> Result<FenceReadResult<CachedMemberPermissionSource>> {
        self.inner.get_by_fence_key(key, fence_key).await
    }

    pub async fn set_if_version_at_least(
        &self,
        key: &MemberPermissionKey,
        source: CachedMemberPermissionSource,
    ) -> Result<bool> {
        self.inner.set_if_version_at_least(key, source).await
    }

    pub async fn invalidate(&self, key: &MemberPermissionKey) -> Result<()> {
        self.inner.invalidate(key).await
    }

    pub async fn clear(&self) {
        self.inner.clear().await;
    }
}

impl fmt::Debug for MemberPermissionCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemberPermissionCache")
            .field("inner", &self.inner)
            .finish()
    }
}
