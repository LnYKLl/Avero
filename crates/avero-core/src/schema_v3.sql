-- Version 3: account ids are never reused, pending local changes.
-- Runs with foreign keys off (the accounts table is rebuilt).

-- AUTOINCREMENT: a deleted account's id (and its keyring entry name)
-- is never given to a new account.
CREATE TABLE accounts_v3 (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT NOT NULL,
    email          TEXT NOT NULL,
    color_r        INTEGER NOT NULL,
    color_g        INTEGER NOT NULL,
    color_b        INTEGER NOT NULL,
    imap_host      TEXT NOT NULL,
    imap_port      INTEGER NOT NULL,
    imap_user      TEXT NOT NULL,
    imap_security  TEXT NOT NULL DEFAULT 'tls',
    smtp_host      TEXT NOT NULL DEFAULT '',
    smtp_port      INTEGER NOT NULL DEFAULT 465,
    smtp_user      TEXT NOT NULL DEFAULT '',
    smtp_security  TEXT NOT NULL DEFAULT 'tls',
    auth           TEXT NOT NULL DEFAULT 'password'
);

INSERT INTO accounts_v3
    (id, name, email, color_r, color_g, color_b,
     imap_host, imap_port, imap_user, imap_security,
     smtp_host, smtp_port, smtp_user, smtp_security, auth)
SELECT id, name, email, color_r, color_g, color_b,
       imap_host, imap_port, imap_user, imap_security,
       smtp_host, smtp_port, smtp_user, smtp_security, auth
FROM accounts;

DROP TABLE accounts;
ALTER TABLE accounts_v3 RENAME TO accounts;

-- Number of local read/unread changes not yet confirmed by the server:
-- while it is above zero, the server's flag does not overwrite ours.
ALTER TABLE messages ADD COLUMN seen_pending INTEGER NOT NULL DEFAULT 0;
-- Hidden while an archive or delete is running on the server.
ALTER TABLE messages ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
