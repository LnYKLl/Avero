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

-- "Bloquer" rules made before version 11 said "the sender contains the
-- address", which also caught `marjo@gmail.com` when `jo@gmail.com` was
-- blocked, and any sender whose name held it. They now compare the
-- address whole ('from_address'). They are told apart from the rules
-- written by hand (which keep "contains") by the shape `block_rule`
-- gives them: one sender condition holding an address, to the trash,
-- every account, not marked read, named "Bloquer ...".
UPDATE rule_conditions SET field = 'from_address'
WHERE field = 'from' AND instr(value, '@') > 0
  AND rule_id IN (
      SELECT r.id FROM rules r
      WHERE r.move_to = 'trash' AND r.account_id IS NULL AND r.mark_read = 0
        AND r.name LIKE 'Bloquer %'
        AND (SELECT COUNT(*) FROM rule_conditions c WHERE c.rule_id = r.id) = 1
  );

-- A message without Message-ID is known to "Plus tard" and to the spam
-- filter by its row ('row:' and its id). Row ids are given again once
-- the newest row is deleted, so a new message could be born snoozed or
-- "already learned". When such a row is deleted, its snooze goes; what
-- the spam filter learned from it stays counted (its words are still in
-- `spam_tokens`) under a key no row can have ('gone:' and random hex).
-- The keys earlier versions left behind are treated the same way now.
CREATE TRIGGER IF NOT EXISTS messages_forget_row_keys AFTER DELETE ON messages
WHEN old.message_id IS NULL
BEGIN
    DELETE FROM snoozes WHERE key = 'row:' || old.id;
    UPDATE spam_learned SET key = 'gone:' || hex(randomblob(8))
    WHERE key = 'row:' || old.id;
END;

DELETE FROM snoozes
WHERE key LIKE 'row:%'
  AND key NOT IN (SELECT 'row:' || id FROM messages WHERE message_id IS NULL);
UPDATE spam_learned SET key = 'gone:' || hex(randomblob(8))
WHERE key LIKE 'row:%'
  AND key NOT IN (SELECT 'row:' || id FROM messages WHERE message_id IS NULL);
