CREATE TABLE IF NOT EXISTS media (
    id CHAR(12) PRIMARY KEY,

    playlist_id CHAR(12),

    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,

    name VARCHAR(255) NOT NULL,

    position DOUBLE PRECISION NOT NULL,

    source_provider VARCHAR(64) NOT NULL,

    source_config JSONB NOT NULL,

    provider_instance_name VARCHAR(64),

    added_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    version INTEGER NOT NULL DEFAULT 0,

    CONSTRAINT valid_media_name CHECK (char_length(name) <= 255),
    CONSTRAINT media_playlist_same_room_fk
        FOREIGN KEY (playlist_id, room_id)
        REFERENCES playlists(id, room_id)
        ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_media_id_room_unique ON media(id, room_id);

CREATE INDEX idx_media_room ON media(room_id);
CREATE INDEX idx_media_creator ON media(creator_id);
CREATE INDEX idx_media_added_at ON media(added_at DESC);
CREATE INDEX idx_media_source_provider ON media(source_provider);
CREATE INDEX idx_media_provider_name ON media(provider_instance_name);
CREATE INDEX idx_media_source_config ON media USING gin(source_config);
CREATE INDEX idx_media_playlist_covering ON media(playlist_id, position, id, source_provider, name);
CREATE INDEX idx_media_room_root_covering ON media(room_id, position, id, source_provider, name)
    WHERE playlist_id IS NULL;

COMMENT ON TABLE media IS 'Media items (videos/audio) in playlists';
COMMENT ON COLUMN media.id IS '12-character base62 ID';
COMMENT ON COLUMN media.playlist_id IS 'Associated playlist (directory). NULL means the media is directly under the room root.';
COMMENT ON COLUMN media.name IS 'Media display name. It is not a routing key or unique path segment.';
COMMENT ON COLUMN media.position IS 'Floating-point order position within the playlist scope';
COMMENT ON COLUMN media.source_provider IS 'Media provider type name (e.g., "bilibili", "alist", "emby", "direct_url")';
COMMENT ON COLUMN media.source_config IS 'Media provider-specific configuration (persistent)';
COMMENT ON COLUMN media.provider_instance_name IS 'Optional media provider instance name for registry lookup (e.g., "bilibili_main"). NULL or empty means use the default local instance for source_provider.';
COMMENT ON COLUMN media.version IS 'Optimistic locking version, incremented on each update';
COMMENT ON COLUMN media.updated_at IS 'Timestamp of last update (auto-maintained by trigger)';
COMMENT ON CONSTRAINT valid_media_name ON media IS 'Media display name must fit within the database column limit.';

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
