//! State of the window and how it reacts to messages.
//!
//! `mod.rs` holds the state, the message list and the dispatch; the
//! details live in `mail` (list, reader, actions), `selection` (lines
//! picked with Ctrl, Maj or `x`), `places` (tabs, favourites, one
//! sender's mail, the star), `unsubscribe` ("Se désinscrire"), `keys`
//! (keyboard shortcuts), `accounts` (adding, fixing and removing
//! accounts, worker events), `filters` (spam, the spam filter and the
//! rules), `cleanup` ("Grand nettoyage"), `composer`, `drafts` (saving
//! what is written), `later` ("Plus tard" and "Programmés"), `spaces`
//! (groups of accounts), `server_search` ("Chercher aussi sur le
//! serveur") and `quick_reply` ("Répondre à tous…" under a conversation).

mod accounts;
mod cleanup;
mod composer;
mod drafts;
mod filters;
mod keys;
mod later;
mod mail;
mod places;
mod quick_reply;
mod selection;
mod server_search;
mod spaces;
mod unsubscribe;

pub use accounts::{EMAIL_ID, PASSWORD_ID};
pub use cleanup::{Cleanup, CleanupMsg};
pub use composer::{BODY_ID, ComposeMsg, Composer, TO_ID};
pub use filters::{RuleForm, RuleMsg, field_name, field_verb, move_name};
pub use keys::SHORTCUTS;
pub use later::{LaterMsg, LaterPicker, LaterPurpose, scheduled_status};
pub use places::{SenderView, tab_name};
pub use quick_reply::{QUICK_REPLY_ID, QuickMsg, QuickReply};
pub use server_search::{ServerSearch, server_search_result};
pub use spaces::{SPACE_ICONS, SpaceForm, SpaceMsg};
pub use unsubscribe::UnsubscribeOffer;

use crate::account_form::{AccountForm, Field};
use crate::auth::OAuthGrant;
use crate::background::{LoginError, OpenedBody};
use crate::compose::Mode;
use crate::settings::{ColorTheme, Settings, ThemeChoice};
use crate::theme;
use crate::view;
use crate::worker::{self, AccountError, Destination, Event, Job, Reply, WorkerHandle};
use avero_autoconfig::ProviderConfig;
use avero_core::rules::Rule;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{
    Account, AccountFilter, AccountId, Category, DraftRecord, FolderRole, Security, ServerConfig,
    Space, SpaceId, UnifiedItem,
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
    /// "Grand nettoyage".
    Cleanup,
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
    /// The signature of an account, being edited.
    Signature {
        account: AccountId,
        content: text_editor::Content,
    },
    /// The list of keyboard shortcuts (`?`).
    Shortcuts,
    /// "Se désinscrire", waiting for a confirmation.
    ConfirmUnsubscribe(UnsubscribeOffer),
    /// A rule being written or changed.
    RuleEditor(RuleForm),
    /// "Appliquer maintenant": the rule changes `count` mails of the
    /// inbox.
    ConfirmApplyRule {
        rule: i64,
        name: String,
        count: usize,
    },
    /// "Tout mettre à la corbeille" for a sender of "Grand nettoyage".
    ConfirmTrashSender {
        name: String,
        rows: Vec<i64>,
    },
    /// "Plus tard" or "Envoyer plus tard": when.
    Later(LaterPicker),
    /// A space being created or changed.
    SpaceEditor(SpaceForm),
    /// "Supprimer l'espace", waiting for a confirmation.
    ConfirmDeleteSpace {
        space: SpaceId,
        name: String,
    },
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
    /// Who the conversation is from: the address of its newest message
    /// written by someone else than the user (for "Classer dans").
    pub sender: String,
    /// The tab of that message.
    pub category: Option<Category>,
    /// How to unsubscribe from that message's list, if it offers it.
    pub unsubscribe: Option<UnsubscribeOffer>,
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
    /// Ctrl, Maj… pressed or released (a click on a line reads them).
    ModifiersChanged(keyboard::Modifiers),
    ToastExpired(u64),
    OpenUrl(String),
    /// Puts a value on the clipboard (a scope or redirect URI to paste in
    /// a provider's console).
    CopyText(String),
    ThemeChosen(ThemeChoice),
    /// "Avero", "Orange" or "Océan" in the settings.
    ColorChosen(ColorTheme),
    /// The window's new width (the side bar folds when it is narrow).
    WindowResized(f32),
    /// The mouse entered (`true`) or left line `.0` of the list (its
    /// checkbox shows).
    LineHovered(usize, bool),
    /// The checkbox of line `.0` of the list.
    LineChecked(usize),
    /// Opens (`true`) or closes the menu of spaces of the side bar.
    SpaceMenu(bool),
    NotificationsToggled(bool),
    /// "Regrouper les conversations" in the settings.
    ThreadsToggled(bool),
    /// "Trier la réception en onglets" in the settings.
    TabsToggled(bool),
    /// "Filtre anti-spam" in the settings.
    SpamFilterToggled(bool),
    /// The spam filter learned from an account's folders (how many
    /// messages), or could not.
    SpamLearned(AccountId, Result<usize, String>),

    ShowMail,
    ShowSettings,
    ShowCleanup,
    FolderSelected(FolderRole),
    /// "Favoris": the starred mail of every folder.
    StarredSelected,
    /// A tab above the inbox.
    TabSelected(Category),
    /// "Mails de X" (address, name).
    ShowSender(String, String),
    /// Leaves "Mails de X".
    SenderBack,
    FilterSelected(AccountFilter),
    SearchChanged(String),
    SearchDebounced(u64),
    SearchSubmitted,
    SearchCleared,
    /// A click on a line of the list (Ctrl and Maj change what it does).
    MessageSelected(usize),
    /// "Tout sélectionner": every loaded line, or none.
    SelectAll(bool),
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
    MarkRead,
    MarkUnread,
    /// Stars the lines acted on (or unstars them, all starred).
    ToggleStar,
    /// The star of line `.0` of the list.
    StarLine(usize),
    /// "Classer dans" for the conversation being read.
    Reclassify(Category),
    /// "Spam" (`true`) or "Pas spam" for the lines acted on.
    MarkSpam(bool),
    /// "Se désinscrire" (asks for a confirmation).
    Unsubscribe,
    ConfirmUnsubscribe,
    /// Unsubscribing from a sender was done (or failed).
    Unsubscribed(String, Result<(), String>),
    /// Attachment `.1` of reader entry `.0`.
    SaveAttachment(usize, usize),
    AttachmentSaved(Result<Option<String>, String>),

    Compose(ComposeMsg),
    Add(AddMsg),
    Account(AccountMsg),
    Rule(RuleMsg),
    Cleanup(CleanupMsg),
    Later(LaterMsg),
    Space(SpaceMsg),
    QuickReply(QuickMsg),
    /// "Chercher aussi sur le serveur".
    SearchServer,
    /// What an account's server found for search number `.0` (see
    /// `App::server_generation`).
    ServerFound(u64, AccountId, Event),
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
    /// Opens the signature of an account.
    EditSignature(AccountId),
    SignatureEdit(text_editor::Action),
    SaveSignature,
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
            AccountMsg::EditSignature(id) => write!(f, "EditSignature({id:?})"),
            AccountMsg::SignatureEdit(action) => write!(f, "SignatureEdit({action:?})"),
            AccountMsg::SaveSignature => f.write_str("SaveSignature"),
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
    /// Width of the window, in logical pixels.
    pub(crate) window_width: f32,
    /// The line of the list under the mouse.
    pub(crate) hovered_line: Option<usize>,
    /// The menu of spaces of the side bar is open.
    pub(crate) space_menu: bool,

    pub(crate) accounts: Vec<Account>,
    pub(crate) status: HashMap<AccountId, AccountStatus>,
    pub(crate) unread: HashMap<AccountId, usize>,
    /// The counters of the side bar: conversations in "Plus tard" and
    /// in the drafts folder (of the space shown).
    pub(crate) snoozed_count: usize,
    pub(crate) drafts_count: usize,

    pub(crate) screen: Screen,
    pub(crate) folder: FolderRole,
    /// "Favoris" is shown instead of `folder`.
    pub(crate) starred: bool,
    /// "Plus tard" (the snoozed conversations) is shown instead.
    pub(crate) snoozed: bool,
    /// "Programmés" (the mail to send later) is shown instead.
    pub(crate) scheduled: bool,
    /// The scheduled mail, for "Programmés".
    pub(crate) outbox: Vec<DraftRecord>,
    /// Scheduled mail being sent right now.
    pub(crate) sending: HashSet<i64>,
    /// The row of the drafts folder whose body is awaited to open it in
    /// the composer.
    pub(crate) pending_draft: Option<i64>,
    /// Where the time comes from (tests set it by hand).
    pub(crate) clock: crate::later::Clock,
    /// The tab of the inbox shown (see `App::tabs_shown`).
    pub(crate) tab: Category,
    /// "Mails de X", shown over the rest.
    pub(crate) sender: Option<SenderView>,
    /// Unread lines of each tab of the inbox.
    pub(crate) tab_unread: HashMap<Category, usize>,
    pub(crate) filter: AccountFilter,
    /// The spaces, in order, and the one shown (`None`: every account).
    pub(crate) spaces: Vec<Space>,
    pub(crate) space: Option<SpaceId>,
    pub(crate) search_text: String,
    pub(crate) search_query: Option<String>,
    search_generation: u64,
    /// "Chercher aussi sur le serveur" for the search shown.
    pub(crate) server_search: ServerSearch,
    /// The rows the servers found, added to the local results.
    pub(crate) server_rows: Vec<i64>,
    /// Bumped by each search on the server, so late answers to an older
    /// one are ignored.
    server_generation: u64,
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
    /// Row ids of the selected item (stable across reloads): the line
    /// read, or the keyboard's line while lines are checked.
    pub(crate) selected: Option<Vec<i64>>,
    /// Lines picked with Ctrl+click, Maj+click, `x` or "Tout
    /// sélectionner" (row ids of each); the actions apply to them.
    pub(crate) checked: Vec<Vec<i64>>,
    /// Where a Maj+click range starts.
    pub(crate) anchor: Option<Vec<i64>>,
    /// The modifier keys held (read by a click on a line).
    pub(crate) modifiers: keyboard::Modifiers,
    /// When `g` was pressed, waiting for the folder letter.
    pub(crate) pending_g: Option<Instant>,
    pub(crate) reading: Option<Reading>,
    /// "Répondre à tous…" under the conversation read.
    pub(crate) quick_reply: QuickReply,
    /// Bumped when another conversation is opened (its quick reply
    /// starts empty).
    quick_generation: u64,

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

    /// The user's rules, in order (read again after each change).
    pub(crate) rules: Vec<Rule>,
    /// Accounts whose folders the spam filter is learning from.
    learning: HashSet<AccountId>,
    /// "Grand nettoyage".
    pub(crate) cleanup: Cleanup,
}

/// Erases the secrets of the accounts the store merged when it was
/// migrated (the store cannot reach the keyring itself). Each one is
/// crossed off once erased; one that could not be is tried again at the
/// next start.
fn forget_merged_secrets(store: &Store, secrets: &dyn SecretStore) {
    let merged = store.merged_accounts().unwrap_or_else(|e| {
        eprintln!("Impossible de lire les comptes fusionnés : {e}");
        Vec::new()
    });
    for id in merged {
        match secrets.delete(id) {
            Ok(()) => {
                if let Err(e) = store.secret_deleted(id) {
                    eprintln!("Impossible de noter l'effacement d'un secret : {e}");
                }
            }
            Err(e) => eprintln!("Impossible d'effacer le secret d'un compte fusionné : {e}"),
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
            current_theme: theme::build(settings.color, settings.theme.is_dark(false)),
            window_width: 1280.0,
            hovered_line: None,
            space_menu: false,
            settings,
            settings_path,
            system_dark: false,
            accounts,
            status: HashMap::new(),
            unread: HashMap::new(),
            snoozed_count: 0,
            drafts_count: 0,
            screen: Screen::Mail,
            folder: FolderRole::Inbox,
            starred: false,
            snoozed: false,
            scheduled: false,
            outbox: Vec::new(),
            sending: HashSet::new(),
            pending_draft: None,
            clock: crate::later::Clock::default(),
            tab: Category::Important,
            sender: None,
            tab_unread: HashMap::new(),
            filter: AccountFilter::All,
            spaces: Vec::new(),
            space: None,
            search_text: String::new(),
            search_query: None,
            search_generation: 0,
            server_search: ServerSearch::Idle,
            server_rows: Vec::new(),
            server_generation: 0,
            items: Vec::new(),
            trash_accounts: HashSet::new(),
            archive_view: false,
            last_reload: None,
            reload_scheduled: false,
            selected: None,
            checked: Vec::new(),
            anchor: None,
            modifiers: keyboard::Modifiers::default(),
            pending_g: None,
            reading: None,
            quick_reply: QuickReply::default(),
            quick_generation: 0,
            composer: None,
            overlay: None,
            oauth_cancel: None,
            oauth_clients: HashMap::new(),
            toast: None,
            toast_seq: 0,
            spinner: 0.0,
            rules: Vec::new(),
            learning: HashSet::new(),
            cleanup: Cleanup::default(),
        };
        app.read_oauth_clients();
        app.refresh_rules();
        app.spaces = app.store.spaces().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les espaces : {e}");
            Vec::new()
        });
        // The space shown last time, if it still exists.
        app.space = app
            .settings
            .active_space
            .map(SpaceId)
            .filter(|id| app.spaces.iter().any(|s| s.id == *id));
        // Scheduled mail whose sending a stop cut short is not sent twice.
        if let Err(e) = app.store.interrupted_sends(later::INTERRUPTED) {
            eprintln!("Impossible de relire les envois programmés : {e}");
        }
        app.refresh_outbox();
        let drafts = app.restore_drafts();
        let watching: Vec<Task<Message>> = app
            .accounts
            .clone()
            .iter()
            .map(|account| app.start_worker(account))
            .collect();
        app.load_first_page();
        let system = iced::system::theme().map(Message::SystemTheme);
        let complete = app.complete_smtp_settings();
        let ids: Vec<AccountId> = app.accounts.iter().map(|a| a.id).collect();
        let learning: Vec<Task<Message>> = ids
            .into_iter()
            .map(|id| app.learn_from_folders(id))
            .collect();
        (
            app,
            Task::batch([
                system,
                Task::done(Message::AutoSync),
                complete,
                Task::batch(watching),
                Task::batch(learning),
                drafts,
                Task::done(Message::Later(LaterMsg::Tick)),
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
            iced::window::resize_events().map(|(_, size)| Message::WindowResized(size.width)),
            iced::time::every(AUTO_SYNC).map(|_| Message::AutoSync),
            iced::time::every(later::TICK).map(|_| Message::Later(LaterMsg::Tick)),
            keyboard::listen().filter_map(|event| match event {
                // Ctrl+0 to Ctrl+9 change the space, read from the key's
                // place: on a French keyboard the digits need Maj.
                keyboard::Event::KeyPressed {
                    physical_key,
                    modifiers,
                    ..
                } if modifiers.command() && !modifiers.alt() && digit(physical_key).is_some() => {
                    digit(physical_key).map(|n| Message::Space(SpaceMsg::Number(n)))
                }
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
            // Every modifier change, even while a field has the focus.
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                    Some(Message::ModifiersChanged(modifiers))
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
            Message::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                Task::none()
            }
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
            Message::ColorChosen(color) => {
                self.settings.color = color;
                self.refresh_theme();
                self.save_settings();
                Task::none()
            }
            Message::WindowResized(width) => {
                self.window_width = width;
                Task::none()
            }
            Message::LineHovered(index, inside) => {
                if inside {
                    self.hovered_line = Some(index);
                } else if self.hovered_line == Some(index) {
                    self.hovered_line = None;
                }
                Task::none()
            }
            Message::LineChecked(index) => {
                self.check_line(index);
                Task::none()
            }
            Message::SpaceMenu(open) => {
                self.space_menu = open;
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
            Message::ShowCleanup => {
                self.show_cleanup();
                Task::none()
            }
            Message::SpamFilterToggled(on) => self.spam_filter_toggled(on),
            Message::SpamLearned(account, result) => {
                self.spam_learned(account, result);
                Task::none()
            }
            Message::MarkSpam(spam) => self.mark_spam(spam),
            Message::Rule(message) => self.update_rules(message),
            Message::Cleanup(message) => self.update_cleanup(message),
            Message::Later(message) => self.update_later(message),
            Message::Space(message) => {
                if self.overlay.is_some() && matches!(message, SpaceMsg::Number(_)) {
                    return Task::none();
                }
                if matches!(message, SpaceMsg::Select(_) | SpaceMsg::Number(_)) {
                    self.space_menu = false;
                }
                self.update_spaces(message)
            }
            Message::QuickReply(message) => self.update_quick_reply(message),
            Message::SearchServer => self.search_on_server(),
            Message::ServerFound(generation, _account, event) => {
                self.server_found(generation, event)
            }
            Message::FolderSelected(_)
            | Message::FilterSelected(_)
            | Message::SearchChanged(_)
            | Message::SearchDebounced(_)
            | Message::SearchSubmitted
            | Message::SearchCleared
            | Message::MessageSelected(_)
            | Message::SelectAll(_)
            | Message::LoadMore
            | Message::ReaderAction(..)
            | Message::EntryToggled(_)
            | Message::Answer(_)
            | Message::Archive
            | Message::Delete
            | Message::MarkRead
            | Message::MarkUnread
            | Message::SaveAttachment(..)
            | Message::AttachmentSaved(_) => self.update_mail(message),
            Message::StarredSelected
            | Message::TabSelected(_)
            | Message::TabsToggled(_)
            | Message::ShowSender(..)
            | Message::SenderBack
            | Message::ToggleStar
            | Message::StarLine(_)
            | Message::Reclassify(_) => self.update_places(message),
            Message::Unsubscribe | Message::ConfirmUnsubscribe | Message::Unsubscribed(..) => {
                self.update_unsubscribe(message)
            }

            Message::Worker(account, event) => self.on_worker_event(account, event),
            Message::Background(account, event) => self.on_background_event(account, event),
            Message::ReloadDue => {
                self.reload_scheduled = false;
                self.reload_now();
                Task::none()
            }
            Message::BodyParsed(row, result) => self.body_parsed(row, result),
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
        self.current_theme = theme::build(
            self.settings.color,
            self.settings.theme.is_dark(self.system_dark),
        );
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
        overlay || loading || self.server_search.is_running() || self.quick_reply.sending
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
}

/// The digit of a key of the top row or of the keypad (Ctrl+1 changes
/// the space, whatever the keyboard layout).
fn digit(key: keyboard::key::Physical) -> Option<u8> {
    use keyboard::key::{Code, Physical};
    let Physical::Code(code) = key else {
        return None;
    };
    let digits = [
        (Code::Digit0, Code::Numpad0),
        (Code::Digit1, Code::Numpad1),
        (Code::Digit2, Code::Numpad2),
        (Code::Digit3, Code::Numpad3),
        (Code::Digit4, Code::Numpad4),
        (Code::Digit5, Code::Numpad5),
        (Code::Digit6, Code::Numpad6),
        (Code::Digit7, Code::Numpad7),
        (Code::Digit8, Code::Numpad8),
        (Code::Digit9, Code::Numpad9),
    ];
    digits
        .iter()
        .position(|(row, pad)| code == *row || code == *pad)
        .map(|n| n as u8)
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
        // Kept on this computer only.
        Job::SaveDraft { reply, .. } => {
            let _ = reply.send(Event::DraftSaved(Ok(None)));
        }
        // The demo searches its own data (see `demo::server_hits`).
        Job::SearchServer { reply, .. } => {
            let _ = reply.send(Event::Found(Ok(Default::default())));
        }
        Job::SetPassword { reply, .. }
        | Job::SetOAuth { reply, .. }
        | Job::SetSeen { reply, .. }
        | Job::SetFlagged { reply, .. }
        | Job::Move { reply, .. }
        | Job::DeleteForever { reply, .. }
        | Job::DropDraft { reply, .. } => {
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
        Destination::Junk => "Placé dans le spam",
        Destination::Inbox => "Remis dans la réception",
    }
}

#[cfg(test)]
mod design_tests;
#[cfg(test)]
mod filter_tests;
#[cfg(test)]
mod later_tests;
#[cfg(test)]
mod reply_tests;
#[cfg(test)]
mod space_tests;
#[cfg(test)]
mod tests;
