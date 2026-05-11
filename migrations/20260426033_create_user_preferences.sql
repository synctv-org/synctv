CREATE TABLE IF NOT EXISTS user_preferences (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    two_factor_enabled BOOLEAN NOT NULL DEFAULT FALSE,

    notify_room_invitation_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_room_event_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_system_announcement_in_app BOOLEAN NOT NULL DEFAULT TRUE,
    notify_room_invitation_email BOOLEAN NOT NULL DEFAULT FALSE,
    notify_room_event_email BOOLEAN NOT NULL DEFAULT FALSE,
    notify_system_announcement_email BOOLEAN NOT NULL DEFAULT TRUE,

    provider_default_instance_names JSONB NOT NULL DEFAULT '{}'::jsonb,

    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT user_preferences_provider_defaults_object
        CHECK (jsonb_typeof(provider_default_instance_names) = 'object'),
    CONSTRAINT user_preferences_settings_object
        CHECK (jsonb_typeof(settings) = 'object')
);

CREATE TRIGGER update_user_preferences_updated_at
    BEFORE UPDATE ON user_preferences
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
