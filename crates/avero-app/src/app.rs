//! State of the main window and how it reacts to messages.

use crate::view;
use crate::worker::spawn_sync;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{Account, AccountFilter, AccountId, FolderRole, UnifiedItem};
use avero_sync::sync::SyncReport;
use iced::widget::row;
use iced::{Element, Fill, Task};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// How many messages one page of the list holds.
pub const PAGE: usize = 200;

/// Everything that can happen in the main window.
// `MessageSelected` is about a mail, not about this enum: the name is wanted.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum Message {
    FilterSelected(AccountFilter),
    MessageSelected(usize),
    SyncAll,
    SyncFinished(AccountId, Result<SyncReport, String>),
    LoadMore,
}

/// The main window.
pub struct App {
    store: Store,
    secrets: Box<dyn SecretStore>,
    db_path: PathBuf,
    accounts: Vec<Account>,
    filter: AccountFilter,
    items: Vec<UnifiedItem>,
    selected: Option<usize>,
    errors: HashMap<AccountId, String>,
    syncing: HashSet<AccountId>,
}

impl App {
    /// Loads the accounts and the first page of the unified inbox, then
    /// asks for a synchronisation of every account.
    pub fn new(
        store: Store,
        secrets: Box<dyn SecretStore>,
        db_path: PathBuf,
    ) -> (App, Task<Message>) {
        let accounts = store.accounts().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les comptes : {e}");
            Vec::new()
        });
        let mut app = App {
            store,
            secrets,
            db_path,
            accounts,
            filter: AccountFilter::All,
            items: Vec::new(),
            selected: None,
            errors: HashMap::new(),
            syncing: HashSet::new(),
        };
        app.load_first_page();
        (app, Task::done(Message::SyncAll))
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FilterSelected(filter) => {
                self.filter = filter;
                self.load_first_page();
                Task::none()
            }
            Message::MessageSelected(index) => {
                if index < self.items.len() {
                    self.selected = Some(index);
                }
                Task::none()
            }
            Message::SyncAll => self.sync_all(),
            Message::SyncFinished(id, result) => {
                self.syncing.remove(&id);
                match result {
                    Ok(_) => {
                        self.errors.remove(&id);
                    }
                    Err(error) => {
                        self.errors.insert(id, error);
                    }
                }
                self.reload();
                Task::none()
            }
            Message::LoadMore => {
                let next = self.fetch(PAGE, self.items.len());
                self.items.extend(next);
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let selected = self.selected.and_then(|i| self.items.get(i));
        row![
            view::rail::view(),
            view::list::view(
                &self.accounts,
                self.filter,
                &self.items,
                self.selected,
                &self.errors,
                !self.syncing.is_empty(),
            ),
            view::reader::view(selected, &self.accounts),
        ]
        .height(Fill)
        .into()
    }

    /// Starts one background sync per account that has a password.
    fn sync_all(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for account in &self.accounts {
            if self.syncing.contains(&account.id) {
                continue;
            }
            let password = match self.secrets.get(account.id) {
                Ok(Some(password)) => password,
                Ok(None) => {
                    self.errors
                        .insert(account.id, "Mot de passe manquant".to_string());
                    continue;
                }
                Err(e) => {
                    self.errors
                        .insert(account.id, format!("Coffre-fort inaccessible : {e}"));
                    continue;
                }
            };
            self.syncing.insert(account.id);
            let sync = spawn_sync(self.db_path.clone(), account.clone(), password);
            tasks.push(Task::perform(sync, |(id, result)| {
                Message::SyncFinished(id, result)
            }));
        }
        Task::batch(tasks)
    }

    /// Shows the first page for the current filter, with nothing selected.
    fn load_first_page(&mut self) {
        self.selected = None;
        self.items = self.fetch(PAGE, 0);
    }

    /// Reads the list again after a sync, keeping as many pages as were
    /// loaded and keeping the selected message selected.
    fn reload(&mut self) {
        let pages = self.items.len().div_ceil(PAGE).max(1);
        let selected_rows = self
            .selected
            .and_then(|i| self.items.get(i))
            .map(|item| item.row_ids.clone());
        self.items = self.fetch(pages * PAGE, 0);
        self.selected =
            selected_rows.and_then(|rows| self.items.iter().position(|i| i.row_ids == rows));
    }

    /// One slice of the unified inbox; on a database error, logs it and
    /// returns nothing so the window keeps working.
    fn fetch(&self, limit: usize, offset: usize) -> Vec<UnifiedItem> {
        self.store
            .unified(FolderRole::Inbox, self.filter, limit, offset)
            .unwrap_or_else(|e| {
                eprintln!("Impossible de lire la boîte de réception : {e}");
                Vec::new()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::secrets::MemorySecrets;
    use avero_core::{NewAccount, NewMessage, ServerConfig};

    /// Adds an account with an inbox holding `count` messages.
    fn add_account_with_mail(store: &Store, email: &str, count: u32) -> AccountId {
        let account = store
            .add_account(NewAccount {
                name: email.to_string(),
                email: email.to_string(),
                imap: ServerConfig {
                    host: "imap.example.com".to_string(),
                    port: 993,
                    username: email.to_string(),
                },
            })
            .unwrap();
        let inbox = store
            .upsert_folder(account.id, "INBOX", FolderRole::Inbox)
            .unwrap();
        let messages: Vec<NewMessage> = (1..=count)
            .map(|uid| NewMessage {
                uid,
                message_id: Some(format!("<{uid}@{email}>")),
                subject: format!("Sujet {uid}"),
                from_name: None,
                from_addr: "x@y.z".to_string(),
                date: 1_700_000_000 + i64::from(uid),
                seen: false,
            })
            .collect();
        store.insert_messages(inbox.id, &messages).unwrap();
        account.id
    }

    /// An app on an in-memory store; no password is stored, so the
    /// initial sync task (dropped here anyway) could never reach a server.
    fn app_with(setup: impl FnOnce(&Store)) -> App {
        let store = Store::open_in_memory().unwrap();
        setup(&store);
        let (app, _task) = App::new(store, Box::new(MemorySecrets::new()), PathBuf::new());
        app
    }

    fn ids(app: &App) -> (AccountId, AccountId) {
        (app.accounts[0].id, app.accounts[1].id)
    }

    #[test]
    fn starts_on_all_accounts_unified_inbox() {
        let app = app_with(|s| {
            add_account_with_mail(s, "a@x", 3);
            add_account_with_mail(s, "b@x", 2);
        });
        let (a, b) = ids(&app);
        assert_eq!(app.filter, AccountFilter::All);
        assert_eq!(app.items.len(), 5);
        assert!(app.items.iter().any(|i| i.accounts.contains(&a)));
        assert!(app.items.iter().any(|i| i.accounts.contains(&b)));
    }

    #[test]
    fn selecting_account_filters_list() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 3);
            add_account_with_mail(s, "b@x", 2);
        });
        let (a, _) = ids(&app);
        let _ = app.update(Message::MessageSelected(1));
        assert_eq!(app.selected, Some(1));

        let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
        assert_eq!(app.filter, AccountFilter::Only(a));
        assert_eq!(app.items.len(), 3);
        assert!(app.items.iter().all(|i| i.accounts.contains(&a)));
        assert_eq!(app.selected, None);
    }

    #[test]
    fn sync_error_is_recorded_per_account() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 3);
            add_account_with_mail(s, "b@x", 2);
        });
        let (a, b) = ids(&app);
        let _ = app.update(Message::SyncFinished(a, Err("x".to_string())));
        assert_eq!(app.errors.get(&a).map(String::as_str), Some("x"));
        assert!(!app.errors.contains_key(&b));
        assert_eq!(app.items.len(), 5);
    }

    #[test]
    fn sync_success_reloads_and_clears_error() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 3);
            add_account_with_mail(s, "b@x", 2);
        });
        let (a, _) = ids(&app);
        let _ = app.update(Message::SyncFinished(a, Err("x".to_string())));
        assert!(app.errors.contains_key(&a));

        // A message arrives in the database (as a sync would do).
        let inbox = app.store.folders(a).unwrap()[0].id;
        let new_message = NewMessage {
            uid: 99,
            message_id: Some("<99@a>".to_string()),
            subject: "Nouveau".to_string(),
            from_name: None,
            from_addr: "x@y.z".to_string(),
            date: 1_800_000_000,
            seen: false,
        };
        app.store.insert_messages(inbox, &[new_message]).unwrap();

        let report = SyncReport {
            folders: 1,
            new_messages: 1,
        };
        let _ = app.update(Message::SyncFinished(a, Ok(report)));
        assert!(!app.errors.contains_key(&a));
        assert_eq!(app.items.len(), 6);
        assert_eq!(app.items[0].subject, "Nouveau");
    }

    #[test]
    fn load_more_appends_next_page() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 450);
        });
        assert_eq!(app.items.len(), 200);
        let _ = app.update(Message::LoadMore);
        assert_eq!(app.items.len(), 400);
        let _ = app.update(Message::LoadMore);
        assert_eq!(app.items.len(), 450);
    }

    #[test]
    fn reload_after_sync_keeps_loaded_pages() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 450);
        });
        let a = app.accounts[0].id;
        let _ = app.update(Message::LoadMore);
        let _ = app.update(Message::SyncFinished(a, Err("x".to_string())));
        assert_eq!(app.items.len(), 400);
    }

    #[test]
    fn sync_all_without_password_records_error() {
        let mut app = app_with(|s| {
            add_account_with_mail(s, "a@x", 1);
        });
        let a = app.accounts[0].id;
        let _ = app.update(Message::SyncAll);
        assert_eq!(
            app.errors.get(&a).map(String::as_str),
            Some("Mot de passe manquant")
        );
        assert!(app.syncing.is_empty());
    }
}
