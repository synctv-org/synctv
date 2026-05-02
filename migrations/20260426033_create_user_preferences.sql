CREATE TABLE IF NOT EXISTS user_preferences (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    two_factor_enabled BOOLEAN NOT NULL DEFAULT FALSE,

    notify_room_invitation_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_room_event_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_system_announcement_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_room_invitation_email BOOLEAN NOT NULL DEFAULT FALSE,
    notify_room_event_email BOOLEAN NOT NULL DEFAULT FALSE,
    notify_system_announcement_email BOOLEAN NOT NULL DEFAULT TRUE,

    default_alist_instance_name VARCHAR(64),
    default_emby_instance_name VARCHAR(64),
    default_bilibili_instance_name VARCHAR(64),

    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT user_preferences_default_alist_instance_name_nonblank
        CHECK (default_alist_instance_name IS NULL OR length(trim(default_alist_instance_name)) > 0),
    CONSTRAINT user_preferences_default_emby_instance_name_nonblank
        CHECK (default_emby_instance_name IS NULL OR length(trim(default_emby_instance_name)) > 0),
    CONSTRAINT user_preferences_default_bilibili_instance_name_nonblank
        CHECK (default_bilibili_instance_name IS NULL OR length(trim(default_bilibili_instance_name)) > 0),
    CONSTRAINT user_preferences_settings_object
        CHECK (jsonb_typeof(settings) = 'object')
);

CREATE TRIGGER update_user_preferences_updated_at
    BEFORE UPDATE ON user_preferences
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE user_preferences IS 'Per-user configurable preferences and security options';
COMMENT ON COLUMN user_preferences.two_factor_enabled IS 'Whether user-level two-factor authentication is enabled';
COMMENT ON COLUMN user_preferences.settings IS 'Low-priority extension payload; core preferences use typed columns';
