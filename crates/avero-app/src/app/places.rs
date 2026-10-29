//! Where the list looks besides plain folders, and what sorts mail into
//! them: the tabs of the inbox (Important, Newsletters, Notifications),
//! "Favoris" (starred mail of every folder) and "Mails de X" (one
//! sender's mail, every account and folder). Also the star itself and
//! moving a conversation to another tab by hand.

use super::{App, Message, Screen};
use crate::worker::Job;
use avero_core::{Category, FolderRole, Listing};
use iced::Task;

/// "Mails de X": one sender's messages, shown over the folder that was
/// open (the "Retour" button goes back to it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderView {
    pub addr: String,
    /// The name shown in the title (the address when there is none).
    pub name: String,
}

/// French name of a tab of the inbox.
pub fn tab_name(category: Category) -> &'static str {
    match category {
        Category::Important => "Important",
        Category::Newsletter => "Newsletters",
        Category::Notification => "Notifications",
    }
}

impl App {
    /// What the list shows (when no search is running).
    pub fn listing(&self) -> Listing {
        if let Some(sender) = &self.sender {
            Listing::Sender(sender.addr.clone())
        } else if self.snoozed {
            Listing::Snoozed
        } else if self.starred {
            Listing::Starred
        } else if self.tabs_shown() {
            Listing::Tab(self.tab)
        } else {
            Listing::Folder(self.folder)
        }
    }

    /// Whether the list shows a folder as it is (not a search, the
    /// favourites, "Plus tard", "Programmés" or a sender's mail).
    pub fn in_folder(&self) -> bool {
        self.search_query.is_none()
            && !self.starred
            && !self.snoozed
            && !self.scheduled
            && self.sender.is_none()
    }

    /// Whether the list shows "Programmés" (the mail to send later).
    pub fn scheduled_shown(&self) -> bool {
        self.scheduled && self.search_query.is_none() && self.sender.is_none()
    }

    /// Whether the tabs are above the list: the inbox, with the setting
    /// on.
    pub fn tabs_shown(&self) -> bool {
        self.settings.inbox_tabs && self.folder == FolderRole::Inbox && self.in_folder()
    }

    /// The folder role a conversation opened from the list is read in:
    /// outside a folder, every folder but the trash and spam ones.
    pub(super) fn reading_role(&self) -> FolderRole {
        if self.starred || self.snoozed || self.sender.is_some() {
            FolderRole::Inbox
        } else {
            self.folder
        }
    }

    pub(super) fn update_places(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::StarredSelected => {
                self.starred = true;
                self.snoozed = false;
                self.scheduled = false;
                self.sender = None;
                self.search_text.clear();
                self.search_query = None;
                self.screen = Screen::Mail;
                self.load_first_page();
                Task::none()
            }
            Message::TabSelected(category) => {
                self.tab = category;
                self.load_first_page();
                Task::none()
            }
            Message::TabsToggled(on) => {
                self.settings.inbox_tabs = on;
                self.save_settings();
                self.load_first_page();
                Task::none()
            }
            Message::ShowSender(addr, name) => {
                let name = if name.trim().is_empty() {
                    addr.clone()
                } else {
                    name
                };
                self.sender = Some(SenderView { addr, name });
                self.search_text.clear();
                self.search_query = None;
                self.screen = Screen::Mail;
                self.load_first_page();
                Task::none()
            }
            Message::SenderBack => {
                self.sender = None;
                self.load_first_page();
                Task::none()
            }
            Message::ToggleStar => self.toggle_star(),
            Message::StarLine(index) => {
                let Some(item) = self.items.get(index) else {
                    return Task::none();
                };
                let rows = item.row_ids.clone();
                let star = !item.flagged;
                self.set_star(rows, star)
            }
            Message::Reclassify(category) => self.reclassify(category),
            _ => Task::none(),
        }
    }

    /// `s` or the star of the reader: stars the lines acted on, or
    /// unstars them when they all are starred already.
    fn toggle_star(&mut self) -> Task<Message> {
        let items = self.action_items();
        if items.is_empty() {
            return Task::none();
        }
        let star = !items.iter().all(|item| item.flagged);
        let rows = self.action_rows();
        self.set_star(rows, star)
    }

    /// Stars or unstars rows, locally first, then on each account's
    /// server (one job per account). Unstarring a conversation also
    /// unstars its starred messages of other folders, so that it leaves
    /// "Favoris".
    fn set_star(&mut self, mut rows: Vec<i64>, star: bool) -> Task<Message> {
        if rows.is_empty() {
            return Task::none();
        }
        if !star {
            let others = self
                .store
                .starred_in_threads(&rows, self.scope())
                .unwrap_or_default();
            rows.extend(others);
        }
        if let Err(e) = self.store.set_flagged(&rows, star) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        self.reload();
        let groups = super::mail::by_account(self.refs(&rows));
        Task::batch(groups.into_iter().map(|(account, targets)| {
            self.request(account, |reply| Job::SetFlagged {
                targets,
                flagged: star,
                reply,
            })
        }))
    }

    /// "Classer dans": moves the conversation being read to another tab,
    /// and remembers its sender for the mail to come.
    fn reclassify(&mut self, category: Category) -> Task<Message> {
        let Some(reading) = &mut self.reading else {
            return Task::none();
        };
        if reading.category == Some(category) {
            return Task::none();
        }
        let sender = reading.sender.clone();
        let rows: Vec<i64> = reading
            .entries
            .iter()
            .flat_map(|entry| entry.item.row_ids.iter().copied())
            .chain(reading.rows.iter().copied())
            .collect();
        reading.category = Some(category);
        if let Err(e) = self.store.set_sender_category(&sender, category, &rows) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        // The line leaves the tab shown: the reader closes with it.
        self.reload();
        let text = format!(
            "Classé dans « {} » ; les prochains messages de {} aussi",
            tab_name(category),
            sender
        );
        self.toast(text, false)
    }
}
