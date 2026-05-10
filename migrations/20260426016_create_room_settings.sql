CREATE TABLE IF NOT EXISTS room_settings (
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    key VARCHAR(100) NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, key)
);

CREATE TRIGGER update_room_settings_updated_at
    BEFORE UPDATE ON room_settings
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
