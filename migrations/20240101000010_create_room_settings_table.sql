-- Room settings table
-- Each room can have multiple settings stored as key-value pairs
-- Only settings that have been explicitly set are stored (no default rows)

CREATE TABLE IF NOT EXISTS room_settings (
    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    key VARCHAR(100) NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, key)
);

-- Create indexes
CREATE INDEX idx_room_settings_key ON room_settings(key);
CREATE INDEX idx_room_settings_version ON room_settings(room_id, key, version);

-- Trigger to update updated_at timestamp
CREATE TRIGGER update_room_settings_updated_at BEFORE UPDATE ON room_settings
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Comments
COMMENT ON TABLE room_settings IS 'Room configuration settings stored as key-value pairs';
COMMENT ON COLUMN room_settings.room_id IS 'Room ID (references rooms table)';
COMMENT ON COLUMN room_settings.key IS 'Setting key (e.g., require_password, max_members, admin_added_permissions)';
COMMENT ON COLUMN room_settings.value IS 'Setting value (stored as text, parsed based on key)';
COMMENT ON COLUMN room_settings.version IS 'Optimistic lock version for concurrent update detection (CAS)';

-- Common setting keys:
-- Basic settings:
--   - require_password (bool)
--   - max_members (int)
--   - chat_enabled (bool)
--   - danmaku_enabled (bool)
--   - require_approval (bool)
--   - allow_auto_join (bool)
--   - auto_play (json) - AutoPlaySettings serialized as JSON
--
-- Permission overrides (u64 BIGINT UNSIGNED):
--   - admin_added_permissions
--   - admin_removed_permissions
--   - member_added_permissions
--   - member_removed_permissions
--   - guest_added_permissions
--   - guest_removed_permissions
