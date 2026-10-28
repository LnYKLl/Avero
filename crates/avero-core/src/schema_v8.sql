-- Version 8: rules, the local spam filter, message sizes.

-- Size of the whole message on the server (RFC822.SIZE), in bytes.
-- NULL until known: messages stored before version 8 get theirs at the
-- next syncs, in batches (see `Store::missing_sizes`).
ALTER TABLE messages ADD COLUMN size INTEGER;

-- 1 once the user said "Pas spam" for a sender: the spam filter never
-- moves their mail.
ALTER TABLE senders ADD COLUMN not_spam INTEGER NOT NULL DEFAULT 0;

-- 1 once the spam filter learned from what the account's spam and inbox
-- folders already held (done once, after the first sync).
ALTER TABLE accounts ADD COLUMN spam_bootstrapped INTEGER NOT NULL DEFAULT 0;

-- The user's rules (see `rules.rs`), applied in `position` order.
-- `move_to` is 'archive', 'trash', 'junk' or NULL (no move);
-- `account_id` NULL means every account.
CREATE TABLE rules (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    match_all   INTEGER NOT NULL,
    mark_read   INTEGER NOT NULL,
    move_to     TEXT,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
    enabled     INTEGER NOT NULL DEFAULT 1,
    position    INTEGER NOT NULL
);

-- The conditions of a rule: `field` is 'from', 'to' or 'subject', and
-- the rule checks that it contains `value` (case ignored).
CREATE TABLE rule_conditions (
    rule_id   INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    field     TEXT NOT NULL,
    value     TEXT NOT NULL,
    PRIMARY KEY (rule_id, position)
);

-- The spam filter's model (see `spam.rs`): for each token, how many
-- messages learned as spam and as wanted mail ("ham") held it.
CREATE TABLE spam_tokens (
    token  TEXT PRIMARY KEY,
    spam   INTEGER NOT NULL DEFAULT 0,
    ham    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- The messages learned (by Message-ID, else 'row:' and the row id), and
-- as what (1: spam): a message is never counted twice, and learning it
-- the other way first takes back what it taught.
CREATE TABLE spam_learned (
    key   TEXT PRIMARY KEY,
    spam  INTEGER NOT NULL
) WITHOUT ROWID;
