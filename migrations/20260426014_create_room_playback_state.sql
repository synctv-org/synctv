CREATE TABLE IF NOT EXISTS room_playback_state (
    room_id BIGINT PRIMARY KEY REFERENCES rooms(id) ON DELETE RESTRICT,
    playing_media_id BIGINT NULL,
    playing_playlist_id BIGINT NULL,
    target BYTEA NOT NULL DEFAULT ''::bytea,
    "current_time" DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    speed DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    is_playing BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT room_playback_state_current_time_non_negative
        CHECK ("current_time" >= 0),
    CONSTRAINT room_playback_state_speed_positive
        CHECK (speed > 0),
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
CREATE INDEX IF NOT EXISTS idx_room_playback_state_updated_at ON room_playback_state(updated_at);

CREATE TRIGGER update_room_playback_state_updated_at
    BEFORE UPDATE ON room_playback_state
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE room_playback_state IS 'Current playback state for each room';
COMMENT ON COLUMN room_playback_state.playing_media_id IS 'Currently playing media item';
COMMENT ON COLUMN room_playback_state.playing_playlist_id IS 'Currently playing playlist';
COMMENT ON COLUMN room_playback_state.target IS 'Opaque provider-facing playback target payload; empty for static media or cleared state';
COMMENT ON COLUMN room_playback_state."current_time" IS 'Playback position in seconds';
COMMENT ON COLUMN room_playback_state.speed IS 'Playback speed';
COMMENT ON COLUMN room_playback_state.version IS 'Optimistic locking version';
