//! Account authentication helpers: what goes in the `SecretStore` for
//! OAuth accounts, access-token freshness, app-password clean-up and the
//! servers used by OAuth providers.

use avero_core::{AuthKind, Security, ServerConfig};
use avero_oauth::Provider;
use serde::{Deserialize, Serialize};
use std::fmt;

/// An access token is refreshed when it expires within this many seconds.
pub const REFRESH_MARGIN: i64 = 60;

/// The secret kept in the keyring for an OAuth account:
/// `{"refresh_token":"…"}`. Never written anywhere else.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthSecret {
    pub refresh_token: String,
}

impl fmt::Debug for OAuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthSecret(***)")
    }
}

impl OAuthSecret {
    pub fn to_json(&self) -> String {
        // Serialising a struct holding one string cannot fail.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Reads the keyring value; `None` if it is not our JSON.
    pub fn from_json(text: &str) -> Option<OAuthSecret> {
        serde_json::from_str::<OAuthSecret>(text)
            .ok()
            .filter(|s| !s.refresh_token.is_empty())
    }
}

/// An access token kept in memory only.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("token", &"***")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AccessToken {
    /// Usable now: does not expire within [`REFRESH_MARGIN`] seconds.
    pub fn is_fresh(&self, now: i64) -> bool {
        self.expires_at - now > REFRESH_MARGIN
    }
}

/// What a successful browser sign-in gives the app. `Debug` hides tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthGrant {
    pub provider: Provider,
    pub email: Option<String>,
    pub refresh_token: Option<String>,
    pub access: AccessToken,
}

impl fmt::Debug for OAuthGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthGrant")
            .field("provider", &self.provider)
            .field("email", &self.email)
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("access", &self.access)
            .finish()
    }
}

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The OAuth provider of an account, if it uses OAuth.
pub fn provider_of(auth: AuthKind) -> Option<Provider> {
    match auth {
        AuthKind::Password => None,
        AuthKind::OAuthGoogle => Some(Provider::Google),
        AuthKind::OAuthMicrosoft => Some(Provider::Microsoft),
    }
}

/// The account kind for a provider.
pub fn auth_kind(provider: Provider) -> AuthKind {
    match provider {
        Provider::Google => AuthKind::OAuthGoogle,
        Provider::Microsoft => AuthKind::OAuthMicrosoft,
    }
}

/// The provider's name as shown in buttons.
pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
    }
}

/// Converts the autoconfig provider enum to the OAuth one.
pub fn from_autoconfig(provider: avero_autoconfig::OAuthProvider) -> Provider {
    match provider {
        avero_autoconfig::OAuthProvider::Google => Provider::Google,
        avero_autoconfig::OAuthProvider::Microsoft => Provider::Microsoft,
    }
}

/// IMAP and SMTP servers of an OAuth provider, for any address it hosts
/// (including Google Workspace and Microsoft 365 custom domains).
pub fn oauth_servers(provider: Provider, username: &str) -> (ServerConfig, ServerConfig) {
    let server = |host: &str, port, security| ServerConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        security,
    };
    match provider {
        Provider::Google => (
            server("imap.gmail.com", 993, Security::Tls),
            server("smtp.gmail.com", 465, Security::Tls),
        ),
        Provider::Microsoft => (
            server("outlook.office365.com", 993, Security::Tls),
            server("smtp.office365.com", 587, Security::StartTls),
        ),
    }
}

/// Providers whose passwords are generated "app passwords": Google shows
/// them as `abcd efgh ijkl mnop` and people paste the spaces.
const APP_PASSWORD_PROVIDERS: [&str; 5] = ["Gmail", "Yahoo", "iCloud", "Fastmail", "AOL"];

/// Whether typed passwords for this provider should lose their spaces.
pub fn uses_app_password(display_name: &str, help_text: Option<&str>) -> bool {
    APP_PASSWORD_PROVIDERS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(display_name))
        || help_text.is_some_and(|text| {
            let text = text.to_lowercase();
            text.contains("mot de passe d'application") || text.contains("spécifique à l'app")
        })
}

/// The password to send: all whitespace removed for app passwords,
/// untouched otherwise (a real password may contain spaces).
pub fn clean_password(typed: &str, app_password: bool) -> String {
    if app_password {
        typed.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        typed.to_string()
    }
}

/// Hosts that save sent mail by themselves: appending a copy to "Sent"
/// would make it appear twice.
pub fn server_saves_sent_mail(smtp_host: &str, imap_host: &str) -> bool {
    [smtp_host, imap_host].iter().any(|host| {
        let host = host.to_ascii_lowercase();
        host.ends_with("gmail.com")
            || host.ends_with("googlemail.com")
            || host.ends_with("office365.com")
            || host.ends_with("outlook.com")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_json_roundtrip_and_debug_is_masked() {
        let secret = OAuthSecret {
            refresh_token: "1//rt-value".into(),
        };
        let json = secret.to_json();
        assert_eq!(json, r#"{"refresh_token":"1//rt-value"}"#);
        assert_eq!(OAuthSecret::from_json(&json), Some(secret.clone()));
        assert!(!format!("{secret:?}").contains("rt-value"));
    }

    #[test]
    fn plain_password_is_not_an_oauth_secret() {
        assert_eq!(OAuthSecret::from_json("hunter2"), None);
        assert_eq!(OAuthSecret::from_json(r#"{"refresh_token":""}"#), None);
    }

    #[test]
    fn token_freshness_keeps_a_minute_of_margin() {
        let token = AccessToken {
            token: "ya29".into(),
            expires_at: 1_000,
        };
        assert!(token.is_fresh(900));
        assert!(token.is_fresh(939));
        assert!(!token.is_fresh(940));
        assert!(!token.is_fresh(1_000));
        assert!(!token.is_fresh(2_000));
        assert!(!format!("{token:?}").contains("ya29"));
    }

    #[test]
    fn grant_debug_hides_tokens() {
        let grant = OAuthGrant {
            provider: Provider::Google,
            email: Some("a@gmail.com".into()),
            refresh_token: Some("1//secret-rt".into()),
            access: AccessToken {
                token: "ya29.secret".into(),
                expires_at: 5,
            },
        };
        let debug = format!("{grant:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains("a@gmail.com"));
    }

    #[test]
    fn app_passwords_lose_every_space() {
        assert!(uses_app_password("Gmail", None));
        assert!(uses_app_password("yahoo", None));
        assert!(uses_app_password(
            "Mon FAI",
            Some("Créez un mot de passe d'application")
        ));
        assert!(!uses_app_password("Orange", None));
        assert_eq!(
            clean_password(" abcd efgh\tijkl mnop\n", true),
            "abcdefghijklmnop"
        );
        assert_eq!(clean_password("pass word", false), "pass word");
    }

    #[test]
    fn oauth_servers_follow_provider() {
        let (imap, smtp) = oauth_servers(Provider::Microsoft, "a@corp.fr");
        assert_eq!(imap.host, "outlook.office365.com");
        assert_eq!(smtp.security, Security::StartTls);
        assert_eq!(smtp.username, "a@corp.fr");
        let (imap, smtp) = oauth_servers(Provider::Google, "a@x.fr");
        assert_eq!((imap.port, smtp.port), (993, 465));
    }

    #[test]
    fn sent_copy_skipped_only_for_gmail_and_microsoft() {
        assert!(server_saves_sent_mail("smtp.gmail.com", "imap.gmail.com"));
        assert!(server_saves_sent_mail(
            "smtp.office365.com",
            "outlook.office365.com"
        ));
        assert!(server_saves_sent_mail(
            "smtp-mail.outlook.com",
            "outlook.office365.com"
        ));
        assert!(!server_saves_sent_mail("smtp.orange.fr", "imap.orange.fr"));
    }

    #[test]
    fn auth_kind_and_provider_match() {
        for p in [Provider::Google, Provider::Microsoft] {
            assert_eq!(provider_of(auth_kind(p)), Some(p));
        }
        assert_eq!(provider_of(AuthKind::Password), None);
    }
}
