CREATE TABLE IF NOT EXISTS chat_moderation_jobs (
    id TEXT PRIMARY KEY,
    room_id BIGINT NOT NULL,
    target_user_id BIGINT NOT NULL,
    actor_user_id BIGINT NOT NULL,
    actor_username TEXT NOT NULL,
    actor_role SMALLINT NOT NULL,
    message_id BIGINT,
    explicit_message_done BOOLEAN NOT NULL DEFAULT FALSE,
    ban_user BOOLEAN NOT NULL DEFAULT FALSE,
    ban_done BOOLEAN NOT NULL DEFAULT FALSE,
    delete_all_messages BOOLEAN NOT NULL DEFAULT FALSE,
    delete_all_reactions BOOLEAN NOT NULL DEFAULT FALSE,
    reason TEXT,
    phase SMALLINT NOT NULL DEFAULT 1,
    status SMALLINT NOT NULL DEFAULT 1,
    snapshot_at TIMESTAMPTZ NOT NULL,
    message_cursor_created_at TIMESTAMPTZ,
    message_cursor_id BIGINT,
    reaction_cursor_created_at TIMESTAMPTZ,
    reaction_cursor_id BIGINT,
    hidden_reaction_cursor_created_at TIMESTAMPTZ,
    hidden_reaction_cursor_id BIGINT,
    hidden_reaction_cursor_key TEXT,
    deleted_messages BIGINT NOT NULL DEFAULT 0,
    deleted_reactions BIGINT NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    lock_version BIGINT NOT NULL DEFAULT 1,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMPTZ,
    FOREIGN KEY (room_id) REFERENCES rooms(id) ON DELETE CASCADE,
    FOREIGN KEY (target_user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_moderation_jobs_pending
    ON chat_moderation_jobs (status, next_attempt_at, updated_at, id)
    WHERE status = 1;

CREATE INDEX IF NOT EXISTS idx_chat_moderation_jobs_processing_lease
    ON chat_moderation_jobs (locked_at)
    WHERE status = 2 AND locked_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_chat_moderation_jobs_target
    ON chat_moderation_jobs (room_id, target_user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_chat_moderation_jobs_terminal_updated
    ON chat_moderation_jobs (updated_at)
    WHERE status IN (3, 4);
