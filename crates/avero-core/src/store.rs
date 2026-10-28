//! SQLite storage: accounts, folders, messages and the unified inbox query.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::{
    Account, AccountColor, AccountFilter, AccountId, Folder, FolderId, FolderRole, NewAccount,
    NewMessage, ServerConfig, UnifiedItem,
};

/// Error returned by every `Store` operation.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// The Avero database (one SQLite connection).
pub struct Store {
    conn: Connection,
}

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

    /// Applies the schema versions the database has not seen yet.
    fn migrate(&self) -> Result<(), StoreError> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            self.conn.execute_batch(&format!(
                "BEGIN;\n{}\nPRAGMA user_version = 1;\nCOMMIT;",
                include_str!("schema.sql")
            ))?;
        }
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
                 (name, email, color_r, color_g, color_b, imap_host, imap_port, imap_user)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new.name,
                new.email,
                color.r,
                color.g,
                color.b,
                new.imap.host,
                new.imap.port,
                new.imap.username
            ],
        )?;
        Ok(Account {
            id: AccountId(self.conn.last_insert_rowid()),
            name: new.name,
            email: new.email,
            color,
            imap: new.imap,
        })
    }

    /// All accounts, ordered by id.
    pub fn accounts(&self) -> Result<Vec<Account>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, email, color_r, color_g, color_b, imap_host, imap_port, imap_user
             FROM accounts ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
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
                },
            })
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            accounts.push(row?);
        }
        Ok(accounts)
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
            "SELECT id, account_id, path, role, uid_validity, last_seen_uid
             FROM folders WHERE account_id = ?1 AND path = ?2",
            params![account.0, path],
            folder_from_row,
        )?;
        Ok(folder)
    }

    /// All folders of an account, ordered by id.
    pub fn folders(&self, account: AccountId) -> Result<Vec<Folder>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, path, role, uid_validity, last_seen_uid
             FROM folders WHERE account_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account.0], folder_from_row)?;
        let mut folders = Vec::new();
        for row in rows {
            folders.push(row?);
        }
        Ok(folders)
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
    pub fn insert_messages(&self, folder: FolderId, msgs: &[NewMessage]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO messages
                     (folder_id, uid, message_id, subject, from_name, from_addr, date, seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                    m.seen
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
    /// (same Message-ID) are merged into one item, newest first.
    pub fn unified(
        &self,
        role: FolderRole,
        filter: AccountFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        // `None` means "all accounts" (`?2 IS NULL` is true).
        let account_id: Option<i64> = match filter {
            AccountFilter::All => None,
            AccountFilter::Only(id) => Some(id.0),
        };
        let mut stmt = self.conn.prepare(
            "SELECT m.id, f.account_id, COALESCE(m.message_id, 'row:' || m.id),
                    m.subject, m.from_name, m.from_addr, m.date, m.seen
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE f.role = ?1 AND (?2 IS NULL OR f.account_id = ?2)
             ORDER BY m.date DESC, m.id DESC",
        )?;
        let rows = stmt.query_map(params![role.as_str(), account_id], |row| {
            let from_name: Option<String> = row.get(4)?;
            let from_addr: String = row.get(5)?;
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, i64>(0)?,
                AccountId(row.get(1)?),
                row.get::<_, String>(3)?,
                from_name.unwrap_or(from_addr),
                row.get::<_, i64>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?;

        // Rows arrive newest first, so the first row of a group is its newest.
        let mut items: Vec<UnifiedItem> = Vec::new();
        let mut position: HashMap<String, usize> = HashMap::new();
        for row in rows {
            let (key, id, account, subject, from_display, date, seen) = row?;
            match position.get(&key) {
                Some(&i) => {
                    let item = &mut items[i];
                    item.row_ids.push(id);
                    item.accounts.push(account);
                    item.seen = item.seen && seen;
                }
                None => {
                    position.insert(key, items.len());
                    items.push(UnifiedItem {
                        row_ids: vec![id],
                        accounts: vec![account],
                        subject,
                        from_display,
                        date,
                        seen,
                    });
                }
            }
        }
        for item in &mut items {
            item.accounts.sort();
            item.accounts.dedup();
        }
        Ok(items.into_iter().skip(offset).take(limit).collect())
    }
}

/// Builds a `Folder` from a row of
/// `id, account_id, path, role, uid_validity, last_seen_uid`.
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

    fn acct(store: &Store, email: &str) -> AccountId {
        store
            .add_account(NewAccount {
                name: email.to_string(),
                email: email.to_string(),
                imap: ServerConfig {
                    host: "imap.example.com".to_string(),
                    port: 993,
                    username: email.to_string(),
                },
            })
            .unwrap()
            .id
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
}
