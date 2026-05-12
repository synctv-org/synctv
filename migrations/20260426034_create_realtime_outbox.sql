CREATE TABLE IF NOT EXISTS realtime_outbox (
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
    CONSTRAINT realtime_outbox_status_check
        CHECK (status IN ('pending', 'processing', 'sent', 'dead'))
);

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_pending
    ON realtime_outbox (status, next_retry_at, created_at)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_aggregate
    ON realtime_outbox (aggregate_type, aggregate_id, created_at);

CREATE OR REPLACE FUNCTION notify_realtime_outbox_new()
RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('realtime_outbox_new', NEW.id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_realtime_outbox_notify ON realtime_outbox;
CREATE TRIGGER trg_realtime_outbox_notify
AFTER INSERT ON realtime_outbox
FOR EACH ROW
WHEN (NEW.status = 'pending')
EXECUTE FUNCTION notify_realtime_outbox_new();
