-- Create notifications table (partitioned by month for efficient retention)
-- Partitioning enables O(1) retention: drop entire monthly partitions instead of DELETE millions of rows

CREATE TABLE IF NOT EXISTS notifications (
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type VARCHAR(50) NOT NULL, -- room_invitation, system_announcement, room_event, etc.
    title VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    data JSONB DEFAULT '{}', -- Additional metadata (room_id, etc.)
    is_read BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)  -- Partition key must be in PK
) PARTITION BY RANGE (created_at);

-- Default partition catches any rows that don't match a specific monthly partition.
-- This prevents insert failures if a new partition hasn't been created yet.
CREATE TABLE IF NOT EXISTS notifications_default PARTITION OF notifications DEFAULT;

-- ============================================================================
-- Partition management functions
-- ============================================================================

-- Function 1: Create a single monthly partition with indexes
CREATE OR REPLACE FUNCTION create_notification_partition(
    partition_month DATE DEFAULT DATE_TRUNC('month', CURRENT_DATE)
) RETURNS JSON AS $$
DECLARE
    partition_name TEXT;
    start_date DATE;
    end_date DATE;
    index_count INTEGER := 0;
BEGIN
    -- Normalize to start of month
    start_date := DATE_TRUNC('month', partition_month);
    end_date := start_date + INTERVAL '1 month';
    partition_name := 'notifications_' || TO_CHAR(start_date, 'YYYY_MM');

    -- Create partition
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF notifications
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, start_date, end_date
    );

    -- Index 1: User notifications by read status and created_at (primary query pattern)
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(user_id, is_read, created_at DESC)',
        partition_name || '_idx_user_read_created', partition_name
    );
    index_count := index_count + 1;

    -- Index 2: Unread notifications by user (partial index)
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(user_id, created_at DESC) WHERE is_read = FALSE',
        partition_name || '_idx_user_unread', partition_name
    );
    index_count := index_count + 1;

    RETURN json_build_object(
        'partition_name', partition_name,
        'start_date', start_date,
        'end_date', end_date,
        'indexes_created', index_count,
        'status', 'success'
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION create_notification_partition(DATE) IS
'Create a single notification partition for a given month with indexes (idempotent). Parameter: month date (default: current month)';

-- Function 2: Batch-create future partitions
CREATE OR REPLACE FUNCTION create_notification_partitions(
    months_ahead INTEGER DEFAULT 3
) RETURNS JSON AS $$
DECLARE
    i INTEGER;
    partition_month DATE;
    result JSON;
    partitions JSONB := '[]'::JSONB;
    success_count INTEGER := 0;
BEGIN
    partition_month := DATE_TRUNC('month', CURRENT_DATE);

    FOR i IN 0..months_ahead LOOP
        result := create_notification_partition(partition_month);
        partitions := partitions || result::JSONB;
        success_count := success_count + 1;
        partition_month := partition_month + INTERVAL '1 month';
    END LOOP;

    RETURN json_build_object(
        'status', 'completed',
        'total_requested', months_ahead + 1,
        'success_count', success_count,
        'partitions', partitions
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION create_notification_partitions(INTEGER) IS
'Batch-create notification partitions for current month + N months ahead (monthly granularity). Parameter: months ahead (default: 3)';

-- Function 3: Drop old partitions (retention enforcement)
CREATE OR REPLACE FUNCTION drop_old_notification_partitions(
    retain_months INTEGER DEFAULT 6
) RETURNS JSON AS $$
DECLARE
    cutoff_date DATE;
    cutoff_name TEXT;
    partition_record RECORD;
    dropped JSON := '[]'::JSON;
    drop_count INTEGER := 0;
BEGIN
    cutoff_date := DATE_TRUNC('month', CURRENT_DATE) - (retain_months || ' months')::INTERVAL;
    cutoff_name := 'notifications_' || TO_CHAR(cutoff_date, 'YYYY_MM');

    -- Find and drop old monthly partitions (never drop the default partition)
    FOR partition_record IN
        SELECT tablename
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'notifications_%'
          AND tablename != 'notifications_default'
          AND tablename ~ '^notifications_[0-9]{4}_[0-9]{2}$'
          AND tablename < cutoff_name
        ORDER BY tablename
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %I', partition_record.tablename);
        dropped := dropped || json_build_object('partition', partition_record.tablename);
        drop_count := drop_count + 1;

        RAISE NOTICE 'Dropped notification partition: %', partition_record.tablename;
    END LOOP;

    RETURN json_build_object(
        'status', 'success',
        'dropped_count', drop_count,
        'retain_months', retain_months,
        'cutoff_date', cutoff_date,
        'dropped_partitions', dropped
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION drop_old_notification_partitions(INTEGER) IS
'Drop notification partitions older than N months. Parameter: months to retain (default: 6)';

-- Function 4: Retention-based cleanup within current partitions (delete by policy)
--
-- Deletes read notifications older than `read_retention_days` and ALL
-- notifications (including unread) older than `max_retention_days`.
-- This prevents indefinite accumulation of unread notifications within a partition.
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

-- ============================================================================
-- Initial partition creation
-- ============================================================================

-- Create partitions for current month + next 3 months
SELECT create_notification_partitions(3) AS initial_partitions;

-- ============================================================================
-- Comments
-- ============================================================================

COMMENT ON TABLE notifications IS 'User notifications partitioned by month (RANGE on created_at). Use drop_old_notification_partitions() for O(1) retention.';
COMMENT ON COLUMN notifications.type IS 'Notification type: room_invitation, system_announcement, room_event, etc.';
COMMENT ON COLUMN notifications.data IS 'Additional metadata in JSON format (e.g., room_id, sender_id)';

-- Trigger to update updated_at timestamp (uses generic function from users migration)
-- Note: triggers must be created on each partition individually in PostgreSQL 14+.
-- For simplicity we apply a single statement trigger rule on the parent table.
-- The trigger function is defined in the users migration (update_updated_at_column).
CREATE TRIGGER trigger_update_notifications_updated_at
BEFORE UPDATE ON notifications
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();
