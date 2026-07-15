CREATE TABLE IF NOT EXISTS room_members (
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    role SMALLINT NOT NULL,
    added_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (added_permissions >= 0),
    removed_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (removed_permissions >= 0),
    admin_added_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (admin_added_permissions >= 0),
    admin_removed_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (admin_removed_permissions >= 0),

    remark_name VARCHAR(64) NOT NULL DEFAULT '',
    display_tag VARCHAR(16) NOT NULL DEFAULT '',

    joined_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_visited_at TIMESTAMPTZ,
    last_counted_visit_at TIMESTAMPTZ,
    visit_count BIGINT NOT NULL DEFAULT 0
        CHECK (visit_count >= 0),

    version BIGINT NOT NULL DEFAULT 0,

    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_room_members_user_id ON room_members(user_id);
CREATE INDEX IF NOT EXISTS idx_room_members_joined_at ON room_members(joined_at);
CREATE INDEX IF NOT EXISTS idx_room_members_room ON room_members(room_id);
CREATE INDEX IF NOT EXISTS idx_room_members_user_active
    ON room_members(user_id, room_id, role, joined_at DESC);
CREATE INDEX IF NOT EXISTS idx_room_members_role
ON room_members(room_id, role);
CREATE INDEX IF NOT EXISTS idx_room_members_user_frequency
ON room_members(user_id, visit_count DESC, last_visited_at DESC NULLS LAST, room_id);
