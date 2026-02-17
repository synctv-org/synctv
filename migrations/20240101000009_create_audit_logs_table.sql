-- Create audit_logs table (partitioned by month)
CREATE TABLE IF NOT EXISTS audit_logs (
    id BIGSERIAL,
    actor_id CHAR(12),
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

-- Partition management functions are defined in migration 20240202120002_audit_log_partition_complete.sql
-- Initial partition creation is also handled there.
