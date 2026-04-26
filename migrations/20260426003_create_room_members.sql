CREATE TABLE IF NOT EXISTS room_members (
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    role SMALLINT NOT NULL DEFAULT 3,
    added_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (added_permissions >= 0),
    removed_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (removed_permissions >= 0),
    admin_added_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (admin_added_permissions >= 0),
    admin_removed_permissions BIGINT NOT NULL DEFAULT 0
        CHECK (admin_removed_permissions >= 0),

    joined_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    left_at TIMESTAMPTZ NULL,

    version BIGINT NOT NULL DEFAULT 0,

    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_room_members_user_id ON room_members(user_id);
CREATE INDEX IF NOT EXISTS idx_room_members_joined_at ON room_members(joined_at);
CREATE INDEX IF NOT EXISTS idx_room_members_room_active ON room_members(room_id) WHERE left_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_room_members_user_active
    ON room_members(user_id, room_id, role, joined_at DESC)
    WHERE left_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_room_members_role
    ON room_members(room_id, role)
    WHERE left_at IS NULL;

COMMENT ON TABLE room_members IS 'Room membership records';
COMMENT ON COLUMN room_members.added_permissions IS 'Additional permission bitmask';
COMMENT ON COLUMN room_members.removed_permissions IS 'Removed permission bitmask';
COMMENT ON COLUMN room_members.version IS 'Optimistic lock version for permission updates';
COMMENT ON COLUMN room_members.left_at IS 'Timestamp when the member left the room';

CREATE OR REPLACE FUNCTION update_room_member_permissions_cas(
    p_room_id CHAR(12),
    p_user_id CHAR(12),
    p_expected_version BIGINT,
    p_added_permissions BIGINT DEFAULT NULL,
    p_removed_permissions BIGINT DEFAULT NULL,
    p_admin_added_permissions BIGINT DEFAULT NULL,
    p_admin_removed_permissions BIGINT DEFAULT NULL
) RETURNS JSON AS $$
DECLARE
    rows_updated INTEGER;
    new_version BIGINT;
BEGIN
    UPDATE room_members
    SET
        added_permissions = COALESCE(p_added_permissions, added_permissions),
        removed_permissions = COALESCE(p_removed_permissions, removed_permissions),
        admin_added_permissions = COALESCE(p_admin_added_permissions, admin_added_permissions),
        admin_removed_permissions = COALESCE(p_admin_removed_permissions, admin_removed_permissions),
        version = version + 1
    WHERE room_id = p_room_id
      AND user_id = p_user_id
      AND version = p_expected_version
      AND left_at IS NULL
    RETURNING version INTO new_version;

    GET DIAGNOSTICS rows_updated = ROW_COUNT;

    IF rows_updated = 0 THEN
        RETURN json_build_object(
            'success', FALSE,
            'error', 'version_mismatch',
            'message', 'Concurrent update detected or member not found',
            'expected_version', p_expected_version
        );
    END IF;

    RETURN json_build_object(
        'success', TRUE,
        'new_version', new_version,
        'message', 'Permissions updated successfully'
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION update_room_member_permissions_cas IS
    'Update room member permissions with optimistic locking';
