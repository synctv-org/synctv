-- Create rooms table
CREATE TABLE IF NOT EXISTS rooms (
    id CHAR(12) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- ON DELETE CASCADE: 删除用户时自动删除该用户创建的所有房间及相关数据
    created_by CHAR(12) NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status SMALLINT NOT NULL DEFAULT 1,  -- 1=active, 2=pending, 3=closed
    is_banned BOOLEAN NOT NULL DEFAULT FALSE,  -- Independent ban flag, set by global admin only
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMPTZ NULL
);

-- Create indexes
CREATE INDEX idx_rooms_created_by ON rooms(created_by);
CREATE INDEX idx_rooms_status ON rooms(status) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_is_banned ON rooms(is_banned) WHERE is_banned = TRUE;  -- Quick lookup of banned rooms
CREATE INDEX idx_rooms_created_at ON rooms(created_at);
CREATE INDEX idx_rooms_deleted_at ON rooms(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_rooms_name ON rooms USING gin(to_tsvector('english', name));
CREATE INDEX idx_rooms_description ON rooms USING gin(to_tsvector('english', description));

-- pg_trgm GIN indexes for ILIKE pattern matching (the tsvector indexes above
-- only support full-text search @@, not ILIKE/LIKE queries used in room search)
-- NOTE: pg_trgm extension is created in 20240101000001_create_users_table.sql
CREATE INDEX idx_rooms_name_trgm ON rooms USING gin (name gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_description_trgm ON rooms USING gin (description gin_trgm_ops) WHERE deleted_at IS NULL;

-- Performance optimization indexes
CREATE INDEX idx_rooms_status_created_at ON rooms(status, created_at DESC) WHERE deleted_at IS NULL AND is_banned = FALSE;
CREATE INDEX idx_rooms_creator_status ON rooms(created_by, status, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_name_lower ON rooms(LOWER(name)) WHERE deleted_at IS NULL;

-- Create updated_at trigger
CREATE TRIGGER update_rooms_updated_at BEFORE UPDATE ON rooms
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Add check constraint for status: 1=active, 2=pending, 3=closed
ALTER TABLE rooms ADD CONSTRAINT rooms_status_check
    CHECK (status BETWEEN 1 AND 3);

-- Comments
COMMENT ON TABLE rooms IS 'Video watching rooms - all settings stored in room_settings table';
COMMENT ON COLUMN rooms.id IS '12-character nanoid';
COMMENT ON COLUMN rooms.description IS 'Room description, max 500 characters';
COMMENT ON COLUMN rooms.status IS 'Room lifecycle status: 1=active, 2=pending, 3=closed';
COMMENT ON COLUMN rooms.is_banned IS 'Ban flag set by global admin. Room retains its status when banned/unbanned.';

-- ============================================================================
-- Room Deletion Notification (for cluster-wide resource cleanup)
-- ============================================================================

CREATE OR REPLACE FUNCTION notify_room_deleted()
RETURNS TRIGGER AS $$
BEGIN
    -- Send notification to all cluster nodes listening on 'synctv_room_deleted'
    -- Enables stateless cleanup without requiring PersistentVolumeClaim/WAL
    PERFORM pg_notify(
        'synctv_room_deleted',
        json_build_object(
            'room_id', OLD.id,
            'timestamp', CURRENT_TIMESTAMP
        )::text
    );
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_room_deleted
AFTER DELETE ON rooms
FOR EACH ROW
EXECUTE FUNCTION notify_room_deleted();

COMMENT ON FUNCTION notify_room_deleted IS
'Sends a notification when a room is deleted. Cluster nodes listen for this to clear room caches, disconnect connections, and invalidate room state.';

