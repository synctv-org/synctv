-- Create playlists table (supporting tree structure and dynamic folders)

CREATE TABLE IF NOT EXISTS playlists (
    id CHAR(12) PRIMARY KEY,

    -- Basic information
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE SET NULL,

    -- Playlist display name
    name VARCHAR(255) NOT NULL DEFAULT '',

    -- Tree structure (file system style)
    parent_id CHAR(12) REFERENCES playlists(id) ON DELETE CASCADE,

    -- Sort position (support manual directory reordering).
    -- No DEFAULT: callers must always compute the next position via MAX+1
    -- to avoid UNIQUE constraint violations on concurrent inserts.
    position INT NOT NULL,

    -- ========== Dynamic folder support ==========
    source_provider VARCHAR(64),
    source_config JSONB,
    provider_instance_name VARCHAR(64),

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Optimistic locking version (incremented on each update)
    version INTEGER NOT NULL DEFAULT 0,

    -- Constraints
    CONSTRAINT valid_parent CHECK (parent_id IS NULL OR parent_id != id),
    CONSTRAINT valid_dynamic_folder CHECK (
        (source_provider IS NOT NULL AND source_config IS NOT NULL)
        OR
        (source_provider IS NULL AND source_config IS NULL)
    ),
    CONSTRAINT unique_playlist_id_room UNIQUE (id, room_id),
    CONSTRAINT unique_playlist_position UNIQUE (room_id, parent_id, position)
);

-- Indexes
CREATE INDEX idx_playlists_room ON playlists(room_id);
CREATE INDEX idx_playlists_parent ON playlists(parent_id, position);
CREATE INDEX idx_playlists_tree ON playlists(room_id, parent_id, position);
CREATE INDEX idx_playlists_creator ON playlists(creator_id);
CREATE INDEX idx_playlists_source_provider ON playlists(source_provider) WHERE source_provider IS NOT NULL;
CREATE INDEX idx_playlists_created_at ON playlists(created_at DESC);

-- Same NULL handling for position uniqueness: the UNIQUE constraint
-- unique_playlist_position on (room_id, parent_id, position) does not
-- prevent duplicates when parent_id IS NULL. This partial index ensures
-- unique positions among top-level playlists in a room.
CREATE UNIQUE INDEX IF NOT EXISTS idx_playlists_unique_root_position
    ON playlists(room_id, position)
    WHERE parent_id IS NULL;

-- Trigger to update updated_at
CREATE TRIGGER update_playlists_updated_at
    BEFORE UPDATE ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE playlists IS 'Playlist table supporting top-level and nested static or dynamic folders.';
COMMENT ON COLUMN playlists.name IS 'Playlist display name. It is not a routing key or unique path segment.';
COMMENT ON COLUMN playlists.parent_id IS 'Parent playlist ID. NULL means this playlist is directly under the room root.';
COMMENT ON COLUMN playlists.position IS 'Sort position in parent directory';
COMMENT ON COLUMN playlists.source_provider IS 'Media provider type name (NULL=static folder, non-NULL=dynamic folder, e.g., "alist", "emby")';
COMMENT ON COLUMN playlists.source_config IS 'Media provider configuration (required for dynamic folders)';
COMMENT ON COLUMN playlists.provider_instance_name IS 'Recommended media provider backend instance name (optional)';
COMMENT ON CONSTRAINT valid_dynamic_folder ON playlists IS 'Dynamic folder constraint: source_provider/source_config must either both exist or both be NULL';
COMMENT ON COLUMN playlists.version IS 'Optimistic locking version, incremented on each update';

-- ============================================================================
-- Circular Reference Protection
-- ============================================================================

-- Function: Detect circular references in playlist tree
-- This function checks if setting parent_id would create a cycle
-- Uses recursive CTE to traverse the tree from the proposed parent upwards
CREATE OR REPLACE FUNCTION check_playlist_cycle(
    playlist_id CHAR(12),
    new_parent_id CHAR(12)
) RETURNS BOOLEAN AS $$
DECLARE
    cycle_detected BOOLEAN;
BEGIN
    -- If new parent is NULL, no cycle possible
    IF new_parent_id IS NULL THEN
        RETURN FALSE;
    END IF;

    -- Check if playlist_id appears in the ancestor chain of new_parent_id
    SELECT EXISTS (
        WITH RECURSIVE ancestors AS (
            -- Start from the proposed parent
            SELECT id, parent_id, 0 AS depth
            FROM playlists
            WHERE id = new_parent_id

            UNION ALL

            -- Traverse up the tree
            SELECT p.id, p.parent_id, a.depth + 1
            FROM playlists p
            JOIN ancestors a ON p.id = a.parent_id
            WHERE a.depth < 50  -- Prevent infinite loop (max depth protection)
        )
        SELECT 1
        FROM ancestors
        WHERE id = playlist_id
    ) INTO cycle_detected;

    RETURN cycle_detected;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION check_playlist_cycle(CHAR, CHAR) IS
'Check if setting parent_id would create a circular reference. Returns TRUE if cycle detected. Max depth: 50 levels.';

-- Trigger function: Prevent circular references
CREATE OR REPLACE FUNCTION prevent_playlist_cycle()
RETURNS TRIGGER AS $$
BEGIN
    -- Only check on UPDATE if parent_id is being changed, or on INSERT with non-NULL parent
    IF (TG_OP = 'INSERT' AND NEW.parent_id IS NOT NULL) OR
       (TG_OP = 'UPDATE' AND NEW.parent_id IS DISTINCT FROM OLD.parent_id AND NEW.parent_id IS NOT NULL) THEN

        -- Check for cycle
        IF check_playlist_cycle(NEW.id, NEW.parent_id) THEN
            RAISE EXCEPTION 'Circular reference detected: setting parent_id=% for playlist % would create a cycle',
                NEW.parent_id, NEW.id
                USING ERRCODE = '23514',  -- check_violation
                      HINT = 'Cannot set a descendant as parent';
        END IF;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Trigger: Validate playlist tree integrity before INSERT/UPDATE
CREATE TRIGGER trigger_prevent_playlist_cycle
    BEFORE INSERT OR UPDATE OF parent_id ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION prevent_playlist_cycle();

COMMENT ON TRIGGER trigger_prevent_playlist_cycle ON playlists IS
'Prevent circular references in playlist tree. Validates that parent_id does not create a cycle (max depth: 50).';

-- ============================================================================
-- Cross-Room Parent Validation (Task #17)
-- ============================================================================

-- Function: Validate that parent playlist belongs to the same room
-- This enforces room isolation - a playlist cannot have a parent from another room
CREATE OR REPLACE FUNCTION validate_parent_same_room()
RETURNS TRIGGER AS $$
DECLARE
    parent_room_id CHAR(12);
BEGIN
    -- If parent_id is NULL (top-level playlist), no validation needed
    IF NEW.parent_id IS NULL THEN
        RETURN NEW;
    END IF;

    -- On UPDATE, only check if parent_id is being changed
    IF TG_OP = 'UPDATE' AND NEW.parent_id IS NOT DISTINCT FROM OLD.parent_id THEN
        RETURN NEW;
    END IF;

    -- Get the room_id of the parent playlist
    SELECT room_id INTO parent_room_id
    FROM playlists
    WHERE id = NEW.parent_id;

    -- Parent must exist (FK will catch this, but check anyway)
    IF parent_room_id IS NULL THEN
        RAISE EXCEPTION 'Parent playlist % does not exist', NEW.parent_id
            USING ERRCODE = '23503';  -- foreign_key_violation
    END IF;

    -- Validate that parent belongs to the same room
    IF parent_room_id != NEW.room_id THEN
        RAISE EXCEPTION 'Cross-room parent_id violation: playlist % in room % cannot have parent % from room %',
            NEW.id, NEW.room_id, NEW.parent_id, parent_room_id
            USING ERRCODE = '23514',  -- check_violation
                  HINT = 'Parent playlist must belong to the same room';
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION validate_parent_same_room() IS
'Enforce room isolation for playlist parent_id. A playlist cannot have a parent from a different room.';

-- Trigger: Validate cross-room parent before INSERT/UPDATE
CREATE TRIGGER trigger_validate_parent_same_room
    BEFORE INSERT OR UPDATE OF parent_id, room_id ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION validate_parent_same_room();

COMMENT ON TRIGGER trigger_validate_parent_same_room ON playlists IS
'Prevent cross-room parent_id references. Ensures parent playlist belongs to the same room as child.';
