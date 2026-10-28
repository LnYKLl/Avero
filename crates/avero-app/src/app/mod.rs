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
use crate::background::{LoginError, OpenedBody};
use crate::compose::Mode;
use crate::settings::{Settings, ThemeChoice};
use crate::theme;
use crate::view;
use crate::worker::{self, AccountError, Destination, Event, Job, Reply, WorkerHandle};
use avero_autoconfig::ProviderConfig;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{
    Account, AccountFilter, AccountId, FolderRole, Security, ServerConfig, UnifiedItem,
};
use avero_oauth::Provider;
use iced::widget::text_editor;
use iced::{Element, Subscription, Task, Theme, keyboard};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// How many messages one page of the list holds.
pub const PAGE: usize = 200;
/// Automatic sync interval.
pub const AUTO_SYNC: Duration = Duration::from_secs(5 * 60);
/// Id of the search field (Ctrl+F focuses it).
pub const SEARCH_ID: &str = "search";
/// The list is read again at most this often while a sync reports
/// progress (plus once at the end).
pub const RELOAD_EVERY: Duration = Duration::from_millis(700);

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
    /// The IMAP and SMTP servers of an account.
    Servers(ServersForm),
    /// Deleting messages that are already in the trash, for good.
    ConfirmDeleteForever(Vec<i64>),
}

/// "Changer le mot de passe".
#[derive(Clone)]
pub struct PasswordDialog {
    pub account: AccountId,
    pub password: String,
    pub error: Option<String>,
    pub checking: bool,
    pub generation: u64,
}

impl fmt::Debug for PasswordDialog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordDialog")
            .field("account", &self.account)
            .field("password", &"***")
            .field("error", &self.error)
            .field("checking", &self.checking)
            .finish()
    }
}

/// Editable fields of the "Serveurs" dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerField {
    ImapHost,
    ImapPort,
    SmtpHost,
    SmtpPort,
}

/// "Serveurs": where an account receives (IMAP) and sends (SMTP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServersForm {
    pub account: AccountId,
    pub imap_host: String,
    pub imap_port: String,
    pub smtp_host: String,
    pub smtp_port: String,
    pub smtp_security: Security,
    pub error: Option<String>,
}

impl ServersForm {
    pub fn of(account: &Account) -> ServersForm {
        ServersForm {
            account: account.id,
            imap_host: account.imap.host.clone(),
            imap_port: account.imap.port.to_string(),
            smtp_host: account.smtp.host.clone(),
            smtp_port: account.smtp.port.to_string(),
            smtp_security: account.smtp.security,
            error: None,
        }
    }

    pub fn set(&mut self, field: ServerField, value: String) {
        self.error = None;
        match field {
            ServerField::ImapHost => self.imap_host = value,
            ServerField::ImapPort => self.imap_port = value,
            ServerField::SmtpHost => self.smtp_host = value,
            ServerField::SmtpPort => {
                if let Some(security) = crate::account_form::security_for_port(&value) {
                    self.smtp_security = security;
                }
                self.smtp_port = value;
            }
        }
    }

    /// The new IMAP and SMTP settings (usernames kept), or what is wrong.
    pub fn apply(&self, account: &Account) -> Result<(ServerConfig, ServerConfig), String> {
        let imap_host = self.imap_host.trim();
        let smtp_host = self.smtp_host.trim();
        if imap_host.is_empty() || smtp_host.is_empty() {
            return Err("Indiquez les serveurs de réception et d'envoi".to_string());
        }
        let port = |text: &str| text.trim().parse::<u16>().ok().filter(|&p| p >= 1);
        let imap_port = port(&self.imap_port).ok_or("Port IMAP invalide (1 à 65535)")?;
        let smtp_port = port(&self.smtp_port).ok_or("Port SMTP invalide (1 à 65535)")?;
        let smtp_user = match account.smtp.username.trim() {
            "" => account.imap.username.clone(),
            user => user.to_string(),
        };
        Ok((
            ServerConfig {
                host: imap_host.to_string(),
                port: imap_port,
                ..account.imap.clone()
            },
            ServerConfig {
                host: smtp_host.to_string(),
                port: smtp_port,
                username: smtp_user,
                security: self.smtp_security,
            },
        ))
    }
}

/// Sync state and last problem of one account.
#[derive(Debug, Clone, Default)]
pub struct AccountStatus {
    pub syncing: bool,
    pub error: Option<AccountError>,
    /// Whether an archive folder exists (`None`: not known yet).
    pub archive: Option<bool>,
}

/// The conversation (or the single message) shown in the reading pane.
pub struct Reading {
    /// The rows of the list item read: every copy of the message, or of
    /// every message of the conversation in the folder.
    pub rows: Vec<i64>,
    pub accounts: Vec<AccountId>,
    /// The conversation read, when the list groups them.
    pub thread: Option<i64>,
    /// The messages, oldest first: just one outside conversations.
    pub entries: Vec<Entry>,
    /// The entry Reply and Forward answer: the last one unfolded (the
    /// newest at first).
    pub focus: usize,
}

impl Reading {
    /// The entry Reply and Forward answer.
    pub fn focused(&self) -> &Entry {
        &self.entries[self.focus]
    }

    /// The body of the focused entry.
    pub fn body(&self) -> &Body {
        &self.focused().body
    }

    /// Whether the pane shows a conversation of several messages.
    pub fn is_thread(&self) -> bool {
        self.entries.len() > 1
    }
}

/// One message of the reading pane.
pub struct Entry {
    /// Its copies (the one with a cached body first), sender, date,
    /// snippet.
    pub item: UnifiedItem,
    /// Unfolded (its text shown) or folded to one line.
    pub expanded: bool,
    pub body: Body,
}

/// The body of a message of the reading pane.
pub enum Body {
    /// Not asked for yet: a folded message of a conversation.
    NotLoaded,
    Loading,
    Ready {
        /// Parsed once, attachments decoded (forward and save reuse them).
        opened: Arc<OpenedBody>,
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
    /// Puts a value on the clipboard (a scope or redirect URI to paste in
    /// a provider's console).
    CopyText(String),
    ThemeChosen(ThemeChoice),
    NotificationsToggled(bool),
    /// "Regrouper les conversations" in the settings.
    ThreadsToggled(bool),

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
    /// What a worker did on its own: a sync because the inbox changed,
    /// new mail to notify (see `Job::Watch`).
    Background(AccountId, Event),
    /// A throttled list reload is due.
    ReloadDue,
    /// A body was parsed off the UI thread.
    BodyParsed(i64, Result<Arc<OpenedBody>, String>),
    /// What a worker answered to an archive or delete of these rows.
    Moved(Vec<i64>, Event),
    ConfirmDeleteForever,

    /// Selection or scrolling in the text of a reader entry.
    ReaderAction(usize, text_editor::Action),
    /// Unfolds (or folds) a message of the conversation being read.
    EntryToggled(usize),
    Answer(Mode),
    Archive,
    Delete,
    MarkUnread,
    /// Attachment `.1` of reader entry `.0`.
    SaveAttachment(usize, usize),
    AttachmentSaved(Result<Option<String>, String>),

    Compose(ComposeMsg),
    Add(AddMsg),
    Account(AccountMsg),
}

/// Messages of the add-account card. `Debug` hides a typed password.
#[derive(Clone)]
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
    /// Shows the OAuth assistant in the open card.
    ShowSetup(Provider),
    /// Opens the OAuth assistant alone (settings: change the client).
    EditClient(Provider),
    /// "Enregistrer et se connecter" in the assistant.
    SaveSetup,
    /// Removes the client saved in `oauth.json` (settings).
    RemoveClient(Provider),
}

impl fmt::Debug for AddMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddMsg::Field(Field::Password, _) => f.write_str("Field(Password, \"***\")"),
            AddMsg::Field(Field::ClientSecret, _) => f.write_str("Field(ClientSecret, \"***\")"),
            AddMsg::Field(field, value) => write!(f, "Field({field:?}, {value:?})"),
            AddMsg::Open(provider) => write!(f, "Open({provider:?})"),
            AddMsg::SubmitEmail => f.write_str("SubmitEmail"),
            AddMsg::Detected(generation, config) => {
                write!(f, "Detected({generation}, {})", config.is_some())
            }
            AddMsg::Back => f.write_str("Back"),
            AddMsg::ToggleAdvanced => f.write_str("ToggleAdvanced"),
            AddMsg::UsePassword => f.write_str("UsePassword"),
            AddMsg::SmtpSecurity(security) => write!(f, "SmtpSecurity({security:?})"),
            AddMsg::SubmitPassword => f.write_str("SubmitPassword"),
            AddMsg::Checked(generation, result) => write!(f, "Checked({generation}, {result:?})"),
            AddMsg::SignIn(provider) => write!(f, "SignIn({provider:?})"),
            AddMsg::SignedIn(generation, result) => {
                write!(f, "SignedIn({generation}, {result:?})")
            }
            AddMsg::CancelWait => f.write_str("CancelWait"),
            AddMsg::Close => f.write_str("Close"),
            AddMsg::ShowSetup(provider) => write!(f, "ShowSetup({provider:?})"),
            AddMsg::EditClient(provider) => write!(f, "EditClient({provider:?})"),
            AddMsg::SaveSetup => f.write_str("SaveSetup"),
            AddMsg::RemoveClient(provider) => write!(f, "RemoveClient({provider:?})"),
        }
    }
}

/// Messages of account management (settings and banners). `Debug` hides
/// a typed password.
#[derive(Clone)]
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
    OpenServers(AccountId),
    ServerText(ServerField, String),
    ServerSecurity(Security),
    SaveServers,
    /// The SMTP server found for an account that had none.
    SmtpFound(AccountId, Option<ServerConfig>),
    CloseDialog,
}

impl fmt::Debug for AccountMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountMsg::PasswordText(_) => f.write_str("PasswordText(\"***\")"),
            AccountMsg::StartRename(id) => write!(f, "StartRename({id:?})"),
            AccountMsg::RenameText(text) => write!(f, "RenameText({text:?})"),
            AccountMsg::SaveRename => f.write_str("SaveRename"),
            AccountMsg::OpenPassword(id) => write!(f, "OpenPassword({id:?})"),
            AccountMsg::SubmitPassword => f.write_str("SubmitPassword"),
            AccountMsg::PasswordChecked(generation, result) => {
                write!(f, "PasswordChecked({generation}, {result:?})")
            }
            AccountMsg::AskDelete(id) => write!(f, "AskDelete({id:?})"),
            AccountMsg::ConfirmDelete => f.write_str("ConfirmDelete"),
            AccountMsg::Reconnect(id) => write!(f, "Reconnect({id:?})"),
            AccountMsg::Reconnected(id, result) => write!(f, "Reconnected({id:?}, {result:?})"),
            AccountMsg::OpenServers(id) => write!(f, "OpenServers({id:?})"),
            AccountMsg::ServerText(field, text) => write!(f, "ServerText({field:?}, {text:?})"),
            AccountMsg::ServerSecurity(security) => write!(f, "ServerSecurity({security:?})"),
            AccountMsg::SaveServers => f.write_str("SaveServers"),
            AccountMsg::SmtpFound(id, found) => write!(f, "SmtpFound({id:?}, {found:?})"),
            AccountMsg::CloseDialog => f.write_str("CloseDialog"),
        }
    }
}

/// An OAuth client as the settings show it: never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// The id shortened (`123456…tent.com`).
    pub masked_id: String,
    /// Saved in `oauth.json` (can be removed), not compiled in.
    pub saved: bool,
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
    /// Accounts that have a trash folder (Supprimer needs one).
    pub(crate) trash_accounts: HashSet<AccountId>,
    /// Whether some account has a real archive folder to show (Gmail's
    /// archive is "All Mail", which is not synced).
    pub(crate) archive_view: bool,
    /// When the list was last read during a sync, and whether a reload is
    /// waiting for its turn.
    last_reload: Option<Instant>,
    reload_scheduled: bool,
    /// Row ids of the selected item (stable across reloads).
    pub(crate) selected: Option<Vec<i64>>,
    pub(crate) reading: Option<Reading>,

    pub(crate) composer: Option<Composer>,
    pub(crate) overlay: Option<Overlay>,
    /// Stops the browser sign-in in progress.
    oauth_cancel: Option<oneshot::Sender<()>>,
    /// The OAuth clients configured, shown in the settings (read again
    /// whenever the assistant saves or removes one).
    pub(crate) oauth_clients: HashMap<Provider, ClientInfo>,
    pub(crate) toast: Option<Toast>,
    toast_seq: u64,
    /// Angle of the refresh icon while syncing.
    pub(crate) spinner: f32,
}

/// Erases the secrets of the accounts the store merged at startup (the
/// store cannot reach the keyring itself).
fn forget_merged_secrets(store: &Store, secrets: &dyn SecretStore) {
    for id in store.merged_accounts() {
        if let Err(e) = secrets.delete(*id) {
            eprintln!("Impossible d'effacer le secret d'un compte fusionné : {e}");
        }
    }
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
        // The jobs of the previous run are gone: hidden rows come back,
        // server flags apply again.
        if let Err(e) = store.clear_pending() {
            eprintln!("Impossible de remettre la base en ordre : {e}");
        }
        // Accounts merged by the database migration lose their secrets too.
        if let Backend::Live { secrets, .. } = &backend {
            forget_merged_secrets(&store, secrets.as_ref());
        }
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
            trash_accounts: HashSet::new(),
            archive_view: false,
            last_reload: None,
            reload_scheduled: false,
            selected: None,
            reading: None,
            composer: None,
            overlay: None,
            oauth_cancel: None,
            oauth_clients: HashMap::new(),
            toast: None,
            toast_seq: 0,
            spinner: 0.0,
        };
        app.read_oauth_clients();
        let watching: Vec<Task<Message>> = app
            .accounts
            .clone()
            .iter()
            .map(|account| app.start_worker(account))
            .collect();
        app.load_first_page();
        let system = iced::system::theme().map(Message::SystemTheme);
        let complete = app.complete_smtp_settings();
        (
            app,
            Task::batch([
                system,
                Task::done(Message::AutoSync),
                complete,
                Task::batch(watching),
            ]),
        )
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
                if webbrowser::open(&url).is_err() {
                    // Without a browser the address can still be pasted.
                    let toast = self.toast(
                        "Navigateur introuvable : l'adresse a été copiée dans le presse-papiers",
                        true,
                    );
                    return Task::batch([iced::clipboard::write(url), toast]);
                }
                Task::none()
            }
            Message::CopyText(text) => {
                let toast = self.toast(format!("Copié : {text}"), false);
                Task::batch([iced::clipboard::write(text), toast])
            }
            Message::ThemeChosen(choice) => {
                self.settings.theme = choice;
                self.refresh_theme();
                self.save_settings();
                Task::none()
            }
            Message::NotificationsToggled(on) => {
                self.settings.notifications = on;
                self.save_settings();
                Task::none()
            }
            Message::ThreadsToggled(on) => {
                self.settings.group_threads = on;
                self.save_settings();
                self.load_first_page();
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
            | Message::ReaderAction(..)
            | Message::EntryToggled(_)
            | Message::Answer(_)
            | Message::Archive
            | Message::Delete
            | Message::MarkUnread
            | Message::SaveAttachment(..)
            | Message::AttachmentSaved(_) => self.update_mail(message),
            Message::Worker(account, event) => self.on_worker_event(account, event),
            Message::Background(account, event) => self.on_background_event(account, event),
            Message::ReloadDue => {
                self.reload_scheduled = false;
                self.reload_now();
                Task::none()
            }
            Message::BodyParsed(row, result) => {
                self.body_parsed(row, result);
                Task::none()
            }
            Message::Moved(rows, event) => self.moved(rows, event),
            Message::ConfirmDeleteForever => self.delete_forever(),
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
        let loading = self.reading.as_ref().is_some_and(|reading| {
            reading
                .entries
                .iter()
                .any(|entry| matches!(entry.body, Body::Loading))
        });
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

    /// Starts the worker of an account (live mode only) and has it watch
    /// the inbox; what it does on its own comes back as
    /// `Message::Background`. Demo mode watches nothing.
    fn start_worker(&mut self, account: &Account) -> Task<Message> {
        let Backend::Live {
            db_path,
            secrets,
            config_dir,
            workers,
        } = &mut self.backend
        else {
            return Task::none();
        };
        let handle = WorkerHandle::spawn(worker::WorkerSetup {
            account: account.clone(),
            db_path: db_path.clone(),
            secrets: secrets.clone(),
            config_dir: config_dir.clone(),
        });
        let id = account.id;
        let watching =
            Task::stream(handle.watch()).map(move |event| Message::Background(id, event));
        workers.insert(id, handle);
        watching
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
        | Job::Move { reply, .. }
        | Job::DeleteForever { reply, .. } => {
            let _ = reply.send(Event::Done(Ok(())));
        }
        // Never sent in demo mode: no inbox to watch.
        Job::Watch { .. } | Job::UpdateAccount(_) | Job::Shutdown { .. } => {}
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
