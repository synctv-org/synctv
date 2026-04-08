CREATE TABLE IF NOT EXISTS room_settings (
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    key VARCHAR(100) NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, key)
);

CREATE TRIGGER update_room_settings_updated_at BEFORE UPDATE ON room_settings
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE room_settings IS 'Room configuration settings stored as key-value pairs';
COMMENT ON COLUMN room_settings.room_id IS 'Room ID (references rooms table)';
COMMENT ON COLUMN room_settings.key IS 'Setting key';
COMMENT ON COLUMN room_settings.value IS 'Setting value (stored as text, parsed based on key)';
COMMENT ON COLUMN room_settings.version IS 'Optimistic lock version for concurrent update detection (CAS)';
