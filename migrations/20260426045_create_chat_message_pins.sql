CREATE TABLE IF NOT EXISTS chat_message_pins (
    room_id BIGINT NOT NULL,
    message_id BIGINT NOT NULL,
    message_created_at TIMESTAMPTZ NOT NULL,
    pinned_by BIGINT,
    note TEXT,
    pinned_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, message_id, message_created_at),
    FOREIGN KEY (room_id) REFERENCES rooms(id) ON DELETE CASCADE,
    FOREIGN KEY (pinned_by) REFERENCES users(id) ON DELETE SET NULL,
    FOREIGN KEY (room_id, message_id, message_created_at)
        REFERENCES chat_messages(room_id, id, created_at) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_message_pins_room_pinned
    ON chat_message_pins(room_id, pinned_at DESC, message_id DESC);

CREATE INDEX IF NOT EXISTS idx_chat_message_pins_message
    ON chat_message_pins(message_id, message_created_at);

CREATE INDEX IF NOT EXISTS idx_chat_message_pins_pinned_by
    ON chat_message_pins(pinned_by, pinned_at DESC)
    WHERE pinned_by IS NOT NULL;
