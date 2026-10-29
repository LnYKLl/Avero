//! SQLite storage: accounts, folders, messages, cached bodies, the unified
//! inbox query and local full-text search.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    Account, AccountColor, AccountFilter, AccountId, AuthKind, Folder, FolderId, FolderRole,
    MessageRef, NewAccount, NewMessage, Security, ServerConfig, UnifiedItem,
};

/// Error returned by every `Store` operation.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Length of `UnifiedItem::snippet`, in characters.
const SNIPPET_CHARS: usize = 140;

/// The Avero database (one SQLite connection).
pub struct Store {
    conn: Connection,
}

/// Columns read by `account_from_row`, in order.
const ACCOUNT_COLUMNS: &str = "id, name, email, color_r, color_g, color_b,
     imap_host, imap_port, imap_user, imap_security,
     smtp_host, smtp_port, smtp_user, smtp_security, auth";

/// Columns read by `folder_from_row`, in order.
const FOLDER_COLUMNS: &str = "id, account_id, path, role, uid_validity, last_seen_uid";

impl Store {
    /// Opens (or creates) the database file, in WAL mode.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let conn = Connection::open(path)?;
        // This pragma returns a row, so it is read with `query_row`.
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        Store::init(conn)
    }

    /// Opens a throw-away database in memory (used by tests).
    pub fn open_in_memory() -> Result<Store, StoreError> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store, StoreError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Applies the schema versions the database has not seen yet, each in
    /// its own transaction.
    fn migrate(&self) -> Result<(), StoreError> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let steps: [(i64, &str); 3] = [
            (1, include_str!("schema.sql")),
            (2, include_str!("schema_v2.sql")),
            (3, include_str!("schema_v3.sql")),
        ];
        for (target, sql) in steps {
            if version < target {
                // Rebuilding a table needs foreign keys off, which can only
                // be switched outside a transaction.
                self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
                let result = self.conn.execute_batch(&format!(
                    "BEGIN;\n{sql}\nPRAGMA user_version = {target};\nCOMMIT;"
                ));
                if result.is_err() {
                    let _ = self.conn.execute_batch("ROLLBACK;");
                }
                self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
                result?;
            }
        }
        Ok(())
    }

    /// Forgets local changes left pending by a previous run (its jobs are
    /// gone): hidden rows come back, server flags apply again.
    pub fn clear_pending(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET seen_pending = 0, hidden = 0
             WHERE seen_pending != 0 OR hidden != 0",
            [],
        )?;
        Ok(())
    }

    /// Saves a new account; its colour is the next one in the palette.
    pub fn add_account(&self, new: NewAccount) -> Result<Account, StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
        let color = AccountColor::palette(count as usize);
        self.conn.execute(
            "INSERT INTO accounts
                 (name, email, color_r, color_g, color_b,
                  imap_host, imap_port, imap_user, imap_security,
                  smtp_host, smtp_port, smtp_user, smtp_security, auth)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                new.name,
                new.email,
                color.r,
                color.g,
                color.b,
                new.imap.host,
                new.imap.port,
                new.imap.username,
                new.imap.security.as_str(),
                new.smtp.host,
                new.smtp.port,
                new.smtp.username,
                new.smtp.security.as_str(),
                new.auth.as_str(),
            ],
        )?;
        Ok(Account {
            id: AccountId(self.conn.last_insert_rowid()),
            name: new.name,
            email: new.email,
            color,
            imap: new.imap,
            smtp: new.smtp,
            auth: new.auth,
        })
    }

    /// All accounts, ordered by id.
    pub fn accounts(&self) -> Result<Vec<Account>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY id"
        ))?;
        let rows = stmt.query_map([], account_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Changes the name, servers and authentication of an account
    /// (its e-mail address and colour stay).
    pub fn update_account(
        &self,
        id: AccountId,
        name: &str,
        imap: &ServerConfig,
        smtp: &ServerConfig,
        auth: AuthKind,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE accounts SET name = ?1,
                 imap_host = ?2, imap_port = ?3, imap_user = ?4, imap_security = ?5,
                 smtp_host = ?6, smtp_port = ?7, smtp_user = ?8, smtp_security = ?9,
                 auth = ?10
             WHERE id = ?11",
            params![
                name,
                imap.host,
                imap.port,
                imap.username,
                imap.security.as_str(),
                smtp.host,
                smtp.port,
                smtp.username,
                smtp.security.as_str(),
                auth.as_str(),
                id.0,
            ],
        )?;
        Ok(())
    }

    /// Removes an account with its folders, messages, bodies and search
    /// entries. Its secret is not touched (that is the `SecretStore`'s job).
    pub fn delete_account(&self, id: AccountId) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        // Bodies go by ON DELETE CASCADE, search entries by trigger.
        tx.execute(
            "DELETE FROM messages
             WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            params![id.0],
        )?;
        tx.execute("DELETE FROM folders WHERE account_id = ?1", params![id.0])?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", params![id.0])?;
        tx.commit()?;
        Ok(())
    }

    /// Creates the folder, or updates its role if `(account, path)` exists.
    pub fn upsert_folder(
        &self,
        account: AccountId,
        path: &str,
        role: FolderRole,
    ) -> Result<Folder, StoreError> {
        self.conn.execute(
            "INSERT INTO folders (account_id, path, role) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id, path) DO UPDATE SET role = excluded.role",
            params![account.0, path, role.as_str()],
        )?;
        let folder = self.conn.query_row(
            &format!("SELECT {FOLDER_COLUMNS} FROM folders WHERE account_id = ?1 AND path = ?2"),
            params![account.0, path],
            folder_from_row,
        )?;
        Ok(folder)
    }

    /// All folders of an account, ordered by id.
    pub fn folders(&self, account: AccountId) -> Result<Vec<Folder>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FOLDER_COLUMNS} FROM folders WHERE account_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![account.0], folder_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The first folder of the account with this role, if any.
    pub fn folder_by_role(
        &self,
        account: AccountId,
        role: FolderRole,
    ) -> Result<Option<Folder>, StoreError> {
        let folder = self
            .conn
            .query_row(
                &format!(
                    "SELECT {FOLDER_COLUMNS} FROM folders
                     WHERE account_id = ?1 AND role = ?2 ORDER BY id LIMIT 1"
                ),
                params![account.0, role.as_str()],
                folder_from_row,
            )
            .optional()?;
        Ok(folder)
    }

    /// Forgets the folder's messages and records its new UID validity.
    pub fn reset_folder(&self, folder: FolderId, uid_validity: u32) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE folder_id = ?1",
            params![folder.0],
        )?;
        tx.execute(
            "UPDATE folders SET uid_validity = ?1, last_seen_uid = NULL WHERE id = ?2",
            params![uid_validity, folder.0],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stores messages in one transaction; an existing `(folder, uid)` is ignored.
    /// Each new message is also added to the search index (by trigger).
    pub fn insert_messages(&self, folder: FolderId, msgs: &[NewMessage]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO messages
                     (folder_id, uid, message_id, subject, from_name, from_addr, date, seen,
                      has_attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for m in msgs {
                stmt.execute(params![
                    folder.0,
                    m.uid,
                    m.message_id,
                    m.subject,
                    m.from_name,
                    m.from_addr,
                    m.date,
                    m.seen,
                    m.has_attachments,
                ])?;
            }
        }
        if let Some(max_uid) = msgs.iter().map(|m| m.uid).max() {
            tx.execute(
                "UPDATE folders SET last_seen_uid = MAX(COALESCE(last_seen_uid, 0), ?1)
                 WHERE id = ?2",
                params![max_uid, folder.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Moves the folder's last seen UID up to `uid` (it never goes down).
    /// Used when the server's highest UIDs were deleted: we know there is
    /// nothing below `uid` left to fetch.
    pub fn set_last_seen_uid(&self, folder: FolderId, uid: u32) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET last_seen_uid = MAX(COALESCE(last_seen_uid, 0), ?1)
             WHERE id = ?2",
            params![uid, folder.0],
        )?;
        Ok(())
    }

    /// The unified list for one folder role: copies of the same message
    /// (same Message-ID) are merged into one item, newest first. Grouping
    /// and paging happen in SQL.
    pub fn unified(
        &self,
        role: FolderRole,
        filter: AccountFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        self.grouped(Some(role), filter, None, limit, offset)
    }

    /// Local full-text search over subject, sender and cached bodies.
    ///
    /// Every word of `query` must match the start of a word (`fact` finds
    /// "factures"); accents and case are ignored. Any text is accepted:
    /// FTS syntax characters are neutralised. `role = None` searches every
    /// folder. Results are merged like `unified` and sorted newest first.
    pub fn search(
        &self,
        query: &str,
        role: Option<FolderRole>,
        filter: AccountFilter,
        limit: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        match fts_query(query) {
            Some(fts) => self.grouped(role, filter, Some(&fts), limit, 0),
            None => Ok(Vec::new()),
        }
    }

    /// Shared by `unified` and `search`: one item per Message-ID (or per
    /// row when it has none), showing the newest copy.
    fn grouped(
        &self,
        role: Option<FolderRole>,
        filter: AccountFilter,
        fts: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        // `None` means "no condition" (`?n IS NULL` is true).
        let role = role.map(|r| r.as_str());
        let account_id = filter_account(filter);
        let fts_condition = match fts {
            Some(_) => "AND m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?3)",
            None => "AND ?3 IS NULL",
        };
        let sql = format!(
            "WITH sel AS (
                 SELECT m.id, f.account_id, m.date, m.seen, m.has_attachments,
                        COALESCE(m.message_id, 'row:' || m.id) AS k
                 FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE (?1 IS NULL OR f.role = ?1)
                   AND (?2 IS NULL OR f.account_id = ?2)
                   AND m.hidden = 0
                   {fts_condition}
             ),
             ranked AS (
                 SELECT id,
                        ROW_NUMBER() OVER newest AS rn,
                        group_concat(id) OVER whole AS ids,
                        group_concat(account_id) OVER whole AS accounts,
                        MIN(seen) OVER whole AS all_seen,
                        MAX(has_attachments) OVER whole AS any_attachment
                 FROM sel
                 WINDOW newest AS (PARTITION BY k ORDER BY date DESC, id DESC),
                        whole AS (PARTITION BY k ORDER BY date DESC, id DESC
                                  ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
             )
             SELECT r.ids, r.accounts, m.subject, m.from_name, m.from_addr, m.date,
                    r.all_seen, r.any_attachment, substr(b.text, 1, 1000)
             FROM ranked r
             JOIN messages m ON m.id = r.id
             LEFT JOIN bodies b ON b.message_row = m.id
             WHERE r.rn = 1
             ORDER BY m.date DESC, m.id DESC
             LIMIT ?4 OFFSET ?5"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![role, account_id, fts, limit as i64, offset as i64],
            |row| {
                let ids: String = row.get(0)?;
                let accounts: String = row.get(1)?;
                let from_name: Option<String> = row.get(3)?;
                let from_addr: String = row.get(4)?;
                let body: Option<String> = row.get(8)?;
                let mut accounts: Vec<AccountId> = parse_id_list(&accounts)
                    .into_iter()
                    .map(AccountId)
                    .collect();
                accounts.sort();
                accounts.dedup();
                Ok(UnifiedItem {
                    row_ids: parse_id_list(&ids),
                    accounts,
                    subject: row.get(2)?,
                    from_display: from_name.unwrap_or_else(|| from_addr.clone()),
                    from_addr,
                    date: row.get(5)?,
                    seen: row.get(6)?,
                    has_attachments: row.get(7)?,
                    snippet: body.as_deref().map(snippet).unwrap_or_default(),
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Number of unified items (merged like `unified`) not seen yet.
    pub fn unread_count(
        &self,
        role: FolderRole,
        filter: AccountFilter,
    ) -> Result<usize, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM (
                 SELECT 1
                 FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE f.role = ?1 AND (?2 IS NULL OR f.account_id = ?2) AND m.hidden = 0
                 GROUP BY COALESCE(m.message_id, 'row:' || m.id)
                 HAVING MIN(m.seen) = 0
             )",
            params![role.as_str(), filter_account(filter)],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Where the given rows live on the server, in the order asked.
    /// Unknown rows are left out.
    pub fn message_refs(&self, row_ids: &[i64]) -> Result<Vec<MessageRef>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, f.account_id, f.id, f.path, m.uid
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.id = ?1",
        )?;
        let mut refs = Vec::new();
        for &id in row_ids {
            let found = stmt
                .query_row(params![id], |row| {
                    Ok(MessageRef {
                        row_id: row.get(0)?,
                        account_id: AccountId(row.get(1)?),
                        folder_id: FolderId(row.get(2)?),
                        folder_path: row.get(3)?,
                        uid: row.get(4)?,
                    })
                })
                .optional()?;
            refs.extend(found);
        }
        Ok(refs)
    }

    /// The cached raw message and its readable text, if downloaded.
    pub fn body(&self, row_id: i64) -> Result<Option<(Vec<u8>, String)>, StoreError> {
        let body = self
            .conn
            .query_row(
                "SELECT raw, text FROM bodies WHERE message_row = ?1",
                params![row_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(body)
    }

    /// Caches a downloaded body (replacing any previous one), makes its
    /// text searchable and records whether it has attachments.
    pub fn put_body(
        &self,
        row_id: i64,
        raw: &[u8],
        text: &str,
        has_attachments: bool,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO bodies (message_row, raw, text) VALUES (?1, ?2, ?3)
             ON CONFLICT(message_row) DO UPDATE SET raw = excluded.raw, text = excluded.text",
            params![row_id, raw, text],
        )?;
        tx.execute(
            "UPDATE messages SET has_attachments = ?1 WHERE id = ?2",
            params![has_attachments, row_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks rows as read or unread locally. The change stays pending
    /// (the server's flag does not overwrite it) until `confirm_seen`.
    pub fn set_seen(&self, row_ids: &[i64], seen: bool) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE messages SET seen = ?1, seen_pending = seen_pending + 1 WHERE id = ?2",
            )?;
            for &id in row_ids {
                stmt.execute(params![seen, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// One pending read/unread change of these rows reached the server
    /// (or failed): the server's flags apply again once none is left.
    pub fn confirm_seen(&self, row_ids: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE messages SET seen_pending = MAX(seen_pending - 1, 0) WHERE id = ?1",
            )?;
            for &id in row_ids {
                stmt.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Hides rows from every list (while they are being moved on the
    /// server) or shows them again (the move failed).
    pub fn set_hidden(&self, row_ids: &[i64], hidden: bool) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE messages SET hidden = ?1 WHERE id = ?2")?;
            for &id in row_ids {
                stmt.execute(params![hidden, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes rows locally (after a move or delete on the server), with
    /// their bodies and search entries.
    pub fn delete_messages(&self, row_ids: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM messages WHERE id = ?1")?;
            for &id in row_ids {
                stmt.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// UID and seen flag of every stored message of a folder, by UID.
    pub fn inbox_uids(&self, folder: FolderId) -> Result<Vec<(u32, bool)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT uid, seen FROM messages WHERE folder_id = ?1 ORDER BY uid")?;
        let rows = stmt.query_map(params![folder.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Applies the flags the server reported for the UIDs `from..=to` of a
    /// folder: `present` holds `(uid, seen)` for every message the server
    /// has in that range.
    ///
    /// Stored UIDs listed in `present` get its seen flag, unless a local
    /// read/unread change is still on its way to the server. Stored UIDs in
    /// the range but missing from `present` were expunged on the server and
    /// are removed. UIDs outside the range are not touched.
    pub fn apply_flags(
        &self,
        folder: FolderId,
        from: u32,
        to: u32,
        present: &[(u32, bool)],
    ) -> Result<(), StoreError> {
        let listed: std::collections::HashSet<u32> = present.iter().map(|&(uid, _)| uid).collect();
        let stored = self.inbox_uids(folder)?;

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut update = tx.prepare(
                "UPDATE messages SET seen = ?1
                 WHERE folder_id = ?2 AND uid = ?3 AND seen != ?1 AND seen_pending = 0",
            )?;
            for &(uid, seen) in present {
                update.execute(params![seen, folder.0, uid])?;
            }
            let mut delete =
                tx.prepare("DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
            for (uid, _) in stored {
                if (from..=to).contains(&uid) && !listed.contains(&uid) {
                    delete.execute(params![folder.0, uid])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// `None` for all accounts, else the account id (for `?n IS NULL OR ...`).
fn filter_account(filter: AccountFilter) -> Option<i64> {
    match filter {
        AccountFilter::All => None,
        AccountFilter::Only(id) => Some(id.0),
    }
}

/// Parses a `group_concat` of integers ("3,1,7").
fn parse_id_list(text: &str) -> Vec<i64> {
    text.split(',').filter_map(|s| s.parse().ok()).collect()
}

/// First characters of a body text, on one line.
fn snippet(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(SNIPPET_CHARS).collect()
}

/// Turns what the user typed into a safe FTS5 query: each word becomes a
/// quoted prefix term (`"fact"*`), all words required. Words without any
/// letter or digit are dropped. `None` when nothing is left to search.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Builds an `Account` from a row of `ACCOUNT_COLUMNS`.
fn account_from_row(row: &rusqlite::Row) -> rusqlite::Result<Account> {
    let imap_security: String = row.get(9)?;
    let smtp_security: String = row.get(13)?;
    let auth: String = row.get(14)?;
    Ok(Account {
        id: AccountId(row.get(0)?),
        name: row.get(1)?,
        email: row.get(2)?,
        color: AccountColor {
            r: row.get(3)?,
            g: row.get(4)?,
            b: row.get(5)?,
        },
        imap: ServerConfig {
            host: row.get(6)?,
            port: row.get(7)?,
            username: row.get(8)?,
            security: Security::parse(&imap_security),
        },
        smtp: ServerConfig {
            host: row.get(10)?,
            port: row.get(11)?,
            username: row.get(12)?,
            security: Security::parse(&smtp_security),
        },
        auth: AuthKind::parse(&auth),
    })
}

/// Builds a `Folder` from a row of `FOLDER_COLUMNS`.
fn folder_from_row(row: &rusqlite::Row) -> rusqlite::Result<Folder> {
    let role: String = row.get(3)?;
    Ok(Folder {
        id: FolderId(row.get(0)?),
        account_id: AccountId(row.get(1)?),
        path: row.get(2)?,
        role: FolderRole::parse(&role),
        uid_validity: row.get(4)?,
        last_seen_uid: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn server(host: &str, port: u16, user: &str, security: Security) -> ServerConfig {
        ServerConfig {
            host: host.to_string(),
            port,
            username: user.to_string(),
            security,
        }
    }

    fn new_account(email: &str) -> NewAccount {
        NewAccount {
            name: email.to_string(),
            email: email.to_string(),
            imap: server("imap.example.com", 993, email, Security::Tls),
            smtp: server("smtp.example.com", 587, email, Security::StartTls),
            auth: AuthKind::Password,
        }
    }

    fn acct(store: &Store, email: &str) -> AccountId {
        store.add_account(new_account(email)).unwrap().id
    }

    fn inbox(store: &Store, account: AccountId) -> Folder {
        store
            .upsert_folder(account, "INBOX", FolderRole::Inbox)
            .unwrap()
    }

    fn all_inbox(store: &Store) -> Vec<UnifiedItem> {
        store
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap()
    }

    /// Row ids of the inbox, newest first.
    fn inbox_rows(store: &Store) -> Vec<i64> {
        all_inbox(store)
            .into_iter()
            .flat_map(|item| item.row_ids)
            .collect()
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn msg(uid: u32, message_id: Option<&str>, date: i64) -> NewMessage {
        NewMessage {
            uid,
            message_id: message_id.map(|s| s.to_string()),
            subject: format!("subject {uid}"),
            from_name: None,
            from_addr: "x@y.z".to_string(),
            date,
            seen: false,
            has_attachments: false,
        }
    }

    #[test]
    fn add_account_assigns_palette_colors() {
        let s = Store::open_in_memory().unwrap();
        acct(&s, "a@x");
        acct(&s, "b@x");
        let accounts = s.accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].color, AccountColor::palette(0));
        assert_eq!(accounts[1].color, AccountColor::palette(1));
    }

    #[test]
    fn same_message_id_in_two_inboxes_is_merged() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        let fb = s.upsert_folder(b, "INBOX", FolderRole::Inbox).unwrap();
        s.insert_messages(fa.id, &[msg(1, Some("<m1>"), 100)])
            .unwrap();
        s.insert_messages(fb.id, &[msg(7, Some("<m1>"), 100)])
            .unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].accounts, vec![a, b]);
        assert_eq!(items[0].row_ids.len(), 2);
    }

    #[test]
    fn same_message_id_in_sent_and_inbox_is_not_merged() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let sent = s.upsert_folder(a, "Sent", FolderRole::Sent).unwrap();
        let inbox = s.upsert_folder(b, "INBOX", FolderRole::Inbox).unwrap();
        s.insert_messages(sent.id, &[msg(1, Some("<m1>"), 100)])
            .unwrap();
        s.insert_messages(inbox.id, &[msg(1, Some("<m1>"), 100)])
            .unwrap();
        let all = AccountFilter::All;
        assert_eq!(s.unified(FolderRole::Inbox, all, 200, 0).unwrap().len(), 1);
        assert_eq!(s.unified(FolderRole::Sent, all, 200, 0).unwrap().len(), 1);
    }

    #[test]
    fn missing_message_id_is_never_merged() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        s.insert_messages(f.id, &[msg(1, None, 100), msg(2, None, 100)])
            .unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn filter_only_returns_one_account() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        let fb = s.upsert_folder(b, "INBOX", FolderRole::Inbox).unwrap();
        s.insert_messages(fa.id, &[msg(1, Some("<a>"), 100)])
            .unwrap();
        s.insert_messages(fb.id, &[msg(1, Some("<b>"), 200)])
            .unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::Only(a), 200, 0)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].accounts, vec![a]);
    }

    #[test]
    fn sorted_newest_first_and_paginated() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        let msgs: Vec<NewMessage> = (1..=5u32)
            .map(|i| msg(i, Some(&format!("<{i}>")), i as i64 * 10))
            .collect();
        s.insert_messages(f.id, &msgs).unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 2, 2)
            .unwrap();
        let dates: Vec<i64> = items.iter().map(|i| i.date).collect();
        assert_eq!(dates, vec![30, 20]);
    }

    #[test]
    fn insert_is_idempotent_and_tracks_last_uid() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        let msgs = [msg(1, None, 1), msg(2, None, 2), msg(3, None, 3)];
        s.insert_messages(f.id, &msgs).unwrap();
        s.insert_messages(f.id, &msgs).unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert_eq!(items.len(), 3);
        let folder = &s.folders(a).unwrap()[0];
        assert_eq!(folder.last_seen_uid, Some(3));
    }

    #[test]
    fn set_last_seen_uid_keeps_the_highest() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        s.set_last_seen_uid(f.id, 10).unwrap();
        assert_eq!(s.folders(a).unwrap()[0].last_seen_uid, Some(10));
        s.set_last_seen_uid(f.id, 4).unwrap();
        assert_eq!(s.folders(a).unwrap()[0].last_seen_uid, Some(10));
        s.set_last_seen_uid(f.id, 25).unwrap();
        assert_eq!(s.folders(a).unwrap()[0].last_seen_uid, Some(25));
    }

    #[test]
    fn reset_folder_clears_messages() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        s.insert_messages(f.id, &[msg(1, None, 1)]).unwrap();
        s.reset_folder(f.id, 42).unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert!(items.is_empty());
        let folder = &s.folders(a).unwrap()[0];
        assert_eq!(folder.uid_validity, Some(42));
        assert_eq!(folder.last_seen_uid, None);
    }

    #[test]
    fn merged_seen_only_if_all_copies_seen() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = s.upsert_folder(a, "INBOX", FolderRole::Inbox).unwrap();
        let fb = s.upsert_folder(b, "INBOX", FolderRole::Inbox).unwrap();
        let mut read = msg(1, Some("<m1>"), 100);
        read.seen = true;
        s.insert_messages(fa.id, &[read]).unwrap();
        s.insert_messages(fb.id, &[msg(1, Some("<m1>"), 100)])
            .unwrap();
        let items = s
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(!items[0].seen);
    }

    #[test]
    fn accounts_roundtrip_smtp_and_auth() {
        let s = Store::open_in_memory().unwrap();
        let added = s.add_account(new_account("a@x")).unwrap();
        let read = &s.accounts().unwrap()[0];
        assert_eq!(read, &added);
        assert_eq!(read.smtp.security, Security::StartTls);
        assert_eq!(read.auth, AuthKind::Password);
    }

    #[test]
    fn update_account_changes_settings() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let imap = server("imap.new.com", 994, "u", Security::Tls);
        let smtp = server("smtp.new.com", 465, "u", Security::Tls);
        s.update_account(a, "Perso", &imap, &smtp, AuthKind::OAuthGoogle)
            .unwrap();
        let read = &s.accounts().unwrap()[0];
        assert_eq!(read.name, "Perso");
        assert_eq!(read.email, "a@x");
        assert_eq!(read.imap, imap);
        assert_eq!(read.smtp, smtp);
        assert_eq!(read.auth, AuthKind::OAuthGoogle);
    }

    #[test]
    fn delete_account_removes_everything_it_owns() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = inbox(&s, a);
        let fb = inbox(&s, b);
        s.insert_messages(fa.id, &[msg(1, Some("<a>"), 1)]).unwrap();
        s.insert_messages(fb.id, &[msg(1, Some("<b>"), 2)]).unwrap();
        let row = inbox_rows(&s)[1];
        s.put_body(row, b"raw", "texte", false).unwrap();

        s.delete_account(a).unwrap();

        assert_eq!(s.accounts().unwrap().len(), 1);
        assert!(s.folders(a).unwrap().is_empty());
        assert_eq!(count(&s, "messages"), 1);
        assert_eq!(count(&s, "bodies"), 0);
        assert_eq!(count(&s, "messages_fts"), 1);
        assert_eq!(all_inbox(&s)[0].accounts, vec![b]);
    }

    #[test]
    fn folder_by_role_finds_first_match() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        assert_eq!(s.folder_by_role(a, FolderRole::Sent).unwrap(), None);
        let sent = s.upsert_folder(a, "Sent", FolderRole::Sent).unwrap();
        s.upsert_folder(a, "Sent2", FolderRole::Sent).unwrap();
        assert_eq!(s.folder_by_role(a, FolderRole::Sent).unwrap(), Some(sent));
        let b = acct(&s, "b@x");
        assert_eq!(s.folder_by_role(b, FolderRole::Sent).unwrap(), None);
    }

    #[test]
    fn unified_items_carry_address_attachments_and_snippet() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        let mut m = msg(1, Some("<m>"), 1);
        m.from_name = Some("Marie".into());
        m.has_attachments = true;
        s.insert_messages(f.id, &[m]).unwrap();
        let item = &all_inbox(&s)[0];
        assert_eq!(item.from_display, "Marie");
        assert_eq!(item.from_addr, "x@y.z");
        assert!(item.has_attachments);
        assert_eq!(item.snippet, "");

        let long = format!("Bonjour,\r\n\r\n  à   tous\n{}", "x".repeat(300));
        s.put_body(item.row_ids[0], b"raw", &long, false).unwrap();
        let item = &all_inbox(&s)[0];
        assert!(
            item.snippet.starts_with("Bonjour, à tous x"),
            "{}",
            item.snippet
        );
        assert_eq!(item.snippet.chars().count(), SNIPPET_CHARS);
        assert!(!item.has_attachments);
    }

    #[test]
    fn merged_item_shows_newest_copy_and_paginates_groups() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = inbox(&s, a);
        let fb = inbox(&s, b);
        // <m1> is in both inboxes; the copy in b is newer.
        s.insert_messages(
            fa.id,
            &[msg(1, Some("<m1>"), 100), msg(2, Some("<m2>"), 50)],
        )
        .unwrap();
        s.insert_messages(
            fb.id,
            &[msg(9, Some("<m1>"), 110), msg(3, Some("<m3>"), 10)],
        )
        .unwrap();
        let first = s
            .unified(FolderRole::Inbox, AccountFilter::All, 1, 0)
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].date, 110);
        assert_eq!(first[0].subject, "subject 9");
        assert_eq!(first[0].row_ids.len(), 2);
        let rest = s
            .unified(FolderRole::Inbox, AccountFilter::All, 10, 1)
            .unwrap();
        let dates: Vec<i64> = rest.iter().map(|i| i.date).collect();
        assert_eq!(dates, vec![50, 10]);
    }

    #[test]
    fn unread_count_merges_duplicates() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let fa = inbox(&s, a);
        let fb = inbox(&s, b);
        let mut read = msg(2, Some("<r>"), 1);
        read.seen = true;
        s.insert_messages(fa.id, &[msg(1, Some("<m1>"), 1), read])
            .unwrap();
        s.insert_messages(fb.id, &[msg(1, Some("<m1>"), 1), msg(2, None, 1)])
            .unwrap();
        let all = AccountFilter::All;
        assert_eq!(s.unread_count(FolderRole::Inbox, all).unwrap(), 2);
        let only_a = AccountFilter::Only(a);
        assert_eq!(s.unread_count(FolderRole::Inbox, only_a).unwrap(), 1);
        assert_eq!(s.unread_count(FolderRole::Sent, all).unwrap(), 0);
    }

    #[test]
    fn search_finds_subject_sender_and_body_prefixes() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        let mut invoice = msg(1, Some("<1>"), 10);
        invoice.subject = "Vos factures d'octobre".into();
        let mut meeting = msg(2, Some("<2>"), 20);
        meeting.subject = "Réunion jeudi".into();
        meeting.from_name = Some("Marie Durand".into());
        s.insert_messages(f.id, &[invoice, meeting]).unwrap();
        let all = AccountFilter::All;

        let found = s.search("FACT", None, all, 50).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subject, "Vos factures d'octobre");
        // Accents are ignored, and the sender is searched too.
        assert_eq!(s.search("reunion", None, all, 50).unwrap().len(), 1);
        assert_eq!(s.search("durand", None, all, 50).unwrap().len(), 1);
        // Every word must match.
        assert!(
            s.search("réunion facture", None, all, 50)
                .unwrap()
                .is_empty()
        );

        let row = found[0].row_ids[0];
        s.put_body(row, b"raw", "Le montant est de 42 euros", false)
            .unwrap();
        assert_eq!(s.search("montant euro", None, all, 50).unwrap().len(), 1);
        let sent = Some(FolderRole::Sent);
        assert!(s.search("montant", sent, all, 50).unwrap().is_empty());
        // Replacing the body replaces what is searchable.
        s.put_body(row, b"raw", "autre chose", false).unwrap();
        assert!(s.search("montant", None, all, 50).unwrap().is_empty());
    }

    #[test]
    fn search_never_fails_on_odd_input() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        let mut m = msg(1, None, 1);
        m.subject = "C++ \"quoted\" AND stuff".into();
        s.insert_messages(f.id, &[m]).unwrap();
        let all = AccountFilter::All;
        for query in [
            "", "   ", "\"", "*", "AND", "OR NOT", "a\"b", "(x", "NEAR(", "-", "c++", "^x", ":",
            "col:val", "\"quoted",
        ] {
            let result = s.search(query, None, all, 10);
            assert!(result.is_ok(), "{query:?}: {result:?}");
        }
        assert_eq!(s.search("\"quoted", None, all, 10).unwrap().len(), 1);
        assert_eq!(s.search("and", None, all, 10).unwrap().len(), 1);
    }

    #[test]
    fn message_refs_in_requested_order() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(5, None, 1), msg(8, None, 2)])
            .unwrap();
        let rows = inbox_rows(&s); // newest first: uid 8, then 5
        let refs = s.message_refs(&[rows[1], 9999, rows[0]]).unwrap();
        let expected = |row_id, uid| MessageRef {
            row_id,
            account_id: a,
            folder_id: f.id,
            folder_path: "INBOX".into(),
            uid,
        };
        assert_eq!(refs, vec![expected(rows[1], 5), expected(rows[0], 8)]);
    }

    #[test]
    fn body_put_get_and_attachment_flag() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(1, None, 1)]).unwrap();
        let row = inbox_rows(&s)[0];
        assert_eq!(s.body(row).unwrap(), None);
        s.put_body(row, b"\x00raw\xff", "texte", true).unwrap();
        assert_eq!(
            s.body(row).unwrap(),
            Some((b"\x00raw\xff".to_vec(), "texte".to_string()))
        );
        assert!(all_inbox(&s)[0].has_attachments);
        s.put_body(row, b"new", "nouveau", false).unwrap();
        assert_eq!(s.body(row).unwrap().unwrap().1, "nouveau");
    }

    #[test]
    fn set_seen_and_delete_messages() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(1, None, 1), msg(2, None, 2)])
            .unwrap();
        let rows = inbox_rows(&s); // uid 2, then uid 1
        s.set_seen(&rows, true).unwrap();
        assert!(all_inbox(&s).iter().all(|i| i.seen));
        s.set_seen(&rows[..1], false).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap(), vec![(1, true), (2, false)]);

        s.put_body(rows[0], b"raw", "texte", false).unwrap();
        s.delete_messages(&rows[..1]).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap(), vec![(1, true)]);
        assert_eq!(count(&s, "bodies"), 0);
        assert_eq!(count(&s, "messages_fts"), 1);
    }

    #[test]
    fn apply_flags_updates_seen_and_removes_expunged() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        let msgs: Vec<NewMessage> = [1, 2, 3, 4, 7].iter().map(|&u| msg(u, None, 1)).collect();
        s.insert_messages(f.id, &msgs).unwrap();

        // 2 was expunged, 1 and 4 were read elsewhere; 7 is outside the
        // range asked, so it stays.
        s.apply_flags(f.id, 1, 4, &[(1, true), (3, false), (4, true)])
            .unwrap();

        assert_eq!(
            s.inbox_uids(f.id).unwrap(),
            vec![(1, true), (3, false), (4, true), (7, false)]
        );
        // An empty range on the server: its stored messages are gone.
        s.apply_flags(f.id, 5, 9, &[]).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap().len(), 3);
    }

    #[test]
    fn pending_local_seen_change_survives_server_flags() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(1, None, 1), msg(2, None, 2)])
            .unwrap();
        let row1 = inbox_rows(&s)[1];

        // Read locally; a sync that started before the SetSeen job still
        // sees the old flag on the server.
        s.set_seen(&[row1], true).unwrap();
        s.apply_flags(f.id, 1, 2, &[(1, false), (2, true)]).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap(), vec![(1, true), (2, true)]);

        // Read, then unread at once: two changes pending.
        s.set_seen(&[row1], false).unwrap();
        s.confirm_seen(&[row1]).unwrap();
        s.apply_flags(f.id, 1, 2, &[(1, true), (2, true)]).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap()[0], (1, false));

        // Both confirmed: the server's flag applies again.
        s.confirm_seen(&[row1]).unwrap();
        s.confirm_seen(&[row1]).unwrap();
        s.apply_flags(f.id, 1, 2, &[(1, true), (2, true)]).unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap()[0], (1, true));
    }

    #[test]
    fn hidden_rows_leave_lists_and_counts_until_shown() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(1, None, 1), msg(2, None, 2)])
            .unwrap();
        let rows = inbox_rows(&s);
        s.set_hidden(&rows[..1], true).unwrap();
        assert_eq!(inbox_rows(&s), rows[1..].to_vec());
        assert_eq!(
            s.unread_count(FolderRole::Inbox, AccountFilter::All)
                .unwrap(),
            1
        );
        assert!(
            s.search("subject", None, AccountFilter::All, 10)
                .unwrap()
                .iter()
                .all(|i| i.row_ids != rows[..1])
        );
        s.set_hidden(&rows[..1], false).unwrap();
        assert_eq!(inbox_rows(&s), rows);

        s.set_hidden(&rows, true).unwrap();
        s.set_seen(&rows, true).unwrap();
        s.clear_pending().unwrap();
        assert_eq!(inbox_rows(&s), rows);
        s.apply_flags(f.id, 1, 2, &[(1, false), (2, false)])
            .unwrap();
        assert!(all_inbox(&s).iter().all(|i| !i.seen));
    }

    #[test]
    fn deleted_account_id_is_never_reused() {
        let s = Store::open_in_memory().unwrap();
        let _a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        s.delete_account(b).unwrap();
        let c = acct(&s, "c@x");
        assert!(c.0 > b.0, "{c:?} reuses {b:?}");
    }

    #[test]
    fn reset_folder_also_clears_bodies_and_search() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(1, None, 1)]).unwrap();
        s.put_body(inbox_rows(&s)[0], b"raw", "texte", false)
            .unwrap();
        s.reset_folder(f.id, 2).unwrap();
        assert_eq!(count(&s, "bodies"), 0);
        assert_eq!(count(&s, "messages_fts"), 0);
    }

    #[test]
    fn fts_query_quotes_every_word() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("- * ("), None);
        assert_eq!(
            fts_query("fact a\"b"),
            Some("\"fact\"* \"a\"\"b\"*".to_string())
        );
    }

    /// The schema of version 1, as shipped with plan 1.
    const SCHEMA_V1: &str = "
        CREATE TABLE accounts (
            id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL,
            color_r INTEGER NOT NULL, color_g INTEGER NOT NULL, color_b INTEGER NOT NULL,
            imap_host TEXT NOT NULL, imap_port INTEGER NOT NULL, imap_user TEXT NOT NULL
        );
        CREATE TABLE folders (
            id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL REFERENCES accounts(id),
            path TEXT NOT NULL, role TEXT NOT NULL, uid_validity INTEGER,
            last_seen_uid INTEGER, UNIQUE(account_id, path)
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL REFERENCES folders(id),
            uid INTEGER NOT NULL, message_id TEXT, subject TEXT NOT NULL, from_name TEXT,
            from_addr TEXT NOT NULL, date INTEGER NOT NULL, seen INTEGER NOT NULL,
            UNIQUE(folder_id, uid)
        );
        CREATE INDEX messages_message_id ON messages(message_id);
        PRAGMA user_version = 1;
    ";

    #[test]
    fn version_1_database_is_upgraded_in_place() {
        let path = std::env::temp_dir().join(format!("avero-migration-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(SCHEMA_V1).unwrap();
            old.execute_batch(
                "INSERT INTO accounts VALUES (1, 'Perso', 'a@x', 1, 2, 3, 'imap.x.com', 993, 'a@x');
                 INSERT INTO folders VALUES (1, 1, 'INBOX', 'inbox', 7, 1);
                 INSERT INTO messages VALUES (1, 1, 1, '<m>', 'Facture', 'Marie', 'm@x', 5, 0);",
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();

        let version: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let account = &s.accounts().unwrap()[0];
        assert_eq!(account.imap.host, "imap.x.com");
        assert_eq!(account.imap.security, Security::Tls);
        assert_eq!(account.smtp.host, "");
        assert_eq!(account.smtp.username, "a@x");
        assert_eq!(account.auth, AuthKind::Password);
        let item = &all_inbox(&s)[0];
        assert_eq!(item.subject, "Facture");
        assert!(!item.has_attachments);
        // Old messages are searchable, and bodies can be stored.
        let found = s.search("marie", None, AccountFilter::All, 10).unwrap();
        assert_eq!(found.len(), 1);
        s.put_body(item.row_ids[0], b"raw", "texte", false).unwrap();

        // The accounts table was rebuilt: folders still point to it and
        // ids are not reused any more.
        assert_eq!(s.folders(AccountId(1)).unwrap().len(), 1);
        s.delete_account(AccountId(1)).unwrap();
        let again = s.add_account(new_account("b@x")).unwrap();
        assert_eq!(again.id, AccountId(2));
        drop(s);

        // Opening again does not run the migration twice.
        let s = Store::open(&path).unwrap();
        assert_eq!(s.accounts().unwrap().len(), 1);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }
}
