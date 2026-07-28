CREATE TABLE IF NOT EXISTS email_outbox (
    id TEXT PRIMARY KEY,
    kind SMALLINT NOT NULL,
    recipient TEXT NOT NULL,
    encrypted_payload TEXT NOT NULL,
    dedupe_key TEXT NOT NULL UNIQUE,
    status SMALLINT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    lock_version BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    sent_at TIMESTAMPTZ,
    cleanup_completed_at TIMESTAMPTZ,
    last_error TEXT,
    CHECK (attempts >= 0),
    CHECK (lock_version >= 0)
);

CREATE INDEX IF NOT EXISTS idx_email_outbox_pending
    ON email_outbox (next_attempt_at, created_at, id)
    WHERE status = 1;

CREATE INDEX IF NOT EXISTS idx_email_outbox_processing_lease
    ON email_outbox (locked_at)
    WHERE status = 2 AND locked_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_email_outbox_terminal_retention
    ON email_outbox (COALESCE(sent_at, created_at))
    WHERE status IN (3, 4);

CREATE INDEX IF NOT EXISTS idx_email_outbox_dead_cleanup
    ON email_outbox (created_at, id)
    WHERE status = 4 AND cleanup_completed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_email_outbox_recipient_created
    ON email_outbox (recipient, created_at DESC);
