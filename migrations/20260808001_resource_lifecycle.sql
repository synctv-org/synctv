-- Unified lifecycle metadata for account-owned resources.
-- A deleted row remains recoverable during the configured retention window.
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS deletion_source TEXT,
    ADD COLUMN IF NOT EXISTS deletion_reason TEXT,
    ADD COLUMN IF NOT EXISTS deleted_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS restored_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS restored_by BIGINT REFERENCES users(id) ON DELETE SET NULL;

ALTER TABLE rooms
    ADD COLUMN IF NOT EXISTS deletion_source TEXT,
    ADD COLUMN IF NOT EXISTS deleted_owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE playlists
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deletion_source TEXT,
    ADD COLUMN IF NOT EXISTS deleted_owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE media
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deletion_source TEXT,
    ADD COLUMN IF NOT EXISTS deleted_owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE chat_messages
    ADD COLUMN IF NOT EXISTS deletion_source TEXT,
    ADD COLUMN IF NOT EXISTS deleted_owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE auth_email_identities
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deletion_source TEXT;

ALTER TABLE auth_oauth2_identities
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deletion_source TEXT;

-- Before lifecycle metadata existed, chat moderation already used
-- `deleted_at` for user-requested message deletion. Preserve that state and
-- let the resource retention cleanup retire those historical rows.
UPDATE chat_messages
SET deletion_source = 'user'
WHERE deleted_at IS NOT NULL
  AND deletion_source IS NULL;

-- Existing table-level uniqueness prevents a deleted identity from releasing
-- its email/provider subject. Replace it with active-row partial indexes.
ALTER TABLE auth_oauth2_identities
    DROP CONSTRAINT IF EXISTS auth_oauth2_identities_instance_subject_unique;

DROP INDEX IF EXISTS idx_auth_email_identities_email_lower;
CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_email_identities_email_lower_active
    ON auth_email_identities(LOWER(email)) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_oauth2_identities_instance_subject_active
    ON auth_oauth2_identities(provider_instance_name, provider_user_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_users_deleted_at_source
    ON users(deleted_at, deletion_source) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_rooms_deletion_source
    ON rooms(deletion_source, deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_rooms_deleted_owner
    ON rooms(deleted_owner_id) WHERE deleted_owner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_playlists_deleted_at
    ON playlists(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_playlists_deletion_source
    ON playlists(deletion_source, deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_playlists_deleted_owner
    ON playlists(deleted_owner_id) WHERE deleted_owner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_media_deleted_at
    ON media(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_media_deletion_source
    ON media(deletion_source, deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_media_deleted_owner
    ON media(deleted_owner_id) WHERE deleted_owner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_chat_messages_deletion_source
    ON chat_messages(deletion_source, deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_chat_messages_deleted_owner
    ON chat_messages(deleted_owner_id) WHERE deleted_owner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_auth_email_identities_deleted_at
    ON auth_email_identities(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_auth_oauth2_identities_deleted_at
    ON auth_oauth2_identities(deleted_at) WHERE deleted_at IS NOT NULL;

ALTER TABLE users
    ADD CONSTRAINT users_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'admin', 'system'));
ALTER TABLE playlists
    ADD CONSTRAINT playlists_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'room', 'user'));
ALTER TABLE rooms
    ADD CONSTRAINT rooms_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'user', 'admin'));
ALTER TABLE media
    ADD CONSTRAINT media_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'room', 'user'));
ALTER TABLE chat_messages
    ADD CONSTRAINT chat_messages_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'room', 'user'));
ALTER TABLE auth_email_identities
    ADD CONSTRAINT auth_email_identities_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'user'));
ALTER TABLE auth_oauth2_identities
    ADD CONSTRAINT auth_oauth2_identities_deletion_source_valid
    CHECK (deletion_source IS NULL OR deletion_source IN ('account', 'user'));
