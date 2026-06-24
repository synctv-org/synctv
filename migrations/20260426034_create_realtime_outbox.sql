CREATE TABLE IF NOT EXISTS realtime_outbox (
    id TEXT PRIMARY KEY,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_version BIGINT NOT NULL DEFAULT 1,
    aggregate_version BIGINT,
    payload JSONB NOT NULL,
    status SMALLINT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    dispatched_at TIMESTAMPTZ,
    last_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_pending
    ON realtime_outbox (next_retry_at, created_at, id)
    WHERE status = 1;

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_aggregate
    ON realtime_outbox (aggregate_type, aggregate_id, created_at);

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_sent_dispatched
    ON realtime_outbox (dispatched_at)
    WHERE status = 3 AND dispatched_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_dead_created
    ON realtime_outbox (created_at)
    WHERE status = 4;

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_processing_locked
    ON realtime_outbox (locked_at)
    WHERE status = 2 AND locked_at IS NOT NULL;
