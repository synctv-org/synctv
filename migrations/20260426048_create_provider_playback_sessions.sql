CREATE TABLE provider_playback_sessions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    playback_generation BIGINT NOT NULL,
    provider_instance_name TEXT,
    credential_owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    resource_key TEXT NOT NULL,
    resource_version TEXT,
    session JSONB NOT NULL,
    state SMALLINT NOT NULL,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    stop_position DOUBLE PRECISION,
    stop_reason SMALLINT,
    cleanup_attempts INTEGER NOT NULL,
    next_cleanup_at TIMESTAMPTZ,
    cleanup_lease_until TIMESTAMPTZ,
    cleanup_fence BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT provider_playback_sessions_generation_positive
        CHECK (playback_generation > 0),
    CONSTRAINT provider_playback_sessions_session_object
        CHECK (jsonb_typeof(session) = 'object')
);

CREATE UNIQUE INDEX provider_playback_sessions_identity_unique
    ON provider_playback_sessions (
        room_id,
        playback_generation,
        COALESCE(provider_instance_name, ''),
        resource_key
    );
CREATE INDEX provider_playback_sessions_generation_idx
    ON provider_playback_sessions (room_id, playback_generation, state);
CREATE INDEX provider_playback_sessions_cleanup_idx
    ON provider_playback_sessions (
        state,
        lease_expires_at,
        next_cleanup_at,
        cleanup_lease_until
    );

CREATE TRIGGER update_provider_playback_sessions_updated_at
    BEFORE UPDATE ON provider_playback_sessions
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
