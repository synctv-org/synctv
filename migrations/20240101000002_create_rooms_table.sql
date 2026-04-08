CREATE TABLE IF NOT EXISTS rooms (
    id CHAR(12) PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_by CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    status SMALLINT NOT NULL DEFAULT 1,
    is_banned BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMPTZ NULL,
    version INTEGER NOT NULL DEFAULT 0,
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_rooms_created_by_name ON rooms(created_by, name) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_created_by ON rooms(created_by);
CREATE INDEX idx_rooms_is_banned ON rooms(is_banned) WHERE is_banned = TRUE;  -- Quick lookup of banned rooms
CREATE INDEX idx_rooms_deleted_at ON rooms(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_rooms_name_trgm ON rooms USING gin (name gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_description_trgm ON rooms USING gin (description gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_last_activity ON rooms(last_activity_at) WHERE deleted_at IS NULL;
CREATE INDEX idx_rooms_status_created_at ON rooms(status, created_at DESC) WHERE deleted_at IS NULL AND is_banned = FALSE;
CREATE INDEX idx_rooms_creator_status ON rooms(created_by, status, created_at DESC) WHERE deleted_at IS NULL;

CREATE TRIGGER update_rooms_updated_at BEFORE UPDATE ON rooms
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

ALTER TABLE rooms ADD CONSTRAINT rooms_status_check
    CHECK (status BETWEEN 1 AND 4);

COMMENT ON TABLE rooms IS 'Video watching rooms - all settings stored in room_settings table';
COMMENT ON COLUMN rooms.id IS '12-character base62 ID';
COMMENT ON COLUMN rooms.name IS 'Room display name, unique per creator among active (non-deleted) rooms';
COMMENT ON COLUMN rooms.description IS 'Room description, max 500 characters';
COMMENT ON COLUMN rooms.status IS 'Room lifecycle status: 1=active, 2=pending, 3=rejected, 4=closed';
COMMENT ON COLUMN rooms.is_banned IS 'Ban flag set by global admin. Room retains its status when banned/unbanned.';
COMMENT ON COLUMN rooms.version IS 'Monotonically increasing integer for optimistic locking. Incremented on each UPDATE.';
