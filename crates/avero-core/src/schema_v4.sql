-- Version 4: one account per address (the duplicates were merged just
-- before this script runs). Case and surrounding spaces do not count.

CREATE UNIQUE INDEX accounts_email_unique ON accounts(lower(trim(email)));
