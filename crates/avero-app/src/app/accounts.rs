//! Adding, fixing and removing accounts, and what workers report.

use super::{
    AccountMsg, AccountStatus, AddMsg, App, Backend, Message, Overlay, PasswordDialog, ServersForm,
};
use crate::account_form::{self, AccountForm, Stage};
use crate::auth::{self, OAuthGrant};
use crate::background::{self, LoginError};
use crate::notify::{self, Notice};
use crate::worker::{AccountError, ErrorKind, Event, Job};
use avero_core::store::StoreError;
use avero_core::{AccountFilter, AccountId, Credentials, NewAccount, ServerConfig};
use avero_oauth::Provider;
use avero_sync::sync::Arrival;
use iced::Task;
use std::path::PathBuf;
use tokio::sync::oneshot;

/// Ids of the add-account fields that get the focus.
pub const EMAIL_ID: &str = "add-email";
pub const PASSWORD_ID: &str = "add-password";

/// The secret of an account being created.
enum NewSecret {
    Password(String),
    OAuth(OAuthGrant),
}

impl App {
    // ------------------------------------------------------ worker events

    pub(super) fn on_worker_event(&mut self, account: AccountId, event: Event) -> Task<Message> {
        match event {
            Event::Progress => self.reload_soon(),
            Event::Synced(result) => {
                let status = self.status.entry(account).or_default();
                status.syncing = false;
                match result {
                    Ok(archive) => {
                        status.error = None;
                        status.archive = Some(archive);
                    }
                    Err(error) => status.error = Some(error),
                }
                self.reload_now();
                Task::none()
            }
            Event::Body { row_id, result } => {
                let parse = self.body_arrived(row_id, result);
                Task::batch([parse, self.reload_soon()])
            }
            Event::Done(Ok(())) => Task::none(),
            Event::Done(Err(error)) => {
                // The local copy may now differ from the server: resync.
                let toast = self.toast(error.message.clone(), true);
                Task::batch([toast, self.sync_account(account)])
            }
            Event::Sent(result) => self.sent(result),
            Event::NewMail(arrivals) => {
                self.notify(account, &arrivals);
                Task::none()
            }
            Event::Gone => {
                let status = self.status.entry(account).or_default();
                status.syncing = false;
                status.error = Some(AccountError::new(
                    ErrorKind::Other,
                    "Le service de ce compte s'est arrêté ; redémarrez Avero",
                ));
                if let Some(composer) = &mut self.composer {
                    composer.sending = false;
                }
                self.body_lost();
                self.reload_now();
                Task::none()
            }
        }
    }

    /// What a worker did on its own. Unlike `on_worker_event`, a sync
    /// ending here does not touch `syncing`: the user's own sync may
    /// still be running.
    pub(super) fn on_background_event(
        &mut self,
        account: AccountId,
        event: Event,
    ) -> Task<Message> {
        if self.account(account).is_none() {
            // Removed meanwhile.
            return Task::none();
        }
        match event {
            Event::Progress => self.reload_soon(),
            Event::Synced(result) => {
                let status = self.status.entry(account).or_default();
                match result {
                    Ok(archive) => {
                        status.error = None;
                        status.archive = Some(archive);
                    }
                    Err(error) => status.error = Some(error),
                }
                self.reload_now();
                Task::none()
            }
            Event::NewMail(arrivals) => {
                self.notify(account, &arrivals);
                Task::none()
            }
            Event::Body { .. } | Event::Done(_) | Event::Sent(_) | Event::Gone => Task::none(),
        }
    }

    /// The system notifications for new mail of `account`, unless the
    /// user turned them off.
    pub(super) fn notices(&self, account: AccountId, arrivals: &[Arrival]) -> Vec<Notice> {
        if !self.settings.notifications {
            return Vec::new();
        }
        let name = self.account(account).map_or("", |a| a.name.as_str());
        notify::notices(arrivals, name)
    }

    fn notify(&self, account: AccountId, arrivals: &[Arrival]) {
        notify::show(self.notices(account, arrivals));
    }

    // ------------------------------------------------------- add account

    fn form(&mut self) -> Option<&mut AccountForm> {
        match &mut self.overlay {
            Some(Overlay::AddAccount(form)) => Some(form.as_mut()),
            _ => None,
        }
    }

    fn config_dir(&self) -> Option<PathBuf> {
        match &self.backend {
            Backend::Live { config_dir, .. } => Some(config_dir.clone()),
            Backend::Demo => None,
        }
    }

    /// Whether this build can sign in with `provider`.
    fn oauth_client(&self, provider: Provider) -> Option<avero_oauth::ClientConfig> {
        avero_oauth::client_config(provider, &self.config_dir()?)
    }

    /// Reads which OAuth clients are configured, for the settings.
    pub(super) fn read_oauth_clients(&mut self) {
        let dir = self.config_dir();
        self.oauth_clients = [Provider::Google, Provider::Microsoft]
            .into_iter()
            .filter_map(|provider| {
                let dir = dir.as_deref()?;
                let client = avero_oauth::client_config(provider, dir)?;
                let saved = avero_oauth::saved_client_config(provider, dir).is_some();
                Some((
                    provider,
                    super::ClientInfo {
                        masked_id: avero_oauth::mask_client_id(&client.client_id),
                        saved,
                    },
                ))
            })
            .collect();
    }

    /// Shows the assistant in the open add-account card.
    fn show_setup(&mut self, provider: Provider) -> Task<Message> {
        if let Some(form) = self.form() {
            form.next_generation();
            form.stage = Stage::Setup(provider);
            form.use_password = false;
            form.error = None;
        }
        Task::none()
    }

    /// "Enregistrer et se connecter": saves the client in `oauth.json`,
    /// then starts the sign-in at once (or just closes, from the settings).
    fn save_setup(&mut self) -> Task<Message> {
        let config_dir = self.config_dir();
        let Some(form) = self.form() else {
            return Task::none();
        };
        let Stage::Setup(provider) = form.stage else {
            return Task::none();
        };
        let client = match form.setup.client(provider) {
            Ok(client) => client,
            Err(error) => {
                form.error = Some(error.to_string());
                return Task::none();
            }
        };
        let Some(config_dir) = config_dir else {
            form.error = Some("Mode démo : rien n'est enregistré ni envoyé.".to_string());
            return Task::none();
        };
        if let Err(e) = avero_oauth::save_client_config(provider, &config_dir, &client) {
            form.error = Some(format!("Impossible d'enregistrer la configuration : {e}"));
            return Task::none();
        }
        let setup_only = form.setup_only;
        form.oauth_configured = true;
        form.use_password = false;
        form.setup = Default::default();
        self.read_oauth_clients();
        let name = auth::provider_name(provider);
        if setup_only {
            self.overlay = None;
            return self.toast(format!("Connexion {name} configurée"), false);
        }
        self.start_sign_in(provider)
    }

    /// Opens the assistant alone, from the settings.
    fn edit_client(&mut self, provider: Provider) -> Task<Message> {
        let current = self
            .config_dir()
            .and_then(|dir| avero_oauth::saved_client_config(provider, &dir));
        self.oauth_cancel = None;
        self.overlay = Some(Overlay::AddAccount(Box::new(AccountForm::setup_only(
            provider, current,
        ))));
        Task::none()
    }

    fn remove_client(&mut self, provider: Provider) -> Task<Message> {
        let name = auth::provider_name(provider);
        let Some(config_dir) = self.config_dir() else {
            return self.toast("Mode démo : rien n'est enregistré", false);
        };
        if let Err(e) = avero_oauth::remove_client_config(provider, &config_dir) {
            return self.toast(
                format!("Impossible de modifier la configuration : {e}"),
                true,
            );
        }
        self.read_oauth_clients();
        let used = self
            .accounts
            .iter()
            .any(|a| auth::provider_of(a.auth) == Some(provider));
        let text = if self.oauth_clients.contains_key(&provider) {
            format!(
                "Identifiant retiré : celui intégré à cette version d'Avero est utilisé pour {name}"
            )
        } else if used {
            format!(
                "Connexion {name} retirée. Les comptes {name} ne pourront plus se connecter \
                 tant qu'elle n'est pas configurée à nouveau."
            )
        } else {
            format!("Connexion {name} retirée")
        };
        self.toast(text, false)
    }

    pub(super) fn update_add(&mut self, message: AddMsg) -> Task<Message> {
        match message {
            AddMsg::Open(provider) => self.open_add(provider),
            AddMsg::Field(field, value) => {
                if let Some(form) = self.form() {
                    form.set(field, value);
                }
                Task::none()
            }
            AddMsg::SubmitEmail => self.submit_email(),
            AddMsg::Detected(generation, config) => {
                let configured = config
                    .as_ref()
                    .and_then(|c| {
                        c.auth.iter().find_map(|m| match m {
                            avero_autoconfig::AuthMethod::OAuth(p) => {
                                Some(auth::from_autoconfig(*p))
                            }
                            avero_autoconfig::AuthMethod::Password => None,
                        })
                    })
                    .is_some_and(|p| self.oauth_client(p).is_some());
                if let Some(form) = self.form()
                    && form.generation == generation
                    && form.stage == Stage::Detecting
                {
                    form.apply_detection(config, configured);
                    if let Some(provider) = form.needs_setup() {
                        form.stage = Stage::Setup(provider);
                        return Task::none();
                    }
                    return iced::widget::operation::focus(PASSWORD_ID);
                }
                Task::none()
            }
            AddMsg::Back => {
                if let Some(form) = self.form() {
                    form.next_generation();
                    form.stage = Stage::Email;
                    form.error = None;
                    form.use_password = false;
                }
                Task::none()
            }
            AddMsg::ToggleAdvanced => {
                if let Some(form) = self.form() {
                    form.advanced = !form.advanced;
                }
                Task::none()
            }
            AddMsg::UsePassword => {
                let Some(form) = self.form() else {
                    return Task::none();
                };
                let from_welcome =
                    form.stage == Stage::Setup(Provider::Google) && form.provider.is_none();
                form.choose_password();
                if from_welcome {
                    form.note = Some(
                        "Saisissez votre adresse Gmail : vous pourrez ensuite coller un mot de \
                         passe d'application."
                            .to_string(),
                    );
                    return iced::widget::operation::focus(EMAIL_ID);
                }
                iced::widget::operation::focus(PASSWORD_ID)
            }
            AddMsg::ShowSetup(provider) => self.show_setup(provider),
            AddMsg::EditClient(provider) => self.edit_client(provider),
            AddMsg::SaveSetup => self.save_setup(),
            AddMsg::RemoveClient(provider) => self.remove_client(provider),
            AddMsg::SmtpSecurity(security) => {
                if let Some(form) = self.form() {
                    form.smtp_security = security;
                    form.smtp_port = match security {
                        avero_core::Security::Tls => "465".into(),
                        avero_core::Security::StartTls => "587".into(),
                    };
                }
                Task::none()
            }
            AddMsg::SubmitPassword => self.check_password_form(),
            AddMsg::Checked(generation, result) => self.password_checked(generation, result),
            AddMsg::SignIn(provider) => self.start_sign_in(provider),
            AddMsg::SignedIn(generation, result) => self.signed_in(generation, result),
            AddMsg::CancelWait => {
                self.oauth_cancel = None;
                if let Some(form) = self.form() {
                    form.cancel_wait();
                }
                Task::none()
            }
            AddMsg::Close => {
                self.oauth_cancel = None;
                if matches!(self.overlay, Some(Overlay::AddAccount(_))) {
                    self.overlay = None;
                }
                Task::none()
            }
        }
    }

    fn open_add(&mut self, provider: Option<Provider>) -> Task<Message> {
        self.oauth_cancel = None;
        let mut form = AccountForm::new();
        let focus = iced::widget::operation::focus(EMAIL_ID);
        let Some(provider) = provider else {
            self.overlay = Some(Overlay::AddAccount(Box::new(form)));
            return focus;
        };
        if self.oauth_client(provider).is_some() {
            self.overlay = Some(Overlay::AddAccount(Box::new(form)));
            return self.start_sign_in(provider);
        }
        // No client yet: the one-time assistant, then the sign-in.
        form.stage = Stage::Setup(provider);
        self.overlay = Some(Overlay::AddAccount(Box::new(form)));
        Task::none()
    }

    fn submit_email(&mut self) -> Task<Message> {
        let demo = self.is_demo();
        let Some(form) = self.form() else {
            return Task::none();
        };
        let Some(email) = form.valid_email() else {
            form.error = Some("Adresse e-mail invalide".to_string());
            return Task::none();
        };
        let generation = form.next_generation();
        form.stage = Stage::Detecting;
        form.error = None;
        if let Some(config) = avero_autoconfig::known(&email) {
            return Task::done(Message::Add(AddMsg::Detected(generation, Some(config))));
        }
        if demo {
            return Task::done(Message::Add(AddMsg::Detected(generation, None)));
        }
        Task::perform(background::discover(email), move |config| {
            Message::Add(AddMsg::Detected(generation, config))
        })
    }

    fn check_password_form(&mut self) -> Task<Message> {
        let demo = self.is_demo();
        let Some(form) = self.form() else {
            return Task::none();
        };
        let (account, password) = match form.validate() {
            Ok(valid) => valid,
            Err(error) => {
                form.error = Some(error);
                return Task::none();
            }
        };
        let generation = form.next_generation();
        form.stage = Stage::Checking;
        if demo {
            return Task::done(Message::Add(AddMsg::Checked(generation, Ok(()))));
        }
        let check =
            background::check_account(account.imap, account.smtp, Credentials::Password(password));
        Task::perform(check, move |result| {
            Message::Add(AddMsg::Checked(generation, result))
        })
    }

    fn password_checked(
        &mut self,
        generation: u64,
        result: Result<(), LoginError>,
    ) -> Task<Message> {
        let Some(form) = self.form() else {
            return Task::none();
        };
        if form.generation != generation || form.stage != Stage::Checking {
            return Task::none();
        }
        form.stage = Stage::Credentials;
        if let Err(error) = result {
            form.error = Some(login_message(&error, form.app_password()));
            return Task::none();
        }
        let Ok((account, password)) = form.validate() else {
            return Task::none();
        };
        self.finish_account(account, NewSecret::Password(password))
    }

    fn start_sign_in(&mut self, provider: Provider) -> Task<Message> {
        let Some(client) = self.oauth_client(provider) else {
            return Task::none();
        };
        let Some(form) = self.form() else {
            return Task::none();
        };
        let generation = form.next_generation();
        form.stage = Stage::Authorizing(provider);
        form.error = None;
        let hint = form.valid_email();
        let (cancel, cancelled) = oneshot::channel();
        self.oauth_cancel = Some(cancel);
        Task::perform(
            background::sign_in(provider, client, hint, cancelled),
            move |result| Message::Add(AddMsg::SignedIn(generation, result)),
        )
    }

    fn signed_in(&mut self, generation: u64, result: Result<OAuthGrant, String>) -> Task<Message> {
        if self.form().is_some_and(|f| f.generation == generation) {
            self.oauth_cancel = None;
        }
        let Some(form) = self.form() else {
            return Task::none();
        };
        if form.generation != generation {
            return Task::none();
        }
        let Stage::Authorizing(provider) = form.stage else {
            return Task::none();
        };
        let grant = match result {
            Ok(grant) => grant,
            Err(error) => {
                form.stage = if form.email.trim().is_empty() {
                    Stage::Email
                } else {
                    Stage::Credentials
                };
                form.error = (!error.is_empty()).then_some(error);
                return Task::none();
            }
        };
        // Out of "Authorizing", so that a refusal below is shown.
        form.stage = Stage::Credentials;
        let email = grant
            .email
            .clone()
            .or_else(|| form.valid_email())
            .unwrap_or_default();
        let account = account_form::oauth_account(provider, &email);
        let existing: Vec<AccountId> = self
            .accounts
            .iter()
            .filter(|a| a.email.eq_ignore_ascii_case(&email))
            .map(|a| a.id)
            .collect();
        if !existing.is_empty() {
            return self.switch_to_oauth(&existing, account, grant);
        }
        self.finish_account(account, NewSecret::OAuth(grant))
    }

    /// Signing in to an address already in Avero (typically a password
    /// account whose password is refused) moves that account to OAuth
    /// instead of refusing a duplicate.
    fn switch_to_oauth(
        &mut self,
        ids: &[AccountId],
        new: NewAccount,
        grant: OAuthGrant,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        for &id in ids {
            let Some(mut account) = self.account(id).cloned() else {
                continue;
            };
            if let Err(e) =
                self.store
                    .update_account(id, &account.name, &new.imap, &new.smtp, new.auth)
            {
                if let Some(form) = self.form() {
                    form.error = Some(format!("Erreur de la base locale : {e}"));
                }
                return Task::batch(tasks);
            }
            account.imap = new.imap.clone();
            account.smtp = new.smtp.clone();
            account.auth = new.auth;
            self.send_job(id, Job::UpdateAccount(Box::new(account.clone())));
            if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == id) {
                *slot = account;
            }
            self.status.insert(id, AccountStatus::default());
            let (refresh_token, access) = (grant.refresh_token.clone(), grant.access.clone());
            let store = self.request(id, |reply| Job::SetOAuth {
                refresh_token,
                access,
                reply,
            });
            tasks.push(store.chain(self.sync_account(id)));
        }
        self.overlay = None;
        self.screen = super::Screen::Mail;
        self.reload();
        tasks.push(self.toast(format!("Compte connecté : {}", new.email), false));
        Task::batch(tasks)
    }

    /// Saves a checked account, hands its secret to the new worker and
    /// starts its first sync.
    fn finish_account(&mut self, new: NewAccount, secret: NewSecret) -> Task<Message> {
        // Quick check for the common case; the database is the guarantee.
        let known = self
            .accounts
            .iter()
            .any(|a| a.email.trim().eq_ignore_ascii_case(new.email.trim()));
        let added = if known {
            Err(StoreError::DuplicateAccount)
        } else {
            self.store.add_account(new)
        };
        let account = match added {
            Ok(account) => account,
            Err(e) => {
                let text = match e {
                    StoreError::DuplicateAccount => "Ce compte est déjà dans Avero.".to_string(),
                    e => format!("Impossible d'ajouter le compte : {e}"),
                };
                if let Some(form) = self.form() {
                    form.error = Some(text);
                }
                return Task::none();
            }
        };
        // The secret goes to the keyring before the worker starts, so its
        // first sync finds it. Never to the database.
        let (keyring_value, access) = match secret {
            NewSecret::Password(password) => (Some(password), None),
            NewSecret::OAuth(grant) => (
                grant.refresh_token.map(|token| {
                    auth::OAuthSecret {
                        refresh_token: token,
                    }
                    .to_json()
                }),
                Some(grant.access),
            ),
        };
        if let (Backend::Live { secrets, .. }, Some(value)) = (&self.backend, keyring_value)
            && let Err(e) = secrets.set(account.id, &value)
        {
            let _ = self.store.delete_account(account.id);
            if let Some(form) = self.form() {
                form.error = Some(format!("Coffre-fort inaccessible : {e}"));
            }
            return Task::none();
        }
        self.overlay = None;
        self.accounts.push(account.clone());
        let watching = self.start_worker(&account);
        self.screen = super::Screen::Mail;
        self.reload();
        let id = account.id;
        let sync = self.sync_account(id);
        let toast = self.toast(format!("Compte ajouté : {}", account.email), false);
        let Some(access) = access else {
            return Task::batch([sync, toast, watching]);
        };
        // The access token only lives in the worker's memory.
        let hand_token = self.request(id, |reply| Job::SetOAuth {
            refresh_token: None,
            access,
            reply,
        });
        Task::batch([hand_token.chain(sync), toast, watching])
    }

    // ---------------------------------------------------- manage accounts

    pub(super) fn update_account(&mut self, message: AccountMsg) -> Task<Message> {
        match message {
            AccountMsg::StartRename(id) => {
                if let Some(account) = self.account(id) {
                    self.overlay = Some(Overlay::Rename {
                        account: id,
                        name: account.name.clone(),
                    });
                }
                Task::none()
            }
            AccountMsg::RenameText(text) => {
                if let Some(Overlay::Rename { name, .. }) = &mut self.overlay {
                    *name = text;
                }
                Task::none()
            }
            AccountMsg::SaveRename => self.save_rename(),
            AccountMsg::OpenPassword(id) => {
                self.overlay = Some(Overlay::Password(PasswordDialog {
                    account: id,
                    password: String::new(),
                    error: None,
                    checking: false,
                    generation: 0,
                }));
                Task::none()
            }
            AccountMsg::PasswordText(text) => {
                if let Some(Overlay::Password(dialog)) = &mut self.overlay {
                    dialog.password = text;
                    dialog.error = None;
                }
                Task::none()
            }
            AccountMsg::SubmitPassword => self.check_new_password(),
            AccountMsg::PasswordChecked(generation, result) => {
                self.new_password_checked(generation, result)
            }
            AccountMsg::AskDelete(id) => {
                self.overlay = Some(Overlay::ConfirmDelete(id));
                Task::none()
            }
            AccountMsg::ConfirmDelete => self.delete_account(),
            AccountMsg::Reconnect(id) => self.reconnect(id),
            AccountMsg::Reconnected(id, result) => self.reconnected(id, result),
            AccountMsg::OpenServers(id) => {
                if let Some(account) = self.account(id) {
                    self.overlay = Some(Overlay::Servers(ServersForm::of(account)));
                }
                Task::none()
            }
            AccountMsg::ServerText(field, text) => {
                if let Some(Overlay::Servers(form)) = &mut self.overlay {
                    form.set(field, text);
                }
                Task::none()
            }
            AccountMsg::ServerSecurity(security) => {
                if let Some(Overlay::Servers(form)) = &mut self.overlay {
                    form.smtp_security = security;
                    form.smtp_port = match security {
                        avero_core::Security::Tls => "465".into(),
                        avero_core::Security::StartTls => "587".into(),
                    };
                }
                Task::none()
            }
            AccountMsg::SaveServers => self.save_servers(),
            AccountMsg::SmtpFound(id, found) => self.smtp_found(id, found),
            AccountMsg::EditSignature(id) => {
                if let Some(account) = self.account(id) {
                    self.overlay = Some(Overlay::Signature {
                        account: id,
                        content: iced::widget::text_editor::Content::with_text(&account.signature),
                    });
                }
                Task::none()
            }
            AccountMsg::SignatureEdit(action) => {
                if let Some(Overlay::Signature { content, .. }) = &mut self.overlay {
                    content.perform(action);
                }
                Task::none()
            }
            AccountMsg::SaveSignature => self.save_signature(),
            AccountMsg::CloseDialog => {
                self.oauth_cancel = None;
                self.overlay = None;
                Task::none()
            }
        }
    }

    fn save_rename(&mut self) -> Task<Message> {
        let Some(Overlay::Rename { account, name }) = &self.overlay else {
            return Task::none();
        };
        let (id, name) = (*account, name.trim().to_string());
        if name.is_empty() {
            return Task::none();
        }
        let Some(mut account) = self.account(id).cloned() else {
            return Task::none();
        };
        if let Err(e) =
            self.store
                .update_account(id, &name, &account.imap, &account.smtp, account.auth)
        {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        account.name = name;
        self.send_job(id, Job::UpdateAccount(Box::new(account.clone())));
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == id) {
            *slot = account;
        }
        self.overlay = None;
        Task::none()
    }

    /// Saves the signature being edited (trailing blank lines dropped).
    fn save_signature(&mut self) -> Task<Message> {
        let Some(Overlay::Signature { account, content }) = &self.overlay else {
            return Task::none();
        };
        let (id, signature) = (*account, content.text().trim_end().to_string());
        if let Err(e) = self.store.set_signature(id, &signature) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        // The worker is not told: the signature is added here, in the
        // composer, and a new copy of the account would reconnect it.
        if let Some(account) = self.accounts.iter_mut().find(|a| a.id == id) {
            account.signature = signature;
        }
        self.overlay = None;
        self.toast("Signature enregistrée", false)
    }

    /// Saves the "Serveurs" dialog and resyncs with the new settings.
    fn save_servers(&mut self) -> Task<Message> {
        let Some(Overlay::Servers(form)) = &self.overlay else {
            return Task::none();
        };
        let Some(account) = self.account(form.account).cloned() else {
            return Task::none();
        };
        match form.apply(&account) {
            Ok((imap, smtp)) => {
                let saved = self.save_account_servers(account.id, imap, smtp);
                self.overlay = None;
                self.status.entry(account.id).or_default().error = None;
                match saved {
                    Ok(()) => {
                        let sync = self.sync_account(account.id);
                        let toast = self.toast("Serveurs enregistrés", false);
                        Task::batch([sync, toast])
                    }
                    Err(e) => self.toast(e, true),
                }
            }
            Err(error) => {
                if let Some(Overlay::Servers(form)) = &mut self.overlay {
                    form.error = Some(error);
                }
                Task::none()
            }
        }
    }

    /// Writes new servers to the database, the account list and its worker.
    fn save_account_servers(
        &mut self,
        id: AccountId,
        imap: ServerConfig,
        smtp: ServerConfig,
    ) -> Result<(), String> {
        let Some(mut account) = self.account(id).cloned() else {
            return Ok(());
        };
        self.store
            .update_account(id, &account.name, &imap, &smtp, account.auth)
            .map_err(|e| format!("Erreur de la base locale : {e}"))?;
        account.imap = imap;
        account.smtp = smtp;
        self.send_job(id, Job::UpdateAccount(Box::new(account.clone())));
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == id) {
            *slot = account;
        }
        Ok(())
    }

    /// Accounts made before V1 have no SMTP server: look it up in the
    /// background (built-in table, then the network) and save it.
    pub(super) fn complete_smtp_settings(&self) -> Task<Message> {
        if self.is_demo() {
            return Task::none();
        }
        Task::batch(
            self.accounts
                .iter()
                .filter(|a| a.smtp.host.trim().is_empty())
                .map(|a| {
                    let id = a.id;
                    Task::perform(background::discover_smtp(a.email.clone()), move |found| {
                        Message::Account(AccountMsg::SmtpFound(id, found))
                    })
                }),
        )
    }

    fn smtp_found(&mut self, id: AccountId, found: Option<ServerConfig>) -> Task<Message> {
        let Some(account) = self.account(id).cloned() else {
            return Task::none();
        };
        let Some(mut smtp) = found else {
            return Task::none();
        };
        if !account.smtp.host.trim().is_empty() {
            return Task::none();
        }
        // The login is the one that already works for IMAP.
        smtp.username = account.imap.username.clone();
        if let Err(e) = self.save_account_servers(id, account.imap.clone(), smtp) {
            return self.toast(e, true);
        }
        Task::none()
    }

    fn check_new_password(&mut self) -> Task<Message> {
        let demo = self.is_demo();
        let accounts = self.accounts.clone();
        let Some(Overlay::Password(dialog)) = &mut self.overlay else {
            return Task::none();
        };
        let Some(account) = accounts.iter().find(|a| a.id == dialog.account) else {
            return Task::none();
        };
        let password = auth::clean_password(&dialog.password, app_password_for(&account.email));
        if password.is_empty() {
            dialog.error = Some("Saisissez le mot de passe".to_string());
            return Task::none();
        }
        dialog.checking = true;
        dialog.generation += 1;
        let generation = dialog.generation;
        if demo {
            return Task::done(Message::Account(AccountMsg::PasswordChecked(
                generation,
                Ok(()),
            )));
        }
        let check = background::check_login(account.imap.clone(), Credentials::Password(password));
        Task::perform(check, move |result| {
            Message::Account(AccountMsg::PasswordChecked(generation, result))
        })
    }

    fn new_password_checked(
        &mut self,
        generation: u64,
        result: Result<(), LoginError>,
    ) -> Task<Message> {
        let accounts = self.accounts.clone();
        let Some(Overlay::Password(dialog)) = &mut self.overlay else {
            return Task::none();
        };
        if dialog.generation != generation {
            return Task::none();
        }
        dialog.checking = false;
        let Some(account) = accounts.iter().find(|a| a.id == dialog.account) else {
            return Task::none();
        };
        let app_password = app_password_for(&account.email);
        if let Err(error) = result {
            dialog.error = Some(login_message(&error, app_password));
            return Task::none();
        }
        let password = auth::clean_password(&dialog.password, app_password);
        let id = account.id;
        self.overlay = None;
        self.status.entry(id).or_default().error = None;
        let store = self.request(id, |reply| Job::SetPassword { password, reply });
        let sync = self.sync_account(id);
        let toast = self.toast("Mot de passe mis à jour", false);
        Task::batch([store.chain(sync), toast])
    }

    fn delete_account(&mut self) -> Task<Message> {
        let Some(Overlay::ConfirmDelete(id)) = self.overlay else {
            return Task::none();
        };
        self.overlay = None;
        if let Err(e) = self.store.delete_account(id) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        if let Backend::Live {
            workers, secrets, ..
        } = &mut self.backend
        {
            match workers.remove(&id) {
                Some(worker) => worker.send(Job::Shutdown {
                    delete_secret: true,
                }),
                None => {
                    let secrets = secrets.clone();
                    std::thread::spawn(move || secrets.delete(id));
                }
            }
        }
        self.accounts.retain(|a| a.id != id);
        self.status.remove(&id);
        if self.filter == AccountFilter::Only(id) {
            self.filter = AccountFilter::All;
        }
        if self.composer.as_ref().is_some_and(|c| c.from == id) {
            self.composer = None;
        }
        self.load_first_page();
        if self.accounts.is_empty() {
            self.screen = super::Screen::Mail;
        }
        self.toast("Compte supprimé de cet ordinateur", false)
    }

    fn reconnect(&mut self, id: AccountId) -> Task<Message> {
        let Some(account) = self.account(id).cloned() else {
            return Task::none();
        };
        let Some(provider) = auth::provider_of(account.auth) else {
            return self.update_account(AccountMsg::OpenPassword(id));
        };
        if self.is_demo() {
            return self.toast("Mode démo : aucune connexion", false);
        }
        let Some(client) = self.oauth_client(provider) else {
            // The client was removed: the assistant, then "Reconnecter" again.
            return self.edit_client(provider);
        };
        let (cancel, cancelled) = oneshot::channel();
        self.oauth_cancel = Some(cancel);
        self.overlay = Some(Overlay::Reconnect(id));
        Task::perform(
            background::sign_in(provider, client, Some(account.email.clone()), cancelled),
            move |result| Message::Account(AccountMsg::Reconnected(id, result)),
        )
    }

    fn reconnected(&mut self, id: AccountId, result: Result<OAuthGrant, String>) -> Task<Message> {
        if !matches!(self.overlay, Some(Overlay::Reconnect(account)) if account == id) {
            return Task::none();
        }
        self.overlay = None;
        self.oauth_cancel = None;
        let grant = match result {
            Ok(grant) => grant,
            Err(error) if error.is_empty() => return Task::none(),
            Err(error) => return self.toast(error, true),
        };
        let email = self
            .account(id)
            .map(|a| a.email.clone())
            .unwrap_or_default();
        if let Err(error) = auth::check_grant_email(&email, grant.email.as_deref()) {
            return self.toast(error, true);
        }
        self.status.insert(id, AccountStatus::default());
        let store = self.request(id, |reply| Job::SetOAuth {
            refresh_token: grant.refresh_token,
            access: grant.access,
            reply,
        });
        let sync = self.sync_account(id);
        let toast = self.toast("Compte reconnecté", false);
        Task::batch([store.chain(sync), toast])
    }
}

/// Whether passwords typed for this address are app passwords.
fn app_password_for(email: &str) -> bool {
    avero_autoconfig::known(email).is_some_and(|c| {
        auth::uses_app_password(
            &c.display_name,
            c.password_help.as_ref().map(|h| h.text.as_str()),
        )
    })
}

/// French explanation of a failed login test.
pub fn login_message(error: &LoginError, app_password: bool) -> String {
    match error {
        LoginError::Refused if app_password => {
            "Adresse ou mot de passe refusé. Ce service demande \
             un mot de passe d'application (pas votre mot de passe habituel)."
                .to_string()
        }
        LoginError::Refused => "Adresse ou mot de passe refusé par le serveur.".to_string(),
        LoginError::Unreachable(detail) => {
            format!(
                "Serveur injoignable : {detail}. Vérifiez l'adresse du serveur et votre connexion."
            )
        }
        LoginError::SmtpRefused if app_password => {
            "Le serveur d'envoi (SMTP) a refusé l'adresse ou le mot de passe. Ce service demande \
             un mot de passe d'application (pas votre mot de passe habituel)."
                .to_string()
        }
        LoginError::SmtpRefused => {
            "La réception fonctionne, mais le serveur d'envoi (SMTP) a refusé l'adresse \
             ou le mot de passe. Vérifiez les paramètres avancés."
                .to_string()
        }
        LoginError::SmtpUnreachable(detail) => format!(
            "Serveur d'envoi (SMTP) injoignable : {detail}. Vérifiez son adresse, son port \
             et le chiffrement dans les paramètres avancés."
        ),
        LoginError::Other(detail) => format!("Connexion impossible : {detail}"),
    }
}
