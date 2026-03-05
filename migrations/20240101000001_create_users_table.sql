-- Create users table
CREATE TABLE IF NOT EXISTS users (
    id CHAR(12) PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(255),  -- NULL allowed (e.g., OAuth2 users without email)
    password_hash VARCHAR(255) NOT NULL,
    signup_method SMALLINT NOT NULL DEFAULT 0,  -- 0=unknown, 1=email, 2=password, 3=oauth2, 4=admin_created
    role SMALLINT NOT NULL DEFAULT 3,  -- 1=root, 2=admin, 3=user
    status SMALLINT NOT NULL DEFAULT 2,  -- 1=active, 2=pending, 3=banned
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    password_changed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- Last password change timestamp (for token invalidation)
    password_version INTEGER NOT NULL DEFAULT 0,  -- Incremented on each password change (for JWT invalidation via claims.pv)
    version INTEGER NOT NULL DEFAULT 0,  -- Monotonically increasing integer for optimistic locking (compare-and-increment)
    deleted_at TIMESTAMPTZ NULL,

    -- Ensure email is not empty or whitespace-only
    CONSTRAINT users_email_not_empty CHECK (email IS NULL OR length(trim(email)) > 0),
    -- Role constraint: 1=root, 2=admin, 3=user
    CONSTRAINT users_role_check CHECK (role BETWEEN 1 AND 3),
    -- Status constraint: 1=active, 2=pending, 3=banned
    CONSTRAINT users_status_check CHECK (status BETWEEN 1 AND 3)
);

-- Create indexes
-- Partial unique indexes: usernames and emails can be reused after soft-delete.
-- Only active (non-deleted) users enforce uniqueness.
-- For email, NULL values don't count as duplicates (multiple users can have NULL email)
CREATE UNIQUE INDEX idx_users_username ON users(username) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX idx_users_email ON users(email) WHERE deleted_at IS NULL;
CREATE INDEX idx_users_created_at ON users(created_at);
CREATE INDEX idx_users_deleted_at ON users(deleted_at) WHERE deleted_at IS NOT NULL;

-- Performance optimization indexes
CREATE INDEX idx_users_username_lower ON users(LOWER(username)) WHERE deleted_at IS NULL;
CREATE INDEX idx_users_email_lower ON users(LOWER(email)) WHERE deleted_at IS NULL AND email IS NOT NULL;
CREATE INDEX idx_users_role ON users(role) WHERE deleted_at IS NULL;
CREATE INDEX idx_users_status ON users(status) WHERE deleted_at IS NULL;
CREATE INDEX idx_users_password_changed_at ON users(password_changed_at) WHERE deleted_at IS NULL;

-- pg_trgm GIN indexes for ILIKE pattern matching in user search
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX idx_users_username_trgm ON users USING gin (username gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX idx_users_email_trgm ON users USING gin (email gin_trgm_ops) WHERE deleted_at IS NULL AND email IS NOT NULL;

-- Create updated_at trigger
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Comments
COMMENT ON TABLE users IS 'User accounts with soft delete support (RBAC: role-based access control)';
COMMENT ON COLUMN users.id IS '12-character nanoid';
COMMENT ON COLUMN users.username IS 'Unique username among active (non-deleted) users';
COMMENT ON COLUMN users.email IS 'User email (NULL allowed for OAuth2 users, unique among active users)';
COMMENT ON COLUMN users.signup_method IS 'Registration method: 0=unknown, 1=email, 2=password, 3=oauth2, 4=admin_created';
COMMENT ON COLUMN users.role IS 'User RBAC role: 1=root, 2=admin, 3=user (global access level)';
COMMENT ON COLUMN users.status IS 'User account status: 1=active, 2=pending (email verification), 3=banned';
COMMENT ON COLUMN users.email_verified IS 'Whether the user email has been verified';
COMMENT ON COLUMN users.password_changed_at IS 'Timestamp of last password change. Tokens issued before this timestamp are invalid.';
COMMENT ON COLUMN users.password_version IS 'Monotonically increasing counter, incremented on each password change. Used to invalidate JWTs via the pv claim.';
COMMENT ON COLUMN users.version IS 'Monotonically increasing integer for optimistic locking. Incremented on each UPDATE. Used by compare-and-increment to detect concurrent modifications.';
COMMENT ON COLUMN users.deleted_at IS 'Soft delete timestamp (NULL = active user)';
COMMENT ON CONSTRAINT users_email_not_empty ON users IS 'Ensures email is either NULL or a non-empty string';

