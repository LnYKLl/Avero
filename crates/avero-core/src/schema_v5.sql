-- Version 5: more headers per message, and conversations (threads).

-- Threading headers: the parent's id, and the References ids separated
-- by spaces.
ALTER TABLE messages ADD COLUMN in_reply_to TEXT;
ALTER TABLE messages ADD COLUMN refs TEXT;
-- Recipients: the To addresses (", " between them) and the mailbox that
-- received the message.
ALTER TABLE messages ADD COLUMN to_addrs TEXT;
ALTER TABLE messages ADD COLUMN delivered_to TEXT;
-- Mailing lists and automatic mail (unsubscribe, sorting in tabs).
ALTER TABLE messages ADD COLUMN list_unsubscribe TEXT;
ALTER TABLE messages ADD COLUMN list_unsubscribe_post TEXT;
ALTER TABLE messages ADD COLUMN list_id TEXT;
ALTER TABLE messages ADD COLUMN precedence TEXT;
ALTER TABLE messages ADD COLUMN auto_submitted TEXT;
-- 1 once the headers above were read. Messages stored before version 5
-- have 0: the next syncs fetch their headers again, in batches.
ALTER TABLE messages ADD COLUMN headers_v2 INTEGER NOT NULL DEFAULT 0;

-- The conversation of the message: the smallest row id among the
-- messages it is linked to (see `Store::insert_messages`). Copies of one
-- message (same Message-ID) are in the same conversation.
ALTER TABLE messages ADD COLUMN thread_id INTEGER;
UPDATE messages SET thread_id = COALESCE(
    (SELECT MIN(o.id) FROM messages o WHERE o.message_id = messages.message_id),
    id
);
CREATE INDEX messages_thread ON messages(thread_id);

-- Every Message-ID a message names: its own, In-Reply-To, References.
-- Two messages naming the same id are in the same conversation.
CREATE TABLE thread_links (
    message_row  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    ref          TEXT NOT NULL,
    PRIMARY KEY (message_row, ref)
);
CREATE INDEX thread_links_ref ON thread_links(ref);

INSERT INTO thread_links (message_row, ref)
SELECT id, message_id FROM messages WHERE message_id IS NOT NULL;
