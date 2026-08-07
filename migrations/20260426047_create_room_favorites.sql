CREATE TABLE IF NOT EXISTS room_favorites (
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, room_id),
    CONSTRAINT room_favorites_room_member_fk
        FOREIGN KEY (room_id, user_id)
        REFERENCES room_members(room_id, user_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_room_favorites_user_created
    ON room_favorites(user_id, created_at DESC, room_id DESC);
CREATE INDEX IF NOT EXISTS idx_room_favorites_room
    ON room_favorites(room_id);
