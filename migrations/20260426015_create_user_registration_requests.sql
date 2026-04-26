CREATE TABLE IF NOT EXISTS user_registration_requests (
    id CHAR(12) PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(255),
    password_hash VARCHAR(255) NOT NULL,
    signup_method SMALLINT NOT NULL DEFAULT 0,
    status SMALLINT NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    reviewed_at TIMESTAMPTZ,
    reviewed_by CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,
    rejection_reason TEXT,

    CONSTRAINT user_registration_requests_email_not_empty
        CHECK (email IS NULL OR length(trim(email)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_user_registration_requests_username_pending
    ON user_registration_requests(username)
    WHERE reviewed_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_user_registration_requests_email_pending
    ON user_registration_requests(email)
    WHERE reviewed_at IS NULL
      AND email IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_user_registration_requests_status_requested
    ON user_registration_requests(status, requested_at DESC);

CREATE INDEX IF NOT EXISTS idx_user_registration_requests_reviewed_by
    ON user_registration_requests(reviewed_by)
    WHERE reviewed_by IS NOT NULL;

COMMENT ON TABLE user_registration_requests IS 'User registration approval workflow records';
COMMENT ON CONSTRAINT user_registration_requests_email_not_empty ON user_registration_requests IS
    'Email is either NULL or non-empty after trimming whitespace';
