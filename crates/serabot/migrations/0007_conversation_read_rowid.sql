-- Track unread state by message sequence instead of by timestamp.
--
-- The previous marker compared RFC3339 strings: `messages.created_at` is stored
-- at nanosecond precision while `last_read_at` was written at millisecond
-- precision, and chrono trims trailing zeros. A message written in the same
-- millisecond as the read marker therefore sorted *before* it ("…26.661123Z" <
-- "…26.661Z" because '1' < 'Z') and was counted as already read.
--
-- A monotonic rowid has no clock resolution, no formatting and no tie-break
-- ambiguity: unread is exactly "messages newer than the last one the owner saw".
ALTER TABLE conversations ADD COLUMN last_read_rowid INTEGER;

-- Preserve existing read state for conversations that already have a marker.
UPDATE conversations
   SET last_read_rowid = (
         SELECT COALESCE(MAX(m.rowid), 0)
           FROM messages m
          WHERE m.conversation_id = conversations.id
            AND m.created_at <= conversations.last_read_at
       )
 WHERE last_read_at IS NOT NULL;
