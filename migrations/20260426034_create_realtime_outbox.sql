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
    ON realtime_outbox (status, next_retry_at, created_at);

CREATE INDEX IF NOT EXISTS idx_realtime_outbox_aggregate
    ON realtime_outbox (aggregate_type, aggregate_id, created_at);
