//! What a search on the server needs: which of the messages it found are
//! already on this computer, and their rows.

use super::{Store, StoreError};
use crate::{AccountId, FolderId};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

impl Store {
    /// The UIDs of `uids` already stored in the folder.
    pub fn known_uids(&self, folder: FolderId, uids: &[u32]) -> Result<HashSet<u32>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
        let mut known = HashSet::new();
        for &uid in uids {
            if stmt
                .query_row(params![folder.0, uid], |_| Ok(()))
                .optional()?
                .is_some()
            {
                known.insert(uid);
            }
        }
        Ok(known)
    }

    /// The rows of the folder's messages with these UIDs (the unknown
    /// ones are left out), in the order of `uids`.
    pub fn rows_of_uids(&self, folder: FolderId, uids: &[u32]) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
        let mut rows = Vec::new();
        for &uid in uids {
            if let Some(row) = stmt
                .query_row(params![folder.0, uid], |row| row.get(0))
                .optional()?
            {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Whether a message with this Message-ID is stored in some folder of
    /// the account.
    pub fn has_message_id(&self, account: AccountId, message_id: &str) -> Result<bool, StoreError> {
        let found = self
            .conn
            .query_row(
                "SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE f.account_id = ?1 AND m.message_id = ?2 LIMIT 1",
                params![account.0, message_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, folder, inbox, msg};
    use crate::store::Store;
    use crate::{AccountFilter, FolderRole};

    #[test]
    fn known_uids_rows_and_message_ids() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let b = acct(&s, "b@x.fr");
        let f = inbox(&s, a);
        s.insert_messages(f.id, &[msg(3, Some("<m3@x>"), 10), msg(7, None, 20)])
            .unwrap();
        let known = s.known_uids(f.id, &[1, 3, 7, 9]).unwrap();
        assert_eq!(known.len(), 2);
        assert!(known.contains(&3) && known.contains(&7));
        let rows = s.rows_of_uids(f.id, &[7, 5, 3]).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(s.has_message_id(a, "<m3@x>").unwrap());
        assert!(!s.has_message_id(b, "<m3@x>").unwrap());
        assert!(!s.has_message_id(a, "<other@x>").unwrap());
    }

    #[test]
    fn search_also_returns_the_rows_found_elsewhere() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let f = inbox(&s, a);
        let archive = folder(&s, a, "Archives", FolderRole::Archive);
        let mut local = msg(1, Some("<1@x>"), 10);
        local.subject = "Facture d'octobre".into();
        s.insert_messages(f.id, &[local]).unwrap();
        // Found by the server in its body, which is not on this computer.
        s.insert_messages(archive, &[msg(2, Some("<2@x>"), 5)])
            .unwrap();
        let found = s.rows_of_uids(archive, &[2]).unwrap();
        let all = AccountFilter::All;
        assert_eq!(s.search("facture", None, all, 10).unwrap().len(), 1);
        let both = s.search_with("facture", None, all, &found, 10).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].subject, "Facture d'octobre");
        // Other accounts' rows stay out when the list is filtered.
        let b = acct(&s, "b@x.fr");
        let only_b = AccountFilter::Only(b);
        assert!(
            s.search_with("facture", None, only_b, &found, 10)
                .unwrap()
                .is_empty()
        );
    }
}
