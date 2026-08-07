CREATE TABLE IF NOT EXISTS chat_message_mentions (
    room_id BIGINT NOT NULL,
    message_id BIGINT NOT NULL,
    message_created_at TIMESTAMPTZ NOT NULL,
    mentioned_user_id BIGINT NOT NULL,
    start_char INTEGER NOT NULL,
    length_chars INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, message_id, message_created_at, start_char, mentioned_user_id),
    CHECK (start_char >= 0),
    CHECK (length_chars > 0),
    FOREIGN KEY (room_id) REFERENCES rooms(id) ON DELETE CASCADE,
    FOREIGN KEY (mentioned_user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (room_id, message_id, message_created_at)
        REFERENCES chat_messages(room_id, id, created_at) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_message_mentions_user
    ON chat_message_mentions(room_id, mentioned_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_chat_message_mentions_message
    ON chat_message_mentions(message_id, message_created_at, start_char, mentioned_user_id);
