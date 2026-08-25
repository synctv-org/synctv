CREATE INDEX IF NOT EXISTS idx_chat_message_reactions_room_user_cleanup
    ON chat_message_reactions (
        room_id,
        user_id,
        message_created_at,
        message_id,
        reaction_key
    );
