//! The message list, the reading pane and the actions on a message.

use super::{App, Body, Message, Overlay, PAGE, RELOAD_EVERY, Reading};
use crate::background::{self, OpenedBody};
use crate::compose::Mode;
use crate::worker::{Destination, Event, Job};
use avero_core::{AccountFilter, AccountId, FolderRole, MessageRef, UnifiedItem};
use iced::Task;
use iced::widget::text_editor;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Delay between the last key typed in the search field and the search.
const SEARCH_DELAY: Duration = Duration::from_millis(250);

impl App {
    pub(super) fn update_mail(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FolderSelected(folder) => {
                self.folder = folder;
                self.search_text.clear();
                self.search_query = None;
                self.screen = super::Screen::Mail;
                self.load_first_page();
                Task::none()
            }
            Message::FilterSelected(filter) => {
                self.filter = filter;
                self.load_first_page();
                Task::none()
            }
            Message::SearchChanged(text) => {
                self.search_text = text;
                self.search_generation += 1;
                let generation = self.search_generation;
                Task::perform(super::sleep(SEARCH_DELAY), move |()| {
                    Message::SearchDebounced(generation)
                })
            }
            Message::SearchDebounced(generation) => {
                if generation == self.search_generation {
                    self.apply_search();
                }
                Task::none()
            }
            Message::SearchSubmitted => {
                self.search_generation += 1;
                self.apply_search();
                Task::none()
            }
            Message::SearchCleared => {
                self.search_generation += 1;
                self.search_text.clear();
                self.apply_search();
                Task::none()
            }
            Message::MessageSelected(index) => self.open(index),
            Message::LoadMore => {
                self.items = self.fetch(self.items.len() + PAGE);
                Task::none()
            }
            Message::ReaderAction(action) => {
                // Read-only: selection and scrolling, never edits.
                if !action.is_edit()
                    && let Some(Reading {
                        body: Body::Ready { content, .. },
                        ..
                    }) = &mut self.reading
                {
                    content.perform(action);
                }
                Task::none()
            }
            Message::Answer(mode) => self.answer(mode),
            Message::Archive => self.move_selected(Destination::Archive),
            Message::Delete => self.move_selected(Destination::Trash),
            Message::MarkUnread => self.mark_unread(),
            Message::SaveAttachment(index) => self.save_attachment(index),
            Message::AttachmentSaved(result) => match result {
                Ok(Some(name)) => self.toast(format!("Enregistré : {name}"), false),
                Ok(None) => Task::none(),
                Err(e) => self.toast(format!("Enregistrement impossible : {e}"), true),
            },
            _ => Task::none(),
        }
    }

    fn apply_search(&mut self) {
        let query = self.search_text.trim();
        self.search_query = (!query.is_empty()).then(|| query.to_string());
        self.load_first_page();
    }

    /// Shows the first page for the current folder, filter and search,
    /// with nothing selected.
    pub(super) fn load_first_page(&mut self) {
        self.selected = None;
        self.reading = None;
        self.items = self.fetch(PAGE);
        self.refresh_unread();
    }

    /// Reads the list again (after a sync or an action), keeping the
    /// loaded pages and the selection.
    pub(super) fn reload(&mut self) {
        let pages = self.items.len().div_ceil(PAGE).max(1);
        self.items = self.fetch(pages * PAGE);
        self.refresh_unread();
    }

    /// `reload`, and remember when (for `reload_soon`).
    pub(super) fn reload_now(&mut self) {
        self.last_reload = Some(Instant::now());
        self.reload();
    }

    /// Reload for sync progress: at most once per [`RELOAD_EVERY`]; a
    /// reload asked too early is done when its turn comes.
    pub(super) fn reload_soon(&mut self) -> Task<Message> {
        let since = self.last_reload.map(|at| at.elapsed());
        match since {
            Some(elapsed) if elapsed < RELOAD_EVERY => {
                if self.reload_scheduled {
                    return Task::none();
                }
                self.reload_scheduled = true;
                Task::perform(super::sleep(RELOAD_EVERY - elapsed), |()| {
                    Message::ReloadDue
                })
            }
            _ => {
                self.reload_now();
                Task::none()
            }
        }
    }

    /// The first `limit` items for the current view; on a database error
    /// logs it and returns nothing so the window keeps working.
    fn fetch(&self, limit: usize) -> Vec<UnifiedItem> {
        let result = match &self.search_query {
            Some(query) => self.store.search(query, None, self.filter, limit),
            None => self.store.unified(self.folder, self.filter, limit, 0),
        };
        result.unwrap_or_else(|e| {
            eprintln!("Impossible de lire la liste : {e}");
            Vec::new()
        })
    }

    fn refresh_unread(&mut self) {
        self.trash_accounts = self
            .accounts
            .iter()
            .filter(|a| {
                matches!(
                    self.store.folder_by_role(a.id, FolderRole::Trash),
                    Ok(Some(_))
                )
            })
            .map(|a| a.id)
            .collect();
        self.archive_view = self.accounts.iter().any(|a| {
            matches!(
                self.store.folder_by_role(a.id, FolderRole::Archive),
                Ok(Some(_))
            )
        });
        self.unread = self
            .accounts
            .iter()
            .map(|a| {
                let count = self
                    .store
                    .unread_count(FolderRole::Inbox, AccountFilter::Only(a.id))
                    .unwrap_or(0);
                (a.id, count)
            })
            .collect();
    }

    /// Index of the selected item in the list.
    pub fn selected_index(&self) -> Option<usize> {
        let rows = self.selected.as_ref()?;
        self.items.iter().position(|i| &i.row_ids == rows)
    }

    /// The selected item.
    pub fn selected_item(&self) -> Option<&UnifiedItem> {
        self.selected_index().map(|i| &self.items[i])
    }

    /// J/K: moves the selection down (`1`) or up (`-1`).
    pub(super) fn step_selection(&mut self, step: isize) -> Task<Message> {
        if self.items.is_empty() {
            return Task::none();
        }
        let next = match self.selected_index() {
            Some(i) => i.saturating_add_signed(step).min(self.items.len() - 1),
            None => 0,
        };
        self.open(next)
    }

    /// Selects an item: shows its body (cached, or downloaded in the
    /// background) and marks it read, locally first.
    fn open(&mut self, index: usize) -> Task<Message> {
        let Some(item) = self.items.get(index).cloned() else {
            return Task::none();
        };
        self.selected = Some(item.row_ids.clone());
        let Some(&row) = item.row_ids.first() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        match self.store.body(row) {
            Ok(Some((raw, _))) => tasks.push(parse_task(row, raw)),
            _ => tasks.push(self.fetch_body(row)),
        }
        self.reading = Some(Reading {
            rows: item.row_ids.clone(),
            accounts: item.accounts.clone(),
            body: Body::Loading,
        });
        if !item.seen {
            let _ = self.store.set_seen(&item.row_ids, true);
            self.reload();
            tasks.push(self.remote_seen(&item.row_ids, true));
        }
        Task::batch(tasks)
    }

    fn fetch_body(&self, row: i64) -> Task<Message> {
        let Some(target) = self.refs(&[row]).into_iter().next() else {
            return Task::none();
        };
        let account = target.account_id;
        self.request(account, |reply| Job::FetchBody { target, reply })
    }

    /// Called when a worker cached a body: it is parsed off the UI thread.
    pub(super) fn body_arrived(&mut self, row: i64, result: Result<(), String>) -> Task<Message> {
        let Some(reading) = &mut self.reading else {
            return Task::none();
        };
        if reading.rows.first() != Some(&row) {
            return Task::none();
        }
        match result {
            Ok(()) => match self.store.body(row) {
                Ok(Some((raw, _))) => return parse_task(row, raw),
                _ => reading.body = Body::Failed("Le message n'a pas pu être lu".to_string()),
            },
            Err(e) => reading.body = Body::Failed(e),
        }
        Task::none()
    }

    /// A parsed body is ready: show it if it is still the one being read.
    pub(super) fn body_parsed(&mut self, row: i64, result: Result<Arc<OpenedBody>, String>) {
        let Some(reading) = &mut self.reading else {
            return;
        };
        if reading.rows.first() != Some(&row) {
            return;
        }
        reading.body = match result {
            Ok(opened) => ready_body(opened),
            Err(e) => Body::Failed(e),
        };
    }

    /// A worker lost the job of the message being loaded.
    pub(super) fn body_lost(&mut self) {
        if let Some(reading) = &mut self.reading
            && matches!(reading.body, Body::Loading)
        {
            reading.body = Body::Failed("Le message n'a pas pu être chargé".to_string());
        }
    }

    fn refs(&self, rows: &[i64]) -> Vec<MessageRef> {
        self.store.message_refs(rows).unwrap_or_default()
    }

    /// Sends the seen flag to the servers, one job per account.
    fn remote_seen(&self, rows: &[i64], seen: bool) -> Task<Message> {
        let groups = by_account(self.refs(rows));
        Task::batch(groups.into_iter().map(|(account, targets)| {
            self.request(account, |reply| Job::SetSeen {
                targets,
                seen,
                reply,
            })
        }))
    }

    fn mark_unread(&mut self) -> Task<Message> {
        let Some(rows) = self.selected.clone() else {
            return Task::none();
        };
        let _ = self.store.set_seen(&rows, false);
        self.reload();
        self.remote_seen(&rows, false)
    }

    /// Whether "Archiver" can be used on the selection: some account of
    /// the message has a known archive folder.
    pub fn can_archive(&self) -> bool {
        self.selected_item().is_some() && self.archive_problem().is_none()
    }

    /// Why "Archiver" cannot be used, for its tooltip.
    pub fn archive_problem(&self) -> Option<&'static str> {
        let item = self.selected_item()?;
        if self.folder == FolderRole::Archive && self.search_query.is_none() {
            return Some("Déjà dans les archives");
        }
        let known: Vec<Option<bool>> = item
            .accounts
            .iter()
            .map(|id| self.status.get(id).and_then(|s| s.archive))
            .collect();
        if known.contains(&Some(true)) {
            None
        } else if known.contains(&None) {
            Some("Disponible après la première synchronisation")
        } else {
            Some("Aucun dossier d'archives sur ce compte")
        }
    }

    /// Whether "Supprimer" can be used on the selection.
    pub fn can_delete(&self) -> bool {
        self.selected_item().is_some() && self.delete_problem().is_none()
    }

    /// Why "Supprimer" cannot be used, for its tooltip.
    pub fn delete_problem(&self) -> Option<&'static str> {
        let item = self.selected_item()?;
        if item
            .accounts
            .iter()
            .any(|id| self.trash_accounts.contains(id))
        {
            None
        } else if item
            .accounts
            .iter()
            .any(|id| self.status.get(id).and_then(|s| s.archive).is_none())
        {
            Some("Disponible après la première synchronisation")
        } else {
            Some("Aucune corbeille sur ce compte")
        }
    }

    /// Whether every copy of the selection is already in a trash folder
    /// (deleting it then means deleting it for good).
    pub fn selection_in_trash(&self) -> bool {
        let Some(rows) = &self.selected else {
            return false;
        };
        let refs = self.refs(rows);
        !refs.is_empty() && refs.iter().all(|r| self.is_trash_folder(r))
    }

    fn is_trash_folder(&self, target: &MessageRef) -> bool {
        matches!(
            self.store.folder_by_role(target.account_id, FolderRole::Trash),
            Ok(Some(folder)) if folder.id == target.folder_id
        )
    }

    /// Archive or delete: hides the rows at once and selects the next
    /// message; the rows are only removed when the server confirms (shown
    /// again otherwise). Messages already in the trash are deleted for
    /// good, after a confirmation.
    fn move_selected(&mut self, to: Destination) -> Task<Message> {
        let allowed = match to {
            Destination::Archive => self.can_archive(),
            Destination::Trash => self.can_delete(),
        };
        let Some(rows) = self.selected.clone() else {
            return Task::none();
        };
        if !allowed {
            return Task::none();
        }
        if to == Destination::Trash && self.selection_in_trash() {
            self.overlay = Some(Overlay::ConfirmDeleteForever(rows));
            return Task::none();
        }
        let has_destination = |app: &App, account: AccountId| match to {
            Destination::Archive => app.status.get(&account).and_then(|s| s.archive) == Some(true),
            Destination::Trash => app.trash_accounts.contains(&account),
        };
        // Copies already in the destination, or on an account without
        // one, stay where they are.
        let targets: Vec<MessageRef> = self
            .refs(&rows)
            .into_iter()
            .filter(|r| has_destination(self, r.account_id))
            .filter(|r| match to {
                Destination::Trash => !self.is_trash_folder(r),
                Destination::Archive => !matches!(
                    self.store.folder_by_role(r.account_id, FolderRole::Archive),
                    Ok(Some(folder)) if folder.id == r.folder_id
                ),
            })
            .collect();
        if targets.is_empty() {
            return Task::none();
        }
        let toast = self.toast(super::destination_label(to), false);
        Task::batch([
            self.hide_and_send(targets, move |targets, reply| Job::Move {
                targets,
                to,
                reply,
            }),
            toast,
        ])
    }

    /// "Supprimer définitivement" was confirmed.
    pub(super) fn delete_forever(&mut self) -> Task<Message> {
        let Some(Overlay::ConfirmDeleteForever(rows)) = self.overlay.take() else {
            return Task::none();
        };
        let targets = self.refs(&rows);
        let toast = self.toast("Supprimé définitivement", false);
        Task::batch([
            self.hide_and_send(targets, |targets, reply| Job::DeleteForever {
                targets,
                reply,
            }),
            toast,
        ])
    }

    /// Hides the rows, opens the next message and sends one job per
    /// account; the answers come back as `Message::Moved`.
    fn hide_and_send(
        &mut self,
        targets: Vec<MessageRef>,
        make: impl Fn(Vec<MessageRef>, crate::worker::Reply) -> Job,
    ) -> Task<Message> {
        let index = self.selected_index();
        let rows: Vec<i64> = targets.iter().map(|t| t.row_id).collect();
        if let Err(e) = self.store.set_hidden(&rows, true) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        let reading_hidden = self
            .reading
            .as_ref()
            .is_some_and(|r| r.rows.iter().any(|row| rows.contains(row)));
        if reading_hidden {
            self.selected = None;
            self.reading = None;
        }
        self.reload_now();
        let mut tasks: Vec<Task<Message>> = by_account(targets)
            .into_iter()
            .map(|(account, targets)| {
                let rows: Vec<i64> = targets.iter().map(|t| t.row_id).collect();
                self.request(account, |reply| make(targets, reply)).map(
                    move |message| match message {
                        Message::Worker(_, event) => Message::Moved(rows.clone(), event),
                        other => other,
                    },
                )
            })
            .collect();
        if reading_hidden
            && let Some(index) = index
            && !self.items.is_empty()
        {
            tasks.push(self.open(index.min(self.items.len() - 1)));
        }
        Task::batch(tasks)
    }

    /// A worker answered an archive or delete: confirmed rows go away,
    /// refused ones come back with the reason.
    pub(super) fn moved(&mut self, rows: Vec<i64>, event: Event) -> Task<Message> {
        match event {
            Event::Done(Ok(())) => {
                // The worker already removed them; in demo mode it is us.
                let _ = self.store.delete_messages(&rows);
                self.reload_now();
                Task::none()
            }
            Event::Done(Err(error)) => {
                let _ = self.store.set_hidden(&rows, false);
                self.reload_now();
                self.toast(error.message, true)
            }
            Event::Gone => {
                let _ = self.store.set_hidden(&rows, false);
                self.reload_now();
                self.toast(
                    "Le service de ce compte s'est arrêté ; redémarrez Avero",
                    true,
                )
            }
            _ => Task::none(),
        }
    }

    fn answer(&mut self, mode: Mode) -> Task<Message> {
        let Some(Reading {
            body: Body::Ready { opened, .. },
            accounts,
            ..
        }) = &self.reading
        else {
            return Task::none();
        };
        let parsed = &opened.parsed;
        let own: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        let fields = crate::compose::answer(parsed, mode, &own);
        let from = accounts.first().copied().or_else(|| self.default_sender());
        let Some(from) = from else {
            return Task::none();
        };
        let attachments = if mode == Mode::Forward {
            parsed
                .attachments
                .iter()
                .filter_map(|a| {
                    Some(crate::compose::Attached {
                        name: a.name.clone(),
                        mime: a.mime.clone(),
                        data: opened.attachments.get(a.index)?.clone(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        self.composer = Some(super::Composer::from_fields(from, fields, attachments));
        self.screen = super::Screen::Mail;
        let target = if mode == Mode::Forward {
            super::composer::TO_ID
        } else {
            super::composer::BODY_ID
        };
        iced::widget::operation::focus(target)
    }

    /// The account a new message is sent from by default.
    pub(super) fn default_sender(&self) -> Option<AccountId> {
        match self.filter {
            AccountFilter::Only(id) if self.account(id).is_some() => Some(id),
            _ => self.accounts.first().map(|a| a.id),
        }
    }

    fn save_attachment(&mut self, index: usize) -> Task<Message> {
        let Some(Reading {
            body: Body::Ready { opened, .. },
            ..
        }) = &self.reading
        else {
            return Task::none();
        };
        let Some(attachment) = opened.parsed.attachments.get(index) else {
            return Task::none();
        };
        let Some(data) = opened.attachments.get(attachment.index).cloned() else {
            return self.toast("Pièce jointe illisible", true);
        };
        let name = attachment.name.clone();
        Task::perform(save_file(name, data), Message::AttachmentSaved)
    }
}

/// Parses a raw body off the UI thread; the result comes back as
/// `Message::BodyParsed`.
fn parse_task(row: i64, raw: Vec<u8>) -> Task<Message> {
    Task::perform(background::parse_message(raw), move |result| {
        Message::BodyParsed(row, result.map(Arc::new))
    })
}

/// The reader's state for a parsed body.
fn ready_body(opened: Arc<OpenedBody>) -> Body {
    let text = if opened.parsed.text.trim().is_empty() {
        "(Ce message ne contient pas de texte.)".to_string()
    } else {
        opened.parsed.text.trim_end().to_string()
    };
    Body::Ready {
        content: text_editor::Content::with_text(&text),
        opened,
    }
}

/// Asks where to save, then writes the file.
async fn save_file(name: String, data: Arc<Vec<u8>>) -> Result<Option<String>, String> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_title("Enregistrer la pièce jointe")
        .set_file_name(&name)
        .save_file()
        .await
    else {
        return Ok(None);
    };
    handle
        .write(&data)
        .await
        .map(|()| Some(handle.file_name()))
        .map_err(|e| e.to_string())
}

/// Groups message locations by account.
pub fn by_account(refs: Vec<MessageRef>) -> BTreeMap<AccountId, Vec<MessageRef>> {
    let mut groups: BTreeMap<AccountId, Vec<MessageRef>> = BTreeMap::new();
    for r in refs {
        groups.entry(r.account_id).or_default().push(r);
    }
    groups
}
