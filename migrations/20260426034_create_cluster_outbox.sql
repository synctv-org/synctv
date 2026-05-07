CREATE TABLE IF NOT EXISTS cluster_outbox (
    id TEXT PRIMARY KEY,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_version BIGINT NOT NULL DEFAULT 1,
    aggregate_version BIGINT,
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    dispatched_at TIMESTAMPTZ,
    last_error TEXT,
    CONSTRAINT cluster_outbox_status_check
        CHECK (status IN ('pending', 'processing', 'sent', 'dead'))
);

CREATE INDEX IF NOT EXISTS idx_cluster_outbox_pending
    ON cluster_outbox (status, next_retry_at, created_at)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_cluster_outbox_aggregate
    ON cluster_outbox (aggregate_type, aggregate_id, created_at);

CREATE TABLE IF NOT EXISTS processed_cluster_events (
    event_id TEXT PRIMARY KEY,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE OR REPLACE FUNCTION notify_cluster_outbox_new()
RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('cluster_outbox_new', NEW.id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_cluster_outbox_notify ON cluster_outbox;
CREATE TRIGGER trg_cluster_outbox_notify
AFTER INSERT ON cluster_outbox
FOR EACH ROW
WHEN (NEW.status = 'pending')
EXECUTE FUNCTION notify_cluster_outbox_new();

COMMENT ON TABLE cluster_outbox IS 'Transactional outbox for durable cross-replica cluster events';
COMMENT ON TABLE processed_cluster_events IS 'Cluster event ids processed by local consumers for idempotency';
