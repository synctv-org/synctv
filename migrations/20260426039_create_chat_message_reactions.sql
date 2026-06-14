CREATE TABLE IF NOT EXISTS chat_message_reactions (
    room_id BIGINT NOT NULL,
    message_id BIGINT NOT NULL,
    message_created_at TIMESTAMPTZ NOT NULL,
    user_id BIGINT NOT NULL,
    reaction_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, message_id, message_created_at, user_id, reaction_key),
    FOREIGN KEY (room_id, message_id, message_created_at)
        REFERENCES chat_messages(room_id, id, created_at) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_message_reactions_message
    ON chat_message_reactions(room_id, message_id, message_created_at, reaction_key);
CREATE INDEX IF NOT EXISTS idx_chat_message_reactions_user
    ON chat_message_reactions(user_id, updated_at DESC);
