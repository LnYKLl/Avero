//! Mail server auto-detection.
//!
//! Order used by [`discover`]: built-in table, Mozilla ISPDB,
//! `autoconfig.<domain>`, then a guess on `imap.<domain>:993`.
//! Only the domain name ever leaves the machine, never the full address.

mod known;
mod parse;

use std::time::Duration;

pub use known::known;
pub use parse::parse_autoconfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Tls,
    StartTls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernameKind {
    Email,
    LocalPart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: UsernameKind,
}

impl Server {
    /// The login name to use for `email` on this server.
    pub fn username_for(&self, email: &str) -> String {
        let email = email.trim();
        match self.username {
            UsernameKind::Email => email.to_string(),
            UsernameKind::LocalPart => email.split('@').next().unwrap_or(email).to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    OAuth(OAuthProvider),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordHelp {
    /// Explanation in French.
    pub text: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub display_name: String,
    pub imap: Server,
    pub smtp: Server,
    /// Possible methods, preferred first.
    pub auth: Vec<AuthMethod>,
    pub password_help: Option<PasswordHelp>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("adresse e-mail invalide")]
    InvalidEmail,
    #[error("aucun serveur trouvé pour ce domaine")]
    NotFound,
}

/// Lowercased, trimmed domain of `email`, if the address has a plausible shape.
pub fn domain_of(email: &str) -> Option<String> {
    let email = email.trim();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || domain.contains('@') || local.chars().any(char::is_whitespace) {
        return None;
    }
    let domain = domain.to_ascii_lowercase();
    if !domain.contains('.') {
        return None;
    }
    let ok = domain.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    ok.then_some(domain)
}

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Detect the servers for `email`. Only the domain is sent over the network.
pub async fn discover(email: &str) -> Result<ProviderConfig, DiscoverError> {
    let domain = domain_of(email).ok_or(DiscoverError::InvalidEmail)?;
    if let Some(config) = known(email) {
        return Ok(config);
    }

    use_ring_crypto();
    if let Ok(client) = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        let urls = [
            format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
            format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
        ];
        for url in urls {
            if let Some(config) = fetch(&client, &url, &domain).await {
                return Ok(config);
            }
        }
    }

    if guess_reachable(&domain).await {
        return Ok(guess(&domain));
    }
    Err(DiscoverError::NotFound)
}

async fn fetch(client: &reqwest::Client, url: &str, domain: &str) -> Option<ProviderConfig> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    parse_autoconfig(&body, domain)
}

async fn guess_reachable(domain: &str) -> bool {
    let addr = format!("imap.{domain}:993");
    matches!(
        tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

fn guess(domain: &str) -> ProviderConfig {
    ProviderConfig {
        display_name: domain.to_string(),
        imap: Server {
            host: format!("imap.{domain}"),
            port: 993,
            security: Security::Tls,
            username: UsernameKind::Email,
        },
        smtp: Server {
            host: format!("smtp.{domain}"),
            port: 465,
            security: Security::Tls,
            username: UsernameKind::Email,
        },
        auth: vec![AuthMethod::Password],
        password_help: None,
    }
}

/// Makes rustls use `ring`, like the IMAP and SMTP code, so the whole
/// app has a single crypto backend (`aws-lc-rs` needs a C toolchain on
/// some platforms). Installing twice is harmless: the second call fails
/// and is ignored.
fn use_ring_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_of_normalizes() {
        assert_eq!(domain_of("  Jean@Gmail.COM "), Some("gmail.com".into()));
    }

    #[test]
    fn domain_of_rejects_invalid() {
        for bad in [
            "a", "a@", "@b", "a@b@c", "a@b", "", "a b@c.fr", "a@b..fr", "a@-b.fr",
        ] {
            assert_eq!(domain_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn username_for_kinds() {
        let mut s = guess("example.org").imap;
        assert_eq!(s.username_for("me@example.org"), "me@example.org");
        s.username = UsernameKind::LocalPart;
        assert_eq!(s.username_for(" me@example.org "), "me");
    }

    #[tokio::test]
    async fn discover_rejects_invalid() {
        assert!(matches!(
            discover("nope").await,
            Err(DiscoverError::InvalidEmail)
        ));
    }

    #[tokio::test]
    async fn discover_uses_table_first() {
        let c = discover("x@gmail.com").await.unwrap();
        assert_eq!(c.imap.host, "imap.gmail.com");
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn discover_ispdb_network() {
        let c = discover("someone@ovh.net").await.unwrap();
        assert_eq!(c.imap.port, 993);
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn discover_unknown_network() {
        let r = discover("someone@this-domain-does-not-exist-avero.invalid").await;
        assert!(matches!(r, Err(DiscoverError::NotFound)));
    }
}
