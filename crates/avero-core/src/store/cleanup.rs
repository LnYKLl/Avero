//! "Grand nettoyage": who fills the mailbox, and message sizes.

use super::{Store, StoreError, filter_account};
use crate::category::sender_key;
use crate::{AccountFilter, AccountId, FolderId, SenderStats};
use rusqlite::{OptionalExtension, params};

/// The messages "Grand nettoyage" counts, on `m` (a message) and `f` (its
/// folder): not hidden, outside the trash, spam, sent and drafts folders,
/// of the accounts of `?1` (NULL: all), not from the user's own
/// addresses.
const CLEANUP_SCOPE: &str = "m.hidden = 0
     AND f.role NOT IN ('trash', 'junk', 'sent', 'drafts')
     AND (?1 IS NULL OR f.account_id IN (SELECT value FROM json_each(?1)))
     AND lower(trim(m.from_addr)) NOT IN (SELECT lower(trim(email)) FROM accounts)";

impl Store {
    /// The senders with the most messages first (then the biggest), one
    /// page of `limit` from `offset`. Copies of a message on two
    /// accounts count twice: both take room.
    pub fn sender_stats(
        &self,
        filter: AccountFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SenderStats>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT lower(trim(m.from_addr)) AS addr,
                    COUNT(*) AS total,
                    SUM(m.seen = 0),
                    COALESCE(SUM(m.size), 0) AS bytes,
                    MAX(m.list_unsubscribe IS NOT NULL),
                    (SELECT n.from_name FROM messages n
                     WHERE lower(trim(n.from_addr)) = lower(trim(m.from_addr))
                       AND n.from_name IS NOT NULL AND trim(n.from_name) != ''
                     ORDER BY n.date DESC LIMIT 1)
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE {CLEANUP_SCOPE} AND trim(m.from_addr) != ''
             GROUP BY addr
             ORDER BY total DESC, bytes DESC, addr
             LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt.query_map(
            params![
                filter_account(&self.conn, filter)?,
                limit as i64,
                offset as i64
            ],
            |row| {
                let addr: String = row.get(0)?;
                let name: Option<String> = row.get(5)?;
                Ok(SenderStats {
                    name: name.unwrap_or_else(|| addr.clone()),
                    addr,
                    count: row.get::<_, i64>(1)? as usize,
                    unread: row.get::<_, i64>(2)? as usize,
                    size: row.get::<_, i64>(3)?.max(0) as u64,
                    can_unsubscribe: row.get(4)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The rows "Grand nettoyage" counts for one sender, newest first
    /// (what "Tout mettre à la corbeille" moves).
    pub fn sender_rows(&self, addr: &str, filter: AccountFilter) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE {CLEANUP_SCOPE} AND lower(trim(m.from_addr)) = ?2
             ORDER BY m.date DESC, m.id DESC"
        ))?;
        let rows = stmt.query_map(
            params![filter_account(&self.conn, filter)?, sender_key(addr)],
            |row| row.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The most recent message of a sender that offers to unsubscribe
    /// (List-Unsubscribe), and the account that received it.
    pub fn latest_unsubscribable(
        &self,
        addr: &str,
        filter: AccountFilter,
    ) -> Result<Option<(i64, AccountId)>, StoreError> {
        let found = self
            .conn
            .query_row(
                "SELECT m.id, f.account_id FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE lower(trim(m.from_addr)) = ?1 AND m.list_unsubscribe IS NOT NULL
                   AND (?2 IS NULL OR f.account_id IN (SELECT value FROM json_each(?2)))
                 ORDER BY m.date DESC, m.id DESC LIMIT 1",
                params![sender_key(addr), filter_account(&self.conn, filter)?],
                |row| Ok((row.get(0)?, AccountId(row.get(1)?))),
            )
            .optional()?;
        Ok(found)
    }

    /// UIDs of the folder's messages whose size is not known yet (stored
    /// before version 8), newest first, at most `limit`.
    pub fn missing_sizes(&self, folder: FolderId, limit: usize) -> Result<Vec<u32>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM messages WHERE folder_id = ?1 AND size IS NULL
             ORDER BY uid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![folder.0, limit as i64], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Saves the sizes fetched for the UIDs `asked`; those the server
    /// did not answer for get 0 (gone: no use asking again).
    pub fn set_sizes(
        &self,
        folder: FolderId,
        asked: &[u32],
        sizes: &[(u32, u32)],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt =
                tx.prepare("UPDATE messages SET size = ?1 WHERE folder_id = ?2 AND uid = ?3")?;
            for &uid in asked {
                let size = sizes
                    .iter()
                    .find(|(known, _)| *known == uid)
                    .map_or(0, |(_, size)| *size);
                stmt.execute(params![size, folder.0, uid])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, folder, inbox, msg};
    use super::*;
    use crate::{FolderRole, NewMessage};

    fn from(uid: u32, addr: &str, name: Option<&str>, size: u32, seen: bool) -> NewMessage {
        let mut m = msg(uid, Some(&format!("<c{uid}@test>")), 1000 + uid as i64);
        m.from_addr = addr.to_string();
        m.from_name = name.map(str::to_string);
        m.size = Some(size);
        m.seen = seen;
        m
    }

    #[test]
    fn senders_by_volume_with_unread_share_and_size() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let b = acct(&s, "pro@x.fr");
        let box_a = inbox(&s, a).id;
        let box_b = inbox(&s, b).id;
        let archive = folder(&s, a, "Archive", FolderRole::Archive);
        let trash = folder(&s, a, "Trash", FolderRole::Trash);
        let sent = folder(&s, a, "Sent", FolderRole::Sent);
        let mut news = from(1, "News@Shop.fr", Some("Shop (old)"), 1000, false);
        news.headers.list_unsubscribe = Some("<https://shop.fr/u>".into());
        s.insert_messages(
            box_a,
            &[
                news,
                from(2, "news@shop.fr ", Some("Shop"), 2000, false),
                from(3, "lea@ami.fr", None, 50, true),
            ],
        )
        .unwrap();
        s.insert_messages(archive, &[from(4, "news@shop.fr", None, 500, true)])
            .unwrap();
        s.insert_messages(box_b, &[from(5, "lea@ami.fr", Some("Léa"), 70, false)])
            .unwrap();
        // Not counted: the trash, the sent folder, the user's own mail.
        s.insert_messages(trash, &[from(6, "news@shop.fr", None, 9, false)])
            .unwrap();
        s.insert_messages(sent, &[from(7, "moi@x.fr", None, 9, true)])
            .unwrap();
        s.insert_messages(box_b, &[from(8, "MOI@x.fr", None, 9, true)])
            .unwrap();

        let all = s.sender_stats(AccountFilter::All, 10, 0).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");
        let shop = &all[0];
        assert_eq!(shop.addr, "news@shop.fr");
        assert_eq!(shop.name, "Shop", "the newest name");
        assert_eq!((shop.count, shop.unread, shop.size), (3, 2, 3500));
        assert!(shop.can_unsubscribe);
        let lea = &all[1];
        assert_eq!((lea.count, lea.unread, lea.size), (2, 1, 120));
        assert_eq!(lea.name, "Léa");
        assert!(!lea.can_unsubscribe);

        // Paging and the account filter.
        let second = s.sender_stats(AccountFilter::All, 1, 1).unwrap();
        assert_eq!(second[0].addr, "lea@ami.fr");
        let only_b = s.sender_stats(AccountFilter::Only(b), 10, 0).unwrap();
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].count, 1);

        // What "Tout mettre à la corbeille" takes, and how to unsubscribe.
        assert_eq!(
            s.sender_rows("NEWS@shop.fr", AccountFilter::All)
                .unwrap()
                .len(),
            3
        );
        let (row, account) = s
            .latest_unsubscribable("news@shop.fr", AccountFilter::All)
            .unwrap()
            .unwrap();
        assert_eq!(account, a);
        assert_eq!(
            s.headers(row).unwrap().unwrap().list_unsubscribe.as_deref(),
            Some("<https://shop.fr/u>")
        );
        assert_eq!(
            s.latest_unsubscribable("lea@ami.fr", AccountFilter::All)
                .unwrap(),
            None
        );
    }

    #[test]
    fn hidden_rows_leave_the_counts() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let box_a = inbox(&s, a).id;
        s.insert_messages(box_a, &[from(1, "a@b.fr", None, 10, false)])
            .unwrap();
        let rows = s.sender_rows("a@b.fr", AccountFilter::All).unwrap();
        s.set_hidden(&rows, true).unwrap();
        assert!(
            s.sender_stats(AccountFilter::All, 10, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unknown_sizes_are_completed() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let box_a = inbox(&s, a).id;
        let mut old = from(1, "a@b.fr", None, 0, false);
        old.size = None;
        let mut gone = from(2, "a@b.fr", None, 0, false);
        gone.size = None;
        s.insert_messages(box_a, &[old, gone, from(3, "a@b.fr", None, 5, true)])
            .unwrap();
        assert_eq!(s.missing_sizes(box_a, 10).unwrap(), [2, 1]);
        s.set_sizes(box_a, &[2, 1], &[(1, 4096)]).unwrap();
        assert!(s.missing_sizes(box_a, 10).unwrap().is_empty());
        let stats = s.sender_stats(AccountFilter::All, 10, 0).unwrap();
        assert_eq!(stats[0].size, 4096 + 5);
    }
}
