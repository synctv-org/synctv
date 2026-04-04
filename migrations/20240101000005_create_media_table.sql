-- Create media table (media files in playlists)

CREATE TABLE IF NOT EXISTS media (
    id CHAR(12) PRIMARY KEY,

    -- ========== Belongs to playlist ==========
    -- NULL means the media lives directly under the room root.
    playlist_id CHAR(12),

    -- ========== Basic information ==========
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE SET NULL,

    -- Media display name
    name VARCHAR(255) NOT NULL,

    -- Sort position (within playlist).
    -- No DEFAULT: callers must always compute the next position via MAX+1
    -- to avoid UNIQUE constraint violations on concurrent inserts.
    position INTEGER NOT NULL,

    -- ========== Video source type (string for flexibility) ==========
    source_provider VARCHAR(64) NOT NULL DEFAULT 'direct_url',

    -- ========== Video source configuration (persistent storage) ==========
    source_config JSONB NOT NULL,

    -- Provider instance name (for registry lookup)
    provider_instance_name VARCHAR(64) NOT NULL,

    -- Timestamps
    added_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    -- Optimistic locking version (incremented on each update)
    version INTEGER NOT NULL DEFAULT 0,

    -- Constraints
    CONSTRAINT valid_media_name CHECK (char_length(name) <= 255),
    CONSTRAINT media_playlist_same_room_fk
        FOREIGN KEY (playlist_id, room_id)
        REFERENCES playlists(id, room_id)
        ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_media_id_room_unique ON media(id, room_id);

-- Unique index: prevent duplicate positions within the same playlist
CREATE UNIQUE INDEX IF NOT EXISTS unique_media_position ON media (playlist_id, position);

-- Root-level media also need unique positions within a room because NULL values
-- are not covered by the composite unique index above.
CREATE UNIQUE INDEX IF NOT EXISTS idx_media_unique_root_position
    ON media(room_id, position)
    WHERE playlist_id IS NULL;

-- Create indexes
CREATE INDEX idx_media_room ON media(room_id);
CREATE INDEX idx_media_creator ON media(creator_id);
CREATE INDEX idx_media_added_at ON media(added_at DESC);
CREATE INDEX idx_media_source_provider ON media(source_provider);
CREATE INDEX idx_media_provider_name ON media(provider_instance_name);
CREATE INDEX idx_media_source_config ON media USING gin(source_config);

-- Performance optimization: covering index for playlist queries
CREATE INDEX idx_media_playlist_covering ON media(playlist_id, position, source_provider, name);

-- Comments
COMMENT ON TABLE media IS 'Media items (videos/audio) in playlists';
COMMENT ON COLUMN media.id IS '12-character base62 ID';
COMMENT ON COLUMN media.playlist_id IS 'Associated playlist (directory). NULL means the media is directly under the room root.';
COMMENT ON COLUMN media.name IS 'Media display name. It is not a routing key or unique path segment.';
COMMENT ON COLUMN media.position IS 'Position in playlist (0-indexed)';
COMMENT ON COLUMN media.source_provider IS 'Media provider type name (e.g., "bilibili", "alist", "emby", "direct_url")';
COMMENT ON COLUMN media.source_config IS 'Media provider-specific configuration (persistent)';
COMMENT ON COLUMN media.provider_instance_name IS 'Media provider instance name for registry lookup (e.g., "bilibili_main"). Always required.';
COMMENT ON COLUMN media.version IS 'Optimistic locking version, incremented on each update';
COMMENT ON COLUMN media.updated_at IS 'Timestamp of last update (auto-maintained by trigger)';
COMMENT ON CONSTRAINT valid_media_name ON media IS 'Media display name must fit within the database column limit.';

-- Trigger to auto-update updated_at on row modification
CREATE OR REPLACE FUNCTION update_media_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_media_updated_at
    BEFORE UPDATE ON media
    FOR EACH ROW
    EXECUTE FUNCTION update_media_updated_at();
