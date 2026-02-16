-- Create playlists table (supporting tree structure and dynamic folders)
-- Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4

CREATE TABLE playlists (
    id CHAR(12) PRIMARY KEY,

    -- Basic information
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE SET NULL,

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
