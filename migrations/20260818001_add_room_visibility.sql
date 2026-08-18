ALTER TABLE rooms
    ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE room_creation_requests
    ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT TRUE;

CREATE INDEX IF NOT EXISTS idx_rooms_public_last_activity
    ON rooms(last_activity_at DESC, id DESC)
    WHERE deleted_at IS NULL AND is_public;
