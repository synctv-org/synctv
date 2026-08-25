CREATE INDEX IF NOT EXISTS idx_chat_messages_room_user_created
    ON chat_messages (room_id, user_id, created_at ASC, id ASC);
