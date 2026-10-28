//! The message list, the reading pane and the actions on a message.

use super::{App, Body, Message, PAGE, Reading};
use crate::compose::Mode;
use crate::worker::{Destination, Job};
use avero_core::{AccountFilter, AccountId, FolderRole, MessageRef, UnifiedItem};
use avero_sync::parse::{attachment_data, parse_body};
use iced::Task;
use iced::widget::text_editor;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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
        let body = match self.store.body(row) {
            Ok(Some((raw, _))) => ready_body(raw),
            _ => {
                tasks.push(self.fetch_body(row));
                Body::Loading
            }
        };
        self.reading = Some(Reading {
            rows: item.row_ids.clone(),
            accounts: item.accounts.clone(),
            body,
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

    /// Called when a worker cached a body.
    pub(super) fn body_arrived(&mut self, row: i64, result: Result<(), String>) {
        let Some(reading) = &mut self.reading else {
            return;
        };
        if reading.rows.first() != Some(&row) {
            return;
        }
        reading.body = match result {
            Ok(()) => match self.store.body(row) {
                Ok(Some((raw, _))) => ready_body(raw),
                _ => Body::Failed("Le message n'a pas pu être lu".to_string()),
            },
            Err(e) => Body::Failed(e),
        };
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

    /// Whether "Archiver" can be used on the selection.
    pub fn can_archive(&self) -> bool {
        let Some(item) = self.selected_item() else {
            return false;
        };
        self.folder != FolderRole::Archive
            && item
                .accounts
                .iter()
                .any(|id| self.status.get(id).and_then(|s| s.archive) != Some(false))
    }

    /// Whether "Supprimer" can be used on the selection.
    pub fn can_delete(&self) -> bool {
        self.selected.is_some() && (self.folder != FolderRole::Trash || self.search_query.is_some())
    }

    /// Archive or delete: removes the rows locally at once, selects the
    /// next message, then moves them on the servers.
    fn move_selected(&mut self, to: Destination) -> Task<Message> {
        let allowed = match to {
            Destination::Archive => self.can_archive(),
            Destination::Trash => self.can_delete(),
        };
        let (Some(rows), Some(index)) = (self.selected.clone(), self.selected_index()) else {
            return Task::none();
        };
        if !allowed {
            return Task::none();
        }
        let targets = self.refs(&rows);
        if let Err(e) = self.store.delete_messages(&rows) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        self.selected = None;
        self.reading = None;
        self.reload();
        let mut tasks: Vec<Task<Message>> = by_account(targets)
            .into_iter()
            .map(|(account, targets)| {
                self.request(account, |reply| Job::Move { targets, to, reply })
            })
            .collect();
        if !self.items.is_empty() {
            tasks.push(self.open(index.min(self.items.len() - 1)));
        }
        tasks.push(self.toast(super::destination_label(to), false));
        Task::batch(tasks)
    }

    fn answer(&mut self, mode: Mode) -> Task<Message> {
        let Some(Reading {
            body: Body::Ready { parsed, raw, .. },
            accounts,
            ..
        }) = &self.reading
        else {
            return Task::none();
        };
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
                        data: Arc::new(attachment_data(raw, a.index)?),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        self.composer = Some(super::Composer::from_fields(from, fields, attachments));
        self.screen = super::Screen::Mail;
        Task::none()
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
            body: Body::Ready { parsed, raw, .. },
            ..
        }) = &self.reading
        else {
            return Task::none();
        };
        let Some(attachment) = parsed.attachments.get(index) else {
            return Task::none();
        };
        let Some(data) = attachment_data(raw, attachment.index) else {
            return self.toast("Pièce jointe illisible", true);
        };
        let name = attachment.name.clone();
        Task::perform(save_file(name, data), Message::AttachmentSaved)
    }
}

/// Parses a cached body for the reader.
fn ready_body(raw: Vec<u8>) -> Body {
    let parsed = parse_body(&raw);
    let text = if parsed.text.trim().is_empty() {
        "(Ce message ne contient pas de texte.)".to_string()
    } else {
        parsed.text.trim_end().to_string()
    };
    Body::Ready {
        content: text_editor::Content::with_text(&text),
        parsed: Box::new(parsed),
        raw: Arc::new(raw),
    }
}

/// Asks where to save, then writes the file.
async fn save_file(name: String, data: Vec<u8>) -> Result<Option<String>, String> {
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
