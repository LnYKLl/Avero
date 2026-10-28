//! SQLite storage: accounts, folders, messages, cached bodies, the unified
//! inbox query and local full-text search.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

mod cleanup;
mod drafts;
mod filters;
mod later;
mod server_search;
mod spaces;

use spaces::filter_account;

use crate::category::{classify, sender_key};
use crate::{
    Account, AccountColor, AccountFilter, AccountId, AuthKind, Category, Folder, FolderId,
    FolderRole, Listing, MessageHeaders, MessageRef, NewAccount, NewMessage, Security,
    ServerConfig, ServerFlags, UnifiedItem,
};

/// Error returned by every `Store` operation.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// An account with this e-mail address (case and spaces ignored) exists.
    #[error("an account with this address already exists")]
    DuplicateAccount,
}

/// Length of `UnifiedItem::snippet`, in characters.
const SNIPPET_CHARS: usize = 140;

/// The Avero database (one SQLite connection).
pub struct Store {
    conn: Connection,
    /// Accounts removed when the database was opened because another
    /// account had the same address (see `merged_accounts`).
    merged: Vec<AccountId>,
}

/// Columns read by `account_from_row`, in order.
const ACCOUNT_COLUMNS: &str = "id, name, email, color_r, color_g, color_b,
     imap_host, imap_port, imap_user, imap_security,
     smtp_host, smtp_port, smtp_user, smtp_security, auth, signature";

/// Columns read by `folder_from_row`, in order.
const FOLDER_COLUMNS: &str = "id, account_id, path, role, uid_validity, last_seen_uid";

/// Columns read by `item_from_row`, in order (`r` is a ranked group, `m`
/// its message shown, `b` that message's cached body).
const ITEM_COLUMNS: &str = "r.ids, r.accounts, m.subject, m.from_name, m.from_addr, m.date,
     r.all_seen, r.any_attachment, substr(b.text, 1, 1000), m.thread_id, r.any_flagged,
     r.snoozed_until";

/// Which messages a list (and its unread count) takes, on `m` (a message)
/// and `f` (its folder). Parameters: ?1 folder role, ?2 accounts (see
/// `spaces::filter_account`), ?6 tab
/// (category), ?7 sender, ?8 starred only, ?9 trash and spam left out;
/// NULL or 0 means "no condition". ?10 is 1 for "Plus tard" (only the
/// snoozed messages), else 0 (every list but that one leaves them out).
/// ?3 (search), ?4 and ?5 (paging) are the caller's.
const LIST_CONDITIONS: &str = "(?1 IS NULL OR f.role = ?1)
     AND (?2 IS NULL OR f.account_id IN (SELECT value FROM json_each(?2)))
     AND m.hidden = 0
     AND (?6 IS NULL OR COALESCE(m.category, 'important') = ?6)
     AND (?7 IS NULL OR lower(trim(m.from_addr)) = ?7)
     AND (?8 = 0 OR m.flagged = 1)
     AND (?9 = 0 OR f.role NOT IN ('trash', 'junk'))
     AND ?10 = EXISTS (SELECT 1 FROM snoozes s
                       WHERE s.key = COALESCE(m.message_id, 'row:' || m.id)
                         AND s.woke_at IS NULL)";

/// The conditions of a `Listing`, as parameters of `LIST_CONDITIONS`.
struct Scope {
    role: Option<&'static str>,
    category: Option<&'static str>,
    sender: Option<String>,
    starred: bool,
    skip_trash: bool,
    /// "Plus tard": the snoozed messages only.
    snoozed: bool,
}

impl Scope {
    /// Every folder, no condition (a search).
    fn everywhere() -> Scope {
        Scope {
            role: None,
            category: None,
            sender: None,
            starred: false,
            skip_trash: false,
            snoozed: false,
        }
    }

    fn of(listing: &Listing) -> Scope {
        let mut scope = Scope::everywhere();
        match listing {
            Listing::Folder(role) => scope.role = Some(role.as_str()),
            Listing::Tab(category) => {
                scope.role = Some(FolderRole::Inbox.as_str());
                scope.category = Some(category.as_str());
            }
            Listing::Starred => {
                scope.starred = true;
                scope.skip_trash = true;
            }
            Listing::Sender(addr) => scope.sender = Some(sender_key(addr)),
            Listing::Snoozed => {
                scope.snoozed = true;
                scope.skip_trash = true;
            }
        }
        scope
    }
}

/// What a search looks for (see `Store::search_with`).
struct Search<'a> {
    /// The FTS query (see `fts_query`).
    fts: &'a str,
    /// Rows found otherwise, as a JSON array.
    rows: String,
}

/// What makes two rows the same message: the Message-ID, or the row
/// itself when there is none.
const MESSAGE_KEY: &str = "COALESCE(m.message_id, 'row:' || m.id)";

/// The rows a conversation shows (`?1` the conversation, `?2` the account
/// or NULL, `?3` the folder role being listed): every folder but the
/// trash and spam ones, unless the list is one of them.
const THREAD_SCOPE: &str = "m.thread_id = ?1 AND m.hidden = 0
     AND (?2 IS NULL OR f.account_id IN (SELECT value FROM json_each(?2)))
     AND (f.role NOT IN ('trash', 'junk') OR f.role = ?3)";

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
        let mut store = Store {
            conn,
            merged: Vec::new(),
        };
        store.migrate()?;
        Ok(store)
    }

    /// The accounts deleted by the version 4 migration, which merged
    /// accounts sharing one address. The caller must erase their secrets:
    /// the store cannot reach the `SecretStore`.
    pub fn merged_accounts(&self) -> &[AccountId] {
        &self.merged
    }

    /// Applies the schema versions the database has not seen yet, each in
    /// its own transaction.
    fn migrate(&mut self) -> Result<(), StoreError> {
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
        if version < 4 {
            self.migrate_to_v4()?;
        }
        if version < 5 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v5.sql"))?;
            tx.execute_batch("PRAGMA user_version = 5;")?;
            tx.commit()?;
        }
        if version < 6 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v6.sql"))?;
            tx.execute_batch("PRAGMA user_version = 6;")?;
            tx.commit()?;
        }
        if version < 7 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v7.sql"))?;
            classify_unsorted(&tx)?;
            tx.execute_batch("PRAGMA user_version = 7;")?;
            tx.commit()?;
        }
        if version < 8 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v8.sql"))?;
            tx.execute_batch("PRAGMA user_version = 8;")?;
            tx.commit()?;
        }
        if version < 9 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v9.sql"))?;
            tx.execute_batch("PRAGMA user_version = 9;")?;
            tx.commit()?;
        }
        if version < 10 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(include_str!("schema_v10.sql"))?;
            tx.execute_batch("PRAGMA user_version = 10;")?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Version 4: one account per address. Accounts with the same address
    /// (case and spaces ignored) are merged into the oldest one, then a
    /// unique index keeps it that way. Foreign keys stay on, so bodies go
    /// with their messages.
    fn migrate_to_v4(&mut self) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let doomed: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM accounts
                 WHERE id NOT IN (SELECT MIN(id) FROM accounts
                                  GROUP BY lower(trim(email)))
                 ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in &doomed {
            delete_account_rows(&tx, AccountId(*id))?;
        }
        tx.execute_batch(include_str!("schema_v4.sql"))?;
        tx.execute_batch("PRAGMA user_version = 4;")?;
        tx.commit()?;
        self.merged.extend(doomed.into_iter().map(AccountId));
        Ok(())
    }

    /// Forgets local changes left pending by a previous run (its jobs are
    /// gone): hidden rows come back, server flags apply again.
    pub fn clear_pending(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET seen_pending = 0, flag_pending = 0, hidden = 0
             WHERE seen_pending != 0 OR flag_pending != 0 OR hidden != 0",
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
        let inserted = self.conn.execute(
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
        );
        // The unique index on the address is the guarantee.
        match inserted {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(StoreError::DuplicateAccount);
            }
            other => {
                other?;
            }
        }
        Ok(Account {
            id: AccountId(self.conn.last_insert_rowid()),
            name: new.name,
            email: new.email,
            color,
            imap: new.imap,
            smtp: new.smtp,
            auth: new.auth,
            signature: String::new(),
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

    /// Changes the signature of an account (empty: none).
    pub fn set_signature(&self, id: AccountId, signature: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE accounts SET signature = ?1 WHERE id = ?2",
            params![signature, id.0],
        )?;
        Ok(())
    }

    /// Removes an account with its folders, messages, bodies and search
    /// entries. Its secret is not touched (that is the `SecretStore`'s job).
    pub fn delete_account(&self, id: AccountId) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        delete_account_rows(&tx, id)?;
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
    /// Each new message is also added to the search index (by trigger),
    /// joins its conversation (see `link_thread`) and gets its tab (see
    /// `category_for`).
    pub fn insert_messages(&self, folder: FolderId, msgs: &[NewMessage]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO messages
                     (folder_id, uid, message_id, subject, from_name, from_addr, date, seen,
                      has_attachments, in_reply_to, refs, to_addrs, delivered_to,
                      list_unsubscribe, list_unsubscribe_post, list_id, precedence,
                      auto_submitted, headers_v2, flagged, category, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                         ?14, ?15, ?16, ?17, ?18, 1, ?19, ?20, ?21)",
            )?;
            for m in msgs {
                let h = &m.headers;
                let category = category_for(&tx, &m.from_addr, h)?;
                let inserted = stmt.execute(params![
                    folder.0,
                    m.uid,
                    m.message_id,
                    m.subject,
                    m.from_name,
                    m.from_addr,
                    m.date,
                    m.seen,
                    m.has_attachments,
                    h.in_reply_to,
                    joined(&h.references, " "),
                    joined(&h.to, ", "),
                    h.delivered_to,
                    h.list_unsubscribe,
                    h.list_unsubscribe_post,
                    h.list_id,
                    h.precedence,
                    h.auto_submitted,
                    m.flagged,
                    category.as_str(),
                    m.size,
                ])?;
                if inserted == 1 {
                    let row = tx.last_insert_rowid();
                    tx.execute(
                        "UPDATE messages SET thread_id = id WHERE id = ?1",
                        params![row],
                    )?;
                    link_thread(&tx, row, m.message_id.as_deref(), &m.headers)?;
                }
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

    /// Marks every message of the folder as lacking its extra headers, so
    /// that the next sync fetches them again (for a version of Avero that
    /// reads more headers; used by tests to stand for old messages).
    pub fn mark_headers_missing(&self, folder: FolderId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET headers_v2 = 0 WHERE folder_id = ?1",
            params![folder.0],
        )?;
        Ok(())
    }

    /// UIDs of the folder's messages stored before their extra headers
    /// were read (see `MessageHeaders`), newest first, at most `limit`.
    pub fn missing_headers(&self, folder: FolderId, limit: usize) -> Result<Vec<u32>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM messages WHERE folder_id = ?1 AND headers_v2 = 0
             ORDER BY uid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![folder.0, limit as i64], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Saves the extra headers fetched again for messages stored before
    /// version 5, and moves those messages into their conversations.
    /// Every UID of `asked` is marked done, even if the server sent
    /// nothing for it (the message is gone: no use asking again).
    pub fn complete_headers(
        &self,
        folder: FolderId,
        asked: &[u32],
        fetched: &[NewMessage],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut find = tx.prepare(
                "SELECT id, message_id, from_addr FROM messages
                 WHERE folder_id = ?1 AND uid = ?2",
            )?;
            let mut update = tx.prepare(
                "UPDATE messages SET in_reply_to = ?1, refs = ?2, to_addrs = ?3,
                     delivered_to = ?4, list_unsubscribe = ?5, list_unsubscribe_post = ?6,
                     list_id = ?7, precedence = ?8, auto_submitted = ?9, headers_v2 = 1,
                     category = ?11
                 WHERE id = ?10",
            )?;
            for m in fetched.iter().filter(|m| asked.contains(&m.uid)) {
                let found: Option<(i64, Option<String>, String)> = find
                    .query_row(params![folder.0, m.uid], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .optional()?;
                // The Message-ID stored at the time is the one that counts.
                let Some((row, message_id, from_addr)) = found else {
                    continue;
                };
                let h = &m.headers;
                // Its list headers were unknown until now: sort it again.
                let category = category_for(&tx, &from_addr, h)?;
                update.execute(params![
                    h.in_reply_to,
                    joined(&h.references, " "),
                    joined(&h.to, ", "),
                    h.delivered_to,
                    h.list_unsubscribe,
                    h.list_unsubscribe_post,
                    h.list_id,
                    h.precedence,
                    h.auto_submitted,
                    row,
                    category.as_str(),
                ])?;

                link_thread(&tx, row, message_id.as_deref(), h)?;
            }
            let mut done =
                tx.prepare("UPDATE messages SET headers_v2 = 1 WHERE folder_id = ?1 AND uid = ?2")?;
            for uid in asked {
                done.execute(params![folder.0, uid])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The extra headers of a stored message (all empty for a message
    /// stored before version 5 whose headers were not fetched again yet).
    pub fn headers(&self, row_id: i64) -> Result<Option<MessageHeaders>, StoreError> {
        let headers = self
            .conn
            .query_row(
                "SELECT in_reply_to, refs, to_addrs, delivered_to, list_unsubscribe,
                        list_unsubscribe_post, list_id, precedence, auto_submitted
                 FROM messages WHERE id = ?1",
                params![row_id],
                |row| {
                    let refs: Option<String> = row.get(1)?;
                    let to: Option<String> = row.get(2)?;
                    Ok(MessageHeaders {
                        in_reply_to: row.get(0)?,
                        references: split_list(refs.as_deref(), ' '),
                        to: split_list(to.as_deref(), ','),
                        delivered_to: row.get(3)?,
                        list_unsubscribe: row.get(4)?,
                        list_unsubscribe_post: row.get(5)?,
                        list_id: row.get(6)?,
                        precedence: row.get(7)?,
                        auto_submitted: row.get(8)?,
                    })
                },
            )
            .optional()?;
        Ok(headers)
    }

    /// The conversation a stored message belongs to.
    pub fn thread_of(&self, row_id: i64) -> Result<Option<i64>, StoreError> {
        let thread = self
            .conn
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                params![row_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(thread.flatten())
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
        self.list(&Listing::Folder(role), filter, false, limit, offset)
    }

    /// The list for one folder role with one item per conversation (see
    /// `list`).
    pub fn threads(
        &self,
        role: FolderRole,
        filter: AccountFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        self.list(&Listing::Folder(role), filter, true, limit, offset)
    }

    /// The items of a listing (a folder, a tab of the inbox, the starred
    /// messages, one sender's messages), the most recent first.
    ///
    /// Without `by_thread`, one item per message, copies merged like
    /// `unified`. With it, one item per conversation: it shows the
    /// conversation's newest message in the listing; its `row_ids` are
    /// all the conversation's rows in the listing (every copy), so an
    /// action on the item acts on all of them. It is unread when one of
    /// these rows is.
    ///
    /// `count` and `senders` describe the whole conversation as the reader
    /// shows it (see `thread_messages`): other folders included, except
    /// the trash and spam ones (unless the listing is one of them).
    pub fn list(
        &self,
        listing: &Listing,
        filter: AccountFilter,
        by_thread: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        let scope = Scope::of(listing);
        let mut items = self.grouped(&scope, filter, None, by_thread, limit, offset)?;
        if !by_thread {
            return Ok(items);
        }
        let own = self.own_addresses()?;
        let accounts = filter_account(&self.conn, filter)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT COALESCE(m.message_id, 'row:' || m.id), m.from_name, m.from_addr
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE {THREAD_SCOPE}
             ORDER BY m.date, m.id"
        ))?;
        for item in &mut items {
            let Some(thread) = item.thread_id else {
                continue;
            };
            let rows = stmt.query_map(params![thread, accounts, scope.role], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut messages = HashSet::new();
            let mut people: Vec<(String, String)> = Vec::new();
            for found in rows {
                let (message, name, addr) = found?;
                messages.insert(message);
                let key = addr.trim().to_lowercase();
                if people.iter().any(|(known, _)| *known == key) {
                    continue;
                }
                let label = if own.contains(&key) {
                    "moi".to_string()
                } else {
                    name.filter(|n| !n.trim().is_empty()).unwrap_or(addr)
                };
                people.push((key, label));
            }
            item.count = messages.len().max(1);
            item.senders = people.into_iter().map(|(_, label)| label).collect();
        }
        Ok(items)
    }

    /// The messages of a conversation, oldest first, one item per message
    /// (copies across accounts merged, the copy with a cached body first
    /// in `row_ids`). Like `threads`, only accounts allowed by `filter`,
    /// and no trash or spam unless `role` is one of them.
    pub fn thread_messages(
        &self,
        thread: i64,
        role: FolderRole,
        filter: AccountFilter,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        let sql = format!(
            "WITH sel AS (
                 SELECT m.id, f.account_id, m.date, m.seen, m.has_attachments, m.flagged,
                        COALESCE(m.message_id, 'row:' || m.id) AS k,
                        EXISTS (SELECT 1 FROM bodies b WHERE b.message_row = m.id) AS cached
                 FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE {THREAD_SCOPE}
             ),
             ranked AS (
                 SELECT id,
                        ROW_NUMBER() OVER best AS rn,
                        group_concat(id) OVER whole AS ids,
                        group_concat(account_id) OVER whole AS accounts,
                        MIN(seen) OVER whole AS all_seen,
                        MAX(has_attachments) OVER whole AS any_attachment,
                        MAX(flagged) OVER whole AS any_flagged,
                        NULL AS snoozed_until
                 FROM sel
                 WINDOW best AS (PARTITION BY k ORDER BY cached DESC, date DESC, id DESC),
                        whole AS (PARTITION BY k ORDER BY cached DESC, date DESC, id DESC
                                  ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
             )
             SELECT {ITEM_COLUMNS}
             FROM ranked r
             JOIN messages m ON m.id = r.id
             LEFT JOIN bodies b ON b.message_row = m.id
             WHERE r.rn = 1
             ORDER BY m.date, m.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![thread, filter_account(&self.conn, filter)?, role.as_str()],
            |row| item_from_row(row, true),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Local full-text search over subject, sender and cached bodies.
    ///
    /// Every word of `query` must match the start of a word (`fact` finds
    /// "factures"); accents and case are ignored. Any text is accepted:
    /// FTS syntax characters are neutralised. `role = None` searches every
    /// folder. Results are merged like `unified` and sorted newest first:
    /// one item per message, even when the list groups conversations.
    pub fn search(
        &self,
        query: &str,
        role: Option<FolderRole>,
        filter: AccountFilter,
        limit: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        self.search_with(query, role, filter, &[], limit)
    }

    /// `search`, plus the rows of `also` (in the folders and accounts
    /// searched): the messages a search on the server found, whose text
    /// may not be on this computer.
    pub fn search_with(
        &self,
        query: &str,
        role: Option<FolderRole>,
        filter: AccountFilter,
        also: &[i64],
        limit: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        let scope = match role {
            Some(role) => Scope::of(&Listing::Folder(role)),
            None => Scope::everywhere(),
        };
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let rows: Vec<String> = also.iter().map(i64::to_string).collect();
        let found = Search {
            fts: &fts,
            rows: format!("[{}]", rows.join(",")),
        };
        self.grouped(&scope, filter, Some(&found), false, limit, 0)
    }

    /// Shared by `list` and `search`: one item per Message-ID (or per row
    /// when it has none), or per conversation when `by_thread`, showing
    /// the newest row.
    fn grouped(
        &self,
        scope: &Scope,
        filter: AccountFilter,
        search: Option<&Search>,
        by_thread: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnifiedItem>, StoreError> {
        let fts_condition = match search {
            Some(_) => {
                "AND (m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?3)
                      OR m.id IN (SELECT value FROM json_each(?11)))"
            }
            None => "AND ?3 IS NULL AND ?11 IS NULL",
        };
        let key = if by_thread {
            "m.thread_id"
        } else {
            MESSAGE_KEY
        };
        // "Plus tard" shows the next to wake first; the other lists the
        // most recent first, a conversation back from "Plus tard" counting
        // as new from the time it woke up.
        let order = if scope.snoozed {
            "r.snoozed_until, m.id"
        } else {
            "r.sort_date DESC, m.id DESC"
        };
        let sql = format!(
            "WITH sel AS (
                 SELECT m.id, f.account_id, m.date, m.seen, m.has_attachments, m.flagged,
                        {key} AS k, s.until AS snoozed,
                        MAX(m.date, COALESCE(s.woke_at, 0)) AS sort_date
                 FROM messages m JOIN folders f ON f.id = m.folder_id
                 LEFT JOIN snoozes s ON s.key = {MESSAGE_KEY}
                 WHERE {LIST_CONDITIONS}
                   {fts_condition}
             ),
             ranked AS (
                 SELECT id,
                        ROW_NUMBER() OVER newest AS rn,
                        group_concat(id) OVER whole AS ids,
                        group_concat(account_id) OVER whole AS accounts,
                        MIN(seen) OVER whole AS all_seen,
                        MAX(has_attachments) OVER whole AS any_attachment,
                        MAX(flagged) OVER whole AS any_flagged,
                        MAX(sort_date) OVER whole AS sort_date,
                        CASE WHEN ?10 THEN MIN(snoozed) OVER whole END AS snoozed_until
                 FROM sel
                 WINDOW newest AS (PARTITION BY k ORDER BY date DESC, id DESC),
                        whole AS (PARTITION BY k ORDER BY date DESC, id DESC
                                  ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
             )
             SELECT {ITEM_COLUMNS}
             FROM ranked r
             JOIN messages m ON m.id = r.id
             LEFT JOIN bodies b ON b.message_row = m.id
             WHERE r.rn = 1
             ORDER BY {order}
             LIMIT ?4 OFFSET ?5"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                scope.role,
                filter_account(&self.conn, filter)?,
                search.map(|s| s.fts),
                limit as i64,
                offset as i64,
                scope.category,
                scope.sender,
                scope.starred,
                scope.skip_trash,
                scope.snoozed,
                search.map(|s| s.rows.as_str()),
            ],
            |row| item_from_row(row, by_thread),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The user's own addresses (of every account), lower case.
    fn own_addresses(&self) -> Result<HashSet<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT lower(trim(email)) FROM accounts")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Number of unified items (merged like `unified`) not seen yet.
    pub fn unread_count(
        &self,
        role: FolderRole,
        filter: AccountFilter,
    ) -> Result<usize, StoreError> {
        self.list_unread(&Listing::Folder(role), filter, false)
    }

    /// Number of conversations of the folder with an unread message
    /// there (the unread items of `threads`).
    pub fn unread_threads(
        &self,
        role: FolderRole,
        filter: AccountFilter,
    ) -> Result<usize, StoreError> {
        self.list_unread(&Listing::Folder(role), filter, true)
    }

    /// Number of unread items of `list` (the same arguments, all pages).
    pub fn list_unread(
        &self,
        listing: &Listing,
        filter: AccountFilter,
        by_thread: bool,
    ) -> Result<usize, StoreError> {
        let scope = Scope::of(listing);
        let key = if by_thread {
            "m.thread_id"
        } else {
            MESSAGE_KEY
        };
        let none: Option<&str> = None;
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM (
                     SELECT 1
                     FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE {LIST_CONDITIONS} AND ?3 IS NULL AND ?4 IS NULL AND ?5 IS NULL
                     GROUP BY {key}
                     HAVING MIN(m.seen) = 0
                 )"
            ),
            params![
                scope.role,
                filter_account(&self.conn, filter)?,
                none,
                none,
                none,
                scope.category,
                scope.sender,
                scope.starred,
                scope.skip_trash,
                scope.snoozed,
            ],
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

    /// Stars or unstars rows locally. Like `set_seen`, the change stays
    /// pending (the server's flag does not overwrite it) until
    /// `confirm_flagged`.
    pub fn set_flagged(&self, row_ids: &[i64], flagged: bool) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE messages SET flagged = ?1, flag_pending = flag_pending + 1 WHERE id = ?2",
            )?;
            for &id in row_ids {
                stmt.execute(params![flagged, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// One pending star change of these rows reached the server (or
    /// failed): the server's flag applies again once none is left.
    pub fn confirm_flagged(&self, row_ids: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE messages SET flag_pending = MAX(flag_pending - 1, 0) WHERE id = ?1",
            )?;
            for &id in row_ids {
                stmt.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The starred rows of the conversations of `row_ids` (in any folder
    /// but the trash and spam ones, of the accounts allowed by `filter`),
    /// besides `row_ids` themselves: unstarring a conversation unstars
    /// them too, so that it leaves "Favoris".
    pub fn starred_in_threads(
        &self,
        row_ids: &[i64],
        filter: AccountFilter,
    ) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.thread_id = (SELECT thread_id FROM messages WHERE id = ?1)
               AND m.flagged = 1 AND m.hidden = 0
               AND (?2 IS NULL OR f.account_id IN (SELECT value FROM json_each(?2)))
               AND f.role NOT IN ('trash', 'junk')
             ORDER BY m.id",
        )?;
        let mut found: Vec<i64> = Vec::new();
        for &id in row_ids {
            let rows = stmt.query_map(params![id, filter_account(&self.conn, filter)?], |row| {
                row.get(0)
            })?;
            for row in rows {
                let row = row?;
                if !row_ids.contains(&row) && !found.contains(&row) {
                    found.push(row);
                }
            }
        }
        Ok(found)
    }

    /// The tab of a stored message.
    pub fn category(&self, row_id: i64) -> Result<Option<Category>, StoreError> {
        let category: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT category FROM messages WHERE id = ?1",
                params![row_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(category.map(|c| Category::parse(c.as_deref().unwrap_or(""))))
    }

    /// Puts a sender's messages in a tab, now and for the messages to
    /// come (it wins over `classify`), along with the rows `also` (the
    /// rest of the conversation being reclassified, whoever wrote it).
    pub fn set_sender_category(
        &self,
        addr: &str,
        category: Category,
        also: &[i64],
    ) -> Result<(), StoreError> {
        let key = sender_key(addr);
        let tx = self.conn.unchecked_transaction()?;
        if !key.is_empty() {
            tx.execute(
                "INSERT INTO senders (addr, category) VALUES (?1, ?2)
                 ON CONFLICT(addr) DO UPDATE SET category = excluded.category",
                params![key, category.as_str()],
            )?;
            tx.execute(
                "UPDATE messages SET category = ?1 WHERE lower(trim(from_addr)) = ?2",
                params![category.as_str(), key],
            )?;
        }
        {
            let mut stmt = tx.prepare("UPDATE messages SET category = ?1 WHERE id = ?2")?;
            for &id in also {
                stmt.execute(params![category.as_str(), id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The tab chosen by hand for a sender, if any.
    pub fn sender_category(&self, addr: &str) -> Result<Option<Category>, StoreError> {
        let category: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT category FROM senders WHERE addr = ?1",
                params![sender_key(addr)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(category.flatten().map(|c| Category::parse(&c)))
    }

    /// Remembers that unsubscribing from a sender was asked at `at`
    /// (Unix time in seconds).
    pub fn mark_unsubscribed(&self, addr: &str, at: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO senders (addr, unsubscribed_at) VALUES (?1, ?2)
             ON CONFLICT(addr) DO UPDATE SET unsubscribed_at = excluded.unsubscribed_at",
            params![sender_key(addr), at],
        )?;
        Ok(())
    }

    /// When unsubscribing from a sender was asked, if it was.
    pub fn unsubscribed_at(&self, addr: &str) -> Result<Option<i64>, StoreError> {
        let at: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT unsubscribed_at FROM senders WHERE addr = ?1",
                params![sender_key(addr)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(at.flatten())
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
    /// folder: `present` holds the flags of every message the server has
    /// in that range.
    ///
    /// Stored UIDs listed in `present` get its seen and starred flags,
    /// unless a local change of that flag is still on its way to the
    /// server. Stored UIDs in the range but missing from `present` were
    /// expunged on the server and are removed. UIDs outside the range are
    /// not touched.
    pub fn apply_flags(
        &self,
        folder: FolderId,
        from: u32,
        to: u32,
        present: &[ServerFlags],
    ) -> Result<(), StoreError> {
        let listed: std::collections::HashSet<u32> = present.iter().map(|f| f.uid).collect();
        let stored = self.inbox_uids(folder)?;

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut seen = tx.prepare(
                "UPDATE messages SET seen = ?1
                 WHERE folder_id = ?2 AND uid = ?3 AND seen != ?1 AND seen_pending = 0",
            )?;
            let mut flagged = tx.prepare(
                "UPDATE messages SET flagged = ?1
                 WHERE folder_id = ?2 AND uid = ?3 AND flagged != ?1 AND flag_pending = 0",
            )?;
            for flags in present {
                seen.execute(params![flags.seen, folder.0, flags.uid])?;
                flagged.execute(params![flags.flagged, folder.0, flags.uid])?;
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

/// Puts a just-stored message in its conversation, inside the caller's
/// transaction.
///
/// A message names Message-IDs: its own, In-Reply-To and References.
/// Messages naming a common id are in the same conversation, whatever
/// their folder or account, and in whatever order they arrive: a parent
/// stored after its replies names its own id, which the replies named
/// already, so the conversations are merged then. Merging keeps the
/// smallest conversation id (a row id; see `schema_v5.sql`). A message
/// that names no id at all stays alone.
///
/// There is no grouping by subject when these headers are missing:
/// automatic mail often shares one subject ("Votre facture", "Nouvelle
/// connexion") and would be glued together wrongly; a reply without
/// headers simply shows on its own line, as before conversations.
///
/// Conversations are never split again: when a message that linked two
/// parts is deleted, they stay together.
fn link_thread(
    conn: &Connection,
    row: i64,
    message_id: Option<&str>,
    headers: &MessageHeaders,
) -> Result<(), StoreError> {
    let mut ids: Vec<&str> = message_id
        .into_iter()
        .chain(headers.in_reply_to.as_deref())
        .chain(headers.references.iter().map(String::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let mut threads: Vec<i64> = vec![conn.query_row(
        "SELECT thread_id FROM messages WHERE id = ?1",
        params![row],
        |r| r.get(0),
    )?];
    {
        let mut link =
            conn.prepare("INSERT OR IGNORE INTO thread_links (message_row, ref) VALUES (?1, ?2)")?;
        let mut linked = conn.prepare(
            "SELECT DISTINCT m.thread_id FROM thread_links l
             JOIN messages m ON m.id = l.message_row
             WHERE l.ref = ?1",
        )?;
        for id in &ids {
            link.execute(params![row, id])?;
            let found = linked.query_map(params![id], |r| r.get::<_, i64>(0))?;
            for thread in found {
                threads.push(thread?);
            }
        }
    }
    threads.sort_unstable();
    threads.dedup();
    let keep = threads[0];
    let mut merge = conn.prepare("UPDATE messages SET thread_id = ?1 WHERE thread_id = ?2")?;
    for &other in &threads[1..] {
        merge.execute(params![keep, other])?;
    }
    Ok(())
}

/// The tab of a message about to be stored or completed: the one chosen
/// by hand for its sender, else what `classify` says.
fn category_for(
    conn: &Connection,
    from_addr: &str,
    headers: &MessageHeaders,
) -> Result<Category, StoreError> {
    let chosen: Option<Option<String>> = conn
        .query_row(
            "SELECT category FROM senders WHERE addr = ?1",
            params![sender_key(from_addr)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match chosen.flatten() {
        Some(category) => Category::parse(&category),
        None => classify(from_addr, headers),
    })
}

/// Gives a tab to the messages that have none (those stored before
/// version 7), inside the caller's transaction.
fn classify_unsorted(conn: &Connection) -> Result<(), StoreError> {
    let unsorted: Vec<(i64, String, MessageHeaders)> = {
        let mut stmt = conn.prepare(
            "SELECT id, from_addr, list_unsubscribe, list_id, precedence, auto_submitted
             FROM messages WHERE category IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                MessageHeaders {
                    list_unsubscribe: row.get(2)?,
                    list_id: row.get(3)?,
                    precedence: row.get(4)?,
                    auto_submitted: row.get(5)?,
                    ..Default::default()
                },
            ))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let mut update = conn.prepare("UPDATE messages SET category = ?1 WHERE id = ?2")?;
    for (id, from_addr, headers) in unsorted {
        let category = category_for(conn, &from_addr, &headers)?;
        update.execute(params![category.as_str(), id])?;
    }
    Ok(())
}

/// `None` for an empty list, else the items joined by `separator`.
fn joined(items: &[String], separator: &str) -> Option<String> {
    (!items.is_empty()).then(|| items.join(separator))
}

/// The items of a list stored by `joined` (`None` gives none).
fn split_list(text: Option<&str>, separator: char) -> Vec<String> {
    text.unwrap_or("")
        .split(separator)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// Deletes the rows an account owns, inside the caller's transaction.
fn delete_account_rows(conn: &Connection, id: AccountId) -> Result<(), StoreError> {
    // Bodies (and the account's place in its spaces) go by ON DELETE
    // CASCADE, search entries by trigger.
    conn.execute(
        "DELETE FROM messages
         WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
        params![id.0],
    )?;
    conn.execute("DELETE FROM folders WHERE account_id = ?1", params![id.0])?;
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id.0])?;
    Ok(())
}

/// Builds a list item from a row of `ITEM_COLUMNS`. With `in_thread`, the
/// item belongs to a conversation (its `thread_id` is set).
fn item_from_row(row: &rusqlite::Row, in_thread: bool) -> rusqlite::Result<UnifiedItem> {
    let ids: String = row.get(0)?;
    let accounts: String = row.get(1)?;
    let from_name: Option<String> = row.get(3)?;
    let from_addr: String = row.get(4)?;
    let body: Option<String> = row.get(8)?;
    let thread: Option<i64> = row.get(9)?;
    let flagged: bool = row.get(10)?;
    let snoozed_until: Option<i64> = row.get(11)?;
    let mut accounts: Vec<AccountId> = parse_id_list(&accounts)
        .into_iter()
        .map(AccountId)
        .collect();
    accounts.sort();
    accounts.dedup();
    let from_display = from_name.unwrap_or_else(|| from_addr.clone());
    Ok(UnifiedItem {
        row_ids: parse_id_list(&ids),
        accounts,
        subject: row.get(2)?,
        senders: vec![from_display.clone()],
        from_display,
        from_addr,
        date: row.get(5)?,
        seen: row.get(6)?,
        flagged,
        has_attachments: row.get(7)?,

        snippet: body.as_deref().map(snippet).unwrap_or_default(),
        thread_id: thread.filter(|_| in_thread),
        count: 1,
        snoozed_until,
    })
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
        signature: row.get(15)?,
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

    pub(super) fn acct(store: &Store, email: &str) -> AccountId {
        store.add_account(new_account(email)).unwrap().id
    }

    pub(super) fn inbox(store: &Store, account: AccountId) -> Folder {
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

    pub(super) fn count(store: &Store, table: &str) -> i64 {
        store
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    pub(super) fn msg(uid: u32, message_id: Option<&str>, date: i64) -> NewMessage {
        NewMessage {
            uid,
            message_id: message_id.map(|s| s.to_string()),
            subject: format!("subject {uid}"),
            from_name: None,
            from_addr: "x@y.z".to_string(),
            date,
            seen: false,
            has_attachments: false,
            ..Default::default()
        }
    }

    /// A reply: `parent` in In-Reply-To, `refs` in References.
    fn reply(uid: u32, id: &str, parent: Option<&str>, refs: &[&str], date: i64) -> NewMessage {
        let mut m = msg(uid, Some(id), date);
        m.headers.in_reply_to = parent.map(str::to_string);
        m.headers.references = refs.iter().map(|r| r.to_string()).collect();
        m
    }

    pub(super) fn folder(
        store: &Store,
        account: AccountId,
        path: &str,
        role: FolderRole,
    ) -> FolderId {
        store.upsert_folder(account, path, role).unwrap().id
    }

    fn threads_of(store: &Store, role: FolderRole) -> Vec<UnifiedItem> {
        store.threads(role, AccountFilter::All, 200, 0).unwrap()
    }

    /// The conversation of each stored message, by subject.
    fn thread_ids(store: &Store) -> std::collections::BTreeMap<String, i64> {
        let mut stmt = store
            .conn
            .prepare("SELECT subject, thread_id FROM messages")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
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
    fn signature_is_kept_per_account() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        assert_eq!(s.accounts().unwrap()[0].signature, "");
        s.set_signature(a, "Marie\nPâtisserie Durand").unwrap();
        let imap = server("imap.new.com", 994, "u", Security::Tls);
        // Changing the other settings keeps it.
        s.update_account(a, "Perso", &imap, &imap, AuthKind::Password)
            .unwrap();
        let accounts = s.accounts().unwrap();
        assert_eq!(accounts[0].signature, "Marie\nPâtisserie Durand");
        assert_eq!(accounts[1].signature, "");
        assert_eq!(accounts[1].id, b);
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
        s.apply_flags(f.id, 1, 4, &seen_flags(&[(1, true), (3, false), (4, true)]))
            .unwrap();

        assert_eq!(
            s.inbox_uids(f.id).unwrap(),
            vec![(1, true), (3, false), (4, true), (7, false)]
        );
        // An empty range on the server: its stored messages are gone.
        s.apply_flags(f.id, 5, 9, &seen_flags(&[])).unwrap();
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
        s.apply_flags(f.id, 1, 2, &seen_flags(&[(1, false), (2, true)]))
            .unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap(), vec![(1, true), (2, true)]);

        // Read, then unread at once: two changes pending.
        s.set_seen(&[row1], false).unwrap();
        s.confirm_seen(&[row1]).unwrap();
        s.apply_flags(f.id, 1, 2, &seen_flags(&[(1, true), (2, true)]))
            .unwrap();
        assert_eq!(s.inbox_uids(f.id).unwrap()[0], (1, false));

        // Both confirmed: the server's flag applies again.
        s.confirm_seen(&[row1]).unwrap();
        s.confirm_seen(&[row1]).unwrap();
        s.apply_flags(f.id, 1, 2, &seen_flags(&[(1, true), (2, true)]))
            .unwrap();
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
        s.apply_flags(f.id, 1, 2, &seen_flags(&[(1, false), (2, false)]))
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
    fn duplicate_address_is_refused_whatever_the_case() {
        let s = Store::open_in_memory().unwrap();
        acct(&s, "a@x.fr");
        let err = s.add_account(new_account(" A@X.fr ")).unwrap_err();
        assert!(matches!(err, StoreError::DuplicateAccount), "{err:?}");
        assert_eq!(s.accounts().unwrap().len(), 1);
    }

    #[test]
    fn version_3_duplicates_are_merged_into_the_oldest() {
        let path = std::env::temp_dir().join(format!("avero-merge-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let s = Store::open(&path).unwrap();
            let first = acct(&s, "A@x.fr");
            let second = acct(&s, "b@x.fr");
            // Same address as `first`: only possible without the v4 index.
            s.conn
                .execute_batch("DROP INDEX accounts_email_unique;")
                .unwrap();
            let dup = acct(&s, "a@x.fr ");
            for (account, uid) in [(first, 1), (second, 2), (dup, 3)] {
                let f = inbox(&s, account);
                s.insert_messages(f.id, &[msg(uid, None, 10)]).unwrap();
            }
            let dup_row = s
                .message_refs(&inbox_rows(&s))
                .unwrap()
                .into_iter()
                .find(|m| m.uid == 3)
                .unwrap();
            s.put_body(dup_row.row_id, b"raw", "texte", false).unwrap();
            s.conn.execute_batch(UNDO_V8).unwrap();
            s.conn.execute_batch(UNDO_V5).unwrap();
            s.conn.execute_batch("PRAGMA user_version = 3;").unwrap();
            assert_eq!((first.0, dup.0), (1, 3));
        }

        let s = Store::open(&path).unwrap();

        let ids: Vec<i64> = s.accounts().unwrap().iter().map(|a| a.id.0).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(s.merged_accounts(), &[AccountId(3)]);
        assert_eq!(count(&s, "messages"), 2);
        assert_eq!(count(&s, "folders"), 2);
        assert_eq!(count(&s, "bodies"), 0);
        assert!(s.add_account(new_account("a@X.fr")).is_err());
        drop(s);
        // A second opening has nothing left to merge.
        let s = Store::open(&path).unwrap();
        assert!(s.merged_accounts().is_empty());
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reply_after_its_parent_joins_its_conversation() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        let sent = folder(&s, a, "Sent", FolderRole::Sent);
        s.insert_messages(inbox, &[msg(1, Some("<p>"), 10)])
            .unwrap();
        s.insert_messages(sent, &[reply(1, "<r>", Some("<p>"), &["<p>"], 20)])
            .unwrap();
        s.insert_messages(inbox, &[msg(2, Some("<other>"), 15)])
            .unwrap();

        let inbox_threads = threads_of(&s, FolderRole::Inbox);
        assert_eq!(inbox_threads.len(), 2);
        // The conversation counts the reply kept in Sent.
        let conversation = inbox_threads.iter().find(|i| i.date == 10).unwrap();
        assert_eq!(conversation.count, 2);
        assert_eq!(conversation.row_ids.len(), 1);
        let messages = s
            .thread_messages(
                conversation.thread_id.unwrap(),
                FolderRole::Inbox,
                AccountFilter::All,
            )
            .unwrap();
        let dates: Vec<i64> = messages.iter().map(|m| m.date).collect();
        assert_eq!(dates, vec![10, 20]);
        let lonely = inbox_threads.iter().find(|i| i.date == 15).unwrap();
        assert_eq!(lonely.count, 1);
    }

    #[test]
    fn parent_arriving_after_its_replies_merges_them() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        // Two replies that only name their own parent, the parent last.
        s.insert_messages(inbox, &[reply(3, "<c>", Some("<b>"), &[], 30)])
            .unwrap();
        s.insert_messages(inbox, &[reply(1, "<a>", None, &[], 10)])
            .unwrap();
        assert_eq!(threads_of(&s, FolderRole::Inbox).len(), 2);
        s.insert_messages(inbox, &[reply(2, "<b>", Some("<a>"), &[], 20)])
            .unwrap();

        let items = threads_of(&s, FolderRole::Inbox);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].count, 3);
        assert_eq!(items[0].row_ids.len(), 3);
        assert_eq!(items[0].date, 30);
        let ids = thread_ids(&s);
        assert!(ids.values().all(|&t| t == ids["subject 1"]), "{ids:?}");
    }

    #[test]
    fn references_chain_links_messages_that_never_name_each_other() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        // The root is not stored: both replies name it in References.
        s.insert_messages(
            inbox,
            &[
                reply(1, "<b>", Some("<root>"), &["<root>"], 10),
                reply(2, "<d>", Some("<c>"), &["<root>", "<b>", "<c>"], 30),
                reply(3, "<z>", Some("<elsewhere>"), &["<elsewhere>"], 40),
            ],
        )
        .unwrap();
        let items = threads_of(&s, FolderRole::Inbox);
        assert_eq!(items.len(), 2);
        let chain = items.iter().find(|i| i.date == 30).unwrap();
        assert_eq!(chain.count, 2);
    }

    #[test]
    fn conversation_spans_accounts_and_keeps_duplicates_merged() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let inbox_a = folder(&s, a, "INBOX", FolderRole::Inbox);
        let inbox_b = folder(&s, b, "INBOX", FolderRole::Inbox);
        let sent_b = folder(&s, b, "Sent", FolderRole::Sent);
        // <m> reached both accounts; b answered it from its own address.
        let mut first = msg(1, Some("<m>"), 10);
        first.from_name = Some("Marie".into());
        s.insert_messages(inbox_a, std::slice::from_ref(&first))
            .unwrap();
        s.insert_messages(inbox_b, &[first]).unwrap();
        let mut answer = reply(1, "<r>", Some("<m>"), &["<m>"], 20);
        answer.from_addr = "B@x".into();
        s.insert_messages(sent_b, &[answer]).unwrap();

        let items = threads_of(&s, FolderRole::Inbox);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].accounts, vec![a, b]);
        assert_eq!(items[0].row_ids.len(), 2);
        // Two messages, not three rows; the user's own address is "moi".
        assert_eq!(items[0].count, 2);
        assert_eq!(items[0].senders, vec!["Marie", "moi"]);
        let messages = s
            .thread_messages(
                items[0].thread_id.unwrap(),
                FolderRole::Inbox,
                AccountFilter::All,
            )
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].accounts, vec![a, b]);
        assert_eq!(messages[0].row_ids.len(), 2);
        // Seen from account a alone, b's reply is not there.
        let only_a = s
            .thread_messages(
                items[0].thread_id.unwrap(),
                FolderRole::Inbox,
                AccountFilter::Only(a),
            )
            .unwrap();
        assert_eq!(only_a.len(), 1);
        // The per-message list (conversations off) is unchanged.
        assert_eq!(all_inbox(&s).len(), 1);
    }

    #[test]
    fn thread_messages_prefer_the_copy_with_a_cached_body() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let b = acct(&s, "b@x");
        let inbox_a = folder(&s, a, "INBOX", FolderRole::Inbox);
        let inbox_b = folder(&s, b, "INBOX", FolderRole::Inbox);
        s.insert_messages(inbox_a, &[msg(1, Some("<m>"), 10)])
            .unwrap();
        s.insert_messages(inbox_b, &[msg(1, Some("<m>"), 10)])
            .unwrap();
        let item = &threads_of(&s, FolderRole::Inbox)[0];
        let cached = *item.row_ids.iter().max().unwrap();
        s.put_body(cached, b"raw", "texte", false).unwrap();
        let messages = s
            .thread_messages(
                item.thread_id.unwrap(),
                FolderRole::Inbox,
                AccountFilter::All,
            )
            .unwrap();
        assert_eq!(messages[0].row_ids[0], cached);
        assert_eq!(messages[0].snippet, "texte");
    }

    #[test]
    fn thread_list_unread_paging_and_trash() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        let trash = folder(&s, a, "Trash", FolderRole::Trash);
        let mut read = msg(1, Some("<a1>"), 10);
        read.seen = true;
        let mut read_too = reply(2, "<a2>", Some("<a1>"), &[], 50);
        read_too.seen = true;
        s.insert_messages(
            inbox,
            &[
                read,
                read_too,
                reply(3, "<a3>", Some("<a2>"), &[], 20),
                msg(4, Some("<b1>"), 40),
                msg(5, Some("<c1>"), 30),
            ],
        )
        .unwrap();
        // A deleted reply: counted in the trash only.
        s.insert_messages(trash, &[reply(1, "<a4>", Some("<a3>"), &[], 60)])
            .unwrap();

        let all = AccountFilter::All;
        let items = threads_of(&s, FolderRole::Inbox);
        let dates: Vec<i64> = items.iter().map(|i| i.date).collect();
        assert_eq!(dates, vec![50, 40, 30]);
        // One unread message is enough to make the conversation unread.
        assert!(!items[0].seen);
        assert_eq!(items[0].count, 3);
        assert_eq!(items[0].row_ids.len(), 3);
        assert_eq!(s.unread_threads(FolderRole::Inbox, all).unwrap(), 3);
        assert_eq!(s.unread_count(FolderRole::Inbox, all).unwrap(), 3);
        s.set_seen(&items[1].row_ids, true).unwrap();
        assert_eq!(s.unread_threads(FolderRole::Inbox, all).unwrap(), 2);

        let page = s.threads(FolderRole::Inbox, all, 2, 1).unwrap();
        let dates: Vec<i64> = page.iter().map(|i| i.date).collect();
        assert_eq!(dates, vec![40, 30]);

        let in_trash = threads_of(&s, FolderRole::Trash);
        assert_eq!(in_trash.len(), 1);
        assert_eq!(in_trash[0].count, 4);
        assert_eq!(in_trash[0].thread_id, items[0].thread_id);

        // Hidden rows (being archived) leave the conversation.
        s.set_hidden(&items[0].row_ids, true).unwrap();
        assert_eq!(threads_of(&s, FolderRole::Inbox).len(), 2);
    }

    #[test]
    fn headers_are_stored_and_read_back() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        let mut m = reply(1, "<n>", Some("<p>"), &["<r>", "<p>"], 10);
        m.headers.to = vec!["a@x".into(), "b@y".into()];
        m.headers.delivered_to = Some("a@x".into());
        m.headers.list_unsubscribe = Some("<https://l.example/u>".into());
        m.headers.list_unsubscribe_post = Some("List-Unsubscribe=One-Click".into());
        m.headers.list_id = Some("<news.l.example>".into());
        m.headers.precedence = Some("bulk".into());
        m.headers.auto_submitted = Some("auto-generated".into());
        let expected = m.headers.clone();
        s.insert_messages(inbox, &[m]).unwrap();
        let row = inbox_rows(&s)[0];
        assert_eq!(s.headers(row).unwrap(), Some(expected));
        assert_eq!(s.headers(9999).unwrap(), None);
        assert!(s.missing_headers(inbox, 10).unwrap().is_empty());
        assert_eq!(s.thread_of(row).unwrap(), Some(row));
    }

    #[test]
    fn old_messages_get_their_headers_and_conversation_later() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x");
        let inbox = folder(&s, a, "INBOX", FolderRole::Inbox);
        s.insert_messages(
            inbox,
            &[
                msg(1, Some("<p>"), 10),
                msg(2, Some("<r>"), 20),
                msg(3, None, 30),
            ],
        )
        .unwrap();
        // As if stored before version 5: no extra headers yet.
        s.conn
            .execute_batch("UPDATE messages SET headers_v2 = 0")
            .unwrap();
        assert_eq!(s.missing_headers(inbox, 2).unwrap(), vec![3, 2]);
        assert_eq!(threads_of(&s, FolderRole::Inbox).len(), 3);

        // The server sends the headers of 2 (a reply to 1); 3 is gone.
        let mut fetched = reply(2, "<r>", Some("<p>"), &["<p>"], 20);
        fetched.headers.list_id = Some("<l>".into());
        s.complete_headers(inbox, &[3, 2], &[fetched, msg(7, None, 0)])
            .unwrap();

        assert_eq!(s.missing_headers(inbox, 10).unwrap(), vec![1]);
        let items = threads_of(&s, FolderRole::Inbox);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].count, 2);
        let row = items[1].row_ids[0];
        assert_eq!(
            s.headers(row).unwrap().unwrap().list_id.as_deref(),
            Some("<l>")
        );
        s.complete_headers(inbox, &[1], &[]).unwrap();
        assert!(s.missing_headers(inbox, 10).unwrap().is_empty());
    }

    #[test]
    fn version_4_messages_get_conversations_from_their_ids() {
        let path = std::env::temp_dir().join(format!("avero-v5-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(SCHEMA_V1).unwrap();
            old.execute_batch(
                "INSERT INTO accounts VALUES (1, 'A', 'a@x', 1, 2, 3, 'imap.x.com', 993, 'a@x');
                 INSERT INTO accounts VALUES (2, 'B', 'b@x', 1, 2, 3, 'imap.x.com', 993, 'b@x');
                 INSERT INTO folders VALUES (1, 1, 'INBOX', 'inbox', 7, 3);
                 INSERT INTO folders VALUES (2, 2, 'INBOX', 'inbox', 7, 3);
                 INSERT INTO messages VALUES (1, 1, 1, '<m>', 'Un', 'Marie', 'm@x', 5, 0);
                 INSERT INTO messages VALUES (2, 2, 1, '<m>', 'Un', 'Marie', 'm@x', 5, 0);
                 INSERT INTO messages VALUES (3, 1, 2, NULL, 'Deux', NULL, 'p@x', 6, 1);",
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();

        let version: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10);
        let ids = thread_ids(&s);
        assert_eq!(ids["Un"], 1);
        assert_eq!(ids["Deux"], 3);
        let items = threads_of(&s, FolderRole::Inbox);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].row_ids.len(), 2);
        assert_eq!(s.missing_headers(FolderId(1), 10).unwrap(), vec![2, 1]);
        assert_eq!(s.headers(1).unwrap(), Some(MessageHeaders::default()));
        // A reply arriving now joins the old message's conversation.
        s.insert_messages(FolderId(2), &[reply(2, "<r>", Some("<m>"), &[], 9)])
            .unwrap();
        assert_eq!(threads_of(&s, FolderRole::Inbox)[0].count, 2);
        // Deleting a message takes its links with it.
        s.delete_messages(&[1]).unwrap();
        assert_eq!(count(&s, "thread_links"), 3);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    /// Undoes versions 10, 9 and 8 (to test older migrations).
    const UNDO_V8: &str = "
        DROP TABLE space_accounts;
        DROP TABLE spaces;
        DROP TABLE draft_files;
        DROP TABLE drafts;
        DROP TABLE snoozes;
        DROP TABLE spam_learned;
        DROP TABLE spam_tokens;
        DROP TABLE rule_conditions;
        DROP TABLE rules;
        ALTER TABLE accounts DROP COLUMN spam_bootstrapped;
        ALTER TABLE senders DROP COLUMN not_spam;
        ALTER TABLE messages DROP COLUMN size;
    ";

    /// Takes a database back to version 4 (to test older migrations): undoes
    /// versions 7, 6 and 5 (after `UNDO_V8`).
    const UNDO_V5: &str = "
        DROP TABLE senders;
        DROP INDEX messages_sender;
        DROP INDEX messages_flagged;
        ALTER TABLE messages DROP COLUMN flagged;
        ALTER TABLE messages DROP COLUMN flag_pending;
        ALTER TABLE messages DROP COLUMN category;
        ALTER TABLE accounts DROP COLUMN signature;
        DROP INDEX messages_thread;
        DROP TABLE thread_links;
        ALTER TABLE messages DROP COLUMN in_reply_to;
        ALTER TABLE messages DROP COLUMN refs;
        ALTER TABLE messages DROP COLUMN to_addrs;
        ALTER TABLE messages DROP COLUMN delivered_to;
        ALTER TABLE messages DROP COLUMN list_unsubscribe;
        ALTER TABLE messages DROP COLUMN list_unsubscribe_post;
        ALTER TABLE messages DROP COLUMN list_id;
        ALTER TABLE messages DROP COLUMN precedence;
        ALTER TABLE messages DROP COLUMN auto_submitted;
        ALTER TABLE messages DROP COLUMN headers_v2;
        ALTER TABLE messages DROP COLUMN thread_id;
    ";

    /// Server flags with only `\Seen` (`(uid, seen)`).
    fn seen_flags(list: &[(u32, bool)]) -> Vec<ServerFlags> {
        list.iter()
            .map(|&(uid, seen)| ServerFlags {
                uid,
                seen,
                flagged: false,
            })
            .collect()
    }

    /// A message from `from` with some list headers.
    fn from(uid: u32, from: &str, date: i64) -> NewMessage {
        let mut m = msg(uid, Some(&format!("<{uid}@x>")), date);
        m.from_addr = from.to_string();
        m.subject = format!("{from} {uid}");
        m
    }

    fn subjects(items: &[UnifiedItem]) -> Vec<String> {
        items.iter().map(|i| i.subject.clone()).collect()
    }

    fn tab(store: &Store, category: Category) -> Vec<String> {
        subjects(
            &store
                .list(&Listing::Tab(category), AccountFilter::All, true, 200, 0)
                .unwrap(),
        )
    }

    #[test]
    fn new_messages_are_sorted_in_tabs() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let f = inbox(&s, a);
        let mut news = from(1, "news@shop.fr", 10);
        news.headers.list_unsubscribe = Some("<https://shop.fr/u>".into());
        let mut github = from(2, "notifications@github.com", 20);
        github.headers.list_id = Some("<repo.github.com>".into());
        let mut person = from(3, "marie@x.fr", 30);
        person.seen = true;
        s.insert_messages(f.id, &[news, github, person]).unwrap();

        assert_eq!(tab(&s, Category::Important), vec!["marie@x.fr 3"]);
        assert_eq!(tab(&s, Category::Newsletter), vec!["news@shop.fr 1"]);
        assert_eq!(
            tab(&s, Category::Notification),
            vec!["notifications@github.com 2"]
        );
        // The whole inbox still lists them all.
        assert_eq!(threads_of(&s, FolderRole::Inbox).len(), 3);
        let unread = |c| {
            s.list_unread(&Listing::Tab(c), AccountFilter::All, true)
                .unwrap()
        };
        assert_eq!(unread(Category::Important), 0);
        assert_eq!(unread(Category::Newsletter), 1);
        assert_eq!(unread(Category::Notification), 1);
        // Tabs follow the account filter.
        let b = acct(&s, "b@x.fr");
        let only_b = s
            .list(
                &Listing::Tab(Category::Newsletter),
                AccountFilter::Only(b),
                true,
                200,
                0,
            )
            .unwrap();
        assert!(only_b.is_empty());
    }

    #[test]
    fn reclassified_sender_is_remembered() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let f = inbox(&s, a);
        let mut first = from(1, "Club@Asso.fr", 10);
        first.headers.list_id = Some("<club.asso.fr>".into());
        s.insert_messages(f.id, &[first, from(2, "paul@x.fr", 20)])
            .unwrap();
        assert_eq!(tab(&s, Category::Newsletter), vec!["Club@Asso.fr 1"]);
        let paul_row = threads_of(&s, FolderRole::Inbox)[0].row_ids[0];

        // Moved to Important by hand, with Paul's message of the thread.
        s.set_sender_category(" club@asso.FR", Category::Important, &[paul_row])
            .unwrap();
        assert!(tab(&s, Category::Newsletter).is_empty());
        assert_eq!(
            s.sender_category("CLUB@asso.fr").unwrap(),
            Some(Category::Important)
        );
        assert_eq!(s.category(paul_row).unwrap(), Some(Category::Important));
        // The next message of that sender goes to Important too.
        let mut next = from(3, "club@asso.fr", 30);
        next.headers.list_id = Some("<club.asso.fr>".into());
        s.insert_messages(f.id, &[next]).unwrap();
        assert_eq!(tab(&s, Category::Important).len(), 3);
        // And to Notifications once chosen so.
        s.set_sender_category("club@asso.fr", Category::Notification, &[])
            .unwrap();
        assert_eq!(tab(&s, Category::Notification).len(), 2);
        assert_eq!(s.sender_category("paul@x.fr").unwrap(), None);
    }

    #[test]
    fn completed_headers_sort_old_messages_again() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[from(1, "news@shop.fr", 10)])
            .unwrap();
        assert_eq!(tab(&s, Category::Important).len(), 1);
        s.mark_headers_missing(f.id).unwrap();
        let mut again = from(1, "news@shop.fr", 10);
        again.headers.list_unsubscribe = Some("<mailto:u@shop.fr>".into());
        s.complete_headers(f.id, &[1], &[again]).unwrap();
        assert_eq!(tab(&s, Category::Newsletter).len(), 1);
    }

    #[test]
    fn unsubscribing_is_remembered_per_sender() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.unsubscribed_at("news@shop.fr").unwrap(), None);
        s.set_sender_category("news@shop.fr", Category::Newsletter, &[])
            .unwrap();
        s.mark_unsubscribed(" News@Shop.fr", 1234).unwrap();
        assert_eq!(s.unsubscribed_at("news@shop.fr").unwrap(), Some(1234));
        // The tab chosen for the sender stays.
        assert_eq!(
            s.sender_category("news@shop.fr").unwrap(),
            Some(Category::Newsletter)
        );
    }

    #[test]
    fn sender_view_lists_every_folder_and_account() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let b = acct(&s, "b@x.fr");
        let fa = inbox(&s, a);
        let archive = folder(&s, a, "Archive", FolderRole::Archive);
        let fb = inbox(&s, b);
        s.insert_messages(
            fa.id,
            &[from(1, "marie@x.fr", 10), from(2, "paul@x.fr", 20)],
        )
        .unwrap();
        s.insert_messages(archive, &[from(3, "Marie@X.fr", 5)])
            .unwrap();
        s.insert_messages(fb.id, &[from(4, "marie@x.fr ", 30)])
            .unwrap();

        let marie = Listing::Sender("MARIE@x.fr".into());
        let all = s.list(&marie, AccountFilter::All, false, 200, 0).unwrap();
        assert_eq!(
            subjects(&all),
            vec!["marie@x.fr  4", "marie@x.fr 1", "Marie@X.fr 3"]
        );
        let only_a = s
            .list(&marie, AccountFilter::Only(a), false, 200, 0)
            .unwrap();
        assert_eq!(only_a.len(), 2);
        assert_eq!(s.list_unread(&marie, AccountFilter::All, false).unwrap(), 3);
    }

    #[test]
    fn starred_messages_of_every_folder_are_listed() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let fa = inbox(&s, a);
        let archive = folder(&s, a, "Archive", FolderRole::Archive);
        let trash = folder(&s, a, "Trash", FolderRole::Trash);
        let mut starred = from(1, "marie@x.fr", 10);
        starred.flagged = true;
        s.insert_messages(fa.id, &[starred, from(2, "paul@x.fr", 20)])
            .unwrap();
        let mut archived = from(3, "luc@x.fr", 30);
        archived.flagged = true;
        s.insert_messages(archive, &[archived]).unwrap();
        let mut binned = from(4, "zoe@x.fr", 40);
        binned.flagged = true;
        s.insert_messages(trash, &[binned]).unwrap();

        let list = s
            .list(&Listing::Starred, AccountFilter::All, true, 200, 0)
            .unwrap();
        assert_eq!(subjects(&list), vec!["luc@x.fr 3", "marie@x.fr 1"]);
        assert!(list.iter().all(|i| i.flagged));
        assert!(!threads_of(&s, FolderRole::Inbox)[0].flagged);

        // Unstarred locally: gone from Favoris, and the server's flag
        // does not bring it back while the change is pending.
        let row = list[1].row_ids[0];
        s.set_flagged(&[row], false).unwrap();
        let flags = |flagged| {
            vec![ServerFlags {
                uid: 1,
                seen: false,
                flagged,
            }]
        };
        s.apply_flags(fa.id, 1, 1, &flags(true)).unwrap();
        let list = s
            .list(&Listing::Starred, AccountFilter::All, true, 200, 0)
            .unwrap();
        assert_eq!(subjects(&list), vec!["luc@x.fr 3"]);
        // Once confirmed, the server is right again (starred elsewhere).
        s.confirm_flagged(&[row]).unwrap();
        s.apply_flags(fa.id, 1, 1, &flags(true)).unwrap();
        assert_eq!(
            s.list_unread(&Listing::Starred, AccountFilter::All, true)
                .unwrap(),
            2
        );
        s.apply_flags(fa.id, 1, 1, &flags(false)).unwrap();
        assert_eq!(
            s.list(&Listing::Starred, AccountFilter::All, true, 200, 0)
                .unwrap()
                .len(),
            1
        );
        // A pending change is forgotten at the next start.
        s.set_flagged(&[row], true).unwrap();
        s.clear_pending().unwrap();
        s.apply_flags(fa.id, 1, 1, &flags(false)).unwrap();
        assert_eq!(
            s.list(&Listing::Starred, AccountFilter::All, true, 200, 0)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn unstarring_a_conversation_finds_its_other_starred_rows() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let fa = inbox(&s, a);
        let sent = folder(&s, a, "Sent", FolderRole::Sent);
        let mut first = reply(1, "<p>", None, &[], 10);
        first.flagged = true;
        s.insert_messages(fa.id, &[first]).unwrap();
        let mut answer = reply(2, "<r>", Some("<p>"), &["<p>"], 20);
        answer.flagged = true;
        s.insert_messages(sent, &[answer]).unwrap();
        s.insert_messages(fa.id, &[msg(3, Some("<other>"), 30)])
            .unwrap();

        let inbox_item = threads_of(&s, FolderRole::Inbox)
            .into_iter()
            .find(|i| i.subject == "subject 1")
            .unwrap();
        let others = s
            .starred_in_threads(&inbox_item.row_ids, AccountFilter::All)
            .unwrap();
        assert_eq!(others.len(), 1);
        assert_ne!(others[0], inbox_item.row_ids[0]);
        assert!(
            s.starred_in_threads(&inbox_item.row_ids, AccountFilter::Only(AccountId(99)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn version_6_messages_are_sorted_by_the_migration() {
        let path = std::env::temp_dir().join(format!("avero-v7-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let s = Store::open(&path).unwrap();
            let a = acct(&s, "a@x.fr");
            let f = inbox(&s, a);
            let mut news = from(1, "news@shop.fr", 10);
            news.headers.precedence = Some("bulk".into());
            s.insert_messages(f.id, &[news, from(2, "noreply@banque.fr", 20)])
                .unwrap();
            s.conn
                .execute_batch(&format!(
                    "{UNDO_V8}
                     DROP TABLE senders;
                     DROP INDEX messages_sender;
                     DROP INDEX messages_flagged;
                     ALTER TABLE messages DROP COLUMN flagged;
                     ALTER TABLE messages DROP COLUMN flag_pending;
                     ALTER TABLE messages DROP COLUMN category;
                     PRAGMA user_version = 6;"
                ))
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let version: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10);
        assert_eq!(tab(&s, Category::Newsletter), vec!["news@shop.fr 1"]);
        assert_eq!(tab(&s, Category::Notification), vec!["noreply@banque.fr 2"]);
        assert!(tab(&s, Category::Important).is_empty());
        drop(s);
        let _ = std::fs::remove_file(&path);
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
        assert_eq!(version, 10);
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
