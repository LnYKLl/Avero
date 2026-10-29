-- Version 10: spaces, named groups of accounts ("Perso", "Pro"). The
-- list, the counters and the search can be limited to the accounts of
-- one space. An account can be in several spaces.

-- `icon`: the name of an icon the app knows; `color`: an index in the
-- accounts' palette; `position`: the order they are shown in.
CREATE TABLE spaces (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT NOT NULL,
    icon      TEXT NOT NULL,
    color     INTEGER NOT NULL,
    position  INTEGER NOT NULL
);

CREATE TABLE space_accounts (
    space_id    INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (space_id, account_id)
) WITHOUT ROWID;
CREATE INDEX space_accounts_account ON space_accounts(account_id);
