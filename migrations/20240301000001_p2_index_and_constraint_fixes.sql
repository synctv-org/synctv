-- P2 Migration: Index redundancy removal and missing constraints
--
-- DB-05: Remove redundant indexes that are covered by composite indexes
-- DB-06: Add missing NOT NULL constraints on notifications timestamps
-- DB-07: Remove low-selectivity standalone indexes
-- DB-08: Remove redundant room_settings indexes (covered by PK)
-- DB-09: Add missing provider constraint on user_media_provider_credentials
-- DB-10: Add covering index for common notification query pattern

-- ============================================================================
-- DB-05: Remove redundant indexes
-- ============================================================================

-- idx_rooms_status is redundant: idx_rooms_status_created_at covers all queries
-- that filter by status (it includes status as the leading column with the same
-- WHERE clause). The partial index condition is also subsumed.
DROP INDEX IF EXISTS idx_rooms_status;

-- idx_rooms_created_at is a standalone index on created_at, but all production
-- queries that sort by created_at also filter by deleted_at IS NULL and/or status.
-- These are already covered by idx_rooms_status_created_at and idx_rooms_creator_status.
DROP INDEX IF EXISTS idx_rooms_created_at;

-- idx_room_settings_key is redundant because the PK (room_id, key) already
-- supports lookups by key when combined with room_id, and standalone key-only
-- lookups are not used in the application.
DROP INDEX IF EXISTS idx_room_settings_key;

-- idx_room_settings_version is redundant: optimistic locking queries always
-- filter by PK (room_id, key) first, then check version in the WHERE clause.
-- The PK index already provides efficient access; an additional version index
-- adds write overhead without query benefit.
DROP INDEX IF EXISTS idx_room_settings_version;

-- ============================================================================
-- DB-06: Add missing NOT NULL constraints on notifications timestamps
-- ============================================================================

-- notifications.created_at and updated_at have DEFAULT but no NOT NULL constraint.
-- This is inconsistent with all other tables and could allow NULL timestamps.
ALTER TABLE notifications
    ALTER COLUMN created_at SET NOT NULL,
    ALTER COLUMN updated_at SET NOT NULL;

-- ============================================================================
-- DB-07: Remove low-selectivity standalone index
-- ============================================================================

-- idx_notifications_is_read is a boolean index with ~50% selectivity at best.
-- The actually useful index is idx_notifications_user_unread which combines
-- user_id + is_read with a partial index condition.
DROP INDEX IF EXISTS idx_notifications_is_read;

-- ============================================================================
-- DB-09: Add missing provider type constraint
-- ============================================================================

-- The provider column on user_media_provider_credentials has no CHECK constraint
-- to validate known provider types. Add one for data integrity.
ALTER TABLE user_media_provider_credentials
    ADD CONSTRAINT valid_provider CHECK (
        provider IN ('bilibili', 'alist', 'emby')
    );

COMMENT ON CONSTRAINT valid_provider ON user_media_provider_credentials IS
'Restrict provider to known media provider types';

-- ============================================================================
-- DB-10: Add covering index for notification pagination
-- ============================================================================

-- The common query pattern is: fetch notifications for a user ordered by
-- created_at DESC with is_read status. Add a covering index that includes
-- the type column to avoid heap lookups for the most common list query.
CREATE INDEX IF NOT EXISTS idx_notifications_user_type_created
    ON notifications(user_id, type, created_at DESC)
    WHERE is_read = FALSE;

COMMENT ON INDEX idx_notifications_user_type_created IS
'Covering index for unread notification queries filtered by user and type';
