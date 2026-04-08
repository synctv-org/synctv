CREATE TABLE IF NOT EXISTS room_members (
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    role SMALLINT NOT NULL DEFAULT 3,
    status SMALLINT NOT NULL DEFAULT 1,

    added_permissions BIGINT NOT NULL DEFAULT 0 CHECK (added_permissions >= 0),
    removed_permissions BIGINT NOT NULL DEFAULT 0 CHECK (removed_permissions >= 0),
    admin_added_permissions BIGINT NOT NULL DEFAULT 0 CHECK (admin_added_permissions >= 0),
    admin_removed_permissions BIGINT NOT NULL DEFAULT 0 CHECK (admin_removed_permissions >= 0),

    joined_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    left_at TIMESTAMPTZ NULL,

    version BIGINT NOT NULL DEFAULT 0,

    banned_at TIMESTAMPTZ,
    banned_by CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,
    banned_reason TEXT,

    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX idx_room_members_user_id ON room_members(user_id);
CREATE INDEX idx_room_members_joined_at ON room_members(joined_at);
CREATE INDEX idx_room_members_room_active ON room_members(room_id) WHERE left_at IS NULL;
CREATE INDEX idx_room_members_user_active ON room_members(user_id, room_id, role, status, joined_at DESC)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_role ON room_members(room_id, role)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_status ON room_members(room_id, status)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_banned ON room_members(banned_at)
    WHERE banned_at IS NOT NULL;

ALTER TABLE room_members
    ADD CONSTRAINT check_room_members_role
    CHECK (role BETWEEN 1 AND 4);

ALTER TABLE room_members
    ADD CONSTRAINT check_room_members_status
    CHECK (status BETWEEN 1 AND 5);

COMMENT ON TABLE room_members IS 'Room membership with Allow/Deny permission pattern';
COMMENT ON COLUMN room_members.role IS 'Room role: 1=creator, 2=admin, 3=member, 4=guest';
COMMENT ON COLUMN room_members.status IS 'Member status: 1=active, 2=pending, 3=rejected, 4=banned, 5=left';
COMMENT ON COLUMN room_members.added_permissions IS 'Extra permissions added to role default (Allow pattern)';
COMMENT ON COLUMN room_members.removed_permissions IS 'Permissions removed from role default (Deny pattern)';
COMMENT ON COLUMN room_members.version IS 'Optimistic lock version for permission updates';
COMMENT ON COLUMN room_members.banned_at IS 'Timestamp when member was banned';
COMMENT ON COLUMN room_members.banned_by IS 'User ID who banned this member';
COMMENT ON COLUMN room_members.banned_reason IS 'Reason for banning';
COMMENT ON COLUMN room_members.left_at IS 'NULL if currently in room, timestamp if left';

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
'Update room member permissions with optimistic locking (CAS pattern). Returns success=false on version mismatch. Use in retry loop in application code.';
