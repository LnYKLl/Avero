-- Version 2: SMTP and authentication settings, cached bodies, local search.

ALTER TABLE accounts ADD COLUMN imap_security TEXT NOT NULL DEFAULT 'tls';
-- Old accounts have no SMTP server yet: the UI asks for it.
ALTER TABLE accounts ADD COLUMN smtp_host TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN smtp_port INTEGER NOT NULL DEFAULT 465;
ALTER TABLE accounts ADD COLUMN smtp_user TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN smtp_security TEXT NOT NULL DEFAULT 'tls';
ALTER TABLE accounts ADD COLUMN auth TEXT NOT NULL DEFAULT 'password';
UPDATE accounts SET smtp_user = imap_user;

ALTER TABLE messages ADD COLUMN has_attachments INTEGER NOT NULL DEFAULT 0;
CREATE INDEX messages_date ON messages(date);

-- Full raw message and its readable text, once the user opened it.
CREATE TABLE bodies (
    message_row  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    raw          BLOB NOT NULL,
    text         TEXT NOT NULL
);

-- Local search; rowid = messages.id. Accents are ignored ("reunion"
-- finds "Réunion").
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, from_text, body,
    tokenize = 'unicode61 remove_diacritics 2'
);

INSERT INTO messages_fts (rowid, subject, from_text, body)
SELECT id, subject, COALESCE(from_name, '') || ' ' || from_addr, '' FROM messages;

-- The triggers keep the index in step with every insert and delete,
-- whichever store method does it (reset_folder, delete_account, ...).
CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, subject, from_text, body)
    VALUES (new.id, new.subject, COALESCE(new.from_name, '') || ' ' || new.from_addr, '');
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TRIGGER bodies_fts_insert AFTER INSERT ON bodies BEGIN
    UPDATE messages_fts SET body = new.text WHERE rowid = new.message_row;
END;

CREATE TRIGGER bodies_fts_update AFTER UPDATE ON bodies BEGIN
    UPDATE messages_fts SET body = new.text WHERE rowid = new.message_row;
END;
