-- Create room_playback_state table
CREATE TABLE IF NOT EXISTS room_playback_state (
    room_id CHAR(12) PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE,
    playing_media_id CHAR(12) NULL REFERENCES media(id) ON DELETE SET NULL,
    playing_playlist_id CHAR(12) NULL REFERENCES playlists(id) ON DELETE SET NULL,
    relative_path TEXT NOT NULL DEFAULT '',
    "current_time" DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    speed DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    is_playing BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 0
);

-- Create indexes
CREATE INDEX idx_room_playback_state_media_id ON room_playback_state(playing_media_id);
CREATE INDEX idx_room_playback_state_playlist_id ON room_playback_state(playing_playlist_id);
CREATE INDEX idx_room_playback_state_updated_at ON room_playback_state(updated_at);

-- Create updated_at trigger
CREATE TRIGGER update_room_playback_state_updated_at
    BEFORE UPDATE ON room_playback_state
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Add check constraints
ALTER TABLE room_playback_state ADD CONSTRAINT playback_current_time_check
    CHECK ("current_time" >= 0);
ALTER TABLE room_playback_state ADD CONSTRAINT playback_speed_check
    CHECK (speed > 0 AND speed <= 16.0);

-- Comments
COMMENT ON TABLE room_playback_state IS 'Current playback state for each room';
COMMENT ON COLUMN room_playback_state.playing_media_id IS 'Currently playing media item';
COMMENT ON COLUMN room_playback_state.playing_playlist_id IS 'Currently playing playlist';
COMMENT ON COLUMN room_playback_state.relative_path IS 'Relative path within dynamic folder (empty for static playlists)';
COMMENT ON COLUMN room_playback_state."current_time" IS 'Playback position in seconds';
COMMENT ON COLUMN room_playback_state.speed IS 'Playback speed (0.5, 1.0, 1.5, 2.0, etc.)';
COMMENT ON COLUMN room_playback_state.version IS 'Optimistic locking version';
