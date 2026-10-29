//! Drafts: what the composer holds is saved on this computer a few
//! seconds after each change (and when it closes), then in the drafts
//! folder of the server when it closes without being sent. A draft of
//! the drafts folder opens in the composer, where it was left. Sending
//! it removes it from both places.
//!
//! On this computer a draft keeps its attachments in the database (see
//! `schema_v9.sql`). Each save on the server adds the new version and
//! deletes the previous one, found by its UID (and its Message-ID, kept
//! from one save to the next).

use super::later::LaterMsg;
use super::{App, BODY_ID, Composer, Message};
use crate::background::OpenedBody;
use crate::compose::{self, split_addresses};
use crate::later;
use crate::worker::{Event, Job};
use avero_core::{Account, DraftCopy, DraftFields, DraftFile, DraftRecord};
use avero_sync::parse::Address;
use avero_sync::smtp::{self, Outgoing, OutgoingAttachment};
use iced::Task;
use std::time::Duration;

/// How long after a change the draft is saved on this computer.
pub const AUTOSAVE: Duration = Duration::from_secs(3);

impl App {
    /// The few seconds after a change are over: saves the draft here.
    pub(super) fn autosave(&mut self) -> Task<Message> {
        match self.save_composer() {
            Ok(_) => Task::none(),
            Err(e) => self.toast(e, true),
        }
    }

    /// Saves what the composer holds on this computer, if it changed.
    /// Returns the draft's id (`None`: nothing written, nothing kept).
    /// A reply or a forward the user has not touched yet is not kept
    /// either: closing it must leave no draft.
    pub(super) fn save_composer(&mut self) -> Result<Option<i64>, String> {
        self.store_composer(false)
    }

    /// `save_composer`; with `untouched`, a composer the user has not
    /// changed is kept too (it is being scheduled as it is).
    pub(super) fn store_composer(&mut self, untouched: bool) -> Result<Option<i64>, String> {
        let now = self.clock.now();
        let Some(composer) = &self.composer else {
            return Ok(None);
        };
        let email = self
            .account(composer.from)
            .map(|a| a.email.clone())
            .unwrap_or_default();
        let Some(composer) = &mut self.composer else {
            return Ok(None);
        };
        if composer.draft_id.is_none() && (composer.is_blank() || (!composer.edited && !untouched))
        {
            return Ok(None);
        }
        if composer.draft_id.is_some() && !composer.dirty {
            return Ok(composer.draft_id);
        }
        if composer.message_id.is_empty() {
            composer.message_id = smtp::new_message_id(&email);
        }
        let fields = DraftFields {
            account: composer.from,
            to: composer.to.clone(),
            cc: composer.cc.clone(),
            bcc: composer.bcc.clone(),
            subject: composer.subject.clone(),
            body: composer.body.text(),
            in_reply_to: composer.in_reply_to.clone(),
            references: composer.references.clone(),
            message_id: composer.message_id.clone(),
        };
        // The files are written again only when they changed.
        let files: Option<Vec<DraftFile>> = (composer.files_dirty || composer.draft_id.is_none())
            .then(|| {
                composer
                    .attachments
                    .iter()
                    .map(|a| DraftFile {
                        name: a.name.clone(),
                        mime: a.mime.clone(),
                        data: a.data.as_ref().clone(),
                    })
                    .collect()
            });
        let id = self
            .store
            .save_draft(composer.draft_id, &fields, files.as_deref(), now)
            .map_err(|e| format!("Brouillon non enregistré : {e}"))?;
        composer.draft_id = Some(id);
        composer.dirty = false;
        composer.files_dirty = false;
        Ok(Some(id))
    }

    /// Closes the composer, keeping the message: saved here, then in the
    /// server's drafts folder if it changed (a scheduled mail keeps its
    /// time instead). Nothing is kept of a composer left blank.
    pub(super) fn close_composer(&mut self) -> Task<Message> {
        if self.composer.as_ref().is_none_or(|c| c.sending) {
            return Task::none();
        }
        let saved = self.save_composer();
        let Some(composer) = self.composer.take() else {
            return Task::none();
        };
        let id = match saved {
            Ok(Some(id)) => id,
            Ok(None) => return Task::none(),
            Err(e) => {
                self.composer = Some(composer);
                return self.toast(e, true);
            }
        };
        let _ = self.store.set_draft_open(id, false);
        if let Some(at) = composer.send_at {
            // Its time again, and a failure forgotten: it was edited.
            let _ = self.store.schedule_draft(id, at);
            self.refresh_outbox();
            let when = later::describe_local(at, self.clock.now());
            return self.toast(format!("Toujours programmé {when}"), false);
        }
        if !composer.edited {
            return Task::none();
        }
        let push = self.push_draft(id);
        Task::batch([push, self.toast("Brouillon enregistré", false)])
    }

    /// "Supprimer le brouillon": closes the composer and throws the
    /// draft away, here and on the server.
    pub(super) fn discard_composer(&mut self) -> Task<Message> {
        if self.composer.as_ref().is_none_or(|c| c.sending) {
            return Task::none();
        }
        let Some(composer) = self.composer.take() else {
            return Task::none();
        };
        let kept = composer.draft_id.is_some();
        let forget = self.forget_draft(composer);
        if kept {
            Task::batch([forget, self.toast("Brouillon supprimé", false)])
        } else {
            forget
        }
    }

    /// Deletes the draft of a composer (sent, or thrown away) here and
    /// its copy on the server.
    pub(super) fn forget_draft(&mut self, composer: Composer) -> Task<Message> {
        let mut copy = composer.server_copy;
        if let Some(id) = composer.draft_id {
            if let Ok(Some(record)) = self.store.draft(id) {
                copy = record.server.or(copy);
            }
            let _ = self.store.delete_draft(id);
            if composer.send_at.is_some() {
                self.refresh_outbox();
            }
        }
        match copy {
            Some(copy) => self.drop_server_draft(copy),
            None => Task::none(),
        }
    }

    /// Saves a draft in the drafts folder of its account; the answer
    /// comes back as `LaterMsg::DraftSaved`.
    pub(super) fn push_draft(&mut self, id: i64) -> Task<Message> {
        let Ok(Some(record)) = self.store.draft(id) else {
            return Task::none();
        };
        let Some(account) = self.account(record.fields.account).cloned() else {
            return Task::none();
        };
        let files = self.store.draft_files(id).unwrap_or_default();
        let raw = match draft_raw(&account, &record.fields, files) {
            Ok(raw) => raw,
            Err(e) => return self.toast(format!("Brouillon gardé sur cet ordinateur : {e}"), true),
        };
        let message_id = record.fields.message_id.clone();
        let replace = record.server.clone();
        self.request(account.id, |reply| Job::SaveDraft {
            raw,
            message_id,
            replace,
            reply,
        })
        .map(move |message| match message {
            Message::Worker(_, event) => Message::Later(LaterMsg::DraftSaved(id, event)),
            other => other,
        })
    }

    /// Deletes a draft's copy from its server. Its line leaves the
    /// drafts folder at once (the worker removes it too once the server
    /// confirms).
    pub(super) fn drop_server_draft(&mut self, copy: DraftCopy) -> Task<Message> {
        let folder = self
            .store
            .folders(copy.account)
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.path == copy.folder);
        if let Some(folder) = folder
            && let Ok(Some(row)) = self.store.message_row(folder.id, copy.uid)
        {
            let _ = self.store.delete_messages(&[row]);
            self.reload();
        }
        let account = copy.account;
        self.request(account, |reply| Job::DropDraft { copy, reply })
    }

    /// A worker answered about saving draft `id` in its drafts folder.
    pub(super) fn draft_saved(&mut self, id: i64, event: Event) -> Task<Message> {
        let copy = match event {
            Event::DraftSaved(Ok(copy)) => copy,
            Event::DraftSaved(Err(error)) => {
                return self.toast(
                    format!("Brouillon gardé sur cet ordinateur ; le serveur l'a refusé : {error}"),
                    true,
                );
            }
            Event::Gone => {
                return self.toast(
                    "Brouillon gardé sur cet ordinateur ; le service du compte s'est arrêté",
                    true,
                );
            }
            _ => return Task::none(),
        };
        let Ok(Some(record)) = self.store.draft(id) else {
            // Sent or thrown away meanwhile: this copy goes too.
            return match copy {
                Some(copy) => self.drop_server_draft(copy),
                None => Task::none(),
            };
        };
        // Back in a composer meanwhile: that one saves it again at close.
        let up_to_date = !record.open;
        let _ = self.store.set_draft_copy(id, copy.as_ref(), up_to_date);
        if let Some(composer) = &mut self.composer
            && composer.draft_id == Some(id)
        {
            composer.server_copy = copy.clone();
        }
        let mut tasks = Vec::new();
        // The worker replaced the previous copy if it was in the same
        // folder; one left on another account goes now.
        if let Some(old) = record.server
            && old.account != record.fields.account
        {
            tasks.push(self.drop_server_draft(old));
        }
        // The drafts folder shows it after a sync.
        tasks.push(self.sync_account(record.fields.account));
        Task::batch(tasks)
    }

    /// At start: the draft whose composer was still open when Avero
    /// stopped opens again; the drafts not yet saved on their server are
    /// saved now. A scheduled mail left open goes back to waiting. The
    /// untouched replies and forwards an older version kept go first.
    pub(super) fn restore_drafts(&mut self) -> Task<Message> {
        let cleanup = self.forget_untouched_answers();
        for record in self.store.scheduled().unwrap_or_default() {
            if record.open {
                let _ = self.store.set_draft_open(record.id, false);
            }
        }
        let mut reopen: Option<DraftRecord> = None;
        for record in self.store.open_drafts().unwrap_or_default() {
            if reopen.is_none() && self.account(record.fields.account).is_some() {
                reopen = Some(record);
            } else {
                let _ = self.store.set_draft_open(record.id, false);
            }
        }
        let mut tasks: Vec<Task<Message>> = self
            .store
            .unsaved_drafts()
            .unwrap_or_default()
            .into_iter()
            .map(|record| self.push_draft(record.id))
            .collect();
        if let Some(record) = reopen {
            let files = self.store.draft_files(record.id).unwrap_or_default();
            self.composer = Some(Composer::from_draft(&record, files));
            tasks.push(self.toast("Brouillon récupéré : Avero s'était arrêté", false));
        }
        tasks.push(cleanup);
        Task::batch(tasks)
    }

    /// Throws away the closed drafts that are replies or forwards nobody
    /// wrote in (see `compose::untouched_answer`), here and on the
    /// server: older versions kept one each time such a composer closed.
    fn forget_untouched_answers(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for record in self.store.closed_drafts().unwrap_or_default() {
            let signature = self.signature_of(record.fields.account);
            let has_files = self
                .store
                .draft_file_count(record.id)
                .map_or(true, |count| count > 0);
            if !compose::untouched_answer(&record.fields, &signature, has_files) {
                continue;
            }
            if self.store.delete_draft(record.id).is_ok()
                && let Some(copy) = record.server
            {
                tasks.push(self.drop_server_draft(copy));
            }
        }
        Task::batch(tasks)
    }

    /// A line of the drafts folder was opened: the draft goes back in the
    /// composer. One this computer saved comes back as it was here (Bcc
    /// and files included); another one (written elsewhere) is read from
    /// its copy on the server.
    pub(super) fn open_draft_line(&mut self, index: usize) -> Task<Message> {
        let Some(item) = self.items.get(index).cloned() else {
            return Task::none();
        };
        let Some(&row) = item.row_ids.first() else {
            return Task::none();
        };
        let Some(target) = self.refs(&[row]).into_iter().next() else {
            return Task::none();
        };
        let copy = DraftCopy {
            account: target.account_id,
            folder: target.folder_path.clone(),
            uid: target.uid,
        };
        let ours = self
            .store
            .message_id_of(row)
            .ok()
            .flatten()
            .and_then(|id| self.store.draft_by_message_id(&id).ok().flatten());
        if let (Some(record), Some(composer)) = (&ours, &self.composer)
            && composer.draft_id == Some(record.id)
        {
            return Task::none();
        }
        // What the composer held is kept as a draft first.
        let close = self.close_composer();
        if self.composer.is_some() {
            return close;
        }
        self.selected = Some(item.row_ids.clone());
        self.reading = None;
        self.clear_checked();
        if let Some(mut record) = ours {
            if record.server.is_none() {
                let _ = self.store.set_draft_copy(record.id, Some(&copy), false);
                record.server = Some(copy);
            }
            let _ = self.store.set_draft_open(record.id, true);
            let files = self.store.draft_files(record.id).unwrap_or_default();
            self.composer = Some(Composer::from_draft(&record, files));
            self.screen = super::Screen::Mail;
            return Task::batch([close, iced::widget::operation::focus(BODY_ID)]);
        }
        self.pending_draft = Some(row);
        let load = match self.store.body(row) {
            Ok(Some((raw, _))) => super::mail::parse_task(row, raw),
            _ => self.fetch_body(row),
        };
        Task::batch([close, load])
    }

    /// The server copy of a draft written elsewhere was read: it opens in
    /// the composer, and is kept here from now on.
    pub(super) fn open_parsed_draft(&mut self, row: i64, opened: &OpenedBody) -> Task<Message> {
        self.pending_draft = None;
        if self.composer.is_some() {
            return Task::none();
        }
        let Some(target) = self.refs(&[row]).into_iter().next() else {
            return Task::none();
        };
        let Some(account) = self.account(target.account_id).cloned() else {
            return Task::none();
        };
        let parsed = &opened.parsed;
        let join = |list: &[Address]| {
            list.iter()
                .map(compose::format_address)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let subject = match parsed.subject.as_str() {
            "(sans objet)" => String::new(),
            subject => subject.to_string(),
        };
        let fields = DraftFields {
            account: account.id,
            to: join(&parsed.to),
            cc: join(&parsed.cc),
            bcc: join(&parsed.bcc),
            subject,
            body: parsed.text.clone(),
            in_reply_to: parsed.in_reply_to.clone(),
            references: parsed.references.clone(),
            message_id: parsed
                .message_id
                .clone()
                .unwrap_or_else(|| smtp::new_message_id(&account.email)),
        };
        let files: Vec<DraftFile> = parsed
            .attachments
            .iter()
            .filter_map(|a| {
                Some(DraftFile {
                    name: a.name.clone(),
                    mime: a.mime.clone(),
                    data: opened.attachments.get(a.index)?.as_ref().clone(),
                })
            })
            .collect();
        let copy = DraftCopy {
            account: account.id,
            folder: target.folder_path,
            uid: target.uid,
        };
        let saved = self
            .store
            .save_draft(None, &fields, Some(&files), self.clock.now())
            .and_then(|id| {
                self.store.set_draft_copy(id, Some(&copy), true)?;
                self.store.draft(id)
            });
        match saved {
            Ok(Some(record)) => {
                self.composer = Some(Composer::from_draft(&record, files));
                self.screen = super::Screen::Mail;
                iced::widget::operation::focus(BODY_ID)
            }
            Ok(None) => Task::none(),
            Err(e) => self.toast(format!("Brouillon illisible : {e}"), true),
        }
    }
}

/// The draft as it is kept in the drafts folder: the message as it would
/// be sent, addresses as typed (none, or half written, is fine).
fn draft_raw(
    account: &Account,
    fields: &DraftFields,
    files: Vec<DraftFile>,
) -> Result<Vec<u8>, String> {
    let recipients = |text: &str| {
        split_addresses(text)
            .iter()
            .map(|entry| compose::parse_recipient(entry))
            .collect()
    };
    let outgoing = Outgoing {
        from_name: Some(account.name.clone()).filter(|n| !n.trim().is_empty()),
        from_addr: account.email.clone(),
        to: recipients(&fields.to),
        cc: recipients(&fields.cc),
        bcc: recipients(&fields.bcc),
        subject: fields.subject.trim().to_string(),
        body: fields.body.clone(),
        in_reply_to: fields.in_reply_to.clone(),
        references: fields.references.clone(),
        attachments: files
            .into_iter()
            .map(|file| OutgoingAttachment {
                name: file.name,
                mime: file.mime,
                data: file.data,
            })
            .collect(),
    };
    smtp::draft_message(&outgoing, &fields.message_id).map_err(|e| e.to_string())
}
