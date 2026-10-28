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
