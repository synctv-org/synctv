-- Create notifications table
CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type VARCHAR(50) NOT NULL, -- room_invitation, system_announcement, room_event, etc.
    title VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    data JSONB DEFAULT '{}', -- Additional metadata (room_id, etc.)
    is_read BOOLEAN DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Indexes for efficient queries
CREATE INDEX idx_notifications_user_id ON notifications(user_id);
CREATE INDEX idx_notifications_is_read ON notifications(is_read);
CREATE INDEX idx_notifications_created_at ON notifications(created_at DESC);
CREATE INDEX idx_notifications_user_unread ON notifications(user_id, is_read) WHERE is_read = FALSE;
CREATE INDEX idx_notifications_user_created ON notifications(user_id, created_at DESC);

-- Trigger to update updated_at timestamp
CREATE OR REPLACE FUNCTION update_notifications_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_notifications_updated_at
BEFORE UPDATE ON notifications
FOR EACH ROW
EXECUTE FUNCTION update_notifications_updated_at();

COMMENT ON TABLE notifications IS 'User notifications for room invitations, system announcements, and room events';
COMMENT ON COLUMN notifications.type IS 'Notification type: room_invitation, system_announcement, room_event, etc.';
COMMENT ON COLUMN notifications.data IS 'Additional metadata in JSON format (e.g., room_id, sender_id)';

-- ============================================================================
-- Retention management
-- ============================================================================

-- Function: Delete old notifications to prevent unbounded table growth
--
-- Deletes read notifications older than `read_retention_days` and ALL
-- notifications (including unread) older than `max_retention_days`.
-- This prevents indefinite accumulation of unread notifications.
CREATE OR REPLACE FUNCTION cleanup_old_notifications(
    read_retention_days INTEGER DEFAULT 30,
    max_retention_days INTEGER DEFAULT 90
) RETURNS JSON AS $$
DECLARE
    read_deleted BIGINT;
    expired_deleted BIGINT;
BEGIN
    -- 1. Delete read notifications past their retention period
    DELETE FROM notifications
    WHERE is_read = TRUE
      AND created_at < CURRENT_TIMESTAMP - (read_retention_days || ' days')::INTERVAL;
    GET DIAGNOSTICS read_deleted = ROW_COUNT;

    -- 2. Delete ALL notifications (including unread) past the maximum retention period
    DELETE FROM notifications
    WHERE created_at < CURRENT_TIMESTAMP - (max_retention_days || ' days')::INTERVAL;
    GET DIAGNOSTICS expired_deleted = ROW_COUNT;

    RETURN json_build_object(
        'status', 'success',
        'read_deleted', read_deleted,
        'expired_deleted', expired_deleted,
        'read_retention_days', read_retention_days,
        'max_retention_days', max_retention_days
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION cleanup_old_notifications(INTEGER, INTEGER) IS
'Delete old notifications: read notifications after read_retention_days (default 30), all notifications after max_retention_days (default 90)';
