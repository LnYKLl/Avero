//! Drafts kept on this computer (saved every few seconds while being
//! written) and the mail scheduled to be sent later ("Programmés"), which
//! are drafts with a sending time. See `schema_v9.sql`.

use super::{Store, StoreError, write_tx};
use crate::{AccountId, DraftCopy, DraftFields, DraftFile, DraftRecord, FolderId};
use rusqlite::{OptionalExtension, params};

/// Columns read by `record_from_row`, in order.
const DRAFT_COLUMNS: &str = "id, account_id, to_addrs, cc, bcc, subject, body, in_reply_to, refs,
     message_id, open, server_dirty, server_account, server_folder, server_uid,
     send_at, sent_at, error, updated_at";

impl Store {
    /// Saves what a composer holds: a new draft when `id` is `None`
    /// (returns its id), else the draft `id` updated. The draft is open
    /// and differs from its server copy. Its files are replaced only when
    /// `files` is given (they rarely change, and can weigh 25 MB).
    pub fn save_draft(
        &self,
        id: Option<i64>,
        fields: &DraftFields,
        files: Option<&[DraftFile]>,
        now: i64,
    ) -> Result<i64, StoreError> {
        let tx = write_tx(&self.conn)?;
        let f = fields;
        let updated = match id {
            Some(id) => tx.execute(
                "UPDATE drafts SET account_id = ?1, to_addrs = ?2, cc = ?3, bcc = ?4,
                     subject = ?5, body = ?6, in_reply_to = ?7, refs = ?8,
                     message_id = ?9, updated_at = ?10, open = 1, server_dirty = 1
                 WHERE id = ?11",
                params![
                    f.account.0,
                    f.to,
                    f.cc,
                    f.bcc,
                    f.subject,
                    f.body,
                    f.in_reply_to,
                    f.references,
                    f.message_id,
                    now,
                    id
                ],
            )?,
            None => 0,
        };
        // A draft deleted meanwhile (sent from elsewhere) is saved anew.
        let id = match id {
            Some(id) if updated > 0 => id,
            _ => {
                tx.execute(
                    "INSERT INTO drafts (account_id, to_addrs, cc, bcc, subject, body,
                                         in_reply_to, refs, message_id, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        f.account.0,
                        f.to,
                        f.cc,
                        f.bcc,
                        f.subject,
                        f.body,
                        f.in_reply_to,
                        f.references,
                        f.message_id,
                        now
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        if let Some(files) = files {
            tx.execute("DELETE FROM draft_files WHERE draft_id = ?1", params![id])?;
            let mut stmt = tx.prepare(
                "INSERT INTO draft_files (draft_id, position, name, mime, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (position, file) in files.iter().enumerate() {
                stmt.execute(params![
                    id,
                    position as i64,
                    file.name,
                    file.mime,
                    file.data
                ])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// A draft, if it still exists.
    pub fn draft(&self, id: i64) -> Result<Option<DraftRecord>, StoreError> {
        let record = self
            .conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM drafts WHERE id = ?1"),
                params![id],
                record_from_row,
            )
            .optional()?;
        Ok(record)
    }

    /// The files of a draft, in order.
    pub fn draft_files(&self, id: i64) -> Result<Vec<DraftFile>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, mime, data FROM draft_files WHERE draft_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![id], |row| {
            Ok(DraftFile {
                name: row.get(0)?,
                mime: row.get(1)?,
                data: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The draft (not scheduled) whose server copies carry this
    /// Message-ID, if this computer has it.
    pub fn draft_by_message_id(&self, message_id: &str) -> Result<Option<DraftRecord>, StoreError> {
        let record = self
            .conn
            .query_row(
                &format!(
                    "SELECT {DRAFT_COLUMNS} FROM drafts
                     WHERE message_id = ?1 AND send_at IS NULL
                     ORDER BY updated_at DESC, id DESC LIMIT 1"
                ),
                params![message_id],
                record_from_row,
            )
            .optional()?;
        Ok(record)
    }

    /// The drafts (not scheduled) still open: their composer was not
    /// closed (Avero stopped meanwhile). The most recent first.
    pub fn open_drafts(&self) -> Result<Vec<DraftRecord>, StoreError> {
        self.drafts_where("open = 1 AND send_at IS NULL ORDER BY updated_at DESC, id DESC")
    }

    /// The closed drafts (not scheduled) whose server copy is missing or
    /// out of date, oldest first.
    pub fn unsaved_drafts(&self) -> Result<Vec<DraftRecord>, StoreError> {
        self.drafts_where(
            "open = 0 AND server_dirty = 1 AND send_at IS NULL ORDER BY updated_at, id",
        )
    }

    /// The closed drafts that are not scheduled (saved or not on their
    /// server), oldest first.
    pub fn closed_drafts(&self) -> Result<Vec<DraftRecord>, StoreError> {
        self.drafts_where("open = 0 AND send_at IS NULL ORDER BY updated_at, id")
    }

    /// How many files a draft holds (without reading them).
    pub fn draft_file_count(&self, id: i64) -> Result<usize, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM draft_files WHERE draft_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn drafts_where(&self, condition: &str) -> Result<Vec<DraftRecord>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRAFT_COLUMNS} FROM drafts WHERE {condition}"
        ))?;
        let rows = stmt.query_map([], record_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Opens or closes a draft (its composer).
    pub fn set_draft_open(&self, id: i64, open: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET open = ?1 WHERE id = ?2",
            params![open, id],
        )?;
        Ok(())
    }

    /// Records where the draft's copy now is on the server (`None`: it
    /// has none). With `up_to_date`, that copy holds the last changes.
    pub fn set_draft_copy(
        &self,
        id: i64,
        copy: Option<&DraftCopy>,
        up_to_date: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET server_account = ?1, server_folder = ?2, server_uid = ?3,
                 server_dirty = CASE WHEN ?4 THEN 0 ELSE server_dirty END
             WHERE id = ?5",
            params![
                copy.map(|c| c.account.0),
                copy.map(|c| c.folder.as_str()),
                copy.map(|c| c.uid),
                up_to_date,
                id
            ],
        )?;
        Ok(())
    }

    /// Deletes a draft with its files (sent, scheduled one cancelled, or
    /// thrown away).
    pub fn delete_draft(&self, id: i64) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The row of the message `uid` of a folder, if stored.
    pub fn message_row(&self, folder: FolderId, uid: u32) -> Result<Option<i64>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
                params![folder.0, uid],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row)
    }

    /// The Message-ID of a stored message, if it has one.
    pub fn message_id_of(&self, row: i64) -> Result<Option<String>, StoreError> {
        let id: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT message_id FROM messages WHERE id = ?1",
                params![row],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id.flatten())
    }

    // ------------------------------------------------------- scheduled

    /// Schedules a draft to be sent at `send_at` (Unix time): it closes
    /// and waits in "Programmés"; a previous failure is forgotten.
    pub fn schedule_draft(&self, id: i64, send_at: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET send_at = ?1, open = 0, attempted_at = NULL, sent_at = NULL,
                 error = NULL
             WHERE id = ?2",
            params![send_at, id],
        )?;
        Ok(())
    }

    /// Every scheduled mail ("Programmés"): waiting, failed, or sent late
    /// (kept to say so), the next to leave first.
    pub fn scheduled(&self) -> Result<Vec<DraftRecord>, StoreError> {
        self.drafts_where("send_at IS NOT NULL ORDER BY sent_at IS NOT NULL, send_at, id")
    }

    /// The scheduled mail to send at `now`: due, not being edited, not
    /// sent, not failed, not tried yet.
    pub fn due_scheduled(&self, now: i64) -> Result<Vec<DraftRecord>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRAFT_COLUMNS} FROM drafts
             WHERE send_at IS NOT NULL AND send_at <= ?1 AND open = 0
               AND sent_at IS NULL AND error IS NULL AND attempted_at IS NULL
             ORDER BY send_at, id"
        ))?;
        let rows = stmt.query_map(params![now], record_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Reserves the sending of a scheduled mail, from `now`: if Avero
    /// stops before knowing how it went, `interrupted_sends` reports it
    /// instead of sending it twice. Returns `false` (and changes nothing)
    /// when it cannot be sent: already being sent (by another window of
    /// Avero, for example), sent, back in a composer, or gone. Checking and
    /// reserving are one statement, so two senders never both get `true`.
    /// A failed mail (with an `error`) can be reserved again ("Envoyer
    /// maintenant").
    pub fn reserve_send(&self, id: i64, now: i64) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "UPDATE drafts SET attempted_at = ?1, error = NULL
             WHERE id = ?2 AND send_at IS NOT NULL AND attempted_at IS NULL
               AND sent_at IS NULL AND open = 0",
            params![now, id],
        )?;
        Ok(changed == 1)
    }

    /// Scheduled mail whose sending was started by a previous run that
    /// never learnt the result: they get `error` (the user checks "Envoyés"
    /// before sending again). Returns how many.
    pub fn interrupted_sends(&self, error: &str) -> Result<usize, StoreError> {
        let count = self.conn.execute(
            "UPDATE drafts SET error = ?1, attempted_at = NULL
             WHERE send_at IS NOT NULL AND attempted_at IS NOT NULL
               AND sent_at IS NULL AND error IS NULL",
            params![error],
        )?;
        Ok(count)
    }

    /// A scheduled mail left at `now`. Sent on time, it is gone; sent
    /// late (more than `late_after` seconds after its time, Avero was
    /// closed), it stays in "Programmés" to say so, without its files.
    pub fn scheduled_sent(&self, id: i64, now: i64, late_after: i64) -> Result<(), StoreError> {
        let tx = write_tx(&self.conn)?;
        let late = tx.execute(
            "UPDATE drafts SET sent_at = ?1, attempted_at = NULL, error = NULL
             WHERE id = ?2 AND send_at IS NOT NULL AND ?1 - send_at > ?3",
            params![now, id, late_after],
        )?;
        if late == 0 {
            tx.execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
        } else {
            tx.execute("DELETE FROM draft_files WHERE draft_id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sending a scheduled mail failed: it stays, with the reason.
    pub fn scheduled_failed(&self, id: i64, error: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET error = ?1, attempted_at = NULL WHERE id = ?2",
            params![error, id],
        )?;
        Ok(())
    }
}

/// Builds a `DraftRecord` from a row of `DRAFT_COLUMNS`.
fn record_from_row(row: &rusqlite::Row) -> rusqlite::Result<DraftRecord> {
    let server_account: Option<i64> = row.get(12)?;
    let server_folder: Option<String> = row.get(13)?;
    let server_uid: Option<u32> = row.get(14)?;
    let server = match (server_account, server_folder, server_uid) {
        (Some(account), Some(folder), Some(uid)) => Some(DraftCopy {
            account: AccountId(account),
            folder,
            uid,
        }),
        _ => None,
    };
    Ok(DraftRecord {
        id: row.get(0)?,
        fields: DraftFields {
            account: AccountId(row.get(1)?),
            to: row.get(2)?,
            cc: row.get(3)?,
            bcc: row.get(4)?,
            subject: row.get(5)?,
            body: row.get(6)?,
            in_reply_to: row.get(7)?,
            references: row.get(8)?,
            message_id: row.get(9)?,
        },
        open: row.get(10)?,
        server_dirty: row.get(11)?,
        server,
        send_at: row.get(15)?,
        sent_at: row.get(16)?,
        error: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, count};
    use super::*;

    fn fields(account: AccountId, subject: &str) -> DraftFields {
        DraftFields {
            account,
            to: "Paul <paul@ex.fr>, zoe@".into(),
            cc: String::new(),
            bcc: "chef@ex.fr".into(),
            subject: subject.into(),
            body: "\n\nBonjour".into(),
            in_reply_to: Some("<1@x>".into()),
            references: Some("<0@x> <1@x>".into()),
            message_id: "<d1@ex.fr>".into(),
        }
    }

    fn file(name: &str, size: usize) -> DraftFile {
        DraftFile {
            name: name.into(),
            mime: "application/pdf".into(),
            data: vec![7; size],
        }
    }

    #[test]
    fn draft_is_saved_then_updated_in_place() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let id = store
            .save_draft(None, &fields(account, "Un"), Some(&[file("a.pdf", 3)]), 10)
            .unwrap();
        let saved = store.draft(id).unwrap().unwrap();
        assert_eq!(saved.fields, fields(account, "Un"));
        assert!(saved.open && saved.server_dirty);
        assert_eq!(saved.updated_at, 10);
        // Files untouched when not given.
        let again = store
            .save_draft(Some(id), &fields(account, "Deux"), None, 20)
            .unwrap();
        assert_eq!(again, id);
        assert_eq!(store.draft(id).unwrap().unwrap().fields.subject, "Deux");
        assert_eq!(store.draft_files(id).unwrap(), vec![file("a.pdf", 3)]);
        store
            .save_draft(Some(id), &fields(account, "Deux"), Some(&[]), 30)
            .unwrap();
        assert!(store.draft_files(id).unwrap().is_empty());
        assert_eq!(count(&store, "drafts"), 1);
        // A draft deleted meanwhile is saved again as a new one.
        store.delete_draft(id).unwrap();
        let new = store
            .save_draft(Some(id), &fields(account, "Trois"), None, 40)
            .unwrap();
        assert_ne!(new, id);
        assert_eq!(count(&store, "drafts"), 1);
    }

    #[test]
    fn open_drafts_survive_a_restart_and_closed_ones_wait_for_the_server() {
        let path = std::env::temp_dir().join(format!(
            "avero-drafts-{}-{}.db",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);
        let (open, closed) = {
            let store = Store::open(&path).unwrap();
            let account = acct(&store, "a@x.fr");
            let open = store
                .save_draft(None, &fields(account, "Ouvert"), None, 10)
                .unwrap();
            let closed = store
                .save_draft(None, &fields(account, "Fermé"), None, 5)
                .unwrap();
            store.set_draft_open(closed, false).unwrap();
            (open, closed)
        };
        // Avero stopped without closing the composer.
        let store = Store::open(&path).unwrap();
        let ids = |list: Vec<DraftRecord>| list.into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!(ids(store.open_drafts().unwrap()), vec![open]);
        assert_eq!(ids(store.unsaved_drafts().unwrap()), vec![closed]);
        // Its server copy is saved: nothing left to save.
        let copy = DraftCopy {
            account: store.draft(closed).unwrap().unwrap().fields.account,
            folder: "Drafts".into(),
            uid: 42,
        };
        store.set_draft_copy(closed, Some(&copy), true).unwrap();
        assert!(store.unsaved_drafts().unwrap().is_empty());
        let record = store.draft_by_message_id("<d1@ex.fr>").unwrap().unwrap();
        assert_eq!(record.id, open, "the most recent");
        assert_eq!(store.draft(closed).unwrap().unwrap().server, Some(copy));
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scheduled_mail_waits_for_its_time() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let id = store
            .save_draft(None, &fields(account, "Plus tard"), None, 10)
            .unwrap();
        store.schedule_draft(id, 1_000).unwrap();
        let record = store.draft(id).unwrap().unwrap();
        assert!(!record.open);
        assert_eq!(record.send_at, Some(1_000));
        assert!(store.open_drafts().unwrap().is_empty());
        assert!(store.unsaved_drafts().unwrap().is_empty());
        assert!(store.draft_by_message_id("<d1@ex.fr>").unwrap().is_none());
        assert!(store.due_scheduled(999).unwrap().is_empty());
        assert_eq!(store.due_scheduled(1_000).unwrap().len(), 1);
        // Being edited: not sent.
        store.set_draft_open(id, true).unwrap();
        assert!(store.due_scheduled(1_000).unwrap().is_empty());
        store.set_draft_open(id, false).unwrap();
        // Being sent: not sent twice, even by a sender that read it as
        // due before (another window of Avero).
        assert!(store.reserve_send(id, 1_000).unwrap());
        assert!(store.due_scheduled(1_000).unwrap().is_empty());
        assert!(!store.reserve_send(id, 1_000).unwrap());
        store.scheduled_failed(id, "refusé").unwrap();
        assert!(store.due_scheduled(2_000).unwrap().is_empty());
        let failed = &store.scheduled().unwrap()[0];
        assert_eq!(failed.error.as_deref(), Some("refusé"));
        // "Envoyer maintenant" after a failure.
        assert!(store.reserve_send(id, 1_200).unwrap());
        store.scheduled_failed(id, "refusé").unwrap();
        // Scheduled again: the error is forgotten.
        store.schedule_draft(id, 1_500).unwrap();
        assert_eq!(store.due_scheduled(2_000).unwrap().len(), 1);
        // Open in a composer, or sent: not reserved.
        store.set_draft_open(id, true).unwrap();
        assert!(!store.reserve_send(id, 2_000).unwrap());
        store.set_draft_open(id, false).unwrap();
        store.scheduled_sent(id, 9_000, 120).unwrap();
        assert!(!store.reserve_send(id, 9_500).unwrap());
    }

    #[test]
    fn mail_sent_on_time_goes_late_mail_stays_without_files() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let on_time = store
            .save_draft(None, &fields(account, "A"), Some(&[file("a.pdf", 5)]), 1)
            .unwrap();
        let late = store
            .save_draft(None, &fields(account, "B"), Some(&[file("b.pdf", 5)]), 1)
            .unwrap();
        store.schedule_draft(on_time, 1_000).unwrap();
        store.schedule_draft(late, 1_000).unwrap();
        store.scheduled_sent(on_time, 1_030, 120).unwrap();
        store.scheduled_sent(late, 9_000, 120).unwrap();
        assert!(store.draft(on_time).unwrap().is_none());
        let kept = store.draft(late).unwrap().unwrap();
        assert_eq!((kept.send_at, kept.sent_at), (Some(1_000), Some(9_000)));
        assert!(store.draft_files(late).unwrap().is_empty());
        assert_eq!(count(&store, "draft_files"), 0);
        assert!(store.due_scheduled(10_000).unwrap().is_empty());
    }

    #[test]
    fn send_cut_short_by_a_stop_is_reported_not_repeated() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        let id = store
            .save_draft(None, &fields(account, "A"), None, 1)
            .unwrap();
        store.schedule_draft(id, 1_000).unwrap();
        assert!(store.reserve_send(id, 1_000).unwrap());
        assert_eq!(store.interrupted_sends("interrompu").unwrap(), 1);
        assert_eq!(
            store.draft(id).unwrap().unwrap().error.as_deref(),
            Some("interrompu")
        );
        assert!(store.due_scheduled(5_000).unwrap().is_empty());
    }

    #[test]
    fn drafts_go_with_their_account() {
        let store = Store::open_in_memory().unwrap();
        let account = acct(&store, "a@x.fr");
        store
            .save_draft(None, &fields(account, "A"), Some(&[file("a", 1)]), 1)
            .unwrap();
        store.delete_account(account).unwrap();
        assert_eq!(count(&store, "drafts"), 0);
        assert_eq!(count(&store, "draft_files"), 0);
    }
}
