//! One background worker per account.
//!
//! Each worker owns a thread with a small tokio runtime, its own `Store`
//! connection (a SQLite connection cannot be shared between threads) and
//! a cached IMAP session, reconnected when it breaks. Jobs for the same
//! account run one after the other; different accounts run in parallel.
//!
//! The UI sends a [`Job`] holding a [`Reply`] sender and reads the
//! [`Event`]s from the matching stream (see [`request`]).

use crate::auth::{self, AccessToken, OAuthSecret};
use crate::background::oauth_message;
use avero_core::secrets::SecretStore;
use avero_core::store::Store;
use avero_core::{Account, AccountFilter, Credentials, FolderRole, MessageRef};
use avero_sync::imap::ImapSource;
use avero_sync::parse::{guarded, parse_body};
use avero_sync::smtp::{self, Outgoing, SendError};
use avero_sync::source::{MailSource, SyncError};
use avero_sync::sync::sync_account;
use iced::futures::stream::{self, BoxStream};
use iced::futures::{Stream, StreamExt};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// How many of the newest inbox messages get their body downloaded in
/// the background after a sync.
pub const PREFETCH: usize = 30;

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
        let (jobs, receiver) = mpsc::unbounded_channel();
        let name = format!("avero-account-{}", setup.account.id.0);
        let _ = std::thread::Builder::new()
            .name(name)
            .spawn(move || run_thread(setup, receiver));
        WorkerHandle { jobs }
    }

    /// Sends a job that needs no answer.
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
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
    /// Whether this event ends a job (everything but `Progress`).
    pub fn is_final(&self) -> bool {
        !matches!(self, Event::Progress)
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

fn run_thread(setup: WorkerSetup, mut jobs: UnboundedReceiver<Job>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async move {
        match Store::open(&setup.db_path) {
            Ok(store) => Worker::new(setup, store).run(jobs).await,
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
        Job::UpdateAccount(_) | Job::Shutdown { .. } => {}
    }
}

/// Whether the worker loop goes on.
#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

struct Worker {
    account: Account,
    store: Store,
    secrets: Arc<dyn SecretStore>,
    config_dir: PathBuf,
    token: Option<AccessToken>,
    session: Option<ImapSource>,
    /// Archive folder path, once looked up (`Some(None)`: there is none).
    archive: Option<Option<String>>,
    prefetch: VecDeque<MessageRef>,
    prefetch_reply: Option<Reply>,
    prefetched: usize,
}

impl Worker {
    fn new(setup: WorkerSetup, store: Store) -> Worker {
        Worker {
            account: setup.account,
            store,
            secrets: setup.secrets,
            config_dir: setup.config_dir,
            token: None,
            session: None,
            archive: None,
            prefetch: VecDeque::new(),
            prefetch_reply: None,
            prefetched: 0,
        }
    }

    /// Handles jobs as they come; when idle, downloads queued bodies one
    /// at a time so user actions never wait for the whole prefetch.
    async fn run(mut self, mut jobs: UnboundedReceiver<Job>) {
        loop {
            let job = if self.prefetch.is_empty() {
                match jobs.recv().await {
                    Some(job) => job,
                    None => break,
                }
            } else {
                match jobs.try_recv() {
                    Ok(job) => job,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        self.prefetch_one().await;
                        continue;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            };
            if self.handle(job).await == Flow::Stop {
                break;
            }
        }
    }

    async fn handle(&mut self, job: Job) -> Flow {
        match job {
            Job::SetPassword { password, reply } => {
                let result = self.store_secret(&password);
                self.session = None;
                let _ = reply.send(Event::Done(result));
            }
            Job::SetOAuth {
                refresh_token,
                access,
                reply,
            } => {
                let result = match refresh_token {
                    Some(token) => self.store_secret(
                        &OAuthSecret {
                            refresh_token: token,
                        }
                        .to_json(),
                    ),
                    None => Ok(()),
                };
                self.token = Some(access);
                self.session = None;
                let _ = reply.send(Event::Done(result));
            }
            Job::UpdateAccount(account) => {
                self.account = account;
                self.session = None;
            }
            Job::Sync { reply } => self.sync(reply).await,
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
                if delete_secret {
                    let _ = self.secrets.delete(self.account.id);
                }
                return Flow::Stop;
            }
        }
        Flow::Continue
    }

    fn store_secret(&self, secret: &str) -> Result<(), AccountError> {
        self.secrets.set(self.account.id, secret).map_err(|e| {
            AccountError::new(ErrorKind::Other, format!("Coffre-fort inaccessible : {e}"))
        })
    }

    /// What to log in with: the stored password, or a fresh OAuth access
    /// token (refreshed when it expires within a minute).
    async fn credentials(&mut self) -> Result<Credentials, AccountError> {
        let user = self.account.imap.username.clone();
        let Some(provider) = auth::provider_of(self.account.auth) else {
            return match self.secrets.get(self.account.id) {
                Ok(Some(password)) => Ok(Credentials::Password(password)),
                Ok(None) => Err(AccountError::new(ErrorKind::Auth, "Mot de passe manquant")),
                Err(e) => Err(AccountError::new(
                    ErrorKind::Other,
                    format!("Coffre-fort inaccessible : {e}"),
                )),
            };
        };
        if let Some(token) = self.token.as_ref().filter(|t| t.is_fresh(auth::now())) {
            return Ok(Credentials::OAuth2 {
                user,
                access_token: token.token.clone(),
            });
        }
        let token = self.refresh(provider).await?;
        Ok(Credentials::OAuth2 {
            user,
            access_token: token,
        })
    }

    async fn refresh(&mut self, provider: avero_oauth::Provider) -> Result<String, AccountError> {
        let reconnect = || AccountError::new(ErrorKind::Reconnect, "Reconnexion nécessaire");
        let secret = match self.secrets.get(self.account.id) {
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
                    "La connexion {} n'est pas configurée dans cette version d'Avero",
                    auth::provider_name(provider)
                ),
            ));
        };
        match avero_oauth::refresh(provider, &client, &secret.refresh_token).await {
            Ok(tokens) => {
                if let Some(rotated) = tokens.refresh_token {
                    self.store_secret(
                        &OAuthSecret {
                            refresh_token: rotated,
                        }
                        .to_json(),
                    )?;
                }
                self.token = Some(AccessToken {
                    token: tokens.access_token.clone(),
                    expires_at: tokens.expires_at,
                });
                Ok(tokens.access_token)
            }
            Err(e) if auth::needs_reconnect(&e) => Err(reconnect()),
            Err(e) => Err(AccountError::new(ErrorKind::Network, oauth_message(&e))),
        }
    }

    async fn connect(&mut self) -> Result<ImapSource, AccountError> {
        let creds = self.credentials().await?;
        ImapSource::connect(&self.account.imap, &creds)
            .await
            .map_err(|e| self.classify(e))
    }

    /// Runs `op` on the IMAP session, connecting first if needed. A lost
    /// or silent connection is retried once on a new session.
    async fn with_session<T>(
        &mut self,
        op: impl AsyncFnMut(&mut ImapSource, &Store) -> Result<T, SyncError>,
    ) -> Result<T, AccountError> {
        self.with_session_retrying(true, op).await
    }

    /// Like `with_session`; a timeout is retried only if `retry_timeout`
    /// (not for an APPEND, which the server may have done anyway).
    async fn with_session_retrying<T>(
        &mut self,
        retry_timeout: bool,
        mut op: impl AsyncFnMut(&mut ImapSource, &Store) -> Result<T, SyncError>,
    ) -> Result<T, AccountError> {
        for attempt in 0..2 {
            let mut session = match self.session.take() {
                Some(session) => session,
                None => self.connect().await?,
            };
            match op(&mut session, &self.store).await {
                Ok(value) => {
                    self.session = Some(session);
                    return Ok(value);
                }
                Err(SyncError::Connect(_)) if attempt == 0 => continue,
                Err(SyncError::Timeout(_)) if attempt == 0 && retry_timeout => continue,
                Err(error @ (SyncError::Connect(_) | SyncError::Timeout(_))) => {
                    return Err(self.classify(error));
                }
                Err(error) => {
                    self.session = Some(session);
                    return Err(self.classify(error));
                }
            }
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// French message for a server or database error.
    fn classify(&mut self, error: SyncError) -> AccountError {
        if matches!(error, SyncError::Auth(_))
            && self.account.auth != avero_core::AuthKind::Password
        {
            // A refused token: forget it so the next try refreshes.
            self.token = None;
        }
        user_error(&error, &self.account)
    }

    async fn sync(&mut self, reply: Reply) {
        let id = self.account.id;
        let progress = reply.clone();
        let result = self
            .with_session(async |session, store| {
                sync_account(store, session, id, |_| {
                    let _ = progress.send(Event::Progress);
                })
                .await
            })
            .await;
        match result {
            Ok(_) => {
                let archive = self.archive_path().await.is_some();
                self.queue_prefetch(reply.clone());
                let _ = reply.send(Event::Synced(Ok(archive)));
            }
            Err(error) => {
                let _ = reply.send(Event::Synced(Err(error)));
            }
        }
    }

    /// Queues the newest inbox messages that have no cached body. The
    /// reply stays open until they are all downloaded.
    fn queue_prefetch(&mut self, reply: Reply) {
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
        self.prefetch_reply = (!self.prefetch.is_empty()).then_some(reply);
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
        if (self.prefetch.is_empty() || self.prefetched.is_multiple_of(5))
            && let Some(reply) = &self.prefetch_reply
        {
            let _ = reply.send(Event::Progress);
        }
        if self.prefetch.is_empty() {
            self.prefetch_reply = None;
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
        op: impl AsyncFn(&mut ImapSource, &[u32]) -> Result<(), SyncError>,
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
        let creds = self.credentials().await.map_err(|e| e.message)?;
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
    use iced::futures::StreamExt;
    use std::path::Path;

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
}
