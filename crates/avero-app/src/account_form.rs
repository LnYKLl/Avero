//! The add-account flow, without any drawing: what the user typed, what
//! was detected, and the checks done before an account is saved.

use crate::auth;
use avero_autoconfig::{AuthMethod, ProviderConfig};
use avero_core::{AuthKind, NewAccount, Security, ServerConfig};
use avero_oauth::Provider;

/// Where the flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Typing the address.
    Email,
    /// Looking for the servers ("Recherche des serveurs…").
    Detecting,
    /// Choosing how to sign in, typing the password.
    Credentials,
    /// Testing the IMAP login ("Vérification de la connexion…").
    Checking,
    /// Waiting for the browser ("Autorisez Avero dans votre navigateur…").
    Authorizing(Provider),
}

/// Editable fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Email,
    Password,
    Username,
    ImapHost,
    ImapPort,
    SmtpHost,
    SmtpPort,
}

/// The state of the add-account card. `Debug` hides the password.
#[derive(Clone)]
pub struct AccountForm {
    pub stage: Stage,
    pub email: String,
    /// Detected settings; `None` in manual mode.
    pub provider: Option<ProviderConfig>,
    pub password: String,
    pub username: String,
    pub imap_host: String,
    pub imap_port: String,
    pub smtp_host: String,
    pub smtp_port: String,
    pub smtp_security: Security,
    /// "Paramètres avancés" is open.
    pub advanced: bool,
    /// The user chose the password path although OAuth is offered.
    pub use_password: bool,
    /// Whether this build has an OAuth client for the detected provider.
    pub oauth_configured: bool,
    /// A note shown at the top (e.g. why a sign-in button cannot work).
    pub note: Option<String>,
    pub error: Option<String>,
    /// Bumped on every new background job, to ignore stale answers.
    pub generation: u64,
}

impl std::fmt::Debug for AccountForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountForm")
            .field("stage", &self.stage)
            .field("email", &self.email)
            .field("password", &"***")
            .field("username", &self.username)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("error", &self.error)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Default for AccountForm {
    fn default() -> Self {
        AccountForm::new()
    }
}

impl AccountForm {
    pub fn new() -> AccountForm {
        AccountForm {
            stage: Stage::Email,
            email: String::new(),
            provider: None,
            password: String::new(),
            username: String::new(),
            imap_host: String::new(),
            imap_port: "993".to_string(),
            smtp_host: String::new(),
            smtp_port: "465".to_string(),
            smtp_security: Security::Tls,
            advanced: false,
            use_password: false,
            oauth_configured: false,
            note: None,
            error: None,
            generation: 0,
        }
    }

    /// Stores a typed value and clears the error.
    pub fn set(&mut self, field: Field, value: String) {
        self.error = None;
        match field {
            Field::Email => self.email = value,
            Field::Password => self.password = value,
            Field::Username => self.username = value,
            Field::ImapHost => self.imap_host = value,
            Field::ImapPort => self.imap_port = value,
            Field::SmtpHost => self.smtp_host = value,
            Field::SmtpPort => {
                if let Some(security) = security_for_port(&value) {
                    self.smtp_security = security;
                }
                self.smtp_port = value;
            }
        }
    }

    /// The trimmed address, if it looks valid.
    pub fn valid_email(&self) -> Option<String> {
        avero_autoconfig::domain_of(&self.email).map(|_| self.email.trim().to_string())
    }

    /// Starts a new background job and returns its generation.
    pub fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Stops waiting (detection, login test or browser) and goes back to
    /// the step the user came from. Late answers are then ignored.
    pub fn cancel_wait(&mut self) {
        self.next_generation();
        self.stage = match self.stage {
            Stage::Detecting => Stage::Email,
            Stage::Checking => Stage::Credentials,
            Stage::Authorizing(_) if self.provider.is_some() => Stage::Credentials,
            Stage::Authorizing(_) => Stage::Email,
            other => other,
        };
    }

    /// Applies the detection result and moves to the credentials step.
    /// `None` switches to manual mode with the advanced settings open.
    pub fn apply_detection(&mut self, config: Option<ProviderConfig>, oauth_configured: bool) {
        let email = self.email.trim().to_string();
        self.stage = Stage::Credentials;
        self.error = None;
        self.oauth_configured = oauth_configured;
        match &config {
            Some(c) => {
                self.username = c.imap.username_for(&email);
                self.imap_host = c.imap.host.clone();
                self.imap_port = c.imap.port.to_string();
                self.smtp_host = c.smtp.host.clone();
                self.smtp_port = c.smtp.port.to_string();
                self.smtp_security = convert(c.smtp.security);
                self.advanced = false;
            }
            None => {
                let domain = email.split('@').nth(1).unwrap_or_default().to_string();
                self.username = email.clone();
                self.imap_host = format!("imap.{domain}");
                self.imap_port = "993".into();
                self.smtp_host = format!("smtp.{domain}");
                self.smtp_port = "465".into();
                self.smtp_security = Security::Tls;
                self.advanced = true;
            }
        }
        self.provider = config;
    }

    /// The OAuth provider offered for this address, if any.
    pub fn oauth_provider(&self) -> Option<Provider> {
        self.provider.as_ref()?.auth.iter().find_map(|m| match m {
            AuthMethod::OAuth(p) => Some(auth::from_autoconfig(*p)),
            AuthMethod::Password => None,
        })
    }

    /// Whether a password (or app password) can be used.
    pub fn allows_password(&self) -> bool {
        match &self.provider {
            Some(c) => c.auth.contains(&AuthMethod::Password),
            None => true,
        }
    }

    /// Show the OAuth button as the main action.
    pub fn offers_oauth(&self) -> bool {
        self.oauth_provider().is_some() && self.oauth_configured && !self.use_password
    }

    /// Show the password field.
    pub fn shows_password(&self) -> bool {
        self.allows_password() && !self.offers_oauth()
    }

    /// Detected provider name, or `None` in manual mode.
    pub fn provider_name(&self) -> Option<&str> {
        self.provider.as_ref().map(|c| c.display_name.as_str())
    }

    /// Whether this provider uses app passwords (spaces are removed).
    pub fn app_password(&self) -> bool {
        match &self.provider {
            Some(c) => auth::uses_app_password(
                &c.display_name,
                c.password_help.as_ref().map(|h| h.text.as_str()),
            ),
            None => false,
        }
    }

    /// The password as it will be sent.
    pub fn password_to_send(&self) -> String {
        auth::clean_password(&self.password, self.app_password())
    }

    /// Checks the password path; on success gives the account to create
    /// and its password (which goes to the keyring, never the database).
    pub fn validate(&self) -> Result<(NewAccount, String), String> {
        let email = self
            .valid_email()
            .ok_or_else(|| "Adresse e-mail invalide".to_string())?;
        let password = self.password_to_send();
        if password.is_empty() {
            return Err("Saisissez le mot de passe".to_string());
        }
        let imap_host = self.imap_host.trim();
        let smtp_host = self.smtp_host.trim();
        if imap_host.is_empty() || smtp_host.is_empty() {
            return Err("Indiquez les serveurs IMAP et SMTP (Paramètres avancés)".to_string());
        }
        let imap_port = parse_port(&self.imap_port).ok_or("Port IMAP invalide (1 à 65535)")?;
        let smtp_port = parse_port(&self.smtp_port).ok_or("Port SMTP invalide (1 à 65535)")?;
        let username = match self.username.trim() {
            "" => email.clone(),
            name => name.to_string(),
        };
        let account = NewAccount {
            name: default_name(&email),
            email,
            imap: ServerConfig {
                host: imap_host.to_string(),
                port: imap_port,
                username: username.clone(),
                security: Security::Tls,
            },
            smtp: ServerConfig {
                host: smtp_host.to_string(),
                port: smtp_port,
                username,
                security: self.smtp_security,
            },
            auth: AuthKind::Password,
        };
        Ok((account, password))
    }

    /// French explanation when OAuth is the only way and is missing.
    pub fn oauth_missing_note(&self) -> Option<String> {
        let provider = self.oauth_provider()?;
        if self.oauth_configured {
            return None;
        }
        let name = auth::provider_name(provider);
        Some(if self.allows_password() {
            format!(
                "La connexion {name} n'est pas configurée dans cette version d'Avero \
                 (voir docs/oauth-setup.md). Vous pouvez utiliser un mot de passe \
                 d'application à la place."
            )
        } else {
            format!(
                "Ce compte nécessite la connexion {name}, qui n'est pas disponible dans \
                 cette version d'Avero (identifiant client manquant, voir docs/oauth-setup.md)."
            )
        })
    }
}

/// The account created by an OAuth sign-in.
pub fn oauth_account(provider: Provider, email: &str) -> NewAccount {
    let (imap, smtp) = auth::oauth_servers(provider, email);
    NewAccount {
        name: default_name(email),
        email: email.to_string(),
        imap,
        smtp,
        auth: auth::auth_kind(provider),
    }
}

/// The default account name: what comes before the `@`.
pub fn default_name(email: &str) -> String {
    let local = email.trim().split('@').next().unwrap_or_default();
    if local.is_empty() {
        email.trim().to_string()
    } else {
        local.to_string()
    }
}

/// The SMTP encryption a typed port implies: 465 is implicit TLS, 587
/// STARTTLS; any other port keeps the user's choice.
pub fn security_for_port(port: &str) -> Option<Security> {
    match port.trim() {
        "465" => Some(Security::Tls),
        "587" => Some(Security::StartTls),
        _ => None,
    }
}

fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|&p| p >= 1)
}

fn convert(security: avero_autoconfig::Security) -> Security {
    match security {
        avero_autoconfig::Security::Tls => Security::Tls,
        avero_autoconfig::Security::StartTls => Security::StartTls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gmail_form(oauth_configured: bool) -> AccountForm {
        let mut form = AccountForm::new();
        form.set(Field::Email, " jean@gmail.com ".into());
        form.apply_detection(avero_autoconfig::known("jean@gmail.com"), oauth_configured);
        form
    }

    #[test]
    fn smtp_security_follows_a_typed_port() {
        let mut form = AccountForm::new();
        form.set(Field::SmtpPort, "587".into());
        assert_eq!(form.smtp_security, Security::StartTls);
        form.set(Field::SmtpPort, " 465 ".into());
        assert_eq!(form.smtp_security, Security::Tls);
        // Another port keeps the choice made with the toggle.
        form.smtp_security = Security::StartTls;
        form.set(Field::SmtpPort, "2525".into());
        assert_eq!(form.smtp_security, Security::StartTls);
        assert_eq!(form.smtp_port, "2525");
    }

    #[test]
    fn debug_never_shows_the_password() {
        let mut form = AccountForm::new();
        form.set(Field::Password, "Sup3r-Secret!".into());
        let debug = format!("{form:?}");
        assert!(!debug.contains("Sup3r-Secret!"), "{debug}");
    }

    #[test]
    fn invalid_email_is_refused() {
        let mut form = AccountForm::new();
        form.set(Field::Email, "jean".into());
        assert_eq!(form.valid_email(), None);
        form.set(Field::Email, " jean@ex.fr ".into());
        assert_eq!(form.valid_email().as_deref(), Some("jean@ex.fr"));
    }

    #[test]
    fn gmail_with_client_offers_oauth_first() {
        let form = gmail_form(true);
        assert_eq!(form.stage, Stage::Credentials);
        assert_eq!(form.provider_name(), Some("Gmail"));
        assert_eq!(form.oauth_provider(), Some(Provider::Google));
        assert!(form.offers_oauth());
        assert!(!form.shows_password());
        assert!(form.oauth_missing_note().is_none());
    }

    #[test]
    fn gmail_without_client_falls_back_to_app_password() {
        let form = gmail_form(false);
        assert!(!form.offers_oauth());
        assert!(form.shows_password());
        let note = form.oauth_missing_note().unwrap();
        assert!(note.contains("docs/oauth-setup.md"));
        assert!(note.contains("mot de passe"));
    }

    #[test]
    fn outlook_without_client_is_unavailable() {
        let mut form = AccountForm::new();
        form.set(Field::Email, "a@hotmail.fr".into());
        form.apply_detection(avero_autoconfig::known("a@hotmail.fr"), false);
        assert!(!form.shows_password());
        assert!(!form.offers_oauth());
        assert!(
            form.oauth_missing_note()
                .unwrap()
                .contains("pas disponible")
        );
    }

    #[test]
    fn gmail_app_password_spaces_are_removed() {
        let mut form = gmail_form(false);
        form.set(Field::Password, "abcd efgh ijkl mnop".into());
        let (account, password) = form.validate().unwrap();
        assert_eq!(password, "abcdefghijklmnop");
        assert_eq!(account.email, "jean@gmail.com");
        assert_eq!(account.name, "jean");
        assert_eq!(account.imap.host, "imap.gmail.com");
        assert_eq!(account.imap.username, "jean@gmail.com");
        assert_eq!(account.auth, AuthKind::Password);
    }

    #[test]
    fn other_providers_keep_spaces() {
        let mut form = AccountForm::new();
        form.set(Field::Email, "a@orange.fr".into());
        form.apply_detection(avero_autoconfig::known("a@orange.fr"), false);
        form.set(Field::Password, "mot de passe".into());
        assert_eq!(form.validate().unwrap().1, "mot de passe");
    }

    #[test]
    fn manual_mode_guesses_servers_and_opens_advanced() {
        let mut form = AccountForm::new();
        form.set(Field::Email, "moi@exemple.org".into());
        form.apply_detection(None, false);
        assert!(form.advanced);
        assert_eq!(form.imap_host, "imap.exemple.org");
        assert_eq!(form.smtp_host, "smtp.exemple.org");
        assert_eq!(form.provider_name(), None);
        assert!(form.shows_password());
    }

    #[test]
    fn validation_reports_missing_fields() {
        let mut form = gmail_form(false);
        assert_eq!(form.validate().unwrap_err(), "Saisissez le mot de passe");
        form.set(Field::Password, "x".into());
        form.set(Field::ImapPort, "99999".into());
        assert_eq!(
            form.validate().unwrap_err(),
            "Port IMAP invalide (1 à 65535)"
        );
        form.set(Field::ImapPort, "993".into());
        form.set(Field::SmtpHost, " ".into());
        assert!(form.validate().unwrap_err().contains("SMTP"));
    }

    #[test]
    fn smtp_security_comes_from_detection() {
        let mut form = AccountForm::new();
        form.set(Field::Email, "a@icloud.com".into());
        form.apply_detection(avero_autoconfig::known("a@icloud.com"), false);
        form.set(Field::Password, "abcd-efgh".into());
        let (account, _) = form.validate().unwrap();
        assert_eq!(account.smtp.security, Security::StartTls);
        assert_eq!(account.smtp.port, 587);
    }

    #[test]
    fn oauth_account_uses_provider_servers() {
        let account = oauth_account(Provider::Microsoft, "a@corp.fr");
        assert_eq!(account.auth, AuthKind::OAuthMicrosoft);
        assert_eq!(account.imap.host, "outlook.office365.com");
        assert_eq!(account.name, "a");
    }

    #[test]
    fn cancel_goes_back_one_step_and_bumps_generation() {
        let mut form = AccountForm::new();
        form.stage = Stage::Detecting;
        form.cancel_wait();
        assert_eq!(form.stage, Stage::Email);
        assert_eq!(form.generation, 1);
        let mut form = gmail_form(true);
        form.stage = Stage::Checking;
        form.cancel_wait();
        assert_eq!(form.stage, Stage::Credentials);
        form.stage = Stage::Authorizing(Provider::Google);
        form.cancel_wait();
        assert_eq!(form.stage, Stage::Credentials);
        let mut fresh = AccountForm::new();
        fresh.stage = Stage::Authorizing(Provider::Google);
        fresh.cancel_wait();
        assert_eq!(fresh.stage, Stage::Email);
    }

    #[test]
    fn typing_clears_the_error() {
        let mut form = AccountForm::new();
        form.error = Some("x".into());
        form.set(Field::Email, "a".into());
        assert!(form.error.is_none());
    }
}
