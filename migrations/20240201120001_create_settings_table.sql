CREATE TABLE IF NOT EXISTS settings (
    key VARCHAR(200) PRIMARY KEY,
    group_name VARCHAR(100) NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    version INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_settings_group ON settings(group_name);

CREATE TRIGGER trigger_update_settings_updated_at
BEFORE UPDATE ON settings
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE OR REPLACE FUNCTION notify_settings_change()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' OR TG_OP = 'UPDATE' THEN
        PERFORM pg_notify('settings_changed', NEW.key);
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('settings_changed', OLD.key);
        RETURN OLD;
    END IF;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER settings_change_trigger
    AFTER INSERT OR UPDATE OR DELETE ON settings
    FOR EACH ROW
    EXECUTE FUNCTION notify_settings_change();

COMMENT ON TABLE settings IS 'Runtime system settings organized by groups with JSON values';
COMMENT ON COLUMN settings.key IS 'Unique settings key (e.g., server, email, oauth)';
COMMENT ON COLUMN settings.group_name IS 'Settings group name (e.g., server, email, oauth)';
COMMENT ON FUNCTION notify_settings_change() IS 'Notifies all replicas via PostgreSQL LISTEN/NOTIFY when settings change';
COMMENT ON TRIGGER settings_change_trigger ON settings IS 'Triggers settings_changed notification for hot reload across replicas';
