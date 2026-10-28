//! Spaces: named groups of accounts ("Perso", "Pro"). The active space
//! limits the list, the counters, the tabs, "Favoris", "Plus tard",
//! "Programmés", "Grand nettoyage" and the search to its accounts; "Tous"
//! shows every account. The account chips filter inside the space.

use super::{App, Message, Overlay, Screen};
use avero_core::{Account, AccountColor, AccountFilter, AccountId, Space, SpaceId};
use iced::Task;

/// The icons a space can take, by the name stored in the database.
pub const SPACE_ICONS: [&str; 8] = [
    "house",
    "briefcase",
    "users",
    "heart",
    "star",
    "bike",
    "book-open",
    "globe",
];

/// Most characters in a space's name.
const NAME_MAX: usize = 40;

/// A space being created or changed in the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceForm {
    /// `None` for a new space.
    pub id: Option<SpaceId>,
    pub name: String,
    pub icon: String,
    pub color: usize,
    pub accounts: Vec<AccountId>,
    pub error: Option<String>,
}

impl SpaceForm {
    /// A new space: the next colour, every account ticked.
    pub fn new(count: usize, accounts: &[Account]) -> SpaceForm {
        SpaceForm {
            id: None,
            name: String::new(),
            icon: SPACE_ICONS[count % SPACE_ICONS.len()].to_string(),
            color: count % AccountColor::COUNT,
            accounts: accounts.iter().map(|a| a.id).collect(),
            error: None,
        }
    }

    /// The form of an existing space.
    pub fn of(space: &Space) -> SpaceForm {
        SpaceForm {
            id: Some(space.id),
            name: space.name.clone(),
            icon: space.icon.clone(),
            color: space.color,
            accounts: space.accounts.clone(),
            error: None,
        }
    }
}

/// Messages of the spaces (the selector and the settings).
#[derive(Debug, Clone)]
pub enum SpaceMsg {
    /// Shows a space, or every account (`None`, "Tous").
    Select(Option<SpaceId>),
    /// Ctrl+0 to Ctrl+9: 0 is "Tous", 1 to 9 the spaces in order.
    Number(u8),
    /// "Nouvel espace" in the settings.
    New,
    Edit(SpaceId),
    Name(String),
    Icon(String),
    Color(usize),
    /// Ticks or unticks an account of the space being edited.
    Account(AccountId, bool),
    Save,
    /// Moves a space up (`-1`) or down (`1`) in the order.
    Move(SpaceId, i32),
    AskDelete(SpaceId),
    ConfirmDelete,
}

impl App {
    pub(super) fn update_spaces(&mut self, message: SpaceMsg) -> Task<Message> {
        match message {
            SpaceMsg::Select(space) => {
                self.select_space(space);
                Task::none()
            }
            SpaceMsg::Number(0) => {
                self.select_space(None);
                Task::none()
            }
            SpaceMsg::Number(n) => {
                if let Some(space) = self.spaces.get(usize::from(n) - 1) {
                    let id = space.id;
                    self.select_space(Some(id));
                }
                Task::none()
            }
            SpaceMsg::New => {
                self.overlay = Some(Overlay::SpaceEditor(SpaceForm::new(
                    self.spaces.len(),
                    &self.accounts,
                )));
                Task::none()
            }
            SpaceMsg::Edit(id) => {
                if let Some(space) = self.spaces.iter().find(|s| s.id == id) {
                    self.overlay = Some(Overlay::SpaceEditor(SpaceForm::of(space)));
                }
                Task::none()
            }
            SpaceMsg::Name(_) | SpaceMsg::Icon(_) | SpaceMsg::Color(_) | SpaceMsg::Account(..) => {
                if let Some(Overlay::SpaceEditor(form)) = &mut self.overlay {
                    form.error = None;
                    match message {
                        SpaceMsg::Name(name) => form.name = name.chars().take(NAME_MAX).collect(),
                        SpaceMsg::Icon(icon) => form.icon = icon,
                        SpaceMsg::Color(color) => form.color = color % AccountColor::COUNT,
                        SpaceMsg::Account(id, true) if !form.accounts.contains(&id) => {
                            form.accounts.push(id);
                        }
                        SpaceMsg::Account(id, false) => form.accounts.retain(|a| *a != id),
                        _ => {}
                    }
                }
                Task::none()
            }
            SpaceMsg::Save => self.save_space(),
            SpaceMsg::Move(id, step) => {
                if let Err(e) = self.store.move_space(id, step) {
                    return self.toast(format!("Erreur de la base locale : {e}"), true);
                }
                self.refresh_spaces();
                Task::none()
            }
            SpaceMsg::AskDelete(id) => {
                if let Some(space) = self.spaces.iter().find(|s| s.id == id) {
                    self.overlay = Some(Overlay::ConfirmDeleteSpace {
                        space: id,
                        name: space.name.clone(),
                    });
                }
                Task::none()
            }
            SpaceMsg::ConfirmDelete => {
                let Some(Overlay::ConfirmDeleteSpace { space, .. }) = self.overlay.take() else {
                    return Task::none();
                };
                if let Err(e) = self.store.delete_space(space) {
                    return self.toast(format!("Erreur de la base locale : {e}"), true);
                }
                self.refresh_spaces();
                self.toast("Espace supprimé (ses comptes restent)", false)
            }
        }
    }

    /// "Enregistrer" in the space editor.
    fn save_space(&mut self) -> Task<Message> {
        let Some(Overlay::SpaceEditor(form)) = &mut self.overlay else {
            return Task::none();
        };
        let name = form.name.trim().to_string();
        if name.is_empty() {
            form.error = Some("Donnez un nom à l'espace".to_string());
            return Task::none();
        }
        let form = form.clone();
        let saved = match form.id {
            Some(id) => self
                .store
                .update_space(id, &name, &form.icon, form.color)
                .and_then(|()| self.store.set_space_accounts(id, &form.accounts)),
            None => self
                .store
                .add_space(&name, &form.icon, form.color, &form.accounts)
                .map(|_| ()),
        };
        if let Err(e) = saved {
            if let Some(Overlay::SpaceEditor(form)) = &mut self.overlay {
                form.error = Some(format!("Erreur de la base locale : {e}"));
            }
            return Task::none();
        }
        self.overlay = None;
        self.refresh_spaces();
        Task::none()
    }

    /// Reads the spaces again (after a change); the list follows when the
    /// active space changed or went away.
    pub(super) fn refresh_spaces(&mut self) {
        self.spaces = self.store.spaces().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les espaces : {e}");
            Vec::new()
        });
        let gone = self
            .space
            .is_some_and(|id| !self.spaces.iter().any(|s| s.id == id));
        if gone {
            self.space = None;
            self.settings.active_space = None;
            self.save_settings();
        }
        self.space_changed();
    }

    /// Shows a space (`None`: every account) and remembers it.
    pub(super) fn select_space(&mut self, space: Option<SpaceId>) {
        let space = space.filter(|id| self.spaces.iter().any(|s| s.id == *id));
        self.space = space;
        self.settings.active_space = space.map(|id| id.0);
        self.save_settings();
        self.space_changed();
    }

    /// Everything that depends on the active space is read again. An
    /// account chip outside the space is let go.
    fn space_changed(&mut self) {
        if let AccountFilter::Only(id) = self.filter
            && !self.in_space(id)
        {
            self.filter = AccountFilter::All;
        }
        if let AccountFilter::Only(id) = self.cleanup.filter
            && !self.in_space(id)
        {
            self.cleanup.filter = AccountFilter::All;
        }
        self.refresh_outbox();
        self.load_first_page();
        if self.screen == Screen::Cleanup {
            self.cleanup.senders.clear();
            self.refresh_cleanup();
        }
    }

    /// The active space, if one is chosen.
    pub fn active_space(&self) -> Option<&Space> {
        let id = self.space?;
        self.spaces.iter().find(|s| s.id == id)
    }

    /// Whether the active space shows this account (always without one).
    pub fn in_space(&self, account: AccountId) -> bool {
        self.active_space()
            .is_none_or(|space| space.contains(account))
    }

    /// The accounts of the active space (all of them without one), in
    /// the usual order.
    pub fn space_accounts(&self) -> Vec<&Account> {
        self.accounts
            .iter()
            .filter(|a| self.in_space(a.id))
            .collect()
    }

    /// Which accounts the list, the counters and the search take: the
    /// account chip chosen, else the active space, else all of them.
    pub fn scope(&self) -> AccountFilter {
        Self::scope_of(self.filter, self.space)
    }

    /// Like `scope`, for "Grand nettoyage" (which has its own chips).
    pub fn cleanup_scope(&self) -> AccountFilter {
        Self::scope_of(self.cleanup.filter, self.space)
    }

    fn scope_of(chip: AccountFilter, space: Option<SpaceId>) -> AccountFilter {
        match (chip, space) {
            (AccountFilter::Only(id), _) => AccountFilter::Only(id),
            (_, Some(space)) => AccountFilter::Space(space),
            _ => AccountFilter::All,
        }
    }

    /// Unread conversations (or messages) of the inbox in a space (`None`:
    /// every account), from the per-account counters.
    pub fn space_unread(&self, space: Option<SpaceId>) -> usize {
        let space = space.and_then(|id| self.spaces.iter().find(|s| s.id == id));
        self.unread
            .iter()
            .filter(|(id, _)| space.is_none_or(|s| s.contains(**id)))
            .map(|(_, count)| count)
            .sum()
    }
}
