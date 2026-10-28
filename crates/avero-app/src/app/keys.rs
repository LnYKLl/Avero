//! Keyboard shortcuts.
//!
//! The keyboard subscription only receives the keys no widget used: a
//! focused text field (search, composer, dialog) keeps the letters typed
//! in it, so the one-key shortcuts never fire while typing. As a second
//! guard they are also off while the composer or a dialog is open, where
//! a click outside the fields would otherwise give them back.

use super::{AccountMsg, AddMsg, App, ComposeMsg, Message, Overlay, SEARCH_ID, Screen};
use crate::compose::Mode;
use avero_core::FolderRole;
use iced::{Task, keyboard};
use std::time::{Duration, Instant};

/// How long `g` waits for the folder letter.
pub const G_TIMEOUT: Duration = Duration::from_millis(1500);

/// Every shortcut, as the help (`?`) lists them: keys, then what they do.
pub const SHORTCUTS: [(&str, &str); 17] = [
    ("J  ou  ↓", "Ligne suivante"),
    ("K  ou  ↑", "Ligne précédente"),
    ("Entrée", "Ouvrir"),
    ("R  ou  Ctrl+R", "Répondre"),
    ("A", "Répondre à tous"),
    ("F", "Transférer"),
    ("C  ou  Ctrl+N", "Nouveau message"),
    ("E", "Archiver"),
    ("#  ou  Suppr", "Supprimer"),
    ("U", "Marquer comme non lu"),
    ("S", "Ajouter aux favoris ou les retirer"),
    ("X", "Sélectionner la ligne"),
    ("Ctrl+clic, Maj+clic", "Sélectionner plusieurs lignes"),
    ("/  ou  Ctrl+F", "Rechercher"),
    (
        "G puis I, S, D, A, T",
        "Réception, Envoyés, Brouillons, Archives, Corbeille",
    ),
    ("?", "Cette aide"),
    ("Échap", "Fermer, vider la sélection, revenir"),
];

impl App {
    pub(super) fn on_key(
        &mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) -> Task<Message> {
        self.on_key_at(key, modifiers, Instant::now())
    }

    /// A key pressed at `now` (tests choose the time).
    pub(super) fn on_key_at(
        &mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
        now: Instant,
    ) -> Task<Message> {
        use keyboard::key::Named;
        // AltGr reads as Ctrl+Alt on Windows: `#` on a French keyboard
        // must not count as a Ctrl shortcut.
        let ctrl = modifiers.command() && !modifiers.alt();
        // A `g` pressed recently waits for its letter; any key ends it.
        let after_g = self
            .pending_g
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) <= G_TIMEOUT);
        match key.as_ref() {
            keyboard::Key::Named(Named::Escape) => self.escape(),
            _ if self.overlay.is_some() => Task::none(),
            keyboard::Key::Character("n") if ctrl => self.update_composer(ComposeMsg::New),
            keyboard::Key::Character("r") if ctrl && self.composer.is_none() => {
                self.update_mail(Message::Answer(Mode::Reply))
            }
            keyboard::Key::Character("f") if ctrl => self.focus_search(),
            _ if self.composer.is_some() || ctrl || modifiers.logo() => Task::none(),
            key if after_g => match go_to(key) {
                Some(folder) => self.update_mail(Message::FolderSelected(folder)),
                None => Task::none(),
            },
            key => self.single_key(key, now),
        }
    }

    /// The one-key shortcuts.
    fn single_key(&mut self, key: keyboard::Key<&str>, now: Instant) -> Task<Message> {
        use keyboard::Key::{Character, Named};
        use keyboard::key::Named as N;
        match key {
            Character("g") => {
                self.pending_g = Some(now);
                Task::none()
            }
            Character("?") => {
                self.overlay = Some(Overlay::Shortcuts);
                Task::none()
            }
            Character("c") => self.update_composer(ComposeMsg::New),
            Character("/") => self.focus_search(),
            // The others act on the list and the reader.
            _ if self.screen != Screen::Mail => Task::none(),
            Character("j") | Named(N::ArrowDown) => self.step_selection(1),
            Character("k") | Named(N::ArrowUp) => self.step_selection(-1),
            Named(N::Enter) => self.open_current(),
            Character("r") => self.update_mail(Message::Answer(Mode::Reply)),
            Character("a") => self.update_mail(Message::Answer(Mode::ReplyAll)),
            Character("f") => self.update_mail(Message::Answer(Mode::Forward)),
            Character("e") => self.update_mail(Message::Archive),
            Character("#") | Named(N::Delete) => self.update_mail(Message::Delete),
            Character("u") => self.update_mail(Message::MarkUnread),
            Character("s") => self.update_places(Message::ToggleStar),
            Character("x") => {
                self.toggle_current();
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn focus_search(&mut self) -> Task<Message> {
        self.screen = Screen::Mail;
        iced::widget::operation::focus(SEARCH_ID)
    }

    /// Échap closes the top-most thing: dialog, composer, selection,
    /// search, "Mails de X".
    pub(super) fn escape(&mut self) -> Task<Message> {
        if self.overlay.is_some() {
            return match self.overlay {
                Some(Overlay::AddAccount(_)) => self.update_add(AddMsg::Close),
                _ => self.update_account(AccountMsg::CloseDialog),
            };
        }
        if self.composer.is_some() {
            return self.update_composer(ComposeMsg::Cancel);
        }
        if !self.checked.is_empty() {
            self.clear_checked();
            return Task::none();
        }
        if self.search_query.is_some() || !self.search_text.is_empty() {
            return self.update_mail(Message::SearchCleared);
        }
        if self.sender.is_some() {
            return self.update_places(Message::SenderBack);
        }
        Task::none()
    }
}

/// The folder of `g` followed by this key.
fn go_to(key: keyboard::Key<&str>) -> Option<FolderRole> {
    match key {
        keyboard::Key::Character("i") => Some(FolderRole::Inbox),
        keyboard::Key::Character("s") => Some(FolderRole::Sent),
        keyboard::Key::Character("d") => Some(FolderRole::Drafts),
        keyboard::Key::Character("a") => Some(FolderRole::Archive),
        keyboard::Key::Character("t") => Some(FolderRole::Trash),
        _ => None,
    }
}
