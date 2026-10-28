//! "Grand nettoyage": the senders who fill the mailbox the most, and what
//! can be done about each of them (everything to the trash, unsubscribe,
//! block their future mail).

use super::{App, Message, Overlay, Screen};
use crate::worker::Destination;
use avero_core::rules::{self, block_rule};
use avero_core::{AccountFilter, SenderStats};
use iced::Task;
use std::collections::HashSet;

/// How many senders one page of the screen holds.
pub const CLEANUP_PAGE: usize = 50;

/// The state of the "Grand nettoyage" screen.
#[derive(Debug, Clone)]
pub struct Cleanup {
    /// The accounts counted (its own filter, apart from the mail list's).
    pub filter: AccountFilter,
    /// The senders loaded, most messages first.
    pub senders: Vec<SenderStats>,
    /// Whether more senders follow.
    pub more: bool,
    /// The senders unsubscribing was asked from.
    pub requested: HashSet<String>,
}

impl Default for Cleanup {
    fn default() -> Cleanup {
        Cleanup {
            filter: AccountFilter::All,
            senders: Vec::new(),
            more: false,
            requested: HashSet::new(),
        }
    }
}

/// Messages of the "Grand nettoyage" screen.
#[derive(Debug, Clone)]
pub enum CleanupMsg {
    Filter(AccountFilter),
    /// "Voir plus".
    More,
    /// "Tout mettre à la corbeille" for a sender (asks first).
    TrashAll(String),
    ConfirmTrash,
    Unsubscribe(String),
    Block(String),
}

impl App {
    pub(super) fn update_cleanup(&mut self, message: CleanupMsg) -> Task<Message> {
        match message {
            CleanupMsg::Filter(filter) => {
                self.cleanup.filter = filter;
                self.cleanup.senders.clear();
                self.refresh_cleanup();
                Task::none()
            }
            CleanupMsg::More => {
                let shown = self.cleanup.senders.len();
                self.load_senders(shown + CLEANUP_PAGE);
                Task::none()
            }
            CleanupMsg::TrashAll(addr) => {
                let rows = self
                    .store
                    .sender_rows(&addr, self.cleanup.filter)
                    .unwrap_or_default();
                if rows.is_empty() {
                    return Task::none();
                }
                let name = self.sender_name(&addr);
                self.overlay = Some(Overlay::ConfirmTrashSender { name, rows });
                Task::none()
            }
            CleanupMsg::ConfirmTrash => {
                let Some(Overlay::ConfirmTrashSender { rows, .. }) = self.overlay.take() else {
                    return Task::none();
                };
                let count = rows.len();
                match self.move_rows(&rows, Destination::Trash) {
                    Some(task) => {
                        let toast = self.toast(
                            if count > 1 {
                                format!("{count} mails placés dans la corbeille")
                            } else {
                                "1 mail placé dans la corbeille".to_string()
                            },
                            false,
                        );
                        Task::batch([task, toast])
                    }
                    None => self.toast("Aucune corbeille sur ces comptes", true),
                }
            }
            CleanupMsg::Unsubscribe(addr) => {
                let found = self
                    .store
                    .latest_unsubscribable(&addr, self.cleanup.filter)
                    .ok()
                    .flatten();
                let name = self.sender_name(&addr);
                let offer = found.and_then(|(row, account)| {
                    self.unsubscribe_offer(row, &addr, &name, Some(account))
                });
                match offer {
                    Some(offer) => {
                        self.overlay = Some(Overlay::ConfirmUnsubscribe(offer));
                        Task::none()
                    }
                    None => self.toast("Ces mails n'offrent pas de désinscription", true),
                }
            }
            CleanupMsg::Block(addr) => {
                if self.is_blocked(&addr) {
                    return self.toast("Cet expéditeur est déjà bloqué", false);
                }
                let name = self.sender_name(&addr);
                if let Err(e) = self.store.add_rule(&block_rule(&addr, &name)) {
                    return self.toast(format!("Erreur de la base locale : {e}"), true);
                }
                self.refresh_rules();
                self.toast(
                    "Bloqué : ses prochains mails iront à la corbeille (règle dans les Réglages)",
                    false,
                )
            }
        }
    }

    /// Opens the screen with the first page.
    pub(super) fn show_cleanup(&mut self) {
        self.screen = Screen::Cleanup;
        self.cleanup.senders.clear();
        self.refresh_cleanup();
    }

    /// Reads the loaded pages again (after a change).
    pub(super) fn refresh_cleanup(&mut self) {
        let shown = self.cleanup.senders.len().max(CLEANUP_PAGE);
        self.load_senders(shown.div_ceil(CLEANUP_PAGE) * CLEANUP_PAGE);
    }

    fn load_senders(&mut self, limit: usize) {
        // One more than asked tells whether another page follows.
        let mut senders = self
            .store
            .sender_stats(self.cleanup.filter, limit + 1, 0)
            .unwrap_or_else(|e| {
                eprintln!("Impossible de compter les expéditeurs : {e}");
                Vec::new()
            });
        self.cleanup.more = senders.len() > limit;
        senders.truncate(limit);
        self.cleanup.requested = senders
            .iter()
            .filter(|s| matches!(self.store.unsubscribed_at(&s.addr), Ok(Some(_))))
            .map(|s| s.addr.clone())
            .collect();
        self.cleanup.senders = senders;
    }

    /// The name shown for a sender of the screen (else the address).
    fn sender_name(&self, addr: &str) -> String {
        self.cleanup
            .senders
            .iter()
            .find(|s| s.addr == addr)
            .map_or_else(|| addr.to_string(), |s| s.name.clone())
    }

    /// Whether a rule already sends this sender's mail to the trash.
    pub fn is_blocked(&self, addr: &str) -> bool {
        self.rules.iter().any(|r| rules::blocks(&r.def, addr))
    }
}
