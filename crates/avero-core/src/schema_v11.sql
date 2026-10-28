-- Version 11: fixes after the review of V2.

-- Accounts whose secret (password or refresh token) must still be erased
-- from the operating system's keyring: those the version 4 migration
-- merged into another account. The store cannot reach the keyring, so
-- the app erases them, then deletes the row; a crash in between leaves
-- the row for the next start. (Created by the version 4 migration too,
-- hence IF NOT EXISTS.)
CREATE TABLE IF NOT EXISTS pending_secret_deletions (
    account_id  INTEGER PRIMARY KEY
);
