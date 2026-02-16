-- Create audit_logs table (partitioned by month)
CREATE TABLE IF NOT EXISTS audit_logs (
    id BIGSERIAL,
    actor_id CHAR(12) REFERENCES users(id) ON DELETE SET NULL,
    actor_username VARCHAR(50),
    action VARCHAR(50) NOT NULL,
    target_type VARCHAR(50),
    target_id VARCHAR(100),
    details JSONB,
    ip_address INET,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

-- Comments
COMMENT ON TABLE audit_logs IS 'Security and operational audit log (partitioned by month with automated management)';
COMMENT ON COLUMN audit_logs.action IS 'Action type: user_created, user_banned, room_deleted, etc.';
COMMENT ON COLUMN audit_logs.details IS 'Event-specific details (JSON)';

-- ============================================================================
-- Partition management functions
-- ============================================================================

-- Function 1: Create a single partition with indexes
CREATE OR REPLACE FUNCTION create_audit_log_partition(
    partition_date DATE DEFAULT CURRENT_DATE
) RETURNS JSON AS $$
DECLARE
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
    index_count INTEGER := 0;
BEGIN
    partition_name := 'audit_logs_' || TO_CHAR(partition_date, 'YYYY_MM');
    start_date := TO_CHAR(partition_date, 'YYYY-MM') || '-01';
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM') || '-01';

    -- Create partition
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF audit_logs
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, start_date, end_date
    );

    -- Index 1: Actor lookup (user who performed the action)
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(actor_id, created_at DESC)',
        partition_name || '_idx_actor', partition_name
    );
    index_count := index_count + 1;

    -- Index 2: Action type lookup
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(action, created_at DESC)',
        partition_name || '_idx_action', partition_name
    );
    index_count := index_count + 1;

    -- Index 3: Target lookup (what was acted upon)
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(target_type, target_id, created_at DESC)',
        partition_name || '_idx_target', partition_name
    );
    index_count := index_count + 1;

    -- Index 4: Timestamp lookup
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(created_at)',
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

COMMENT ON FUNCTION create_audit_log_partition(DATE) IS
'Create a single audit log partition with indexes (idempotent). Parameter: partition date (default: current date)';

-- Function 2: Batch-create future partitions
CREATE OR REPLACE FUNCTION create_audit_log_partitions(
    months_ahead INTEGER DEFAULT 6
) RETURNS JSON AS $$
DECLARE
    i INTEGER;
    partition_date DATE;
    result JSON;
    partitions JSONB := '[]'::JSONB;
    success_count INTEGER := 0;
BEGIN
    partition_date := DATE_TRUNC('month', CURRENT_DATE);

    FOR i IN 0..months_ahead LOOP
        result := create_audit_log_partition(partition_date);
        partitions := partitions || result::JSONB;
        success_count := success_count + 1;
        partition_date := partition_date + INTERVAL '1 month';
    END LOOP;

    RETURN json_build_object(
        'status', 'completed',
        'total_requested', months_ahead + 1,
        'success_count', success_count,
        'partitions', partitions
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION create_audit_log_partitions(INTEGER) IS
'Batch-create audit log partitions for current month + N months ahead. Parameter: months ahead (default: 6)';

-- Function 3: Drop old partitions (retention enforcement)
CREATE OR REPLACE FUNCTION drop_old_audit_log_partitions(
    keep_months INTEGER DEFAULT 12
) RETURNS JSON AS $$
DECLARE
    cutoff_date DATE;
    partition_record RECORD;
    dropped JSON := '[]'::JSON;
    drop_count INTEGER := 0;
BEGIN
    cutoff_date := CURRENT_DATE - (keep_months || ' months')::INTERVAL;

    FOR partition_record IN
        SELECT tablename
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'audit_logs_%'
          AND tablename ~ '^audit_logs_[0-9]{4}_[0-9]{2}$'
          AND tablename < 'audit_logs_' || TO_CHAR(cutoff_date, 'YYYY_MM')
        ORDER BY tablename
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %I', partition_record.tablename);
        dropped := dropped || json_build_object('partition', partition_record.tablename);
        drop_count := drop_count + 1;

        RAISE NOTICE 'Dropped audit log partition: %', partition_record.tablename;
    END LOOP;

    RETURN json_build_object(
        'status', 'success',
        'dropped_count', drop_count,
        'keep_months', keep_months,
        'dropped_partitions', dropped
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION drop_old_audit_log_partitions(INTEGER) IS
'Drop audit log partitions older than N months. Parameter: months to keep (default: 12 - longer retention for audit logs)';

-- Function 4: Check partition health
CREATE OR REPLACE FUNCTION check_audit_log_partitions() RETURNS JSON AS $$
DECLARE
    current_month TEXT;
    expected_months TEXT[] := ARRAY[]::TEXT[];
    missing_partitions JSON := '[]'::JSON;
    total_partitions INTEGER := 0;
    total_size BIGINT := 0;
    partition_record RECORD;
BEGIN
    FOR i IN 0..6 LOOP
        current_month := 'audit_logs_' || TO_CHAR(CURRENT_DATE + (i || ' months')::INTERVAL, 'YYYY_MM');
        expected_months := array_append(expected_months, current_month);
    END LOOP;

    FOREACH current_month IN ARRAY expected_months
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_tables
            WHERE schemaname = 'public' AND tablename = current_month
        ) THEN
            missing_partitions := missing_partitions || json_build_object(
                'partition_name', current_month,
                'status', 'missing'
            );
        END IF;
    END LOOP;

    FOR partition_record IN
        SELECT
            tablename,
            pg_total_relation_size(format('%I.%I', schemaname, tablename)) as size
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'audit_logs_%'
          AND tablename ~ '^audit_logs_[0-9]{4}_[0-9]{2}$'
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

COMMENT ON FUNCTION check_audit_log_partitions() IS
'Check health of audit log partitions. Returns missing partitions and stats.';

-- ============================================================================
-- Initial partition creation
-- ============================================================================

-- Create partitions for current month + next 6 months
SELECT create_audit_log_partitions(6) AS initial_partitions;
