-- Create media table (media files in playlists)
-- Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.2

CREATE TABLE IF NOT EXISTS media (
    id CHAR(12) PRIMARY KEY,

    -- ========== Belongs to playlist ==========
    playlist_id CHAR(12) NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,

    -- ========== Basic information ==========
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE SET NULL,

    -- File name
    name VARCHAR(255) NOT NULL,

    -- Sort position (within playlist)
    position INTEGER NOT NULL DEFAULT 0,

    -- ========== Video source type (string for flexibility) ==========
    source_provider VARCHAR(64) NOT NULL DEFAULT 'direct_url',

    -- ========== Video source configuration (persistent storage) ==========
    source_config JSONB NOT NULL,

    -- Provider instance name (for registry lookup)
    provider_instance_name VARCHAR(64),

    -- Timestamps
    added_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMPTZ NULL,

    -- Constraints
    CONSTRAINT valid_media_name CHECK (
        length(trim(name)) > 0
        AND length(name) <= 255
        AND name NOT LIKE '%/%'
    )
);

-- Partial unique index: enforce uniqueness only on non-deleted rows.
-- This replaces the table-level UNIQUE constraint (which would include
-- soft-deleted rows and prevent re-creating a media item with the same name
-- after deletion).
CREATE UNIQUE INDEX unique_media_name ON media (playlist_id, name)
    WHERE deleted_at IS NULL;

-- Create indexes
CREATE INDEX idx_media_playlist ON media(playlist_id, position) WHERE deleted_at IS NULL;
CREATE INDEX idx_media_room ON media(room_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_media_creator ON media(creator_id);
CREATE INDEX idx_media_added_at ON media(added_at DESC);
CREATE INDEX idx_media_deleted_at ON media(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX idx_media_source_provider ON media(source_provider) WHERE deleted_at IS NULL;
CREATE INDEX idx_media_provider_name ON media(provider_instance_name) WHERE provider_instance_name IS NOT NULL;
CREATE INDEX idx_media_source_config ON media USING gin(source_config) WHERE deleted_at IS NULL;

-- Performance optimization: covering index for playlist queries
CREATE INDEX idx_media_playlist_covering ON media(playlist_id, position, source_provider, name)
    WHERE deleted_at IS NULL;

-- Comments
COMMENT ON TABLE media IS 'Media items (videos/audio) in playlists';
COMMENT ON COLUMN media.id IS '12-character nanoid';
COMMENT ON COLUMN media.playlist_id IS 'Associated playlist (directory)';
COMMENT ON COLUMN media.name IS 'File name (forbids / character)';
COMMENT ON COLUMN media.position IS 'Position in playlist (0-indexed)';
COMMENT ON COLUMN media.source_provider IS 'Media provider type name (e.g., "bilibili", "alist", "emby", "direct_url")';
COMMENT ON COLUMN media.source_config IS 'Media provider-specific configuration (persistent)';
COMMENT ON COLUMN media.provider_instance_name IS 'Media provider instance name for registry lookup (e.g., "bilibili_main")';
COMMENT ON INDEX unique_media_name IS 'No duplicate names in same playlist (excludes soft-deleted rows)';
COMMENT ON CONSTRAINT valid_media_name ON media IS 'File name validation: not empty, 1-255 chars, no / character';
