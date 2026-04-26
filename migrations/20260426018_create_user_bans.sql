CREATE TABLE IF NOT EXISTS user_bans (
    id CHAR(12) PRIMARY KEY,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    banned_by CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,
    reason TEXT,
    starts_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    ends_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    revoked_by CHAR(12) REFERENCES users(id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_user_bans_active_unique
    ON user_bans(user_id)
    WHERE revoked_at IS NULL
      AND ends_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_user_bans_user
    ON user_bans(user_id, starts_at DESC);

CREATE INDEX IF NOT EXISTS idx_user_bans_banned_by
    ON user_bans(banned_by, starts_at DESC)
    WHERE banned_by IS NOT NULL;

COMMENT ON TABLE user_bans IS 'Global user ban records';
