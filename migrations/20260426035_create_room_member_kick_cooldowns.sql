CREATE TABLE IF NOT EXISTS room_member_kick_cooldowns (
    id BIGSERIAL PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kicked_by BIGINT,
    starts_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    ends_at TIMESTAMPTZ NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_room_member_kick_cooldowns_room
    ON room_member_kick_cooldowns(room_id, starts_at DESC);

CREATE INDEX IF NOT EXISTS idx_room_member_kick_cooldowns_user
    ON room_member_kick_cooldowns(user_id, starts_at DESC);

CREATE INDEX IF NOT EXISTS idx_room_member_kick_cooldowns_active
    ON room_member_kick_cooldowns(room_id, user_id, ends_at);
