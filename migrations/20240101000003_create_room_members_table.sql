-- Create room_members table with Allow/Deny permission pattern
CREATE TABLE IF NOT EXISTS room_members (
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- Role and Status (separated as per design)
    role SMALLINT NOT NULL DEFAULT 3,  -- 1=creator, 2=admin, 3=member, 4=guest
    status SMALLINT NOT NULL DEFAULT 1,  -- 1=active, 2=pending, 3=banned

    -- Allow/Deny permission pattern
    -- effective_permissions = ((global_default | room_added | admin_added | member_added) & ~(room_removed | admin_removed | member_removed))
    -- For regular members: uses member_added/member_removed
    -- For admins: uses admin_added/admin_removed (overrides member-level)
    -- NOTE: Stored as BIGINT (signed i64) but logically treated as u64 bitmasks.
    -- CHECK constraints prevent negative values to avoid overflow when cast to u64.
    added_permissions BIGINT DEFAULT 0 CHECK (added_permissions >= 0),      -- For member role: extra permissions
    removed_permissions BIGINT DEFAULT 0 CHECK (removed_permissions >= 0),    -- For member role: removed permissions
    admin_added_permissions BIGINT DEFAULT 0 CHECK (admin_added_permissions >= 0),     -- For admin role: extra permissions (on top of admin default)
    admin_removed_permissions BIGINT DEFAULT 0 CHECK (admin_removed_permissions >= 0),   -- For admin role: removed permissions (overrides admin default)

    -- Timestamps
    joined_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    left_at TIMESTAMPTZ NULL,

    -- Optimistic locking for permission updates
    version BIGINT NOT NULL DEFAULT 0,

    -- Banned info
    banned_at TIMESTAMPTZ,
    banned_by CHAR(12) REFERENCES users(id) ON DELETE SET NULL,
    banned_reason TEXT,

    PRIMARY KEY (room_id, user_id)
);

-- Create indexes
CREATE INDEX idx_room_members_user_id ON room_members(user_id);
CREATE INDEX idx_room_members_joined_at ON room_members(joined_at);
-- Partial index for efficient lookup of active members in a room
CREATE INDEX idx_room_members_room_active ON room_members(room_id) WHERE left_at IS NULL;

-- Performance optimization indexes (covering indexes to avoid table lookups)
CREATE INDEX idx_room_members_user_active ON room_members(user_id, room_id, role, joined_at DESC)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_room_count ON room_members(room_id)
    WHERE left_at IS NULL;

-- Permission-related indexes
CREATE INDEX idx_room_members_role ON room_members(room_id, role)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_status ON room_members(room_id, status)
    WHERE left_at IS NULL;
CREATE INDEX idx_room_members_banned ON room_members(banned_at)
    WHERE banned_at IS NOT NULL;
CREATE INDEX idx_room_members_version ON room_members(room_id, user_id, version)
    WHERE left_at IS NULL;

-- Constraints: 1=creator, 2=admin, 3=member, 4=guest
ALTER TABLE room_members
    ADD CONSTRAINT check_room_members_role
    CHECK (role BETWEEN 1 AND 4);

-- Status constraint: 1=active, 2=pending, 3=banned
ALTER TABLE room_members
    ADD CONSTRAINT check_room_members_status
    CHECK (status BETWEEN 1 AND 3);

-- Consistency constraint: left_at and status must agree.
-- Active/pending members (status 1,2) must have left_at IS NULL.
-- Banned members (status 3) must have banned_at set AND left_at set
-- (a banned member has effectively left the room).
ALTER TABLE room_members
    ADD CONSTRAINT check_room_members_left_at_status
    CHECK (
        (status IN (1, 2) AND left_at IS NULL)
        OR (status = 3 AND banned_at IS NOT NULL AND left_at IS NOT NULL)
        OR (left_at IS NOT NULL AND status NOT IN (1, 2, 3))
    );

-- Comments
COMMENT ON TABLE room_members IS 'Room membership with Allow/Deny permission pattern';
COMMENT ON COLUMN room_members.role IS 'Room role: 1=creator, 2=admin, 3=member, 4=guest';
COMMENT ON COLUMN room_members.status IS 'Member status: 1=active, 2=pending, 3=banned';
COMMENT ON COLUMN room_members.added_permissions IS 'Extra permissions added to role default (Allow pattern)';
COMMENT ON COLUMN room_members.removed_permissions IS 'Permissions removed from role default (Deny pattern)';
COMMENT ON COLUMN room_members.version IS 'Optimistic lock version for permission updates';
COMMENT ON COLUMN room_members.banned_at IS 'Timestamp when member was banned';
COMMENT ON COLUMN room_members.banned_by IS 'User ID who banned this member';
COMMENT ON COLUMN room_members.banned_reason IS 'Reason for banning';
COMMENT ON COLUMN room_members.left_at IS 'NULL if currently in room, timestamp if left';

-- ============================================================================
-- Optimistic Locking with Version Field (CAS pattern)
-- ============================================================================

-- Function: Update room member permissions with optimistic locking
-- This function implements Compare-And-Swap (CAS) pattern to prevent race conditions
-- Returns TRUE if update succeeded, FALSE if version mismatch (indicating concurrent update)
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
    -- Perform atomic CAS update: only update if version matches
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
    RETURNING version INTO new_version;

    GET DIAGNOSTICS rows_updated = ROW_COUNT;

    IF rows_updated = 0 THEN
        -- Version mismatch or row not found
        RETURN json_build_object(
            'success', FALSE,
            'error', 'version_mismatch',
            'message', 'Concurrent update detected or member not found',
            'expected_version', p_expected_version
        );
    END IF;

    -- Success
    RETURN json_build_object(
        'success', TRUE,
        'new_version', new_version,
        'message', 'Permissions updated successfully'
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION update_room_member_permissions_cas IS
'Update room member permissions with optimistic locking (CAS pattern). Returns success=false on version mismatch. Use in retry loop in application code.';

-- Example usage in Rust:
-- let mut retries = 3;
-- loop {
--     let member = get_room_member(room_id, user_id).await?;
--     let result = update_room_member_permissions_cas(
--         room_id, user_id, member.version,
--         Some(new_added), Some(new_removed), None, None
--     ).await?;
--
--     if result["success"].as_bool() == Some(true) {
--         break; // Success
--     }
--
--     retries -= 1;
--     if retries == 0 {
--         return Err("Max retries exceeded due to concurrent updates");
--     }
--     tokio::time::sleep(Duration::from_millis(50)).await;
-- }
