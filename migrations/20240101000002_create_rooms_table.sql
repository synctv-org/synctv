-- Create rooms table
CREATE TABLE IF NOT EXISTS rooms (
    id CHAR(12) PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- ON DELETE RESTRICT: prevent deleting a user who still owns rooms.
    -- Rooms should be explicitly transferred or deleted before removing the user.
    created_by CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    status SMALLINT NOT NULL DEFAULT 1,  -- 1=active, 2=pending, 3=rejected, 4=closed
    is_banned BOOLEAN NOT NULL DEFAULT FALSE,  -- Independent ban flag, set by global admin only
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMPTZ NULL,
    version INTEGER NOT NULL DEFAULT 0,  -- Optimistic locking version, incremented on each UPDATE
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP  -- Tracks last room activity (chat, playback, member join/leave)
);

-- Create indexes
-- Partial unique index: a creator cannot own two active rooms with the same name.
-- Different creators may reuse the same display name. Soft-deleted room names
-- can be reused by the same creator.
CREATE UNIQUE INDEX IF NOT EXISTS idx_rooms_created_by_name ON rooms(created_by, name) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_created_by ON rooms(created_by);
CREATE INDEX idx_rooms_status ON rooms(status) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_is_banned ON rooms(is_banned) WHERE is_banned = TRUE;  -- Quick lookup of banned rooms
CREATE INDEX idx_rooms_created_at ON rooms(created_at);
CREATE INDEX idx_rooms_deleted_at ON rooms(deleted_at) WHERE deleted_at IS NOT NULL;
-- pg_trgm GIN indexes for ILIKE pattern matching
-- NOTE: pg_trgm extension is created in 20240101000001_create_users_table.sql
CREATE INDEX idx_rooms_name_trgm ON rooms USING gin (name gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_description_trgm ON rooms USING gin (description gin_trgm_ops) WHERE deleted_at IS NULL;

-- Index for room TTL cleanup: efficiently find rooms with stale last_activity_at
CREATE INDEX idx_rooms_last_activity ON rooms(last_activity_at) WHERE deleted_at IS NULL;

-- Performance optimization indexes
CREATE INDEX idx_rooms_status_created_at ON rooms(status, created_at DESC) WHERE deleted_at IS NULL AND is_banned = FALSE;
CREATE INDEX idx_rooms_creator_status ON rooms(created_by, status, created_at DESC) WHERE deleted_at IS NULL;

-- Create updated_at trigger
CREATE TRIGGER update_rooms_updated_at BEFORE UPDATE ON rooms
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Add check constraint for status: 1=active, 2=pending, 3=rejected, 4=closed
ALTER TABLE rooms ADD CONSTRAINT rooms_status_check
    CHECK (status BETWEEN 1 AND 4);

-- Comments
COMMENT ON TABLE rooms IS 'Video watching rooms - all settings stored in room_settings table';
COMMENT ON COLUMN rooms.id IS '12-character base62 ID';
COMMENT ON COLUMN rooms.name IS 'Room display name, unique per creator among active (non-deleted) rooms';
COMMENT ON COLUMN rooms.description IS 'Room description, max 500 characters';
COMMENT ON COLUMN rooms.status IS 'Room lifecycle status: 1=active, 2=pending, 3=rejected, 4=closed';
COMMENT ON COLUMN rooms.is_banned IS 'Ban flag set by global admin. Room retains its status when banned/unbanned.';
COMMENT ON COLUMN rooms.version IS 'Monotonically increasing integer for optimistic locking. Incremented on each UPDATE.';
