-- Version 7: favourites, tabs of the inbox, what is known per sender.

-- Starred (`\Flagged` on the server), and the number of local star
-- changes not yet confirmed by the server (like `seen_pending`).
ALTER TABLE messages ADD COLUMN flagged INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN flag_pending INTEGER NOT NULL DEFAULT 0;
CREATE INDEX messages_flagged ON messages(flagged) WHERE flagged = 1;

-- The tab of the inbox: 'important', 'newsletter' or 'notification'
-- (see `category.rs`). Set when the message is stored; the messages
-- already there are sorted by the migration, in Rust.
ALTER TABLE messages ADD COLUMN category TEXT;

-- "Mails de X": every message from one sender.
CREATE INDEX messages_sender ON messages(lower(trim(from_addr)));

-- What the user decided about a sender (address trimmed, lower case):
-- the tab chosen by hand, and when unsubscribing was asked.
CREATE TABLE senders (
    addr             TEXT PRIMARY KEY,
    category         TEXT,
    unsubscribed_at  INTEGER
);
