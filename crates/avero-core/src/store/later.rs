//! "Plus tard": conversations put away until a given time. Only this
//! computer knows (nothing moves on the server).

use super::{MESSAGE_KEY, Store, StoreError, write_tx};
use crate::Woken;
use rusqlite::params;
use std::collections::BTreeSet;

/// The keys (see `MESSAGE_KEY`) of every message of the conversations of
/// `?1` (one row id).
const THREAD_KEYS: &str = "SELECT DISTINCT COALESCE(m.message_id, 'row:' || m.id) FROM messages m
     WHERE m.thread_id = (SELECT thread_id FROM messages WHERE id = ?1)";

/// `THREAD_KEYS` with the column named `key`.
const THREAD_KEYS_AS: &str = "SELECT DISTINCT COALESCE(m.message_id, 'row:' || m.id) AS key
     FROM messages m
     WHERE m.thread_id = (SELECT thread_id FROM messages WHERE id = ?1)";

impl Store {
    /// Snoozes the conversations of `rows` until `until` (Unix time):
    /// every message they hold leaves the lists but "Plus tard". Snoozing
    /// again only moves the time.
    pub fn snooze(&self, rows: &[i64], until: i64) -> Result<(), StoreError> {
        let tx = write_tx(&self.conn)?;
        {
            let mut stmt = tx.prepare(&format!(
                "INSERT INTO snoozes (key, until, woke_at)
                 SELECT key, ?2, NULL FROM ({THREAD_KEYS_AS}) WHERE true
                 ON CONFLICT(key) DO UPDATE SET until = excluded.until, woke_at = NULL"
            ))?;
            for &row in rows {
                stmt.execute(params![row, until])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Takes the conversations of `rows` out of "Plus tard" now ("Annuler
    /// le rappel"): they go back to their place in the lists.
    pub fn unsnooze(&self, rows: &[i64]) -> Result<(), StoreError> {
        let tx = write_tx(&self.conn)?;
        {
            let mut stmt =
                tx.prepare(&format!("DELETE FROM snoozes WHERE key IN ({THREAD_KEYS})"))?;
            for &row in rows {
                stmt.execute(params![row])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// When the next snoozed conversation wakes up, if any.
    pub fn next_wake(&self) -> Result<Option<i64>, StoreError> {
        let next = self.conn.query_row(
            "SELECT MIN(until) FROM snoozes WHERE woke_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(next)
    }

    /// Wakes the conversations whose time came (`until <= now`): they are
    /// back in their lists, sorted as if they arrived at `now`. Returns
    /// one `Woken` per conversation still stored (the caller marks its
    /// newest message unread and tells the user), oldest due first.
    pub fn wake_snoozed(&self, now: i64) -> Result<Vec<Woken>, StoreError> {
        let tx = write_tx(&self.conn)?;
        let threads: Vec<i64> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT m.thread_id, MIN(s.until) FROM snoozes s
                 JOIN messages m ON {MESSAGE_KEY} = s.key
                 WHERE s.woke_at IS NULL AND s.until <= ?1
                 GROUP BY m.thread_id
                 ORDER BY MIN(s.until), m.thread_id"
            ))?;
            let rows = stmt.query_map(params![now], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.execute(
            "UPDATE snoozes SET woke_at = ?1 WHERE woke_at IS NULL AND until <= ?1",
            params![now],
        )?;
        let mut woken = Vec::new();
        {
            // The newest message of the conversation that the lists show.
            let mut newest = tx.prepare(&format!(
                "SELECT {MESSAGE_KEY}, m.from_name, m.from_addr, m.subject
                 FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE m.thread_id = ?1 AND m.hidden = 0
                   AND f.role NOT IN ('trash', 'junk')
                 ORDER BY m.date DESC, m.id DESC LIMIT 1"
            ))?;
            let mut copies = tx.prepare(&format!(
                "SELECT m.id FROM messages m WHERE {MESSAGE_KEY} = ?1 ORDER BY m.id"
            ))?;
            let mut seen = BTreeSet::new();
            for thread in threads {
                if !seen.insert(thread) {
                    continue;
                }
                let found = newest.query_row(params![thread], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                });
                let (key, from_name, from_addr, subject) = match found {
                    Ok(found) => found,
                    Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                    Err(e) => return Err(e.into()),
                };
                let rows = copies.query_map(params![key], |row| row.get(0))?;
                woken.push(Woken {
                    rows: rows.collect::<Result<_, _>>()?,
                    from_name,
                    from_addr,
                    subject,
                });
            }
        }
        tx.commit()?;
        Ok(woken)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, inbox, msg};
    use crate::{AccountFilter, FolderRole, Listing, MessageHeaders, UnifiedItem};

    use super::*;

    fn list(store: &Store, listing: &Listing) -> Vec<UnifiedItem> {
        store
            .list(listing, AccountFilter::All, true, 100, 0)
            .unwrap()
    }

    fn subjects(items: &[UnifiedItem]) -> Vec<String> {
        items.iter().map(|i| i.subject.clone()).collect()
    }

    /// An inbox with three messages; 2 answers 1 (one conversation).
    fn store_with_thread() -> (Store, Vec<i64>) {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let folder = inbox(&store, account);
        let mut reply = msg(2, Some("<2@x>"), 200);
        reply.headers = MessageHeaders {
            in_reply_to: Some("<1@x>".into()),
            references: vec!["<1@x>".into()],
            ..Default::default()
        };
        store
            .insert_messages(
                folder.id,
                &[
                    msg(1, Some("<1@x>"), 100),
                    reply,
                    msg(3, Some("<3@x>"), 300),
                ],
            )
            .unwrap();
        let rows = list(&store, &Listing::Folder(FolderRole::Inbox))
            .into_iter()
            .flat_map(|item| item.row_ids)
            .collect();
        (store, rows)
    }

    #[test]
    fn snoozed_conversation_leaves_every_list_but_later() {
        let (store, _) = store_with_thread();
        let inbox_list = list(&store, &Listing::Folder(FolderRole::Inbox));
        assert_eq!(subjects(&inbox_list), vec!["subject 3", "subject 2"]);
        // Snoozing the conversation by its oldest message takes it all.
        let first = inbox_list[1].row_ids.clone();
        store.snooze(&first, 5_000).unwrap();
        let inbox_list = list(&store, &Listing::Folder(FolderRole::Inbox));
        assert_eq!(subjects(&inbox_list), vec!["subject 3"]);
        assert_eq!(inbox_list[0].snoozed_until, None);
        assert!(
            store
                .search("subject", None, AccountFilter::All, 10)
                .unwrap()
                .iter()
                .all(|item| item.subject == "subject 3")
        );
        let later = list(&store, &Listing::Snoozed);
        assert_eq!(subjects(&later), vec!["subject 2"]);
        assert_eq!(later[0].count, 2);
        assert_eq!(later[0].snoozed_until, Some(5_000));
        // The unread count of the inbox leaves it out too.
        assert_eq!(
            store
                .unread_threads(FolderRole::Inbox, AccountFilter::All)
                .unwrap(),
            1
        );
        assert_eq!(store.next_wake().unwrap(), Some(5_000));
    }

    #[test]
    fn a_new_message_never_inherits_a_deleted_one_s_snooze() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let folder = inbox(&store, account);
        // No Message-ID: known by its row.
        store
            .insert_messages(folder.id, &[msg(1, None, 100)])
            .unwrap();
        let rows: Vec<i64> = list(&store, &Listing::Folder(FolderRole::Inbox))
            .into_iter()
            .flat_map(|item| item.row_ids)
            .collect();
        store.snooze(&rows, 5_000).unwrap();
        // Deleted elsewhere; the next message gets the same row id.
        store.delete_messages(&rows).unwrap();
        store
            .insert_messages(folder.id, &[msg(2, None, 200)])
            .unwrap();
        let inbox_list = list(&store, &Listing::Folder(FolderRole::Inbox));
        assert_eq!(inbox_list[0].row_ids, rows);
        assert_eq!(subjects(&inbox_list), vec!["subject 2"]);
        assert!(list(&store, &Listing::Snoozed).is_empty());
        assert_eq!(store.next_wake().unwrap(), None);
    }

    #[test]
    fn snoozes_of_rows_deleted_before_version_11_go() {
        let path = std::env::temp_dir().join(format!("avero-rowkeys-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            let account = acct(&store, "a@x.fr");
            let folder = inbox(&store, account);
            store
                .insert_messages(folder.id, &[msg(1, None, 100)])
                .unwrap();
            store
                .conn
                .execute_batch(
                    "INSERT INTO snoozes (key, until) VALUES ('row:1', 9), ('row:77', 9);
                     INSERT INTO spam_learned (key, spam) VALUES ('row:77', 1);
                     PRAGMA user_version = 10;",
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let keys: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT key FROM snoozes UNION ALL SELECT key FROM spam_learned")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        // Row 1 is still there; row 77 is gone (still counted as learned).
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], "row:1");
        assert!(keys[1].starts_with("gone:"), "{keys:?}");
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn later_lists_the_next_to_wake_first() {
        let (store, rows) = store_with_thread();
        store.snooze(&rows[..1], 9_000).unwrap();
        store.snooze(&rows[1..2], 7_000).unwrap();
        let later = list(&store, &Listing::Snoozed);
        assert_eq!(subjects(&later), vec!["subject 2", "subject 3"]);
        // Snoozing again moves the time.
        store.snooze(&rows[..1], 6_000).unwrap();
        let later = list(&store, &Listing::Snoozed);
        assert_eq!(subjects(&later), vec!["subject 3", "subject 2"]);
    }

    #[test]
    fn waking_brings_the_conversation_back_on_top() {
        let (store, rows) = store_with_thread();
        // rows: [3, 2, 1] by date; snooze the older conversation.
        store.snooze(&rows[1..2], 5_000).unwrap();
        assert!(store.wake_snoozed(4_999).unwrap().is_empty());
        let woken = store.wake_snoozed(5_000).unwrap();
        assert_eq!(woken.len(), 1);
        // Its newest message (subject 2) is the one to mark unread.
        assert_eq!(woken[0].subject, "subject 2");
        assert_eq!(woken[0].rows, vec![rows[1]]);
        // Back in the inbox, before subject 3 (date 300 < woke at 5000).
        let inbox_list = list(&store, &Listing::Folder(FolderRole::Inbox));
        assert_eq!(subjects(&inbox_list), vec!["subject 2", "subject 3"]);
        assert!(list(&store, &Listing::Snoozed).is_empty());
        assert_eq!(store.next_wake().unwrap(), None);
        // Woken once only.
        assert!(store.wake_snoozed(6_000).unwrap().is_empty());
    }

    #[test]
    fn cancelled_snooze_puts_it_back_in_place() {
        let (store, rows) = store_with_thread();
        store.snooze(&rows[2..3], 5_000).unwrap();
        store.unsnooze(&rows[1..2]).unwrap();
        let inbox_list = list(&store, &Listing::Folder(FolderRole::Inbox));
        assert_eq!(subjects(&inbox_list), vec!["subject 3", "subject 2"]);
        assert!(store.wake_snoozed(10_000).unwrap().is_empty());
    }
}
