-- Backend-owned metadata for concrete playback sources.
-- Static media sources are keyed by media_id + target_hash; dynamic playlist
-- items are keyed by playlist_id + target_hash because they do not have media
-- rows.

CREATE TABLE IF NOT EXISTS playback_source_metadata (
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    media_id BIGINT REFERENCES media(id) ON DELETE CASCADE,
    playlist_id BIGINT REFERENCES playlists(id) ON DELETE CASCADE,
    target_hash TEXT NOT NULL,
    media_name TEXT,
    playlist_name TEXT,
    is_live BOOLEAN,
    duration_seconds DOUBLE PRECISION,
    duration_status SMALLINT NOT NULL DEFAULT 0,
    duration_source SMALLINT,
    duration_error TEXT,
    next_retry_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    version BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT playback_source_metadata_source_present CHECK (
        media_id IS NOT NULL OR playlist_id IS NOT NULL
    ),
    CONSTRAINT playback_source_metadata_duration_non_negative CHECK (
        duration_seconds IS NULL OR duration_seconds >= 0
    ),
    CONSTRAINT playback_source_metadata_live_has_no_duration CHECK (
        is_live IS DISTINCT FROM TRUE OR duration_seconds IS NULL
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS playback_source_metadata_source_unique
    ON playback_source_metadata(
        room_id,
        COALESCE(media_id, 0),
        COALESCE(playlist_id, 0),
        target_hash
    );

CREATE INDEX IF NOT EXISTS idx_playback_source_metadata_room
    ON playback_source_metadata(room_id);

CREATE INDEX IF NOT EXISTS idx_playback_source_metadata_probeable
    ON playback_source_metadata(duration_status, next_retry_at, updated_at)
    WHERE duration_seconds IS NULL AND is_live = FALSE;

CREATE TRIGGER update_playback_source_metadata_updated_at
    BEFORE UPDATE ON playback_source_metadata
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
