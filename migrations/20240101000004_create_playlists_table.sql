-- Create playlists table (supporting tree structure and dynamic folders)
-- Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4

CREATE TABLE playlists (
    id CHAR(12) PRIMARY KEY,

    -- Basic information
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE CASCADE,

    -- Directory name (root directory is empty string)
    name VARCHAR(255) NOT NULL DEFAULT '',

    -- Tree structure (file system style)
    parent_id CHAR(12) REFERENCES playlists(id) ON DELETE CASCADE,

    -- Sort position (support manual directory reordering)
    position INT NOT NULL DEFAULT 0,

    -- ========== Dynamic folder support ==========
    source_provider VARCHAR(64),
    source_config JSONB,
    provider_instance_name VARCHAR(64),

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Constraints
    CONSTRAINT valid_parent CHECK (parent_id IS NULL OR parent_id != id),
    CONSTRAINT unique_playlist_name UNIQUE (room_id, parent_id, name),
    CONSTRAINT valid_name CHECK (
        (parent_id IS NULL AND name = '')
        OR
        (parent_id IS NOT NULL AND (
            length(trim(name)) > 0
            AND length(name) <= 255
            AND name NOT LIKE '%/%'
        ))
    ),
    CONSTRAINT valid_dynamic_folder CHECK (
        (source_provider IS NOT NULL AND source_config IS NOT NULL)
        OR
        (source_provider IS NULL AND source_config IS NULL)
    )
);

-- Indexes
CREATE INDEX idx_playlists_room ON playlists(room_id);
CREATE INDEX idx_playlists_parent ON playlists(parent_id, position);
CREATE INDEX idx_playlists_tree ON playlists(room_id, parent_id, position);
CREATE INDEX idx_playlists_creator ON playlists(creator_id);
CREATE INDEX idx_playlists_source_provider ON playlists(source_provider) WHERE source_provider IS NOT NULL;
CREATE INDEX idx_playlists_created_at ON playlists(created_at DESC);

-- Trigger to update updated_at
CREATE TRIGGER update_playlists_updated_at
    BEFORE UPDATE ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE playlists IS 'Playlist directory table (supporting static folders and dynamic folders). Root playlist (empty name, NULL parent_id) is created in Rust when room is created.';
COMMENT ON COLUMN playlists.name IS 'Directory name (root directory is empty string)';
COMMENT ON COLUMN playlists.parent_id IS 'Parent directory ID, NULL means root directory';
COMMENT ON COLUMN playlists.position IS 'Sort position in parent directory';
COMMENT ON COLUMN playlists.source_provider IS 'Media provider type name (NULL=static folder, non-NULL=dynamic folder, e.g., "alist", "emby")';
COMMENT ON COLUMN playlists.source_config IS 'Media provider configuration (required for dynamic folders)';
COMMENT ON COLUMN playlists.provider_instance_name IS 'Recommended media provider backend instance name (optional)';
COMMENT ON CONSTRAINT unique_playlist_name ON playlists IS 'No duplicate names in same directory';
COMMENT ON CONSTRAINT valid_name ON playlists IS 'Name validation: root directory must be empty string, non-root cannot be empty/spaces, forbids / character';
COMMENT ON CONSTRAINT valid_dynamic_folder ON playlists IS 'Dynamic folder constraint: source_provider/source_config must either both exist or both be NULL';

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
