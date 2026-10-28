//! One background worker per account.
//!
//! Each worker owns a thread with a small tokio runtime, its own `Store`
//! connection (a SQLite connection cannot be shared between threads) and
//! a cached IMAP session, reconnected when it breaks. The user's jobs for
//! the same account run one after the other; different accounts run in
//! parallel. A sync runs beside them on a thread of its own, with its own
//! IMAP session and `Store` connection, so opening a message never waits
//! for a long first sync. Once the UI subscribes ([`WorkerHandle::watch`]),
//! a third connection, on a thread of its own too, watches the inbox
//! (IMAP IDLE, or polling) and asks for a sync whenever it changes.
//!
//! The UI sends a [`Job`] holding a [`Reply`] sender and reads the
//! [`Event`]s from the matching stream (see [`request`]).

use crate::auth::{self, AccessToken, OAuthSecret};
use crate::background::oauth_message;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{
    Account, AccountFilter, AccountId, Credentials, FolderRole, MessageRef, ServerConfig,
};
use avero_sync::imap::{ImapSource, ImapWatch};
use avero_sync::parse::{guarded, parse_body};
use avero_sync::smtp::{self, Outgoing, SendError};
use avero_sync::source::{MailSource, SyncError};
use avero_sync::sync::{Arrival, sync_account};
use avero_sync::watch::{Change, InboxWatch};
use iced::futures::stream::{self, BoxStream};
use iced::futures::{Stream, StreamExt};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// How many of the newest inbox messages get their body downloaded in
/// the background after a sync.
pub const PREFETCH: usize = 30;

/// How long the inbox watcher waits before connecting again after a
/// failure: `first`, doubled after each new failure, at most `max`.
#[derive(Debug, Clone, Copy)]
struct Retry {
    first: Duration,
    max: Duration,
}

const WATCH_RETRY: Retry = Retry {
    first: Duration::from_secs(10),
    max: Duration::from_secs(10 * 60),
};

/// Where the worker sends what happens to one job.
pub type Reply = UnboundedSender<Event>;

/// What kind of problem an account has, which decides the banner action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The server refused the password or token.
    Auth,
    /// The OAuth grant is gone: the user must sign in again.
    Reconnect,
    /// Network, TLS or timeout.
    Network,
    Other,
}

/// An account problem, in French, without any secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountError {
    pub kind: ErrorKind,
    pub message: String,
}

impl AccountError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> AccountError {
        AccountError {
            kind,
            message: message.into(),
        }
    }
}

/// How a failed token refresh shows on the account. A wrong OAuth client
/// (`invalid_client`, what Google answers for an unknown id or a wrong
/// secret) is a settings problem, not a network one: retrying or
/// signing in again cannot fix it.
pub fn refresh_error(
    provider: avero_oauth::Provider,
    error: &avero_oauth::OAuthError,
) -> AccountError {
    if auth::needs_reconnect(error) {
        return AccountError::new(ErrorKind::Reconnect, "Reconnexion nécessaire");
    }
    let kind = if auth::is_client_error(error) {
        ErrorKind::Other
    } else {
        ErrorKind::Network
    };
    AccountError::new(kind, oauth_message(provider, error))
}

/// Where archived or deleted messages go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Archive,
    Trash,
}

/// Something the worker reports back.
#[derive(Debug, Clone)]
pub enum Event {
    /// New data is in the database: reload the list.
    Progress,
    /// The sync finished. `archive` tells whether an archive folder exists.
    Synced(Result<bool, AccountError>),
    /// A body was downloaded and cached (or could not be).
    Body {
        row_id: i64,
        result: Result<(), String>,
    },
    /// An action (flags, move, secret) finished.
    Done(Result<(), AccountError>),
    /// A message was sent; `Ok(Some(warning))` if the Sent copy failed.
    Sent(Result<Option<String>, String>),
    /// Unread messages that just came into the inbox, to notify. Sent
    /// once per sync, to the [`Job::Watch`] subscriber only.
    NewMail(Vec<Arrival>),
    /// The worker is not running any more.
    Gone,
}

/// Work for an account's worker.
pub enum Job {
    /// Stores a new password and forgets the current session.
    SetPassword {
        password: String,
        reply: Reply,
    },
    /// Stores a new OAuth grant (refresh token in the keyring, access
    /// token in memory).
    SetOAuth {
        refresh_token: Option<String>,
        access: AccessToken,
        reply: Reply,
    },
    /// The account was renamed or its servers changed.
    UpdateAccount(Account),
    Sync {
        reply: Reply,
    },
    FetchBody {
        target: MessageRef,
        reply: Reply,
    },
    SetSeen {
        targets: Vec<MessageRef>,
        seen: bool,
        reply: Reply,
    },
    /// Moves messages (already hidden in the list) to the archive or the
    /// trash; their rows are deleted once the server confirms, shown
    /// again otherwise.
    Move {
        targets: Vec<MessageRef>,
        to: Destination,
        reply: Reply,
    },
    /// Deletes messages for good (they are in the trash), same contract
    /// as `Move`.
    DeleteForever {
        targets: Vec<MessageRef>,
        reply: Reply,
    },
    Send {
        outgoing: Box<Outgoing>,
        reply: Reply,
    },
    /// Starts watching the inbox. `reply` then gets what the worker does
    /// on its own: the syncs it starts because the inbox changed (their
    /// `Progress` and `Synced`), and `NewMail` after every sync that
    /// found some. It stays open as long as the worker runs.
    Watch {
        reply: Reply,
    },
    /// Stops the worker, deleting the account's secret if asked.
    Shutdown {
        delete_secret: bool,
    },
}

/// The UI's handle on a running worker.
#[derive(Clone)]
pub struct WorkerHandle {
    jobs: UnboundedSender<Job>,
}

/// What a worker needs to start.
pub struct WorkerSetup {
    pub account: Account,
    pub db_path: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    pub config_dir: PathBuf,
}

impl WorkerHandle {
    /// Starts the worker thread of an account.
    pub fn spawn(setup: WorkerSetup) -> WorkerHandle {
        WorkerHandle::spawn_with(setup, Imap, WATCH_RETRY)
    }

    /// Starts a worker that opens its IMAP sessions with `connector`.
    fn spawn_with<C: Connector>(setup: WorkerSetup, connector: C, retry: Retry) -> WorkerHandle {
        let (jobs, receiver) = mpsc::unbounded_channel();
        let name = format!("avero-account-{}", setup.account.id.0);
        let _ = std::thread::Builder::new()
            .name(name)
            .spawn(move || run_thread(setup, connector, retry, receiver));
        WorkerHandle { jobs }
    }

    /// Sends a job that needs no answer.
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Starts watching the inbox; the stream gives what the worker does
    /// on its own (see [`Job::Watch`]) and ends with the worker.
    pub fn watch(&self) -> BoxStream<'static, Event> {
        let (reply, events) = mpsc::unbounded_channel();
        self.send(Job::Watch { reply });
        drain(events).boxed()
    }

    /// A stream that sends the job when first polled, then yields its
    /// events until the worker drops the reply sender.
    pub fn request(&self, make: impl FnOnce(Reply) -> Job) -> BoxStream<'static, Event> {
        let (reply, events) = mpsc::unbounded_channel();
        let job = make(reply);
        deferred(self.jobs.clone(), job, events).boxed()
    }
}

/// State of a [`deferred`] stream.
enum Pending {
    Start(UnboundedSender<Job>, Job, UnboundedReceiver<Event>),
    /// Waiting for events; `true` once the job's final event came.
    Wait(UnboundedReceiver<Event>, bool),
    Finished,
}

impl Event {
    /// Whether this event ends a job (everything but `Progress` and
    /// `NewMail`).
    pub fn is_final(&self) -> bool {
        !matches!(self, Event::Progress | Event::NewMail(_))
    }
}

fn deferred(
    jobs: UnboundedSender<Job>,
    job: Job,
    events: UnboundedReceiver<Event>,
) -> impl Stream<Item = Event> {
    stream::unfold(Pending::Start(jobs, job, events), |state| async move {
        match state {
            Pending::Start(jobs, job, events) => {
                if jobs.send(job).is_err() {
                    return Some((Event::Gone, Pending::Finished));
                }
                next_event(events, false).await
            }
            Pending::Wait(events, done) => next_event(events, done).await,
            Pending::Finished => None,
        }
    })
}

/// The next event; when the worker drops the reply without having sent
/// a final event (it panicked, or lost the job), a `Gone` so the UI never
/// waits forever.
async fn next_event(mut events: UnboundedReceiver<Event>, done: bool) -> Option<(Event, Pending)> {
    match events.recv().await {
        Some(event) => {
            let done = done || event.is_final();
            Some((event, Pending::Wait(events, done)))
        }
        None if done => None,
        None => Some((Event::Gone, Pending::Finished)),
    }
}

/// Turns a receiver filled synchronously (demo mode) into a stream.
pub fn drain(events: UnboundedReceiver<Event>) -> impl Stream<Item = Event> {
    stream::unfold(events, |mut events| async move {
        events.recv().await.map(|event| (event, events))
    })
}

fn run_thread<C: Connector>(
    setup: WorkerSetup,
    connector: C,
    retry: Retry,
    mut jobs: UnboundedReceiver<Job>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async move {
        match Store::open(&setup.db_path) {
            Ok(store) => Worker::new(setup, connector, retry, store).run(jobs).await,
            Err(e) => {
                let message = format!("Erreur de la base locale : {e}");
                while let Some(job) = jobs.recv().await {
                    refuse(job, &message);
                }
            }
        }
    });
}

/// Answers a job with an error without doing it.
fn refuse(job: Job, message: &str) {
    let error = || AccountError::new(ErrorKind::Other, message);
    match job {
        Job::Sync { reply } => {
            let _ = reply.send(Event::Synced(Err(error())));
        }
        Job::FetchBody { target, reply } => {
            let _ = reply.send(Event::Body {
                row_id: target.row_id,
                result: Err(message.to_string()),
            });
        }
        Job::Send { reply, .. } => {
            let _ = reply.send(Event::Sent(Err(message.to_string())));
        }
        Job::SetPassword { reply, .. }
        | Job::SetOAuth { reply, .. }
        | Job::SetSeen { reply, .. }
        | Job::Move { reply, .. }
        | Job::DeleteForever { reply, .. } => {
            let _ = reply.send(Event::Done(Err(error())));
        }
        // Nothing will happen on its own: the stream just ends.
        Job::Watch { .. } | Job::UpdateAccount(_) | Job::Shutdown { .. } => {}
    }
}

/// Whether the worker loop goes on.
#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Opens logged-in IMAP sessions. The real one is [`Imap`]; tests use a
/// fake server.
// Only used with generics, never as `dyn`. The sessions it opens stay on
// the thread that opened them, so they need not be `Send`.
#[allow(async_fn_in_trait)]
pub trait Connector: Send + Sync + 'static {
    type Source: MailSource;
    type Watch: InboxWatch;
    async fn connect(
        &self,
        cfg: &ServerConfig,
        creds: &Credentials,
    ) -> Result<Self::Source, SyncError>;
    /// A session that watches the inbox.
    async fn watch(
        &self,
        cfg: &ServerConfig,
        creds: &Credentials,
    ) -> Result<Self::Watch, SyncError>;
}

/// The real IMAP server.
pub struct Imap;

impl Connector for Imap {
    type Source = ImapSource;
    type Watch = ImapWatch;

    async fn connect(
        &self,
        cfg: &ServerConfig,
        creds: &Credentials,
    ) -> Result<ImapSource, SyncError> {
        ImapSource::connect(cfg, creds).await
    }

    async fn watch(&self, cfg: &ServerConfig, creds: &Credentials) -> Result<ImapWatch, SyncError> {
        ImapWatch::connect(cfg, creds).await
    }
}

/// How an account logs in, shared by its IMAP connections (the one for
/// the user's actions, and those of the sync and of the inbox watcher,
/// on other threads).
struct Login<C> {
    connector: C,
    secrets: Arc<dyn SecretStore>,
    config_dir: PathBuf,
    /// The OAuth access token in memory. All connections use the same
    /// one: a token refreshed by one serves the others. The lock is held
    /// during a refresh so they never refresh at the same time.
    token: tokio::sync::Mutex<Option<AccessToken>>,
}

impl<C: Connector> Login<C> {
    fn store_secret(&self, account: AccountId, secret: &str) -> Result<(), AccountError> {
        self.secrets.set(account, secret).map_err(|e| {
            AccountError::new(ErrorKind::Other, format!("Coffre-fort inaccessible : {e}"))
        })
    }

    async fn set_token(&self, token: Option<AccessToken>) {
        *self.token.lock().await = token;
    }

    /// What to log in with: the stored password, or a fresh OAuth access
    /// token (refreshed when it expires within a minute).
    async fn credentials(&self, account: &Account) -> Result<Credentials, AccountError> {
        let user = account.imap.username.clone();
        let Some(provider) = auth::provider_of(account.auth) else {
            return match self.secrets.get(account.id) {
                Ok(Some(password)) => Ok(Credentials::Password(password)),
                Ok(None) => Err(AccountError::new(ErrorKind::Auth, "Mot de passe manquant")),
                Err(e) => Err(AccountError::new(
                    ErrorKind::Other,
                    format!("Coffre-fort inaccessible : {e}"),
                )),
            };
        };
        let mut slot = self.token.lock().await;
        if let Some(token) = slot.as_ref().filter(|t| t.is_fresh(auth::now())) {
            return Ok(Credentials::OAuth2 {
                user,
                access_token: token.token.clone(),
            });
        }
        let token = self.refresh(account, provider).await?;
        let access_token = token.token.clone();
        *slot = Some(token);
        Ok(Credentials::OAuth2 { user, access_token })
    }

    async fn refresh(
        &self,
        account: &Account,
        provider: avero_oauth::Provider,
    ) -> Result<AccessToken, AccountError> {
        let reconnect = || AccountError::new(ErrorKind::Reconnect, "Reconnexion nécessaire");
        let secret = match self.secrets.get(account.id) {
            Ok(Some(text)) => OAuthSecret::from_json(&text).ok_or_else(reconnect)?,
            Ok(None) => return Err(reconnect()),
            Err(e) => {
                return Err(AccountError::new(
                    ErrorKind::Other,
                    format!("Coffre-fort inaccessible : {e}"),
                ));
            }
        };
        let Some(client) = avero_oauth::client_config(provider, &self.config_dir) else {
            return Err(AccountError::new(
                ErrorKind::Other,
                format!(
                    "La connexion {} n'est pas configurée (Réglages → Connexion Google / Microsoft)",
                    auth::provider_name(provider)
                ),
            ));
        };
        match avero_oauth::refresh(provider, &client, &secret.refresh_token).await {
            Ok(tokens) => {
                if let Some(rotated) = tokens.refresh_token {
                    self.store_secret(
                        account.id,
                        &OAuthSecret {
                            refresh_token: rotated,
                        }
                        .to_json(),
                    )?;
                }
                Ok(AccessToken {
                    token: tokens.access_token,
                    expires_at: tokens.expires_at,
                })
            }
            Err(e) => Err(refresh_error(provider, &e)),
        }
    }

    async fn connect(&self, account: &Account) -> Result<C::Source, AccountError> {
        let creds = self.credentials(account).await?;
        match self.connector.connect(&account.imap, &creds).await {
            Ok(session) => Ok(session),
            Err(error) => Err(self.classify(account, error).await),
        }
    }

    /// Opens a session that watches the inbox.
    async fn connect_watch(&self, account: &Account) -> Result<C::Watch, AccountError> {
        let creds = self.credentials(account).await?;
        match self.connector.watch(&account.imap, &creds).await {
            Ok(watch) => Ok(watch),
            Err(error) => Err(self.classify(account, error).await),
        }
    }

    /// French message for a server or database error.
    async fn classify(&self, account: &Account, error: SyncError) -> AccountError {
        if matches!(error, SyncError::Auth(_)) && account.auth != avero_core::AuthKind::Password {
            // A refused token: forget it so the next try refreshes.
            self.set_token(None).await;
        }
        user_error(&error, account)
    }

    /// Runs `op` on `session`, connecting first if needed. A lost or
    /// silent connection is retried once on a new session; a timeout only
    /// if `retry_timeout` (not for an APPEND, which the server may have
    /// done anyway).
    async fn with_session<T>(
        &self,
        account: &Account,
        store: &Store,
        session: &mut Option<C::Source>,
        retry_timeout: bool,
        mut op: impl AsyncFnMut(&mut C::Source, &Store) -> Result<T, SyncError>,
    ) -> Result<T, AccountError> {
        for attempt in 0..2 {
            let mut current = match session.take() {
                Some(current) => current,
                None => self.connect(account).await?,
            };
            match op(&mut current, store).await {
                Ok(value) => {
                    *session = Some(current);
                    return Ok(value);
                }
                Err(SyncError::Connect(_)) if attempt == 0 => continue,
                Err(SyncError::Timeout(_)) if attempt == 0 && retry_timeout => continue,
                Err(error @ (SyncError::Connect(_) | SyncError::Timeout(_))) => {
                    return Err(self.classify(account, error).await);
                }
                Err(error) => {
                    *session = Some(current);
                    return Err(self.classify(account, error).await);
                }
            }
        }
        unreachable!("the loop returns on its second attempt")
    }
}

/// The replies waiting for a sync: they get its `Progress` events, then
/// its `Synced`.
type Listeners = Arc<Mutex<Vec<Reply>>>;

fn lock(listeners: &Listeners) -> std::sync::MutexGuard<'_, Vec<Reply>> {
    // A panic while holding the lock leaves a usable list.
    listeners.lock().unwrap_or_else(|e| e.into_inner())
}

/// How a sync ended.
struct SyncEnd {
    result: Result<(), AccountError>,
    /// The new unread inbox messages it found (even if it failed later,
    /// in another folder).
    arrivals: Vec<Arrival>,
}

impl SyncEnd {
    fn failed(error: AccountError) -> SyncEnd {
        SyncEnd {
            result: Err(error),
            arrivals: Vec::new(),
        }
    }
}

/// A sync running on its own thread, with its own IMAP session and its
/// own `Store` connection, so the worker keeps serving the user's jobs.
struct RunningSync {
    listeners: Listeners,
    /// Dropping it (or sending) stops the sync at its next pause.
    cancel: Option<oneshot::Sender<()>>,
    /// How it ended; closed without a value if the sync was stopped.
    done: oneshot::Receiver<SyncEnd>,
    /// The settings version it started with (see `Worker::version`).
    version: u64,
    /// Sync requests that must not join this one (the settings changed,
    /// or the inbox changed after it started): they get a new sync once
    /// this one ends.
    next: Vec<Reply>,
    /// Whether the watch subscriber is already in `next`.
    rerun_for_change: bool,
}

impl RunningSync {
    /// Stops the sync and waits until its thread has let go of the
    /// session and the secrets.
    async fn stop(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        let _ = (&mut self.done).await;
    }
}

/// What a sync thread needs.
struct SyncTask<C> {
    account: Account,
    db_path: PathBuf,
    login: Arc<Login<C>>,
    listeners: Listeners,
}

impl<C: Connector> SyncTask<C> {
    fn run(self, cancel: oneshot::Receiver<()>, done: oneshot::Sender<SyncEnd>) {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = done.send(SyncEnd::failed(AccountError::new(
                ErrorKind::Other,
                "Impossible de lancer la synchronisation",
            )));
            return;
        };
        runtime.block_on(async move {
            tokio::select! {
                end = self.sync() => {
                    let _ = done.send(end);
                }
                // Asked to stop, or the worker is gone.
                _ = cancel => {}
            }
        });
    }

    async fn sync(&self) -> SyncEnd {
        let store = match Store::open(&self.db_path) {
            Ok(store) => store,
            Err(e) => {
                return SyncEnd::failed(AccountError::new(
                    ErrorKind::Other,
                    format!("Erreur de la base locale : {e}"),
                ));
            }
        };
        let id = self.account.id;
        let listeners = &self.listeners;
        let mut session = None;
        let mut arrivals: Vec<Arrival> = Vec::new();
        let result = self
            .login
            .with_session(
                &self.account,
                &store,
                &mut session,
                true,
                async |session, store| {
                    // Kept up to date after every batch, so what an
                    // attempt found survives its failure.
                    let mut found = Vec::new();
                    let result = sync_account(store, session, id, |report| {
                        found.clone_from(&report.arrivals);
                        for reply in lock(listeners).iter() {
                            let _ = reply.send(Event::Progress);
                        }
                    })
                    .await;
                    // A retried attempt may fetch a batch again.
                    for arrival in found {
                        if !arrivals.iter().any(|known| known.uid == arrival.uid) {
                            arrivals.push(arrival);
                        }
                    }
                    result
                },
            )
            .await
            .map(|_| ());
        SyncEnd { result, arrivals }
    }
}

/// What the inbox watcher needs; it runs on its own thread, with its own
/// IMAP session.
struct WatchTask<C> {
    account: Account,
    login: Arc<Login<C>>,
    retry: Retry,
    /// Gets `()` each time the inbox changes.
    changed: UnboundedSender<()>,
}

impl<C: Connector> WatchTask<C> {
    /// Watches until cancelled; `_done` is dropped when the thread ends.
    fn run(self, cancel: oneshot::Receiver<()>, _done: oneshot::Sender<()>) {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            tokio::select! {
                () = self.watch() => {}
                // Asked to stop, or the worker is gone.
                _ = cancel => {}
            }
        });
    }

    /// Connects, waits for changes, and connects again after a failure,
    /// a little later each time. Ends when the server refuses the
    /// credentials: trying again could get the account locked. Watching
    /// starts again once they are fixed (the settings change).
    async fn watch(&self) {
        let mut delay = self.retry.first;
        loop {
            match self.login.connect_watch(&self.account).await {
                Ok(mut watcher) => {
                    while let Ok(change) = watcher.wait().await {
                        delay = self.retry.first;
                        if change == Change::Changed && self.changed.send(()).is_err() {
                            return;
                        }
                    }
                }
                Err(error) if matches!(error.kind, ErrorKind::Auth | ErrorKind::Reconnect) => {
                    return;
                }
                Err(_) => {}
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.retry.max);
        }
    }
}

/// An inbox watcher running on its own thread.
struct RunningWatch {
    /// `()` each time the inbox changes; closed when the watcher ends
    /// on its own (credentials refused).
    changes: UnboundedReceiver<()>,
    /// Dropping it (or sending) stops the watcher.
    cancel: Option<oneshot::Sender<()>>,
    /// Closed when the watcher's thread is done.
    done: oneshot::Receiver<()>,
}

impl RunningWatch {
    /// Stops the watcher and waits until its thread has let go of the
    /// session and the secrets.
    async fn stop(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        let _ = (&mut self.done).await;
    }
}

/// What woke the worker loop up.
enum Wake {
    Job(Option<Job>),
    Synced(SyncEnd),
    /// The inbox changed (`Some`), or the watcher ended (`None`).
    Watch(Option<()>),
}

/// Waits for the running sync to end; never ends when none runs.
async fn sync_end(sync: &mut Option<RunningSync>) -> SyncEnd {
    match sync {
        Some(running) => (&mut running.done)
            .await
            .unwrap_or_else(|_| SyncEnd::failed(sync_stopped())),
        None => std::future::pending().await,
    }
}

/// Waits for news from the watcher; never ends when none runs.
async fn watch_news(watch: &mut Option<RunningWatch>) -> Option<()> {
    match watch {
        Some(running) => running.changes.recv().await,
        None => std::future::pending().await,
    }
}

fn sync_stopped() -> AccountError {
    AccountError::new(ErrorKind::Other, "La synchronisation s'est arrêtée")
}

struct Worker<C: Connector> {
    account: Account,
    store: Store,
    db_path: PathBuf,
    login: Arc<Login<C>>,
    /// The session for the user's actions (the sync has its own).
    session: Option<C::Source>,
    /// Bumped when the account's settings or secret change, so that a
    /// sync started before is not taken for an up-to-date one.
    version: u64,
    sync: Option<RunningSync>,
    /// Where what the worker does on its own goes (see [`Job::Watch`]);
    /// the inbox is only watched once it is set.
    events: Option<Reply>,
    watch: Option<RunningWatch>,
    retry: Retry,
    /// Archive folder path, once looked up (`Some(None)`: there is none).
    archive: Option<Option<String>>,
    prefetch: VecDeque<MessageRef>,
    prefetch_replies: Vec<Reply>,
    prefetched: usize,
}

impl<C: Connector> Worker<C> {
    fn new(setup: WorkerSetup, connector: C, retry: Retry, store: Store) -> Worker<C> {
        Worker {
            account: setup.account,
            store,
            db_path: setup.db_path,
            login: Arc::new(Login {
                connector,
                secrets: setup.secrets,
                config_dir: setup.config_dir,
                token: tokio::sync::Mutex::new(None),
            }),
            session: None,
            version: 0,
            sync: None,
            events: None,
            watch: None,
            retry,
            archive: None,
            prefetch: VecDeque::new(),
            prefetch_replies: Vec::new(),
            prefetched: 0,
        }
    }

    /// Handles jobs as they come, the end of the sync running beside
    /// them and the inbox watcher's news; when idle, downloads queued
    /// bodies one at a time so user actions never wait for the whole
    /// prefetch.
    async fn run(mut self, mut jobs: UnboundedReceiver<Job>) {
        loop {
            let wake = if self.prefetch.is_empty() {
                tokio::select! {
                    job = jobs.recv() => Wake::Job(job),
                    end = sync_end(&mut self.sync) => Wake::Synced(end),
                    news = watch_news(&mut self.watch) => Wake::Watch(news),
                }
            } else if let Some(end) = self.sync_ended() {
                Wake::Synced(end)
            } else if let Some(news) = self.watch_news_now() {
                Wake::Watch(news)
            } else {
                match jobs.try_recv() {
                    Ok(job) => Wake::Job(Some(job)),
                    Err(mpsc::error::TryRecvError::Empty) => {
                        self.prefetch_one().await;
                        continue;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => Wake::Job(None),
                }
            };
            match wake {
                Wake::Job(Some(job)) => {
                    if self.handle(job).await == Flow::Stop {
                        break;
                    }
                }
                Wake::Job(None) => break,
                Wake::Synced(end) => self.finish_sync(end).await,
                Wake::Watch(Some(())) => self.sync_after_change(),
                // The watcher ended on its own (credentials refused).
                Wake::Watch(None) => self.watch = None,
            }
        }
    }

    /// The end of the running sync if it has ended, without waiting.
    fn sync_ended(&mut self) -> Option<SyncEnd> {
        let running = self.sync.as_mut()?;
        match running.done.try_recv() {
            Ok(end) => Some(end),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => Some(SyncEnd::failed(sync_stopped())),
        }
    }

    /// The watcher's news if there is some, without waiting.
    fn watch_news_now(&mut self) -> Option<Option<()>> {
        let running = self.watch.as_mut()?;
        match running.changes.try_recv() {
            Ok(()) => Some(Some(())),
            Err(mpsc::error::TryRecvError::Empty) => None,
            Err(mpsc::error::TryRecvError::Disconnected) => Some(None),
        }
    }

    async fn handle(&mut self, job: Job) -> Flow {
        match job {
            Job::SetPassword { password, reply } => {
                let result = self.login.store_secret(self.account.id, &password);
                self.settings_changed().await;
                let _ = reply.send(Event::Done(result));
            }
            Job::SetOAuth {
                refresh_token,
                access,
                reply,
            } => {
                let result = match refresh_token {
                    Some(token) => self.login.store_secret(
                        self.account.id,
                        &OAuthSecret {
                            refresh_token: token,
                        }
                        .to_json(),
                    ),
                    None => Ok(()),
                };
                self.login.set_token(Some(access)).await;
                self.settings_changed().await;
                let _ = reply.send(Event::Done(result));
            }
            Job::UpdateAccount(account) => {
                self.account = account;
                self.settings_changed().await;
            }
            Job::Sync { reply } => self.request_sync(reply),
            Job::Watch { reply } => {
                self.events = Some(reply);
                if self.watch.is_none() {
                    self.start_watch();
                }
            }
            Job::FetchBody { target, reply } => {
                let result = self.download_body(&target).await;
                let _ = reply.send(Event::Body {
                    row_id: target.row_id,
                    result: result.map_err(|e| e.message),
                });
            }
            Job::SetSeen {
                targets,
                seen,
                reply,
            } => {
                let result = self.set_seen(&targets, seen).await;
                let _ = reply.send(Event::Done(result));
            }
            Job::Move { targets, to, reply } => {
                let result = self.move_to(&targets, to).await;
                let _ = reply.send(Event::Done(result));
            }
            Job::DeleteForever { targets, reply } => {
                let result = self.delete_forever(&targets).await;
                let _ = reply.send(Event::Done(result));
            }
            Job::Send { outgoing, reply } => {
                let result = self.send(&outgoing).await;
                let _ = reply.send(Event::Sent(result));
            }
            Job::Shutdown { delete_secret } => {
                // The sync and watcher threads must be done with the
                // secret before it is deleted (a refresh could store a
                // new one).
                if let Some(running) = self.sync.take() {
                    running.stop().await;
                }
                self.stop_watch().await;
                if delete_secret {
                    let _ = self.login.secrets.delete(self.account.id);
                }
                return Flow::Stop;
            }
        }
        Flow::Continue
    }

    /// Forgets the session; the next sync uses the new settings, and the
    /// inbox watcher starts again with them (also after refused
    /// credentials, which may be fixed now).
    async fn settings_changed(&mut self) {
        self.session = None;
        self.version += 1;
        if self.events.is_some() {
            self.stop_watch().await;
            self.start_watch();
        }
    }

    /// Starts watching the inbox on a thread of its own.
    fn start_watch(&mut self) {
        let (changed, changes) = mpsc::unbounded_channel();
        let (cancel, cancelled) = oneshot::channel();
        let (finished, done) = oneshot::channel();
        let task = WatchTask {
            account: self.account.clone(),
            login: self.login.clone(),
            retry: self.retry,
            changed,
        };
        // If the thread cannot start, `changes` closes: the worker then
        // goes without watching, like after refused credentials.
        let _ = std::thread::Builder::new()
            .name(format!("avero-watch-{}", self.account.id.0))
            .spawn(move || task.run(cancelled, finished));
        self.watch = Some(RunningWatch {
            changes,
            cancel: Some(cancel),
            done,
        });
    }

    async fn stop_watch(&mut self) {
        if let Some(running) = self.watch.take() {
            running.stop().await;
        }
    }

    /// The inbox changed: a sync must start after this moment. The one
    /// running may have passed the inbox already, so instead of joining
    /// it, the watch subscriber gets one more sync right after it.
    fn sync_after_change(&mut self) {
        let Some(events) = self.events.clone() else {
            return;
        };
        match &mut self.sync {
            Some(running) => {
                if !running.rerun_for_change {
                    running.rerun_for_change = true;
                    running.next.push(events);
                }
            }
            None => self.start_sync(vec![events]),
        }
    }

    /// Runs `op` on the user's IMAP session, connecting first if needed.
    async fn with_session<T>(
        &mut self,
        op: impl AsyncFnMut(&mut C::Source, &Store) -> Result<T, SyncError>,
    ) -> Result<T, AccountError> {
        self.with_session_retrying(true, op).await
    }

    /// Like `with_session`; a timeout is retried only if `retry_timeout`.
    async fn with_session_retrying<T>(
        &mut self,
        retry_timeout: bool,
        op: impl AsyncFnMut(&mut C::Source, &Store) -> Result<T, SyncError>,
    ) -> Result<T, AccountError> {
        self.login
            .with_session(
                &self.account,
                &self.store,
                &mut self.session,
                retry_timeout,
                op,
            )
            .await
    }

    /// Only one sync runs at a time. A request during a sync joins it
    /// (it gets the same events): starting another right after would
    /// download almost nothing more. But if the settings or the secret
    /// changed since the running sync started, its result may be stale:
    /// the request then gets a new sync, run as soon as this one ends.
    fn request_sync(&mut self, reply: Reply) {
        let version = self.version;
        if let Some(running) = &mut self.sync {
            if running.version == version {
                lock(&running.listeners).push(reply);
            } else {
                running.next.push(reply);
            }
            return;
        }
        self.start_sync(vec![reply]);
    }

    fn start_sync(&mut self, replies: Vec<Reply>) {
        let listeners: Listeners = Arc::new(Mutex::new(replies));
        let (cancel, cancelled) = oneshot::channel();
        let (finished, done) = oneshot::channel();
        let task = SyncTask {
            account: self.account.clone(),
            db_path: self.db_path.clone(),
            login: self.login.clone(),
            listeners: listeners.clone(),
        };
        // If the thread cannot start, `finished` is dropped and the
        // listeners get `sync_stopped`.
        let _ = std::thread::Builder::new()
            .name(format!("avero-sync-{}", self.account.id.0))
            .spawn(move || task.run(cancelled, finished));
        self.sync = Some(RunningSync {
            listeners,
            cancel: Some(cancel),
            done,
            version: self.version,
            next: Vec::new(),
            rerun_for_change: false,
        });
    }

    /// Answers the replies of the sync that ended, tells the watch
    /// subscriber about new mail, queues the prefetch, and starts the
    /// sync that was asked meanwhile, if any.
    async fn finish_sync(&mut self, end: SyncEnd) {
        let Some(running) = self.sync.take() else {
            return;
        };
        let SyncEnd { result, arrivals } = end;
        if !arrivals.is_empty()
            && let Some(events) = &self.events
        {
            let _ = events.send(Event::NewMail(arrivals));
        }
        if running.version == self.version && self.events.is_some() {
            match &result {
                // No watching with refused credentials (the watcher
                // would try them again and again).
                Err(error) if matches!(error.kind, ErrorKind::Auth | ErrorKind::Reconnect) => {
                    self.stop_watch().await;
                }
                // Accepted again (the server had refused them for a
                // while): watch again.
                Ok(()) if self.watch.is_none() => self.start_watch(),
                _ => {}
            }
        }
        let replies = std::mem::take(&mut *lock(&running.listeners));
        match result {
            Ok(()) => {
                let archive = self.archive_path().await.is_some();
                self.queue_prefetch(replies.clone());
                for reply in &replies {
                    let _ = reply.send(Event::Synced(Ok(archive)));
                }
            }
            Err(error) => {
                for reply in &replies {
                    let _ = reply.send(Event::Synced(Err(error.clone())));
                }
            }
        }
        if !running.next.is_empty() {
            self.start_sync(running.next);
        }
    }

    /// Queues the newest inbox messages that have no cached body. The
    /// replies stay open until they are all downloaded.
    fn queue_prefetch(&mut self, replies: Vec<Reply>) {
        let filter = AccountFilter::Only(self.account.id);
        let items = self
            .store
            .unified(FolderRole::Inbox, filter, PREFETCH, 0)
            .unwrap_or_default();
        let rows: Vec<i64> = items
            .iter()
            .filter_map(|item| item.row_ids.first().copied())
            .filter(|&row| matches!(self.store.body(row), Ok(None)))
            .collect();
        self.prefetch = self
            .store
            .message_refs(&rows)
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.prefetched = 0;
        self.prefetch_replies = if self.prefetch.is_empty() {
            Vec::new()
        } else {
            replies
        };
    }

    async fn prefetch_one(&mut self) {
        let Some(target) = self.prefetch.pop_front() else {
            return;
        };
        if self.download_body(&target).await.is_err() {
            // Not worth insisting: the user can still open the message.
            self.prefetch.clear();
        }
        self.prefetched += 1;
        if self.prefetch.is_empty() || self.prefetched.is_multiple_of(5) {
            for reply in &self.prefetch_replies {
                let _ = reply.send(Event::Progress);
            }
        }
        if self.prefetch.is_empty() {
            self.prefetch_replies.clear();
        }
    }

    /// Downloads, parses and caches one body (nothing to do if cached).
    async fn download_body(&mut self, target: &MessageRef) -> Result<(), AccountError> {
        if matches!(self.store.body(target.row_id), Ok(Some(_))) {
            return Ok(());
        }
        let raw = self
            .with_session(async |session, _store| {
                session.select(&target.folder_path).await?;
                session.fetch_body(target.uid).await
            })
            .await?;
        let Some(raw) = raw else {
            return Err(AccountError::new(
                ErrorKind::Other,
                "Ce message n'existe plus sur le serveur",
            ));
        };
        let parsed =
            guarded(|| parse_body(&raw)).map_err(|e| AccountError::new(ErrorKind::Other, e))?;
        self.store
            .put_body(
                target.row_id,
                &raw,
                &parsed.text,
                !parsed.attachments.is_empty(),
            )
            .map_err(|e| {
                AccountError::new(ErrorKind::Other, format!("Erreur de la base locale : {e}"))
            })
    }

    /// Sends the flag, then lets the server's flags apply to these rows
    /// again (on failure too: the next sync brings the truth back).
    async fn set_seen(&mut self, targets: &[MessageRef], seen: bool) -> Result<(), AccountError> {
        let mut result = Ok(());
        for (folder, (uids, _rows)) in by_folder(targets) {
            result = self
                .with_session(async |session, _store| {
                    session.select(&folder).await?;
                    session.set_seen(&uids, seen).await
                })
                .await;
            if result.is_err() {
                break;
            }
        }
        let rows: Vec<i64> = targets.iter().map(|t| t.row_id).collect();
        let _ = self.store.confirm_seen(&rows);
        result
    }

    /// Shows rows hidden for a move that did not happen.
    fn show(&self, rows: &[i64]) {
        let _ = self.store.set_hidden(rows, false);
    }

    /// Runs `op` on each folder's messages; rows are deleted when the
    /// server confirms, shown again otherwise (and after a failure, the
    /// remaining folders are not tried).
    async fn act_per_folder(
        &mut self,
        targets: &[MessageRef],
        skip: Option<&str>,
        op: impl AsyncFn(&mut C::Source, &[u32]) -> Result<(), SyncError>,
    ) -> Result<(), AccountError> {
        let mut result = Ok(());
        for (folder, (uids, rows)) in by_folder(targets) {
            if result.is_err() || skip == Some(folder.as_str()) {
                self.show(&rows);
                continue;
            }
            let done = self
                .with_session(async |session, _store| {
                    session.select(&folder).await?;
                    op(session, &uids).await
                })
                .await;
            match done {
                Ok(()) => {
                    let _ = self.store.delete_messages(&rows);
                }
                Err(error) => {
                    self.show(&rows);
                    result = Err(error);
                }
            }
        }
        result
    }

    async fn delete_forever(&mut self, targets: &[MessageRef]) -> Result<(), AccountError> {
        self.act_per_folder(targets, None, async |session, uids| {
            session.delete_permanently(uids).await
        })
        .await
    }

    async fn move_to(
        &mut self,
        targets: &[MessageRef],
        to: Destination,
    ) -> Result<(), AccountError> {
        let destination = match to {
            Destination::Archive => self.archive_path().await,
            Destination::Trash => self
                .store
                .folder_by_role(self.account.id, FolderRole::Trash)
                .ok()
                .flatten()
                .map(|f| f.path),
        };
        let Some(destination) = destination else {
            let rows: Vec<i64> = targets.iter().map(|t| t.row_id).collect();
            self.show(&rows);
            let what = match to {
                Destination::Archive => "Aucun dossier d'archives sur ce compte",
                Destination::Trash => "Aucune corbeille sur ce compte",
            };
            return Err(AccountError::new(ErrorKind::Other, what));
        };
        // Messages already in the destination stay where they are.
        let skip = destination.clone();
        self.act_per_folder(targets, Some(&skip), async |session, uids| {
            session.move_to(uids, &destination).await
        })
        .await
    }

    /// The archive folder: one with the Archive role, else (Gmail) the
    /// `\All` folder, which sync skips but which is where archived mail
    /// lives. Looked up once per session of the worker.
    async fn archive_path(&mut self) -> Option<String> {
        if let Some(known) = &self.archive {
            return known.clone();
        }
        if let Ok(Some(folder)) = self
            .store
            .folder_by_role(self.account.id, FolderRole::Archive)
        {
            self.archive = Some(Some(folder.path.clone()));
            return Some(folder.path);
        }
        let listed = self
            .with_session(async |session, _store| session.list_folders().await)
            .await
            .ok()?;
        let found = listed
            .into_iter()
            .find(|f| {
                f.attributes
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case("\\All") || a.eq_ignore_ascii_case("\\Archive"))
            })
            .map(|f| f.path);
        self.archive = Some(found.clone());
        found
    }

    async fn send(&mut self, outgoing: &Outgoing) -> Result<Option<String>, String> {
        if self.account.smtp.host.trim().is_empty() {
            return Err(SMTP_MISSING.to_string());
        }
        let creds = self
            .login
            .credentials(&self.account)
            .await
            .map_err(|e| e.message)?;
        let raw = smtp::send(&self.account.smtp, &creds, outgoing)
            .await
            .map_err(|e| send_message(&e, &self.account))?;
        if auth::server_saves_sent_mail(&self.account.smtp.host, &self.account.imap.host) {
            return Ok(None);
        }
        let sent = self
            .store
            .folder_by_role(self.account.id, FolderRole::Sent)
            .ok()
            .flatten();
        let Some(sent) = sent else {
            return Ok(Some(
                "Message envoyé, mais aucun dossier « Envoyés » pour en garder une copie".into(),
            ));
        };
        // Never retried after a timeout: the copy may already be there.
        let appended = self
            .with_session_retrying(false, async |session, _store| {
                session.append(&sent.path, &raw, true).await
            })
            .await;
        Ok(appended.err().map(|e| {
            format!(
                "Message envoyé, mais la copie dans « Envoyés » a échoué : {}",
                e.message
            )
        }))
    }
}

/// Shown when an account has no SMTP server (accounts made before V1).
pub const SMTP_MISSING: &str =
    "Le serveur d'envoi de ce compte n'est pas configuré (Réglages › Serveurs).";

/// Groups messages by folder: their UIDs and row ids, in order.
fn by_folder(targets: &[MessageRef]) -> BTreeMap<String, (Vec<u32>, Vec<i64>)> {
    let mut groups: BTreeMap<String, (Vec<u32>, Vec<i64>)> = BTreeMap::new();
    for target in targets {
        let (uids, rows) = groups.entry(target.folder_path.clone()).or_default();
        uids.push(target.uid);
        rows.push(target.row_id);
    }
    groups
}

/// Turns a sync error into a message for the user (never includes the
/// password or token).
pub fn user_error(error: &SyncError, account: &Account) -> AccountError {
    let cfg = &account.imap;
    match error {
        SyncError::Connect(detail) => AccountError::new(
            ErrorKind::Network,
            format!("Connexion impossible à {} : {detail}", cfg.host),
        ),
        SyncError::Timeout(detail) => AccountError::new(
            ErrorKind::Network,
            format!("Le serveur {} ne répond pas : {detail}", cfg.host),
        ),
        SyncError::Auth(_) => AccountError::new(
            ErrorKind::Auth,
            format!("Identifiants refusés pour {}", cfg.username),
        ),
        SyncError::Protocol(detail) => AccountError::new(
            ErrorKind::Other,
            format!("Erreur du serveur {} : {detail}", cfg.host),
        ),
        SyncError::Store(detail) => AccountError::new(
            ErrorKind::Other,
            format!("Erreur de la base locale : {detail}"),
        ),
    }
}

/// French message for a sending failure.
fn send_message(error: &SendError, account: &Account) -> String {
    match error {
        SendError::InvalidAddress(address) => format!("Adresse invalide : {address}"),
        SendError::Connect(detail) => {
            format!("Connexion impossible à {} : {detail}", account.smtp.host)
        }
        SendError::Auth(_) => format!(
            "Le serveur d'envoi {} a refusé les identifiants",
            account.smtp.host
        ),
        SendError::Rejected(detail) => format!("Envoi refusé par le serveur : {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::secrets::MemorySecrets;
    use avero_core::{AccountColor, AccountId, AuthKind, FolderId, NewAccount, ServerConfig};
    use avero_sync::source::{FetchedHeader, FolderStatus, RemoteFolder};
    use iced::futures::StreamExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn refresh_errors_are_classified() {
        use avero_oauth::{OAuthError, Provider};
        let wrong_client = refresh_error(
            Provider::Google,
            &OAuthError::Server("invalid_client".into()),
        );
        assert_eq!(wrong_client.kind, ErrorKind::Other);
        assert!(wrong_client.message.contains("Réglages"));
        assert_eq!(
            refresh_error(
                Provider::Google,
                &OAuthError::Server("invalid_grant".into())
            )
            .kind,
            ErrorKind::Reconnect
        );
        assert_eq!(
            refresh_error(Provider::Microsoft, &OAuthError::Network("dns".into())).kind,
            ErrorKind::Network
        );
    }

    fn temp_db(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("avero-worker-{}-{tag}.db", std::process::id()))
    }

    fn account_at(db: &Path, host: &str, port: u16, auth: AuthKind) -> Account {
        let store = Store::open(db).unwrap();
        let server = ServerConfig {
            host: host.into(),
            port,
            username: "a@x".into(),
            security: avero_core::Security::Tls,
        };
        let mut account = store
            .add_account(NewAccount {
                name: "Test".into(),
                email: "a@x".into(),
                imap: server.clone(),
                smtp: server,
                auth,
            })
            .unwrap();
        account.color = AccountColor::palette(0);
        account
    }

    fn spawn(db: &Path, account: Account, secrets: Arc<MemorySecrets>) -> WorkerHandle {
        WorkerHandle::spawn(WorkerSetup {
            account,
            db_path: db.to_path_buf(),
            secrets,
            config_dir: std::env::temp_dir().join("avero-worker-no-config"),
        })
    }

    #[tokio::test]
    async fn unreachable_server_reports_network_error() {
        let db = temp_db("net");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        let secrets = Arc::new(MemorySecrets::new());
        secrets.set(account.id, "pw").unwrap();
        let worker = spawn(&db, account, secrets);
        let events: Vec<Event> = worker.request(|reply| Job::Sync { reply }).collect().await;
        match events.last() {
            Some(Event::Synced(Err(error))) => {
                assert_eq!(error.kind, ErrorKind::Network);
                assert!(error.message.contains("Connexion impossible"), "{error:?}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_password_is_an_auth_error() {
        let db = temp_db("nopw");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        let worker = spawn(&db, account, Arc::new(MemorySecrets::new()));
        let events: Vec<Event> = worker.request(|reply| Job::Sync { reply }).collect().await;
        assert!(matches!(
            events.last(),
            Some(Event::Synced(Err(AccountError {
                kind: ErrorKind::Auth,
                ..
            })))
        ));
    }

    #[tokio::test]
    async fn oauth_account_without_grant_needs_reconnect() {
        let db = temp_db("oauth");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::OAuthGoogle);
        let secrets = Arc::new(MemorySecrets::new());
        // A leftover plain password is not a grant.
        secrets.set(account.id, "not-json").unwrap();
        let worker = spawn(&db, account, secrets);
        let events: Vec<Event> = worker.request(|reply| Job::Sync { reply }).collect().await;
        assert!(matches!(
            events.last(),
            Some(Event::Synced(Err(AccountError {
                kind: ErrorKind::Reconnect,
                ..
            })))
        ));
    }

    #[tokio::test]
    async fn fresh_oauth_token_is_used_without_refresh() {
        let db = temp_db("fresh");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::OAuthGoogle);
        let secrets = Arc::new(MemorySecrets::new());
        let worker = spawn(&db, account.clone(), secrets.clone());
        let access = AccessToken {
            token: "ya29".into(),
            expires_at: auth::now() + 3600,
        };
        let events: Vec<Event> = worker
            .request(|reply| Job::SetOAuth {
                refresh_token: Some("1//rt".into()),
                access,
                reply,
            })
            .collect()
            .await;
        assert!(matches!(events.last(), Some(Event::Done(Ok(())))));
        let stored = secrets.get(account.id).unwrap().unwrap();
        assert_eq!(stored, r#"{"refresh_token":"1//rt"}"#);
        // With a fresh token the worker goes straight to the (closed) server.
        let events: Vec<Event> = worker.request(|reply| Job::Sync { reply }).collect().await;
        assert!(matches!(
            events.last(),
            Some(Event::Synced(Err(AccountError {
                kind: ErrorKind::Network,
                ..
            })))
        ));
    }

    #[tokio::test]
    async fn shutdown_deletes_secret_and_later_jobs_get_gone() {
        let db = temp_db("stop");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        let secrets = Arc::new(MemorySecrets::new());
        secrets.set(account.id, "pw").unwrap();
        let worker = spawn(&db, account.clone(), secrets.clone());
        worker.send(Job::Shutdown {
            delete_secret: true,
        });
        let events: Vec<Event> = worker.request(|reply| Job::Sync { reply }).collect().await;
        // Either the job was refused at send time or dropped unanswered.
        assert!(
            events.is_empty() || matches!(events[0], Event::Gone),
            "{events:?}"
        );
        assert_eq!(secrets.get(account.id).unwrap(), None);
    }

    #[tokio::test]
    async fn dropped_request_never_reaches_the_worker() {
        let db = temp_db("lazy");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        let secrets = Arc::new(MemorySecrets::new());
        let worker = spawn(&db, account.clone(), secrets.clone());
        let stream = worker.request(|reply| Job::SetPassword {
            password: "new".into(),
            reply,
        });
        drop(stream);
        let events: Vec<Event> = worker
            .request(|reply| Job::SetSeen {
                targets: Vec::new(),
                seen: true,
                reply,
            })
            .collect()
            .await;
        assert!(matches!(events.last(), Some(Event::Done(Ok(())))));
        assert_eq!(secrets.get(account.id).unwrap(), None);
    }

    #[test]
    fn user_error_maps_each_error_to_french() {
        let db = temp_db("msg");
        let account = account_at(&db, "imap.ex.com", 993, AuthKind::Password);
        let connect = user_error(&SyncError::Connect("timeout".into()), &account);
        assert_eq!(
            connect.message,
            "Connexion impossible à imap.ex.com : timeout"
        );
        assert_eq!(connect.kind, ErrorKind::Network);
        let auth = user_error(&SyncError::Auth("NO secret-detail".into()), &account);
        assert_eq!(auth.message, "Identifiants refusés pour a@x");
        assert_eq!(auth.kind, ErrorKind::Auth);
        let proto = user_error(&SyncError::Protocol("bad".into()), &account);
        assert_eq!(proto.message, "Erreur du serveur imap.ex.com : bad");
        let store_err = Store::open(&std::env::temp_dir()).err().unwrap();
        let store = user_error(&SyncError::Store(store_err), &account);
        assert!(store.message.starts_with("Erreur de la base locale : "));
    }

    #[test]
    fn targets_are_grouped_by_folder() {
        let target = |folder: &str, uid| MessageRef {
            row_id: i64::from(uid),
            account_id: AccountId(1),
            folder_id: FolderId(1),
            folder_path: folder.into(),
            uid,
        };
        let groups = by_folder(&[target("INBOX", 3), target("Spam", 9), target("INBOX", 1)]);
        assert_eq!(groups["INBOX"], (vec![3, 1], vec![3, 1]));
        assert_eq!(groups["Spam"], (vec![9], vec![9]));
    }

    #[tokio::test]
    async fn closed_reply_without_final_event_gives_gone() {
        let (jobs, mut receiver) = mpsc::unbounded_channel::<Job>();
        // A "worker" that drops every reply after a progress event.
        std::thread::spawn(move || {
            while let Some(job) = receiver.blocking_recv() {
                if let Job::Sync { reply } = job {
                    let _ = reply.send(Event::Progress);
                }
            }
        });
        let handle = WorkerHandle { jobs };
        let events: Vec<Event> = handle.request(|reply| Job::Sync { reply }).collect().await;
        assert!(
            matches!(events.as_slice(), [Event::Progress, Event::Gone]),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn final_event_then_close_ends_quietly() {
        let (jobs, mut receiver) = mpsc::unbounded_channel::<Job>();
        std::thread::spawn(move || {
            while let Some(job) = receiver.blocking_recv() {
                if let Job::Sync { reply } = job {
                    let _ = reply.send(Event::Synced(Ok(true)));
                    let _ = reply.send(Event::Progress);
                }
            }
        });
        let handle = WorkerHandle { jobs };
        let events: Vec<Event> = handle.request(|reply| Job::Sync { reply }).collect().await;
        assert_eq!(events.len(), 2, "{events:?}");
    }

    #[tokio::test]
    async fn sending_without_smtp_server_is_explained() {
        let db = temp_db("nosmtp");
        let mut account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        account.smtp.host.clear();
        let secrets = Arc::new(MemorySecrets::new());
        secrets.set(account.id, "pw").unwrap();
        let worker = spawn(&db, account, secrets);
        let outgoing = Outgoing {
            from_name: None,
            from_addr: "a@x.fr".into(),
            to: vec![avero_sync::smtp::Recipient::new(None, "b@x.fr")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "s".into(),
            body: "b".into(),
            in_reply_to: None,
            references: None,
            attachments: Vec::new(),
        };
        let events: Vec<Event> = worker
            .request(|reply| Job::Send {
                outgoing: Box::new(outgoing),
                reply,
            })
            .collect()
            .await;
        assert!(
            matches!(events.last(), Some(Event::Sent(Err(e))) if e == SMTP_MISSING),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn failed_move_shows_the_rows_again_and_seen_is_confirmed() {
        let db = temp_db("move");
        let account = account_at(&db, "127.0.0.1", 1, AuthKind::Password);
        let store = Store::open(&db).unwrap();
        let inbox = store
            .upsert_folder(account.id, "INBOX", FolderRole::Inbox)
            .unwrap();
        store
            .upsert_folder(account.id, "Trash", FolderRole::Trash)
            .unwrap();
        let message = avero_core::NewMessage {
            uid: 1,
            message_id: None,
            subject: "s".into(),
            from_name: None,
            from_addr: "x@y.z".into(),
            date: 1,
            seen: false,
            has_attachments: false,
            ..Default::default()
        };
        store.insert_messages(inbox.id, &[message]).unwrap();
        let rows: Vec<i64> = store
            .unified(FolderRole::Inbox, AccountFilter::All, 10, 0)
            .unwrap()
            .into_iter()
            .flat_map(|i| i.row_ids)
            .collect();
        let targets = store.message_refs(&rows).unwrap();
        store.set_hidden(&rows, true).unwrap();
        store.set_seen(&rows, true).unwrap();

        let secrets = Arc::new(MemorySecrets::new());
        secrets.set(account.id, "pw").unwrap();
        let worker = spawn(&db, account, secrets);
        // The server is unreachable: the move fails, the row comes back.
        let events: Vec<Event> = worker
            .request(|reply| Job::Move {
                targets: targets.clone(),
                to: Destination::Trash,
                reply,
            })
            .collect()
            .await;
        assert!(
            matches!(events.last(), Some(Event::Done(Err(_)))),
            "{events:?}"
        );
        let visible = store
            .unified(FolderRole::Inbox, AccountFilter::All, 10, 0)
            .unwrap();
        assert_eq!(visible.len(), 1, "row shown again, not deleted");

        // The failed SetSeen still releases the pending marker.
        let _: Vec<Event> = worker
            .request(|reply| Job::SetSeen {
                targets,
                seen: true,
                reply,
            })
            .collect()
            .await;
        store.apply_flags(inbox.id, 1, 1, &[(1, false)]).unwrap();
        assert_eq!(store.inbox_uids(inbox.id).unwrap(), vec![(1, false)]);
    }

    /// A fake IMAP server whose folder list waits until `open` turns
    /// true: a sync on it lasts as long as the test wants.
    ///
    /// Its inbox watchers wait for what the test sends on `inbox`: a
    /// change, or an error (the connection drops).
    struct FakeServer {
        open: tokio::sync::watch::Receiver<bool>,
        /// Signalled when a folder list starts (the sync is running).
        listing: tokio::sync::Notify,
        lists: AtomicUsize,
        connects: AtomicUsize,
        dropped: AtomicUsize,
        inbox: tokio::sync::Mutex<UnboundedReceiver<Result<Change, ()>>>,
        /// Signalled when a watcher starts waiting.
        watching: tokio::sync::Notify,
        /// Watcher connections tried, and dropped.
        watch_connects: AtomicUsize,
        watch_dropped: AtomicUsize,
        /// How many watcher connections fail before one succeeds.
        watch_failures: AtomicUsize,
        /// The watcher's login is refused.
        watch_refused: std::sync::atomic::AtomicBool,
    }

    struct FakeConnector(Arc<FakeServer>);

    impl Connector for FakeConnector {
        type Source = FakeSession;
        type Watch = FakeWatch;

        async fn connect(
            &self,
            _cfg: &ServerConfig,
            _creds: &Credentials,
        ) -> Result<FakeSession, SyncError> {
            self.0.connects.fetch_add(1, Ordering::SeqCst);
            Ok(FakeSession(self.0.clone()))
        }

        async fn watch(
            &self,
            _cfg: &ServerConfig,
            _creds: &Credentials,
        ) -> Result<FakeWatch, SyncError> {
            let server = &self.0;
            server.watch_connects.fetch_add(1, Ordering::SeqCst);
            if server.watch_refused.load(Ordering::SeqCst) {
                return Err(SyncError::Auth("NO refused".into()));
            }
            let failures = server.watch_failures.load(Ordering::SeqCst);
            if failures > 0 {
                server.watch_failures.store(failures - 1, Ordering::SeqCst);
                return Err(SyncError::Connect("unreachable".into()));
            }
            Ok(FakeWatch(server.clone()))
        }
    }

    struct FakeWatch(Arc<FakeServer>);

    impl Drop for FakeWatch {
        fn drop(&mut self) {
            self.0.watch_dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl InboxWatch for FakeWatch {
        async fn wait(&mut self) -> Result<Change, SyncError> {
            let mut inbox = self.0.inbox.lock().await;
            self.0.watching.notify_one();
            match inbox.recv().await {
                Some(Ok(change)) => Ok(change),
                Some(Err(())) | None => Err(SyncError::Connect("lost".into())),
            }
        }
    }

    struct FakeSession(Arc<FakeServer>);

    impl Drop for FakeSession {
        fn drop(&mut self) {
            self.0.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl MailSource for FakeSession {
        async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError> {
            self.0.lists.fetch_add(1, Ordering::SeqCst);
            self.0.listing.notify_one();
            let mut open = self.0.open.clone();
            let _ = open.wait_for(|open| *open).await;
            Ok(Vec::new())
        }
        async fn select(&mut self, _path: &str) -> Result<FolderStatus, SyncError> {
            Ok(FolderStatus {
                uid_validity: 1,
                uid_next: Some(2),
                exists: 1,
            })
        }
        async fn fetch_headers(
            &mut self,
            _from_uid: u32,
            _to_uid: Option<u32>,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            Ok(Vec::new())
        }
        async fn fetch_headers_by_seq(
            &mut self,
            _from_seq: u32,
            _to_seq: u32,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            Ok(Vec::new())
        }
        async fn fetch_uids(&mut self, _from_uid: u32) -> Result<Vec<u32>, SyncError> {
            Ok(Vec::new())
        }
        async fn fetch_flags(
            &mut self,
            _from_uid: u32,
            _to_uid: u32,
        ) -> Result<Vec<(u32, bool)>, SyncError> {
            Ok(Vec::new())
        }
        async fn fetch_body(&mut self, _uid: u32) -> Result<Option<Vec<u8>>, SyncError> {
            Ok(Some(b"Subject: s\r\n\r\nBonjour".to_vec()))
        }
        async fn set_seen(&mut self, _uids: &[u32], _seen: bool) -> Result<(), SyncError> {
            Ok(())
        }
        async fn move_to(&mut self, _uids: &[u32], _dest: &str) -> Result<(), SyncError> {
            Ok(())
        }
        async fn delete_permanently(&mut self, _uids: &[u32]) -> Result<(), SyncError> {
            Ok(())
        }
        async fn append(
            &mut self,
            _folder: &str,
            _raw: &[u8],
            _seen: bool,
        ) -> Result<(), SyncError> {
            Ok(())
        }
    }

    /// A worker on a fake server, with one inbox message (no body yet)
    /// and an archive folder.
    struct Fake {
        worker: WorkerHandle,
        server: Arc<FakeServer>,
        open: tokio::sync::watch::Sender<bool>,
        /// What the inbox watchers see next.
        inbox: UnboundedSender<Result<Change, ()>>,
        target: MessageRef,
        store: Store,
        secrets: Arc<MemorySecrets>,
        account: AccountId,
    }

    fn fake_worker(tag: &str) -> Fake {
        let db = temp_db(tag);
        let account = account_at(&db, "imap.fake", 993, AuthKind::Password);
        let store = Store::open(&db).unwrap();
        let inbox = store
            .upsert_folder(account.id, "INBOX", FolderRole::Inbox)
            .unwrap();
        store
            .upsert_folder(account.id, "Archive", FolderRole::Archive)
            .unwrap();
        let message = avero_core::NewMessage {
            uid: 1,
            message_id: None,
            subject: "s".into(),
            from_name: None,
            from_addr: "x@y.z".into(),
            date: 1,
            seen: false,
            has_attachments: false,
            ..Default::default()
        };
        store.insert_messages(inbox.id, &[message]).unwrap();
        let rows: Vec<i64> = store
            .unified(FolderRole::Inbox, AccountFilter::All, 10, 0)
            .unwrap()
            .into_iter()
            .flat_map(|i| i.row_ids)
            .collect();
        let target = store.message_refs(&rows).unwrap().remove(0);
        let secrets = Arc::new(MemorySecrets::new());
        secrets.set(account.id, "pw").unwrap();
        let (open, gate) = tokio::sync::watch::channel(false);
        let (inbox, changes) = mpsc::unbounded_channel();
        let server = Arc::new(FakeServer {
            open: gate,
            listing: tokio::sync::Notify::new(),
            lists: AtomicUsize::new(0),
            connects: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            inbox: tokio::sync::Mutex::new(changes),
            watching: tokio::sync::Notify::new(),
            watch_connects: AtomicUsize::new(0),
            watch_dropped: AtomicUsize::new(0),
            watch_failures: AtomicUsize::new(0),
            watch_refused: std::sync::atomic::AtomicBool::new(false),
        });
        let worker = WorkerHandle::spawn_with(
            WorkerSetup {
                account: account.clone(),
                db_path: db,
                secrets: secrets.clone(),
                config_dir: std::env::temp_dir().join("avero-worker-no-config"),
            },
            FakeConnector(server.clone()),
            Retry {
                first: Duration::from_millis(10),
                max: Duration::from_millis(40),
            },
        );
        Fake {
            worker,
            server,
            open,
            inbox,
            target,
            store,
            secrets,
            account: account.id,
        }
    }

    /// Starts a sync and collects its events in the background.
    fn start_sync(worker: &WorkerHandle) -> tokio::task::JoinHandle<Vec<Event>> {
        tokio::spawn(worker.request(|reply| Job::Sync { reply }).collect())
    }

    /// Waits until the worker has handled every job sent before.
    async fn flush(worker: &WorkerHandle) {
        let events: Vec<Event> = worker
            .request(|reply| Job::SetSeen {
                targets: Vec::new(),
                seen: true,
                reply,
            })
            .collect()
            .await;
        assert!(matches!(events.last(), Some(Event::Done(Ok(())))));
    }

    /// The sync succeeded (the prefetch may add `Progress` events after).
    fn synced_ok(events: &[Event]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, Event::Synced(Ok(true))))
    }

    #[tokio::test]
    async fn body_is_served_during_a_slow_sync() {
        let fake = fake_worker("readsync");
        let sync = start_sync(&fake.worker);
        fake.server.listing.notified().await;
        let target = fake.target.clone();
        let events: Vec<Event> = fake
            .worker
            .request(|reply| Job::FetchBody { target, reply })
            .collect()
            .await;
        assert!(
            matches!(events.last(), Some(Event::Body { result: Ok(()), .. })),
            "{events:?}"
        );
        assert!(fake.store.body(fake.target.row_id).unwrap().is_some());
        assert!(!sync.is_finished(), "the sync is still waiting");
        fake.open.send(true).unwrap();
        let events = sync.await.unwrap();
        assert!(synced_ok(&events), "{events:?}");
        // Two sessions: the user's and the sync's.
        assert_eq!(fake.server.connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn sync_asked_during_a_sync_joins_it() {
        let fake = fake_worker("merge");
        let first = start_sync(&fake.worker);
        fake.server.listing.notified().await;
        let second = start_sync(&fake.worker);
        flush(&fake.worker).await;
        fake.open.send(true).unwrap();
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert!(synced_ok(&first), "{first:?}");
        assert!(synced_ok(&second), "{second:?}");
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 1, "one sync");
    }

    #[tokio::test]
    async fn sync_asked_after_new_settings_runs_again() {
        let fake = fake_worker("resync");
        let first = start_sync(&fake.worker);
        fake.server.listing.notified().await;
        let _: Vec<Event> = fake
            .worker
            .request(|reply| Job::SetPassword {
                password: "new".into(),
                reply,
            })
            .collect()
            .await;
        let second = start_sync(&fake.worker);
        flush(&fake.worker).await;
        fake.open.send(true).unwrap();
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert!(synced_ok(&first), "{first:?}");
        assert!(synced_ok(&second), "{second:?}");
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 2, "two syncs");
    }

    #[tokio::test]
    async fn shutdown_during_a_sync_stops_it() {
        let fake = fake_worker("stopsync");
        let sync = start_sync(&fake.worker);
        fake.server.listing.notified().await;
        fake.worker.send(Job::Shutdown {
            delete_secret: true,
        });
        // The gate never opens: only the shutdown can end this sync.
        let events = sync.await.unwrap();
        assert!(
            events.is_empty() || matches!(events.as_slice(), [Event::Gone]),
            "{events:?}"
        );
        let later: Vec<Event> = fake
            .worker
            .request(|reply| Job::Sync { reply })
            .collect()
            .await;
        assert!(
            later.is_empty() || matches!(later.as_slice(), [Event::Gone]),
            "{later:?}"
        );
        assert_eq!(fake.secrets.get(fake.account).unwrap(), None);
        // The sync's session was dropped, not left running.
        assert_eq!(
            fake.server.dropped.load(Ordering::SeqCst),
            fake.server.connects.load(Ordering::SeqCst)
        );
    }

    /// Waits until `condition` holds (other threads are at work), at
    /// most a few seconds.
    async fn eventually(what: &str, condition: impl Fn() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never happened: {what}");
    }

    /// The next event of a watch stream that is not a `Progress`.
    async fn next_final(events: &mut BoxStream<'static, Event>) -> Option<Event> {
        let waiting = async {
            loop {
                match events.next().await {
                    Some(Event::Progress) => continue,
                    other => return other,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("no event in time")
    }

    #[tokio::test]
    async fn inbox_change_starts_a_sync() {
        let fake = fake_worker("watch");
        fake.open.send(true).unwrap();
        let mut events = fake.worker.watch();
        fake.server.watching.notified().await;
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 0);
        fake.inbox.send(Ok(Change::Changed)).unwrap();
        let event = next_final(&mut events).await;
        assert!(matches!(event, Some(Event::Synced(Ok(true)))), "{event:?}");
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 1);
        // A quiet round asks for nothing.
        fake.server.watching.notified().await;
        fake.inbox.send(Ok(Change::Quiet)).unwrap();
        fake.server.watching.notified().await;
        flush(&fake.worker).await;
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn inbox_change_during_a_sync_gets_a_sync_after_it() {
        let fake = fake_worker("watchsync");
        let mut events = fake.worker.watch();
        fake.server.watching.notified().await;
        // A sync is running (it may have passed the inbox already).
        let first = start_sync(&fake.worker);
        fake.server.listing.notified().await;
        fake.inbox.send(Ok(Change::Changed)).unwrap();
        // The watcher sent the change and waits again; give the worker
        // a moment to take it while the sync still runs.
        fake.server.watching.notified().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 1);
        fake.open.send(true).unwrap();
        let first = first.await.unwrap();
        assert!(synced_ok(&first), "{first:?}");
        // The change did not just join the running sync: a second one
        // started after it.
        let event = next_final(&mut events).await;
        assert!(matches!(event, Some(Event::Synced(Ok(true)))), "{event:?}");
        assert_eq!(fake.server.lists.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn watcher_reconnects_after_errors() {
        let fake = fake_worker("rewatch");
        fake.open.send(true).unwrap();
        // The first two connections fail, then one drops while waiting.
        fake.server.watch_failures.store(2, Ordering::SeqCst);
        let mut events = fake.worker.watch();
        fake.server.watching.notified().await;
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 3);
        fake.inbox.send(Err(())).unwrap();
        fake.server.watching.notified().await;
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 4);
        assert_eq!(fake.server.watch_dropped.load(Ordering::SeqCst), 1);
        // The new connection still reports changes.
        fake.inbox.send(Ok(Change::Changed)).unwrap();
        let event = next_final(&mut events).await;
        assert!(matches!(event, Some(Event::Synced(Ok(true)))), "{event:?}");
    }

    #[tokio::test]
    async fn refused_credentials_stop_watching_until_fixed() {
        let fake = fake_worker("watchauth");
        fake.server.watch_refused.store(true, Ordering::SeqCst);
        let _events = fake.worker.watch();
        eventually("the watcher's login", || {
            fake.server.watch_connects.load(Ordering::SeqCst) == 1
        })
        .await;
        // No new try on its own, even after several retry delays.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 1);
        // A new password starts the watcher again.
        fake.server.watch_refused.store(false, Ordering::SeqCst);
        let _: Vec<Event> = fake
            .worker
            .request(|reply| Job::SetPassword {
                password: "new".into(),
                reply,
            })
            .collect()
            .await;
        fake.server.watching.notified().await;
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn new_settings_restart_the_watcher() {
        let fake = fake_worker("watchupdate");
        let _events = fake.worker.watch();
        fake.server.watching.notified().await;
        let account = fake
            .store
            .accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == fake.account)
            .unwrap();
        fake.worker.send(Job::UpdateAccount(account));
        fake.server.watching.notified().await;
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 2);
        assert_eq!(fake.server.watch_dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_stops_the_watcher() {
        let fake = fake_worker("watchstop");
        let events = fake.worker.watch();
        fake.server.watching.notified().await;
        fake.worker.send(Job::Shutdown {
            delete_secret: true,
        });
        // The stream ends with the worker, without any event.
        let events: Vec<Event> = tokio::time::timeout(Duration::from_secs(5), events.collect())
            .await
            .expect("the watch stream ends");
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(fake.secrets.get(fake.account).unwrap(), None);
        // Its session was let go before the secret was deleted.
        assert_eq!(fake.server.watch_connects.load(Ordering::SeqCst), 1);
        assert_eq!(fake.server.watch_dropped.load(Ordering::SeqCst), 1);
    }
}
