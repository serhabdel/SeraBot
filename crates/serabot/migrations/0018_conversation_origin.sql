-- Origin channel of each conversation: 'app' (desktop / mobile / API) or
-- 'telegram'. Outbound Telegram delivery (replies, voice, approval prompts)
-- is restricted to conversations whose origin is 'telegram', so a message
-- sent from the app is never mirrored to the owner's Telegram chat.
--
-- Telegram conversations use the chat id as the conversation id. Backfill
-- those from the per-user binding (same user only); everything else was
-- created by the app and keeps the 'app' default.

ALTER TABLE conversations ADD COLUMN origin_channel TEXT NOT NULL DEFAULT 'app';

UPDATE conversations
   SET origin_channel = 'telegram'
 WHERE EXISTS (
        SELECT 1 FROM user_telegram t
         WHERE t.user_id = conversations.user_id
           AND CAST(t.telegram_chat_id AS TEXT) = conversations.id
       );
