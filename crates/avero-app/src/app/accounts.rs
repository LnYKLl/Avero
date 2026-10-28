//! Adding, fixing and removing accounts, and what workers report.

use super::{AccountMsg, AccountStatus, AddMsg, App, Backend, Message, Overlay, PasswordDialog};
use crate::account_form::{self, AccountForm, Stage};
use crate::auth::{self, OAuthGrant};
use crate::background::{self, LoginError};
use crate::worker::{AccountError, ErrorKind, Event, Job};
use avero_core::{AccountFilter, AccountId, Credentials, NewAccount};
use avero_oauth::Provider;
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
            Event::Progress => {
                self.reload();
                Task::none()
            }
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
                self.reload();
                Task::none()
            }
            Event::Body { row_id, result } => {
                self.body_arrived(row_id, result);
                self.reload();
                Task::none()
            }
            Event::Done(Ok(())) => Task::none(),
            Event::Done(Err(error)) => {
                // The local copy may now differ from the server: resync.
                let toast = self.toast(error.message.clone(), true);
                Task::batch([toast, self.sync_account(account)])
            }
            Event::Sent(result) => self.sent(result),
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
                Task::none()
            }
        }
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
                if let Some(form) = self.form() {
                    form.use_password = true;
                }
                Task::none()
            }
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
        let name = auth::provider_name(provider);
        form.note = Some(match provider {
            Provider::Google => format!(
                "La connexion {name} n'est pas configurée dans cette version d'Avero \
                 (voir docs/oauth-setup.md). Saisissez votre adresse Gmail : vous pourrez \
                 utiliser un mot de passe d'application."
            ),
            Provider::Microsoft => format!(
                "La connexion {name} n'est pas disponible dans cette version d'Avero \
                 (identifiant client manquant, voir docs/oauth-setup.md). Les adresses \
                 Outlook.com et Hotmail ne peuvent pas encore être ajoutées."
            ),
        });
        self.overlay = Some(Overlay::AddAccount(Box::new(form)));
        focus
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
        let check = background::check_login(account.imap, Credentials::Password(password));
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
        let email = grant
            .email
            .clone()
            .or_else(|| form.valid_email())
            .unwrap_or_default();
        let account = account_form::oauth_account(provider, &email);
        self.finish_account(account, NewSecret::OAuth(grant))
    }

    /// Saves a checked account, hands its secret to the new worker and
    /// starts its first sync.
    fn finish_account(&mut self, new: NewAccount, secret: NewSecret) -> Task<Message> {
        if self
            .accounts
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(&new.email))
        {
            if let Some(form) = self.form() {
                form.error = Some("Ce compte est déjà dans Avero.".to_string());
            }
            return Task::none();
        }
        let account = match self.store.add_account(new) {
            Ok(account) => account,
            Err(e) => {
                if let Some(form) = self.form() {
                    form.error = Some(format!("Impossible d'ajouter le compte : {e}"));
                }
                return Task::none();
            }
        };
        self.overlay = None;
        self.accounts.push(account.clone());
        self.start_worker(&account);
        self.screen = super::Screen::Mail;
        self.reload();
        let id = account.id;
        let store_secret = self.request(id, |reply| match secret {
            NewSecret::Password(password) => Job::SetPassword { password, reply },
            NewSecret::OAuth(grant) => Job::SetOAuth {
                refresh_token: grant.refresh_token,
                access: grant.access,
                reply,
            },
        });
        let sync = self.sync_account(id);
        let toast = self.toast(format!("Compte ajouté : {}", account.email), false);
        Task::batch([store_secret.chain(sync), toast])
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
        self.send_job(id, Job::UpdateAccount(account.clone()));
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == id) {
            *slot = account;
        }
        self.overlay = None;
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
            let name = auth::provider_name(provider);
            return self.toast(
                format!("La connexion {name} n'est pas configurée dans cette version d'Avero"),
                true,
            );
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
        LoginError::Other(detail) => format!("Connexion impossible : {detail}"),
    }
}
