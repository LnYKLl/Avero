CREATE TABLE accounts (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    email       TEXT NOT NULL,
    color_r     INTEGER NOT NULL,
    color_g     INTEGER NOT NULL,
    color_b     INTEGER NOT NULL,
    imap_host   TEXT NOT NULL,
    imap_port   INTEGER NOT NULL,
    imap_user   TEXT NOT NULL
);

CREATE TABLE folders (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts(id),
    path           TEXT NOT NULL,
    role           TEXT NOT NULL,
    uid_validity   INTEGER,
    last_seen_uid  INTEGER,
    UNIQUE(account_id, path)
);

CREATE TABLE messages (
    id          INTEGER PRIMARY KEY,
    folder_id   INTEGER NOT NULL REFERENCES folders(id),
    uid         INTEGER NOT NULL,
    message_id  TEXT,
    subject     TEXT NOT NULL,
    from_name   TEXT,
    from_addr   TEXT NOT NULL,
    date        INTEGER NOT NULL,
    seen        INTEGER NOT NULL,
    UNIQUE(folder_id, uid)
);

CREATE INDEX messages_message_id ON messages(message_id);
