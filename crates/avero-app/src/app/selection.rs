//! Several lines picked at once: Ctrl+click adds or removes one, Maj+click
//! takes a range, `x` toggles the keyboard's line, "Tout sélectionner"
//! takes every loaded line. While lines are checked, the actions apply to
//! all of them and the right pane lists what can be done.

use super::{App, Message};
use avero_core::{AccountId, UnifiedItem};
use iced::Task;

impl App {
    /// A click on line `index`: with Ctrl it is added or removed, with
    /// Maj the lines from the last one clicked are taken; a plain click
    /// opens it and forgets the checked lines.
    pub(super) fn click_line(&mut self, index: usize) -> Task<Message> {
        if index >= self.items.len() {
            return Task::none();
        }
        if self.modifiers.shift() {
            self.check_range(index);
            Task::none()
        } else if self.modifiers.command() {
            self.toggle_line(index);
            Task::none()
        } else {
            self.clear_checked();
            self.open(index)
        }
    }

    /// Ctrl+click: adds the line, or removes it. The line being read
    /// joins the first one, as in a file manager.
    fn toggle_line(&mut self, index: usize) {
        let rows = self.items[index].row_ids.clone();
        if self.checked.is_empty()
            && let Some(current) = self.selected_index()
            && current != index
        {
            self.checked.push(self.items[current].row_ids.clone());
        }
        self.toggle_rows(rows.clone());
        self.anchor = Some(rows.clone());
        self.move_cursor(rows);
    }

    /// The checkbox of line `index`: checks or unchecks that line only
    /// (the line being read does not join it, unlike Ctrl+click).
    pub(super) fn check_line(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let rows = item.row_ids.clone();
        self.toggle_rows(rows.clone());
        self.anchor = Some(rows.clone());
        self.move_cursor(rows);
    }

    /// `x`: checks or unchecks the keyboard's line.
    pub(super) fn toggle_current(&mut self) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let rows = self.items[index].row_ids.clone();
        self.toggle_rows(rows.clone());
        self.anchor = Some(rows);
        self.reading = None;
    }

    /// Maj+click: checks the lines from the anchor (the last line clicked,
    /// or the one read) to this one, and only them.
    fn check_range(&mut self, index: usize) {
        let start = self
            .anchor
            .as_ref()
            .and_then(|rows| self.items.iter().position(|i| &i.row_ids == rows))
            .or(self.selected_index())
            .unwrap_or(index);
        let (low, high) = (start.min(index), start.max(index));
        self.checked = self.items[low..=high]
            .iter()
            .map(|i| i.row_ids.clone())
            .collect();
        if self.anchor.is_none() {
            self.anchor = Some(self.items[start].row_ids.clone());
        }
        let rows = self.items[index].row_ids.clone();
        self.move_cursor(rows);
    }

    /// "Tout sélectionner": every loaded line, or none.
    pub(super) fn select_all(&mut self, on: bool) {
        if on {
            self.checked = self.items.iter().map(|i| i.row_ids.clone()).collect();
            self.reading = None;
        } else {
            self.clear_checked();
        }
    }

    /// Forgets the checked lines.
    pub(super) fn clear_checked(&mut self) {
        self.checked.clear();
        self.anchor = None;
    }

    /// After the list was read again: lines that left it are no longer
    /// checked.
    pub(super) fn prune_checked(&mut self) {
        let items = &self.items;
        self.checked
            .retain(|rows| items.iter().any(|i| &i.row_ids == rows));
        if self.checked.is_empty() {
            self.anchor = None;
        }
    }

    fn toggle_rows(&mut self, rows: Vec<i64>) {
        match self.checked.iter().position(|r| *r == rows) {
            Some(at) => {
                self.checked.remove(at);
            }
            None => self.checked.push(rows),
        }
    }

    /// The keyboard's line moves without opening anything (the right pane
    /// lists the checked lines).
    pub(super) fn move_cursor(&mut self, rows: Vec<i64>) {
        self.selected = Some(rows);
        self.reading = None;
    }

    /// Whether this line is checked.
    pub fn is_checked(&self, item: &UnifiedItem) -> bool {
        self.checked.contains(&item.row_ids)
    }

    /// Whether a line of the list is a conversation (else a message).
    pub fn lines_are_threads(&self) -> bool {
        self.settings.group_threads && self.search_query.is_none() && self.sender.is_none()
    }

    /// Whether every loaded line is checked.
    pub fn all_checked(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| self.is_checked(i))
    }

    /// The lines the actions apply to: the checked ones, else the one
    /// selected.
    pub fn action_items(&self) -> Vec<&UnifiedItem> {
        if self.checked.is_empty() {
            self.selected_item().into_iter().collect()
        } else {
            self.items.iter().filter(|i| self.is_checked(i)).collect()
        }
    }

    /// The rows of [`App::action_items`].
    pub fn action_rows(&self) -> Vec<i64> {
        self.action_items()
            .into_iter()
            .flat_map(|i| i.row_ids.iter().copied())
            .collect()
    }

    /// The accounts of [`App::action_items`], each once.
    pub fn action_accounts(&self) -> Vec<AccountId> {
        let mut accounts: Vec<AccountId> = self
            .action_items()
            .into_iter()
            .flat_map(|i| i.accounts.iter().copied())
            .collect();
        accounts.sort();
        accounts.dedup();
        accounts
    }
}
