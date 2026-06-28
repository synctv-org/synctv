CREATE TABLE IF NOT EXISTS room_playback_progress (
    id BIGSERIAL PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    media_id BIGINT NULL,
    playlist_id BIGINT NULL,
    target JSONB NULL,
    target_hash TEXT NOT NULL,
    "position" DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT room_playback_progress_position_non_negative
        CHECK ("position" >= 0),
    CONSTRAINT room_playback_progress_has_supported_source
        CHECK (
            (media_id IS NOT NULL AND playlist_id IS NULL AND target IS NULL)
            OR (media_id IS NULL AND playlist_id IS NOT NULL AND target IS NOT NULL)
        ),
    CONSTRAINT room_playback_progress_target_hash_sha256
        CHECK (target_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT room_playback_progress_target_object
        CHECK (target IS NULL OR jsonb_typeof(target) = 'object'),
    CONSTRAINT room_playback_progress_media_same_room_fk
        FOREIGN KEY (media_id, room_id)
        REFERENCES media(id, room_id)
        ON DELETE CASCADE,
    CONSTRAINT room_playback_progress_playlist_same_room_fk
        FOREIGN KEY (playlist_id, room_id)
        REFERENCES playlists(id, room_id)
        ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS room_playback_progress_source_unique
    ON room_playback_progress(
        room_id,
        COALESCE(media_id, 0),
        COALESCE(playlist_id, 0),
        target_hash
    );
CREATE INDEX IF NOT EXISTS idx_room_playback_progress_media_id ON room_playback_progress(media_id);
CREATE INDEX IF NOT EXISTS idx_room_playback_progress_playlist_id ON room_playback_progress(playlist_id);
CREATE INDEX IF NOT EXISTS idx_room_playback_progress_updated_at ON room_playback_progress(updated_at);

CREATE TRIGGER update_room_playback_progress_updated_at
    BEFORE UPDATE ON room_playback_progress
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

CREATE TABLE IF NOT EXISTS room_playback_state (
    room_id BIGINT PRIMARY KEY REFERENCES rooms(id) ON DELETE RESTRICT,
    playing_media_id BIGINT NULL,
    playing_playlist_id BIGINT NULL,
    target JSONB NULL,
    current_progress_id BIGINT NULL REFERENCES room_playback_progress(id) ON DELETE SET NULL,
    speed DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    is_playing BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT room_playback_state_target_object
        CHECK (target IS NULL OR jsonb_typeof(target) = 'object'),
    CONSTRAINT playback_media_same_room_fk
        FOREIGN KEY (playing_media_id, room_id)
        REFERENCES media(id, room_id)
        ON DELETE RESTRICT,
    CONSTRAINT playback_playlist_same_room_fk
        FOREIGN KEY (playing_playlist_id, room_id)
        REFERENCES playlists(id, room_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_room_playback_state_media_id ON room_playback_state(playing_media_id);
CREATE INDEX IF NOT EXISTS idx_room_playback_state_playlist_id ON room_playback_state(playing_playlist_id);
CREATE INDEX IF NOT EXISTS idx_room_playback_state_current_progress_id ON room_playback_state(current_progress_id);
CREATE INDEX IF NOT EXISTS idx_room_playback_state_updated_at ON room_playback_state(updated_at);

CREATE TRIGGER update_room_playback_state_updated_at
    BEFORE UPDATE ON room_playback_state
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
