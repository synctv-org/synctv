-- Create chat_messages table (partitioned by month for efficient time-based retention)
-- Partitioning enables O(1) retention: drop entire monthly partitions instead of DELETE millions of rows

CREATE TABLE IF NOT EXISTS chat_messages (
    id CHAR(12) NOT NULL,
    room_id CHAR(12) NOT NULL,
    user_id CHAR(12),  -- nullable to support SET NULL on user deletion
    content TEXT NOT NULL,
    message_type SMALLINT NOT NULL DEFAULT 1,  -- 1=text, 2=system, 3=action
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (id, created_at),  -- Partition key must be in PK
    CONSTRAINT chat_messages_message_type_check CHECK (message_type BETWEEN 1 AND 3),
    FOREIGN KEY (room_id) REFERENCES rooms(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
) PARTITION BY RANGE (created_at);

-- Comments
COMMENT ON TABLE chat_messages IS 'Persistent chat messages (partitioned by month, retention configurable)';
COMMENT ON COLUMN chat_messages.id IS '12-character nanoid';
COMMENT ON COLUMN chat_messages.content IS 'Message content (HTML sanitized)';

-- ============================================================================
-- Partition management functions
-- ============================================================================

-- Function 1: Create a single partition with indexes (fixed daily granularity)
CREATE OR REPLACE FUNCTION create_chat_message_partition(
    partition_date DATE DEFAULT CURRENT_DATE
) RETURNS JSON AS $$
DECLARE
    partition_name TEXT;
    start_date DATE;
    end_date DATE;
    index_count INTEGER := 0;
BEGIN
    -- Normalize to start of day
    start_date := DATE_TRUNC('day', partition_date);
    end_date := start_date + INTERVAL '1 day';
    partition_name := 'chat_messages_' || TO_CHAR(start_date, 'YYYY_MM_DD');

    -- Create partition
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF chat_messages
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, start_date, end_date
    );

    -- Index 1: Room pagination (primary query pattern)
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(room_id, created_at DESC, user_id)',
        partition_name || '_idx_room_pagination', partition_name
    );
    index_count := index_count + 1;

    -- Index 2: User messages lookup
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(user_id, created_at DESC)',
        partition_name || '_idx_user_created', partition_name
    );
    index_count := index_count + 1;

    -- Index 3: Time-based queries and partition pruning (Task #18)
    -- Supports DELETE WHERE created_at <= NOW() - INTERVAL '90 days'
    -- and other time-range queries without room_id filter
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(created_at DESC)',
        partition_name || '_idx_created_at', partition_name
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

COMMENT ON FUNCTION create_chat_message_partition(DATE) IS
'Create a single chat message partition with indexes (idempotent, daily granularity). Parameter: partition date (default: current date)';

-- Function 2: Batch-create future partitions (fixed daily granularity)
CREATE OR REPLACE FUNCTION create_chat_message_partitions(
    days_ahead INTEGER DEFAULT 30
) RETURNS JSON AS $$
DECLARE
    i INTEGER;
    partition_date DATE;
    result JSON;
    partitions JSONB := '[]'::JSONB;
    success_count INTEGER := 0;
BEGIN
    partition_date := DATE_TRUNC('day', CURRENT_DATE);

    FOR i IN 0..days_ahead LOOP
        result := create_chat_message_partition(partition_date);
        partitions := partitions || result::JSONB;
        success_count := success_count + 1;
        partition_date := partition_date + INTERVAL '1 day';
    END LOOP;

    RETURN json_build_object(
        'status', 'completed',
        'total_requested', days_ahead + 1,
        'success_count', success_count,
        'partitions', partitions
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION create_chat_message_partitions(INTEGER) IS
'Batch-create chat message partitions for current day + N days ahead (daily granularity). Parameter: days ahead (default: 30)';

-- Function 3: Drop old partitions (retention enforcement, fixed daily granularity)
CREATE OR REPLACE FUNCTION drop_old_chat_message_partitions(
    keep_days INTEGER DEFAULT 90
) RETURNS JSON AS $$
DECLARE
    cutoff_date DATE;
    cutoff_name TEXT;
    partition_record RECORD;
    dropped JSON := '[]'::JSON;
    drop_count INTEGER := 0;
BEGIN
    cutoff_date := CURRENT_DATE - (keep_days || ' days')::INTERVAL;
    cutoff_name := 'chat_messages_' || TO_CHAR(cutoff_date, 'YYYY_MM_DD');

    -- Find and drop old partitions
    FOR partition_record IN
        SELECT tablename
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'chat_messages_%'
          AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
          AND tablename < cutoff_name
        ORDER BY tablename
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %I', partition_record.tablename);
        dropped := dropped || json_build_object('partition', partition_record.tablename);
        drop_count := drop_count + 1;

        RAISE NOTICE 'Dropped chat partition: %', partition_record.tablename;
    END LOOP;

    RETURN json_build_object(
        'status', 'success',
        'dropped_count', drop_count,
        'keep_days', keep_days,
        'cutoff_date', cutoff_date,
        'dropped_partitions', dropped
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION drop_old_chat_message_partitions(INTEGER) IS
'Drop chat message partitions older than N days (daily granularity). Parameter: days to keep (default: 90)';

-- Function 4: Check partition health (fixed daily granularity)
CREATE OR REPLACE FUNCTION check_chat_message_partitions(
    days_ahead INTEGER DEFAULT 30
) RETURNS JSON AS $$
DECLARE
    partition_date DATE;
    expected_name TEXT;
    expected_partitions TEXT[] := ARRAY[]::TEXT[];
    missing_partitions JSON := '[]'::JSON;
    total_partitions INTEGER := 0;
    total_size BIGINT := 0;
    partition_record RECORD;
BEGIN
    partition_date := DATE_TRUNC('day', CURRENT_DATE);

    -- Build list of expected partition names
    FOR i IN 0..days_ahead LOOP
        expected_name := 'chat_messages_' || TO_CHAR(partition_date, 'YYYY_MM_DD');
        expected_partitions := array_append(expected_partitions, expected_name);
        partition_date := partition_date + INTERVAL '1 day';
    END LOOP;

    -- Check for missing partitions
    FOREACH expected_name IN ARRAY expected_partitions
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_tables
            WHERE schemaname = 'public' AND tablename = expected_name
        ) THEN
            missing_partitions := missing_partitions || json_build_object(
                'partition_name', expected_name,
                'status', 'missing'
            );
        END IF;
    END LOOP;

    -- Count total partitions and calculate size
    FOR partition_record IN
        SELECT
            tablename,
            pg_total_relation_size(format('%I.%I', schemaname, tablename)) as size
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'chat_messages_%'
          AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
        ORDER BY tablename DESC
    LOOP
        total_partitions := total_partitions + 1;
        total_size := total_size + partition_record.size;
    END LOOP;

    RETURN json_build_object(
        'status', 'checked',
        'total_partitions', total_partitions,
        'total_size_mb', ROUND(total_size::NUMERIC / 1024 / 1024, 2),
        'missing_partitions', missing_partitions,
        'missing_count', json_array_length(missing_partitions),
        'health_status', CASE
            WHEN json_array_length(missing_partitions) = 0 THEN 'healthy'
            ELSE 'warning'
        END
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION check_chat_message_partitions(INTEGER) IS
'Check health of chat message partitions (daily granularity). Parameter: days ahead to check (default: 30)';

-- ============================================================================
-- Initial partition creation
-- ============================================================================

-- Default partition catches any rows that don't match a specific daily partition.
-- This prevents insert failures if ChatPartitionManager background task hasn't
-- created a partition for the target date yet.
CREATE TABLE IF NOT EXISTS chat_messages_default PARTITION OF chat_messages DEFAULT;

-- Create partitions for current day + next 30 days (fixed daily granularity)
SELECT create_chat_message_partitions(30) AS initial_partitions;

-- Per-row trigger removed: partition pre-creation via create_chat_message_partitions(30) above
-- and periodic background task (ChatPartitionManager) handle partition availability.
DROP TRIGGER IF EXISTS trigger_auto_create_chat_partition ON chat_messages;
DROP FUNCTION IF EXISTS auto_create_chat_message_partition();

