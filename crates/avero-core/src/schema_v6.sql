-- Version 6: a signature per account, added under new messages, replies
-- and forwards (empty: none).
ALTER TABLE accounts ADD COLUMN signature TEXT NOT NULL DEFAULT '';
