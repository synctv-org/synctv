CREATE TABLE IF NOT EXISTS room_join_requests (
    id CHAR(12) PRIMARY KEY,
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    requested_role SMALLINT,
    status SMALLINT NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    reviewed_at TIMESTAMPTZ,
    reviewed_by CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,
    rejection_reason TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_room_join_requests_pending_unique
    ON room_join_requests(room_id, user_id)
    WHERE reviewed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_room_join_requests_room_status
    ON room_join_requests(room_id, status, requested_at DESC);

CREATE INDEX IF NOT EXISTS idx_room_join_requests_user_status
    ON room_join_requests(user_id, status, requested_at DESC);

CREATE INDEX IF NOT EXISTS idx_room_join_requests_reviewed_by
    ON room_join_requests(reviewed_by)
    WHERE reviewed_by IS NOT NULL;

COMMENT ON TABLE room_join_requests IS 'Room membership approval workflow records';
