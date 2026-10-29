-- Version 9: "Plus tard" (snoozed conversations), drafts kept on this
-- computer and mail scheduled to be sent later.

-- A snoozed message, by its key (the Message-ID, else 'row:' and the row
-- id, like the list's grouping): snoozing a conversation adds every
-- message it holds. `woke_at` is NULL while it sleeps (the message is
-- left out of every list but "Plus tard"); once woken, the time it woke
-- up, which then sorts its conversation like a new message.
CREATE TABLE snoozes (
    key      TEXT PRIMARY KEY,
    until    INTEGER NOT NULL,
    woke_at  INTEGER
) WITHOUT ROWID;
CREATE INDEX snoozes_due ON snoozes(until) WHERE woke_at IS NULL;

-- A message being written, saved every few seconds (see `store/drafts.rs`).
-- `open`: a composer shows it (still open after a crash: it is offered
-- again at the next start). `server_dirty`: changed since its copy in the
-- server's drafts folder (`server_*`, NULL when it has none). `send_at`:
-- scheduled ("Programmés"); `attempted_at`, `sent_at` and `error` tell
-- how its sending went.
CREATE TABLE drafts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    to_addrs        TEXT NOT NULL,
    cc              TEXT NOT NULL,
    bcc             TEXT NOT NULL,
    subject         TEXT NOT NULL,
    body            TEXT NOT NULL,
    in_reply_to     TEXT,
    refs            TEXT,
    message_id      TEXT NOT NULL,
    open            INTEGER NOT NULL DEFAULT 1,
    server_dirty    INTEGER NOT NULL DEFAULT 1,
    server_account  INTEGER,
    server_folder   TEXT,
    server_uid      INTEGER,
    send_at         INTEGER,
    attempted_at    INTEGER,
    sent_at         INTEGER,
    error           TEXT,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX drafts_message_id ON drafts(message_id);

-- The attachments of a draft, kept here (not as paths: a forwarded
-- attachment has no file, and a file can move or change before the mail
-- leaves). At most 25 MB per draft (the composer's limit).
CREATE TABLE draft_files (
    draft_id  INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    name      TEXT NOT NULL,
    mime      TEXT NOT NULL,
    data      BLOB NOT NULL,
    PRIMARY KEY (draft_id, position)
);
