//! The message list, the reading pane and the actions on a message.

use super::{App, Body, Entry, Message, Overlay, PAGE, RELOAD_EVERY, Reading};
use crate::background::{self, OpenedBody};
use crate::compose::Mode;
use crate::worker::{Destination, Event, Job};
use avero_core::category::sender_key;
use avero_core::{
    AccountFilter, AccountId, Category, FolderRole, Listing, MessageRef, UnifiedItem,
};

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
                self.starred = false;
                self.sender = None;
                self.tab = avero_core::Category::Important;
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
            Message::MessageSelected(index) => self.click_line(index),
            Message::SelectAll(on) => {
                self.select_all(on);
                Task::none()
            }
            Message::LoadMore => {
                self.items = self.fetch(self.items.len() + PAGE);
                Task::none()
            }
            Message::ReaderAction(entry, action) => {
                // Read-only: selection and scrolling, never edits.
                if !action.is_edit()
                    && let Some(reading) = &mut self.reading
                    && let Some(Entry {
                        body: Body::Ready { content, .. },
                        ..
                    }) = reading.entries.get_mut(entry)
                {
                    content.perform(action);
                }
                Task::none()
            }
            Message::EntryToggled(entry) => self.toggle_entry(entry),
            Message::Answer(mode) => self.answer(mode),
            Message::Archive => self.move_selected(Destination::Archive),
            Message::Delete => self.move_selected(Destination::Trash),
            Message::MarkRead => self.mark_seen(true),
            Message::MarkUnread => self.mark_seen(false),
            Message::SaveAttachment(entry, index) => self.save_attachment(entry, index),
            Message::AttachmentSaved(result) => match result {
                Ok(Some(name)) => self.toast(format!("Enregistré : {name}"), false),
                Ok(None) => Task::none(),
                Err(e) => self.toast(format!("Enregistrement impossible : {e}"), true),
            },
            _ => Task::none(),
        }
    }

    /// A search replaces "Mails de X" (it looks everywhere anyway).
    fn apply_search(&mut self) {
        let query = self.search_text.trim();
        self.search_query = (!query.is_empty()).then(|| query.to_string());
        if self.search_query.is_some() {
            self.sender = None;
        }
        self.load_first_page();
    }

    /// Shows the first page for the current folder, filter and search,
    /// with nothing selected.
    pub(super) fn load_first_page(&mut self) {
        self.selected = None;
        self.clear_checked();
        self.reading = None;
        self.items = self.fetch(PAGE);
        self.refresh_unread();
    }

    /// Reads the list again (after a sync or an action), keeping the
    /// loaded pages and the selection.
    pub(super) fn reload(&mut self) {
        let pages = self.items.len().div_ceil(PAGE).max(1);
        self.items = self.fetch(pages * PAGE);
        self.follow_thread();
        self.prune_checked();
        self.refresh_unread();
    }

    /// When a message joins or leaves the conversation being read, its
    /// line in the list has other rows: the selection follows it.
    fn follow_thread(&mut self) {
        if self.selected_index().is_some() {
            return;
        }
        let Some(reading) = &mut self.reading else {
            return;
        };
        let Some(thread) = reading.thread else {
            return;
        };
        if let Some(item) = self.items.iter().find(|i| i.thread_id == Some(thread)) {
            reading.rows = item.row_ids.clone();
            reading.accounts = item.accounts.clone();
            self.selected = Some(item.row_ids.clone());
        }
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
    ///
    /// With "Regrouper les conversations" on, one item per conversation;
    /// search results and a sender's mail stay one item per message.
    fn fetch(&self, limit: usize) -> Vec<UnifiedItem> {
        let result = match &self.search_query {
            Some(query) => self.store.search(query, None, self.filter, limit),
            None => self.store.list(
                &self.listing(),
                self.filter,
                self.lines_are_threads(),
                limit,
                0,
            ),
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
                let only = AccountFilter::Only(a.id);
                let count = if self.settings.group_threads {
                    self.store.unread_threads(FolderRole::Inbox, only)
                } else {
                    self.store.unread_count(FolderRole::Inbox, only)
                };
                (a.id, count.unwrap_or(0))
            })
            .collect();
        self.tab_unread = if self.settings.inbox_tabs {
            Category::ALL
                .into_iter()
                .map(|category| {
                    let count = self.store.list_unread(
                        &Listing::Tab(category),
                        self.filter,
                        self.settings.group_threads,
                    );
                    (category, count.unwrap_or(0))
                })
                .collect()
        } else {
            Default::default()
        };
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

    /// J/K: moves the selection down (`1`) or up (`-1`). While lines are
    /// checked, only the keyboard's line moves (for `x`).
    pub(super) fn step_selection(&mut self, step: isize) -> Task<Message> {
        if self.items.is_empty() {
            return Task::none();
        }
        let next = match self.selected_index() {
            Some(i) => i.saturating_add_signed(step).min(self.items.len() - 1),
            None => 0,
        };
        if !self.checked.is_empty() {
            self.move_cursor(self.items[next].row_ids.clone());
            return Task::none();
        }
        self.open(next)
    }

    /// Entrée: opens the keyboard's line (the first one if none), and
    /// forgets the checked lines.
    pub(super) fn open_current(&mut self) -> Task<Message> {
        self.clear_checked();
        self.open(self.selected_index().unwrap_or(0))
    }

    /// Selects an item: shows its body (cached, or downloaded in the
    /// background) and marks it read, locally first. A conversation shows
    /// all its messages, the newest unfolded; the others are loaded when
    /// unfolded.
    pub(super) fn open(&mut self, index: usize) -> Task<Message> {
        let Some(item) = self.items.get(index).cloned() else {
            return Task::none();
        };
        self.selected = Some(item.row_ids.clone());
        if item.row_ids.is_empty() {
            return Task::none();
        }
        let mut messages = match item.thread_id {
            Some(thread) if item.count > 1 => self
                .store
                .thread_messages(thread, self.reading_role(), self.filter)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if messages.is_empty() {
            messages.push(item.clone());
        }
        let newest = messages.len() - 1;
        // Who the conversation is from: its newest message by someone
        // else (the user's own answers say nothing about the list).
        let own: Vec<String> = self.accounts.iter().map(|a| sender_key(&a.email)).collect();
        let from = messages
            .iter()
            .rev()
            .find(|m| !own.contains(&sender_key(&m.from_addr)))
            .unwrap_or(&messages[newest]);
        let from_row = from.row_ids.first().copied();
        let sender = from.from_addr.clone();
        let account = from.accounts.first().or(item.accounts.first()).copied();
        let category = from_row.and_then(|row| self.store.category(row).ok().flatten());
        let unsubscribe = from_row
            .and_then(|row| self.unsubscribe_offer(row, &sender, &from.from_display, account));
        self.reading = Some(Reading {
            rows: item.row_ids.clone(),
            accounts: item.accounts.clone(),
            thread: item.thread_id,
            entries: messages
                .into_iter()
                .map(|message| Entry {
                    item: message,
                    expanded: false,
                    body: Body::NotLoaded,
                })
                .collect(),
            focus: newest,
            sender,
            category,
            unsubscribe,
        });
        let mut tasks = vec![self.unfold(newest)];
        if !item.seen {
            let _ = self.store.set_seen(&item.row_ids, true);
            self.reload();
            tasks.push(self.remote_seen(&item.row_ids, true));
        }
        Task::batch(tasks)
    }

    /// Unfolds a message of the reader and makes it the one Reply and
    /// Forward answer; its body is loaded the first time.
    fn unfold(&mut self, index: usize) -> Task<Message> {
        let Some(reading) = &mut self.reading else {
            return Task::none();
        };
        let Some(entry) = reading.entries.get_mut(index) else {
            return Task::none();
        };
        entry.expanded = true;
        reading.focus = index;
        if !matches!(entry.body, Body::NotLoaded) {
            return Task::none();
        }
        let Some(&row) = entry.item.row_ids.first() else {
            return Task::none();
        };
        entry.body = Body::Loading;
        match self.store.body(row) {
            Ok(Some((raw, _))) => parse_task(row, raw),
            _ => self.fetch_body(row),
        }
    }

    /// A click on a message of the conversation: unfolds it, or folds it
    /// back (one message always stays unfolded).
    fn toggle_entry(&mut self, index: usize) -> Task<Message> {
        let Some(reading) = &mut self.reading else {
            return Task::none();
        };
        let unfolded = reading.entries.iter().filter(|e| e.expanded).count();
        match reading.entries.get_mut(index) {
            Some(entry) if entry.expanded && unfolded > 1 => {
                entry.expanded = false;
                if let Some(last) = reading.entries.iter().rposition(|e| e.expanded) {
                    reading.focus = last;
                }
                Task::none()
            }
            Some(entry) if !entry.expanded => self.unfold(index),
            _ => Task::none(),
        }
    }

    fn fetch_body(&self, row: i64) -> Task<Message> {
        let Some(target) = self.refs(&[row]).into_iter().next() else {
            return Task::none();
        };
        let account = target.account_id;
        self.request(account, |reply| Job::FetchBody { target, reply })
    }

    /// The reader entry waiting for the body of `row`, if any.
    fn entry_for(&mut self, row: i64) -> Option<&mut Entry> {
        self.reading
            .as_mut()?
            .entries
            .iter_mut()
            .find(|entry| entry.item.row_ids.first() == Some(&row))
    }

    /// Called when a worker cached a body: it is parsed off the UI thread.
    pub(super) fn body_arrived(&mut self, row: i64, result: Result<(), String>) -> Task<Message> {
        let cached = match &result {
            Ok(()) => self.store.body(row),
            Err(_) => Ok(None),
        };
        let Some(entry) = self.entry_for(row) else {
            return Task::none();
        };
        match (result, cached) {
            (Ok(()), Ok(Some((raw, _)))) => return parse_task(row, raw),
            (Ok(()), _) => entry.body = Body::Failed("Le message n'a pas pu être lu".to_string()),
            (Err(e), _) => entry.body = Body::Failed(e),
        }
        Task::none()
    }

    /// A parsed body is ready: show it if its message is still in the
    /// reader.
    pub(super) fn body_parsed(&mut self, row: i64, result: Result<Arc<OpenedBody>, String>) {
        let Some(entry) = self.entry_for(row) else {
            return;
        };
        entry.body = match result {
            Ok(opened) => ready_body(opened),
            Err(e) => Body::Failed(e),
        };
    }

    /// A worker lost the jobs of the messages being loaded.
    pub(super) fn body_lost(&mut self) {
        let Some(reading) = &mut self.reading else {
            return;
        };
        for entry in &mut reading.entries {
            if matches!(entry.body, Body::Loading) {
                entry.body = Body::Failed("Le message n'a pas pu être chargé".to_string());
            }
        }
    }

    pub(super) fn refs(&self, rows: &[i64]) -> Vec<MessageRef> {
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

    /// Marks the selection read or unread, locally first, then on each
    /// account's server (one job per account).
    fn mark_seen(&mut self, seen: bool) -> Task<Message> {
        let rows = self.action_rows();
        if rows.is_empty() {
            return Task::none();
        }
        let _ = self.store.set_seen(&rows, seen);
        self.reload();
        self.remote_seen(&rows, seen)
    }

    /// Whether "Archiver" can be used on the selection: some account of
    /// its messages has a known archive folder.
    pub fn can_archive(&self) -> bool {
        !self.action_rows().is_empty() && self.archive_problem().is_none()
    }

    /// Why "Archiver" cannot be used, for its tooltip.
    pub fn archive_problem(&self) -> Option<&'static str> {
        let accounts = self.action_accounts();
        if accounts.is_empty() {
            return None;
        }
        if self.folder == FolderRole::Archive && self.in_folder() {
            return Some("Déjà dans les archives");
        }
        let known: Vec<Option<bool>> = accounts
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
        !self.action_rows().is_empty() && self.delete_problem().is_none()
    }

    /// Why "Supprimer" cannot be used, for its tooltip.
    pub fn delete_problem(&self) -> Option<&'static str> {
        let accounts = self.action_accounts();
        if accounts.is_empty() {
            return None;
        }
        if accounts.iter().any(|id| self.trash_accounts.contains(id)) {
            None
        } else if accounts
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
        let refs = self.refs(&self.action_rows());
        !refs.is_empty() && refs.iter().all(|r| self.is_trash_folder(r))
    }

    fn is_trash_folder(&self, target: &MessageRef) -> bool {
        matches!(
            self.store.folder_by_role(target.account_id, FolderRole::Trash),
            Ok(Some(folder)) if folder.id == target.folder_id
        )
    }

    /// Archive or delete (the checked lines, else the one read): hides
    /// the rows at once and selects the next message; the rows are only removed when the server confirms (shown
    /// again otherwise). Messages already in the trash are deleted for
    /// good, after a confirmation.
    fn move_selected(&mut self, to: Destination) -> Task<Message> {
        let allowed = match to {
            Destination::Archive => self.can_archive(),
            Destination::Trash => self.can_delete(),
        };
        let rows = self.action_rows();
        if rows.is_empty() || !allowed {
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

    /// Reply, reply all or forward: answers the focused message of the
    /// reader (the newest of a conversation, or the last one unfolded),
    /// from the account that received it.
    fn answer(&mut self, mode: Mode) -> Task<Message> {
        let Some(reading) = &self.reading else {
            return Task::none();
        };
        let entry = reading.focused();
        let Body::Ready { opened, .. } = &entry.body else {
            return Task::none();
        };
        let parsed = &opened.parsed;
        let own: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        let fields = crate::compose::answer(parsed, mode, &own);
        let from = entry
            .item
            .accounts
            .first()
            .or(reading.accounts.first())
            .copied()
            .or_else(|| self.default_sender());
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
        let signature = self.signature_of(from);
        self.composer = Some(super::Composer::from_fields(
            from,
            fields,
            attachments,
            &signature,
        ));
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

    fn save_attachment(&mut self, entry: usize, index: usize) -> Task<Message> {
        let Some(Entry {
            body: Body::Ready { opened, .. },
            ..
        }) = self.reading.as_ref().and_then(|r| r.entries.get(entry))
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
