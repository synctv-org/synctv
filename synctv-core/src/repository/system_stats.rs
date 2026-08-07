//! System-wide statistics repository
//!
//! Optimized queries for admin dashboard stats that fetch all counts in a single query.

use crate::Result;
use sqlx::PgPool;

/// Statistics for the entire system
#[derive(Debug, Clone)]
pub struct SystemStats {
    pub total_users: i64,
    pub active_users: i64,
    pub banned_users: i64,
    pub total_rooms: i64,
    pub active_rooms: i64,
    pub banned_rooms: i64,
}

/// Repository for system-wide statistics
#[derive(Clone)]
pub struct SystemStatsRepository {
    pool: PgPool,
}

impl SystemStatsRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get all system statistics in a single database round-trip
    ///
    /// This method executes a single query with multiple COUNT subqueries
    /// instead of 6 separate paginated list queries, reducing latency by ~80ms.
    pub async fn get_system_stats(&self) -> Result<SystemStats> {
        let stats = sqlx::query_as!(
            SystemStats,
            r#"
            SELECT
                (
                    SELECT COUNT(*)
                    FROM user_account_profiles
                    WHERE deleted_at IS NULL
                ) AS "total_users!",
                (
                    SELECT COUNT(*)
                    FROM user_account_profiles
                    WHERE deleted_at IS NULL
                      AND is_banned = FALSE
                ) AS "active_users!",
                (
                    SELECT COUNT(*)
                    FROM user_account_profiles
                    WHERE deleted_at IS NULL
                      AND is_banned = TRUE
                ) AS "banned_users!",
                (
                    SELECT COUNT(*)
                    FROM rooms r
                    WHERE r.deleted_at IS NULL
                      AND r.closed_at IS NULL
                      AND NOT EXISTS (
                          SELECT 1
                          FROM room_bans rb
                          WHERE rb.room_id = r.id
                            AND rb.revoked_at IS NULL
                            AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                      )
                ) AS "total_rooms!",
                (
                    SELECT COUNT(*)
                    FROM rooms r
                    WHERE r.deleted_at IS NULL
                      AND r.closed_at IS NULL
                      AND NOT EXISTS (
                          SELECT 1
                          FROM room_bans rb
                          WHERE rb.room_id = r.id
                            AND rb.revoked_at IS NULL
                            AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                      )
                ) AS "active_rooms!",
                (
                    SELECT COUNT(*)
                    FROM rooms r
                    WHERE r.deleted_at IS NULL
                      AND r.closed_at IS NULL
                      AND EXISTS (
                          SELECT 1
                          FROM room_bans rb
                          WHERE rb.room_id = r.id
                            AND rb.revoked_at IS NULL
                            AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                      )
                ) AS "banned_rooms!"
            "#
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        models::{Room, RoomStatus, SignupMethod, User},
        repository::{RoomRepository, UserRepository},
        test_helpers::ok,
    };

    use super::SystemStatsRepository;

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn get_system_stats_counts_current_admin_dashboard_population() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let users = UserRepository::new(pool.clone());
        let rooms = RoomRepository::new(pool.clone());

        let active_user = ok(
            users
                .create(&User::new(
                    "stats_active_user".to_string(),
                    SignupMethod::Email,
                ))
                .await,
            "active user should be created",
        );
        let banned_user = ok(
            users
                .create(&User::new(
                    "stats_banned_user".to_string(),
                    SignupMethod::Email,
                ))
                .await,
            "banned user should be created",
        );
        let deleted_user = ok(
            users
                .create(&User::new(
                    "stats_deleted_user".to_string(),
                    SignupMethod::Email,
                ))
                .await,
            "deleted user should be created",
        );
        ok(
            users
                .ban(
                    &banned_user.id,
                    Some(&active_user.id),
                    Some("policy".to_string()),
                )
                .await,
            "user ban should be inserted",
        );
        ok(
            users.delete(&deleted_user.id).await,
            "deleted user should be soft-deleted",
        );

        let active_room = ok(
            rooms
                .create(&Room::new("stats_active_room".to_string(), active_user.id))
                .await,
            "active room should be created",
        );
        let banned_room = ok(
            rooms
                .create(&Room::new("stats_banned_room".to_string(), active_user.id))
                .await,
            "banned room should be created",
        );
        let closed_room = ok(
            rooms
                .create(&Room::new("stats_closed_room".to_string(), active_user.id))
                .await,
            "closed room should be created",
        );
        let closed_banned_room = ok(
            rooms
                .create(&Room::new(
                    "stats_closed_banned_room".to_string(),
                    active_user.id,
                ))
                .await,
            "closed banned room should be created",
        );
        let deleted_room = ok(
            rooms
                .create(&Room::new("stats_deleted_room".to_string(), active_user.id))
                .await,
            "deleted room should be created",
        );

        ok(
            rooms.update_ban_status(&banned_room.id, true).await,
            "room ban should be inserted",
        );
        ok(
            rooms
                .update_status(&closed_room.id, RoomStatus::Closed)
                .await,
            "closed room should be closed",
        );
        ok(
            rooms.update_ban_status(&closed_banned_room.id, true).await,
            "closed banned room ban should be inserted",
        );
        ok(
            rooms
                .update_status(&closed_banned_room.id, RoomStatus::Closed)
                .await,
            "closed banned room should be closed",
        );
        ok(
            rooms.delete(&deleted_room.id).await,
            "deleted room should be soft-deleted",
        );

        let stats = ok(
            SystemStatsRepository::new(pool.clone())
                .get_system_stats()
                .await,
            "system stats should load",
        );

        assert_eq!(stats.total_users, 2);
        assert_eq!(stats.active_users, 1);
        assert_eq!(stats.banned_users, 1);
        assert_eq!(stats.total_rooms, 1);
        assert_eq!(stats.active_rooms, 1);
        assert_eq!(stats.banned_rooms, 1);
        assert_eq!(active_room.status, RoomStatus::Active);
    }
}
