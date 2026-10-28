//! State of the window and how it reacts to messages.
//!
//! `mod.rs` holds the state, the message list and the dispatch; the
//! details live in `mail` (list, reader, actions), `accounts` (adding,
//! fixing and removing accounts, worker events) and `composer`.

mod accounts;
mod composer;
mod mail;

pub use accounts::{EMAIL_ID, PASSWORD_ID};
pub use composer::{BODY_ID, ComposeMsg, Composer, TO_ID};

use crate::account_form::{AccountForm, Field};
use crate::auth::OAuthGrant;
use crate::background::LoginError;
use crate::compose::Mode;
use crate::settings::{Settings, ThemeChoice};
use crate::theme;
use crate::view;
use crate::worker::{self, AccountError, Destination, Event, Job, Reply, WorkerHandle};
use avero_autoconfig::ProviderConfig;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{Account, AccountFilter, AccountId, FolderRole, Security, UnifiedItem};
use avero_oauth::Provider;
use avero_sync::parse::ParsedBody;
use iced::widget::text_editor;
use iced::{Element, Subscription, Task, Theme, keyboard};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// How many messages one page of the list holds.
pub const PAGE: usize = 200;
/// Automatic sync interval.
pub const AUTO_SYNC: Duration = Duration::from_secs(5 * 60);
/// Id of the search field (Ctrl+F focuses it).
pub const SEARCH_ID: &str = "search";

/// Where account work happens.
pub enum Backend {
    /// Real accounts: one worker thread per account.
    Live {
        db_path: PathBuf,
        secrets: Arc<dyn SecretStore>,
        config_dir: PathBuf,
        workers: HashMap<AccountId, WorkerHandle>,
    },
    /// Demo mode: nothing leaves the machine, actions answer at once.
    Demo,
}

/// The main screens reached from the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Mail,
    Settings,
}

/// A dialog drawn over the window.
#[derive(Debug)]
pub enum Overlay {
    AddAccount(Box<AccountForm>),
    Password(PasswordDialog),
    Rename {
        account: AccountId,
        name: String,
    },
    ConfirmDelete(AccountId),
    /// Waiting for the browser to reconnect an OAuth account.
    Reconnect(AccountId),
}

/// "Changer le mot de passe".
#[derive(Debug, Clone)]
pub struct PasswordDialog {
    pub account: AccountId,
    pub password: String,
    pub error: Option<String>,
    pub checking: bool,
    pub generation: u64,
}

/// Sync state and last problem of one account.
#[derive(Debug, Clone, Default)]
pub struct AccountStatus {
    pub syncing: bool,
    pub error: Option<AccountError>,
    /// Whether an archive folder exists (`None`: not known yet).
    pub archive: Option<bool>,
}

/// The message shown in the reading pane.
pub struct Reading {
    /// Every copy of the message (merged across accounts), newest first.
    pub rows: Vec<i64>,
    pub accounts: Vec<AccountId>,
    pub body: Body,
}

/// The body of the message being read.
pub enum Body {
    Loading,
    Ready {
        parsed: Box<ParsedBody>,
        raw: Arc<Vec<u8>>,
        /// Read-only editor: lets the user select and copy the text.
        content: text_editor::Content,
    },
    Failed(String),
}

/// A short notice at the bottom of the window.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub error: bool,
}

/// Everything that can happen in the window.
// `MessageSelected` is about a mail, not about this enum: the name is wanted.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum Message {
    SystemTheme(iced::theme::Mode),
    Tick,
    AutoSync,
    Key(keyboard::Key, keyboard::Modifiers),
    ToastExpired(u64),
    OpenUrl(String),
    ThemeChosen(ThemeChoice),

    ShowMail,
    ShowSettings,
    FolderSelected(FolderRole),
    FilterSelected(AccountFilter),
    SearchChanged(String),
    SearchDebounced(u64),
    SearchSubmitted,
    SearchCleared,
    MessageSelected(usize),
    LoadMore,
    Refresh,
    Worker(AccountId, Event),

    ReaderAction(text_editor::Action),
    Answer(Mode),
    Archive,
    Delete,
    MarkUnread,
    SaveAttachment(usize),
    AttachmentSaved(Result<Option<String>, String>),

    Compose(ComposeMsg),
    Add(AddMsg),
    Account(AccountMsg),
}

/// Messages of the add-account card.
#[derive(Debug, Clone)]
pub enum AddMsg {
    /// Opens the card; with a provider, starts that sign-in directly.
    Open(Option<Provider>),
    Field(Field, String),
    SubmitEmail,
    Detected(u64, Option<ProviderConfig>),
    Back,
    ToggleAdvanced,
    UsePassword,
    SmtpSecurity(Security),
    SubmitPassword,
    Checked(u64, Result<(), LoginError>),
    SignIn(Provider),
    SignedIn(u64, Result<OAuthGrant, String>),
    CancelWait,
    Close,
}

/// Messages of account management (settings and banners).
#[derive(Debug, Clone)]
pub enum AccountMsg {
    StartRename(AccountId),
    RenameText(String),
    SaveRename,
    OpenPassword(AccountId),
    PasswordText(String),
    SubmitPassword,
    PasswordChecked(u64, Result<(), LoginError>),
    AskDelete(AccountId),
    ConfirmDelete,
    Reconnect(AccountId),
    Reconnected(AccountId, Result<OAuthGrant, String>),
    CloseDialog,
}

/// The window.
pub struct App {
    pub(crate) store: Store,
    pub(crate) backend: Backend,
    pub(crate) settings: Settings,
    settings_path: Option<PathBuf>,
    system_dark: bool,
    current_theme: Theme,

    pub(crate) accounts: Vec<Account>,
    pub(crate) status: HashMap<AccountId, AccountStatus>,
    pub(crate) unread: HashMap<AccountId, usize>,

    pub(crate) screen: Screen,
    pub(crate) folder: FolderRole,
    pub(crate) filter: AccountFilter,
    pub(crate) search_text: String,
    pub(crate) search_query: Option<String>,
    search_generation: u64,
    pub(crate) items: Vec<UnifiedItem>,
    /// Row ids of the selected item (stable across reloads).
    pub(crate) selected: Option<Vec<i64>>,
    pub(crate) reading: Option<Reading>,

    pub(crate) composer: Option<Composer>,
    pub(crate) overlay: Option<Overlay>,
    /// Stops the browser sign-in in progress.
    oauth_cancel: Option<oneshot::Sender<()>>,
    pub(crate) toast: Option<Toast>,
    toast_seq: u64,
    /// Angle of the refresh icon while syncing.
    pub(crate) spinner: f32,
}

impl App {
    /// Loads the accounts and the first page, starts one worker per
    /// account and asks for a sync of all of them.
    pub fn new(
        store: Store,
        backend: Backend,
        settings: Settings,
        settings_path: Option<PathBuf>,
    ) -> (App, Task<Message>) {
        let accounts = store.accounts().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les comptes : {e}");
            Vec::new()
        });
        let mut app = App {
            store,
            backend,
            current_theme: theme::build(settings.theme.is_dark(false)),
            settings,
            settings_path,
            system_dark: false,
            accounts,
            status: HashMap::new(),
            unread: HashMap::new(),
            screen: Screen::Mail,
            folder: FolderRole::Inbox,
            filter: AccountFilter::All,
            search_text: String::new(),
            search_query: None,
            search_generation: 0,
            items: Vec::new(),
            selected: None,
            reading: None,
            composer: None,
            overlay: None,
            oauth_cancel: None,
            toast: None,
            toast_seq: 0,
            spinner: 0.0,
        };
        for account in app.accounts.clone() {
            app.start_worker(&account);
        }
        app.load_first_page();
        let system = iced::system::theme().map(Message::SystemTheme);
        (app, Task::batch([system, Task::done(Message::AutoSync)]))
    }

    pub fn theme(&self) -> Theme {
        self.current_theme.clone()
    }

    /// Colour tokens of the current theme.
    pub fn tokens(&self) -> &'static theme::Tokens {
        theme::tokens(&self.current_theme)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::system::theme_changes().map(Message::SystemTheme),
            iced::time::every(AUTO_SYNC).map(|_| Message::AutoSync),
            keyboard::listen().filter_map(|event| match event {
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
        ];
        if self.is_syncing() || self.is_waiting() {
            subscriptions.push(iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick));
        }
        Subscription::batch(subscriptions)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemTheme(mode) => {
                self.system_dark = mode == iced::theme::Mode::Dark;
                self.refresh_theme();
                Task::none()
            }
            Message::Tick => {
                self.spinner = (self.spinner + 0.35) % std::f32::consts::TAU;
                Task::none()
            }
            Message::AutoSync | Message::Refresh => self.sync_all(),
            Message::Key(key, modifiers) => self.on_key(key, modifiers),
            Message::ToastExpired(id) => {
                if self.toast.as_ref().is_some_and(|t| t.id == id) {
                    self.toast = None;
                }
                Task::none()
            }
            Message::OpenUrl(url) => {
                let _ = webbrowser::open(&url);
                Task::none()
            }
            Message::ThemeChosen(choice) => {
                self.settings.theme = choice;
                self.refresh_theme();
                self.save_settings();
                Task::none()
            }
            Message::ShowMail => {
                self.screen = Screen::Mail;
                Task::none()
            }
            Message::ShowSettings => {
                self.screen = Screen::Settings;
                Task::none()
            }
            Message::FolderSelected(_)
            | Message::FilterSelected(_)
            | Message::SearchChanged(_)
            | Message::SearchDebounced(_)
            | Message::SearchSubmitted
            | Message::SearchCleared
            | Message::MessageSelected(_)
            | Message::LoadMore
            | Message::ReaderAction(_)
            | Message::Answer(_)
            | Message::Archive
            | Message::Delete
            | Message::MarkUnread
            | Message::SaveAttachment(_)
            | Message::AttachmentSaved(_) => self.update_mail(message),
            Message::Worker(account, event) => self.on_worker_event(account, event),
            Message::Compose(message) => self.update_composer(message),
            Message::Add(message) => self.update_add(message),
            Message::Account(message) => self.update_account(message),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        view::app(self)
    }

    // ------------------------------------------------------------ helpers

    fn refresh_theme(&mut self) {
        self.current_theme = theme::build(self.settings.theme.is_dark(self.system_dark));
    }

    fn save_settings(&self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            eprintln!("Impossible d'enregistrer les réglages : {e}");
        }
    }

    /// Whether any account is syncing.
    pub fn is_syncing(&self) -> bool {
        self.status.values().any(|s| s.syncing)
    }

    /// Whether a loader is on screen (it needs the animation tick).
    pub fn is_waiting(&self) -> bool {
        use crate::account_form::Stage;
        let overlay = match &self.overlay {
            Some(Overlay::AddAccount(form)) => matches!(
                form.stage,
                Stage::Detecting | Stage::Checking | Stage::Authorizing(_)
            ),
            Some(Overlay::Password(dialog)) => dialog.checking,
            Some(Overlay::Reconnect(_)) => true,
            _ => false,
        };
        let loading = matches!(
            self.reading,
            Some(Reading {
                body: Body::Loading,
                ..
            })
        );
        overlay || loading
    }

    pub fn is_demo(&self) -> bool {
        matches!(self.backend, Backend::Demo)
    }

    /// The account with this id.
    pub fn account(&self, id: AccountId) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Shows a notice for a few seconds.
    fn toast(&mut self, text: impl Into<String>, error: bool) -> Task<Message> {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toast = Some(Toast {
            id,
            text: text.into(),
            error,
        });
        let delay = if error { 6 } else { 3 };
        Task::perform(sleep(Duration::from_secs(delay)), move |()| {
            Message::ToastExpired(id)
        })
    }

    /// Starts the worker of an account (live mode only).
    fn start_worker(&mut self, account: &Account) {
        if let Backend::Live {
            db_path,
            secrets,
            config_dir,
            workers,
        } = &mut self.backend
        {
            let handle = WorkerHandle::spawn(worker::WorkerSetup {
                account: account.clone(),
                db_path: db_path.clone(),
                secrets: secrets.clone(),
                config_dir: config_dir.clone(),
            });
            workers.insert(account.id, handle);
        }
    }

    /// Sends a job to an account's worker; its events come back as
    /// `Message::Worker`. In demo mode the job is answered at once.
    fn request(&self, account: AccountId, make: impl FnOnce(Reply) -> Job) -> Task<Message> {
        let to_message = move |event| Message::Worker(account, event);
        match &self.backend {
            Backend::Demo => {
                let (reply, events) = tokio::sync::mpsc::unbounded_channel();
                demo_answer(make(reply));
                Task::stream(worker::drain(events)).map(to_message)
            }
            Backend::Live { workers, .. } => match workers.get(&account) {
                Some(handle) => Task::stream(handle.request(make)).map(to_message),
                None => Task::done(Message::Worker(account, Event::Gone)),
            },
        }
    }

    /// Sends a job that needs no answer.
    fn send_job(&self, account: AccountId, job: Job) {
        if let Backend::Live { workers, .. } = &self.backend
            && let Some(handle) = workers.get(&account)
        {
            handle.send(job);
        }
    }

    /// Starts a sync of every account.
    fn sync_all(&mut self) -> Task<Message> {
        let ids: Vec<AccountId> = self.accounts.iter().map(|a| a.id).collect();
        Task::batch(ids.into_iter().map(|id| self.sync_account(id)))
    }

    /// Starts the sync of one account unless it is already running.
    fn sync_account(&mut self, id: AccountId) -> Task<Message> {
        let status = self.status.entry(id).or_default();
        if status.syncing {
            return Task::none();
        }
        status.syncing = true;
        self.request(id, |reply| Job::Sync { reply })
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        let ctrl = modifiers.command();
        match key.as_ref() {
            keyboard::Key::Named(Named::Escape) => self.escape(),
            _ if self.overlay.is_some() => Task::none(),
            keyboard::Key::Character("n") if ctrl => self.update_composer(ComposeMsg::New),
            keyboard::Key::Character("r") if ctrl && self.composer.is_none() => {
                self.update_mail(Message::Answer(Mode::Reply))
            }
            keyboard::Key::Character("f") if ctrl => {
                self.screen = Screen::Mail;
                iced::widget::operation::focus(SEARCH_ID)
            }
            _ if self.composer.is_some() || ctrl => Task::none(),
            keyboard::Key::Named(Named::Delete) => self.update_mail(Message::Delete),
            keyboard::Key::Character("j") => self.step_selection(1),
            keyboard::Key::Character("k") => self.step_selection(-1),
            _ => Task::none(),
        }
    }

    /// Échap closes the top-most thing: dialog, composer, search.
    fn escape(&mut self) -> Task<Message> {
        if self.overlay.is_some() {
            return match self.overlay {
                Some(Overlay::AddAccount(_)) => self.update_add(AddMsg::Close),
                _ => self.update_account(AccountMsg::CloseDialog),
            };
        }
        if self.composer.is_some() {
            return self.update_composer(ComposeMsg::Cancel);
        }
        if self.search_query.is_some() || !self.search_text.is_empty() {
            return self.update_mail(Message::SearchCleared);
        }
        Task::none()
    }
}

/// Waits; the timer is only created when the task first runs.
pub(crate) async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Answers a job in demo mode: no network, immediate success.
fn demo_answer(job: Job) {
    let demo = "Mode démo : aucune connexion au serveur";
    match job {
        Job::Sync { reply } => {
            let _ = reply.send(Event::Synced(Ok(true)));
        }
        Job::FetchBody { target, reply } => {
            let _ = reply.send(Event::Body {
                row_id: target.row_id,
                result: Err(demo.to_string()),
            });
        }
        Job::Send { reply, .. } => {
            let _ = reply.send(Event::Sent(Ok(Some(
                "Mode démo : le message n'a pas vraiment été envoyé".to_string(),
            ))));
        }
        Job::SetPassword { reply, .. }
        | Job::SetOAuth { reply, .. }
        | Job::SetSeen { reply, .. }
        | Job::Move { reply, .. } => {
            let _ = reply.send(Event::Done(Ok(())));
        }
        Job::UpdateAccount(_) | Job::Shutdown { .. } => {}
    }
}

/// Which destination an archive or delete goes to (re-exported for views).
pub fn destination_label(destination: Destination) -> &'static str {
    match destination {
        Destination::Archive => "Archivé",
        Destination::Trash => "Placé dans la corbeille",
    }
}

#[cfg(test)]
mod tests;
