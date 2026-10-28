//! Spaces: named groups of accounts ("Perso", "Pro") that limit the list,
//! the counters and the search to their accounts.

use super::{Store, StoreError};
use crate::{AccountFilter, AccountId, Space, SpaceId};
use rusqlite::{Connection, OptionalExtension, params};

impl Store {
    /// Every space, in the order the user chose.
    pub fn spaces(&self) -> Result<Vec<Space>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, icon, color FROM spaces ORDER BY position, id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Space {
                id: SpaceId(row.get(0)?),
                name: row.get(1)?,
                icon: row.get(2)?,
                color: row.get::<_, i64>(3)?.max(0) as usize,
                accounts: Vec::new(),
            })
        })?;
        let mut spaces: Vec<Space> = rows.collect::<Result<_, _>>()?;
        let mut members = self.conn.prepare(
            "SELECT account_id FROM space_accounts WHERE space_id = ?1 ORDER BY account_id",
        )?;
        for space in &mut spaces {
            let rows = members.query_map(params![space.id.0], |row| row.get(0))?;
            space.accounts = rows.map(|id| id.map(AccountId)).collect::<Result<_, _>>()?;
        }
        Ok(spaces)
    }

    /// Creates a space after the others, with these accounts.
    pub fn add_space(
        &self,
        name: &str,
        icon: &str,
        color: usize,
        accounts: &[AccountId],
    ) -> Result<SpaceId, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO spaces (name, icon, color, position)
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(position), 0) + 1 FROM spaces))",
            params![name, icon, color as i64],
        )?;
        let id = SpaceId(tx.last_insert_rowid());
        set_members(&tx, id, accounts)?;
        tx.commit()?;
        Ok(id)
    }

    /// Changes the name, icon and colour of a space.
    pub fn update_space(
        &self,
        id: SpaceId,
        name: &str,
        icon: &str,
        color: usize,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE spaces SET name = ?1, icon = ?2, color = ?3 WHERE id = ?4",
            params![name, icon, color as i64, id.0],
        )?;
        Ok(())
    }

    /// Puts an account in a space, or takes it out.
    pub fn set_space_account(
        &self,
        id: SpaceId,
        account: AccountId,
        member: bool,
    ) -> Result<(), StoreError> {
        if member {
            self.conn.execute(
                "INSERT OR IGNORE INTO space_accounts (space_id, account_id) VALUES (?1, ?2)",
                params![id.0, account.0],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM space_accounts WHERE space_id = ?1 AND account_id = ?2",
                params![id.0, account.0],
            )?;
        }
        Ok(())
    }

    /// Replaces the accounts of a space.
    pub fn set_space_accounts(
        &self,
        id: SpaceId,
        accounts: &[AccountId],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        set_members(&tx, id, accounts)?;
        tx.commit()?;
        Ok(())
    }

    /// Moves a space one place up (`-1`) or down (`1`) in the order.
    pub fn move_space(&self, id: SpaceId, step: i32) -> Result<(), StoreError> {
        let order: Vec<i64> = self.spaces()?.iter().map(|s| s.id.0).collect();
        let Some(from) = order.iter().position(|&s| s == id.0) else {
            return Ok(());
        };
        let to = from as i64 + i64::from(step);
        if to < 0 || to as usize >= order.len() {
            return Ok(());
        }
        let mut order = order;
        order.swap(from, to as usize);
        let tx = self.conn.unchecked_transaction()?;
        for (position, space) in order.iter().enumerate() {
            tx.execute(
                "UPDATE spaces SET position = ?1 WHERE id = ?2",
                params![position as i64 + 1, space],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes a space (its accounts stay).
    pub fn delete_space(&self, id: SpaceId) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM spaces WHERE id = ?1", params![id.0])?;
        Ok(())
    }

    /// Whether this space exists.
    pub fn has_space(&self, id: SpaceId) -> Result<bool, StoreError> {
        let found = self
            .conn
            .query_row("SELECT 1 FROM spaces WHERE id = ?1", params![id.0], |_| {
                Ok(())
            })
            .optional()?;
        Ok(found.is_some())
    }
}

/// Replaces the accounts of a space, inside the caller's transaction.
fn set_members(conn: &Connection, id: SpaceId, accounts: &[AccountId]) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM space_accounts WHERE space_id = ?1",
        params![id.0],
    )?;
    for account in accounts {
        conn.execute(
            "INSERT OR IGNORE INTO space_accounts (space_id, account_id) VALUES (?1, ?2)",
            params![id.0, account.0],
        )?;
    }
    Ok(())
}

/// The accounts a filter allows, as a JSON array for the
/// `(?n IS NULL OR f.account_id IN (SELECT value FROM json_each(?n)))`
/// condition of the queries; `None` for all accounts.
pub(super) fn filter_account(
    conn: &Connection,
    filter: AccountFilter,
) -> Result<Option<String>, StoreError> {
    let ids: Vec<i64> = match filter {
        AccountFilter::All => return Ok(None),
        AccountFilter::Only(id) => vec![id.0],
        AccountFilter::Space(space) => {
            let mut stmt = conn.prepare_cached(
                "SELECT account_id FROM space_accounts WHERE space_id = ?1 ORDER BY account_id",
            )?;
            let rows = stmt.query_map(params![space.0], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        }
    };
    let list: Vec<String> = ids.iter().map(i64::to_string).collect();
    Ok(Some(format!("[{}]", list.join(","))))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, folder, inbox, msg};
    use crate::store::Store;
    use crate::{AccountFilter, Category, FolderRole, Listing};

    /// Two accounts with two inbox messages each (`a1`, `a2`, `b1`,
    /// `b2`), the second one unread.
    fn two_accounts() -> (Store, crate::AccountId, crate::AccountId) {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "a@x.fr");
        let b = acct(&s, "b@x.fr");
        for (account, tag) in [(a, "a"), (b, "b")] {
            let f = inbox(&s, account);
            let mut first = msg(1, Some(&format!("<{tag}1@x>")), 10);
            first.subject = format!("{tag}1 facture");
            first.seen = true;
            let mut second = msg(2, Some(&format!("<{tag}2@x>")), 20);
            second.subject = format!("{tag}2 facture");
            s.insert_messages(f.id, &[first, second]).unwrap();
        }
        (s, a, b)
    }

    fn subjects(items: &[crate::UnifiedItem]) -> Vec<String> {
        let mut list: Vec<String> = items.iter().map(|i| i.subject.clone()).collect();
        list.sort();
        list
    }

    #[test]
    fn spaces_are_stored_in_order_with_their_accounts() {
        let (s, a, b) = two_accounts();
        let perso = s.add_space("Perso", "house", 1, &[a]).unwrap();
        let pro = s.add_space("Pro", "briefcase", 4, &[a, b]).unwrap();
        let spaces = s.spaces().unwrap();
        assert_eq!(spaces.len(), 2);
        assert_eq!(spaces[0].name, "Perso");
        assert_eq!(spaces[0].accounts, vec![a]);
        assert_eq!(spaces[1].icon, "briefcase");
        assert_eq!(spaces[1].color, 4);
        // `a` is in both spaces.
        assert_eq!(spaces[1].accounts, vec![a, b]);

        s.update_space(perso, "Maison", "heart", 2).unwrap();
        s.set_space_account(perso, b, true).unwrap();
        s.set_space_account(pro, a, false).unwrap();
        s.move_space(pro, -1).unwrap();
        let spaces = s.spaces().unwrap();
        assert_eq!(spaces[0].id, pro);
        assert_eq!(spaces[0].accounts, vec![b]);
        assert_eq!(spaces[1].name, "Maison");
        assert_eq!(spaces[1].accounts, vec![a, b]);
        // Out of range: nothing moves.
        s.move_space(pro, -1).unwrap();
        assert_eq!(s.spaces().unwrap()[0].id, pro);

        s.delete_space(pro).unwrap();
        assert!(!s.has_space(pro).unwrap());
        assert_eq!(s.spaces().unwrap().len(), 1);
        assert_eq!(s.accounts().unwrap().len(), 2);
    }

    #[test]
    fn a_space_filters_lists_counts_and_search() {
        let (s, a, b) = two_accounts();
        let perso = s.add_space("Perso", "house", 0, &[a]).unwrap();
        let both = s.add_space("Tout", "users", 1, &[a, b]).unwrap();
        let empty = s.add_space("Vide", "star", 2, &[]).unwrap();
        let inbox = Listing::Folder(FolderRole::Inbox);

        let only_a = s
            .list(&inbox, AccountFilter::Space(perso), false, 50, 0)
            .unwrap();
        assert_eq!(subjects(&only_a), vec!["a1 facture", "a2 facture"]);
        let all = s
            .list(&inbox, AccountFilter::Space(both), true, 50, 0)
            .unwrap();
        assert_eq!(all.len(), 4);
        assert!(
            s.list(&inbox, AccountFilter::Space(empty), false, 50, 0)
                .unwrap()
                .is_empty()
        );

        // Counters.
        let unread = |filter| s.unread_count(FolderRole::Inbox, filter).unwrap();
        assert_eq!(unread(AccountFilter::Space(perso)), 1);
        assert_eq!(unread(AccountFilter::Space(both)), 2);
        assert_eq!(unread(AccountFilter::Space(empty)), 0);
        let tab = s
            .list_unread(
                &Listing::Tab(Category::Important),
                AccountFilter::Space(perso),
                true,
            )
            .unwrap();
        assert_eq!(tab, 1);

        // Search.
        let found = s
            .search("facture", None, AccountFilter::Space(perso), 50)
            .unwrap();
        assert_eq!(subjects(&found), vec!["a1 facture", "a2 facture"]);
        let found = s
            .search("facture", None, AccountFilter::Space(both), 50)
            .unwrap();
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn deleting_an_account_takes_it_out_of_its_spaces() {
        let (s, a, b) = two_accounts();
        let sent = folder(&s, b, "Sent", FolderRole::Sent);
        s.insert_messages(sent, &[msg(5, Some("<s@x>"), 30)])
            .unwrap();
        let perso = s.add_space("Perso", "house", 0, &[a, b]).unwrap();
        let pro = s.add_space("Pro", "briefcase", 1, &[b]).unwrap();
        s.delete_account(b).unwrap();
        let spaces = s.spaces().unwrap();
        assert_eq!(spaces[0].id, perso);
        assert_eq!(spaces[0].accounts, vec![a]);
        // The space stays, empty.
        assert_eq!(spaces[1].id, pro);
        assert!(spaces[1].accounts.is_empty());
        assert_eq!(
            s.unread_count(FolderRole::Inbox, AccountFilter::Space(pro))
                .unwrap(),
            0
        );
    }
}
