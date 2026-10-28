//! OAuth2 sign-in for Gmail and Outlook accounts.
//!
//! Desktop flow from RFC 8252: the system browser opens the provider's
//! consent page, the provider redirects to a loopback port we listen on, and
//! the authorization code is exchanged with PKCE (RFC 7636, S256).
//!
//! Nothing in this crate logs or formats tokens, codes, verifiers or client
//! secrets: `Tokens` and `ClientConfig` implement `Debug` by hand and
//! `OAuthError` only carries fixed messages or server error codes.

mod config;
mod loopback;
mod token;

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

pub use config::{ClientConfig, client_config};

/// How long we wait for the user to finish in the browser.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Timeout of one request to a token endpoint.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// OAuth provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    fn auth_endpoint(self) -> &'static str {
        match self {
            Provider::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Provider::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        }
    }

    fn token_endpoint(self) -> &'static str {
        match self {
            Provider::Google => "https://oauth2.googleapis.com/token",
            Provider::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Provider::Google => "https://mail.google.com/ openid email",
            Provider::Microsoft => {
                "https://outlook.office.com/IMAP.AccessAsUser.All \
                 https://outlook.office.com/SMTP.Send offline_access openid email"
            }
        }
    }

    /// Loopback redirect URI. Microsoft only accepts `localhost` for the
    /// "Mobile and desktop applications" platform; we still bind 127.0.0.1.
    fn redirect_uri(self, port: u16) -> String {
        match self {
            Provider::Google => format!("http://127.0.0.1:{port}"),
            Provider::Microsoft => format!("http://localhost:{port}"),
        }
    }
}

/// Tokens returned by a provider.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Expiry of `access_token`, unix seconds.
    pub expires_at: i64,
    /// Account address from the id_token, when available.
    pub email: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_at", &self.expires_at)
            .field("email", &self.email)
            .finish()
    }
}

/// Sign-in or refresh failure. Never contains tokens, codes or secrets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    #[error("connexion annulée")]
    Cancelled,
    #[error("délai de connexion dépassé")]
    Timeout,
    #[error("impossible d'ouvrir le navigateur : {0}")]
    Browser(String),
    #[error("erreur réseau : {0}")]
    Network(String),
    #[error("le serveur a refusé : {0}")]
    Server(String),
    #[error("réponse invalide : {0}")]
    InvalidResponse(String),
}

/// Opens the system browser on the provider's consent page and waits (at
/// most 5 minutes) for the redirect, then exchanges the code for tokens.
pub async fn authorize(
    provider: Provider,
    client: &ClientConfig,
    login_hint: Option<&str>,
) -> Result<Tokens, OAuthError> {
    let loopback_err = |e: std::io::Error| OAuthError::Network(format!("loopback: {e}"));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(loopback_err)?;
    let port = listener.local_addr().map_err(loopback_err)?.port();
    let redirect_uri = provider.redirect_uri(port);

    let verifier = random_urlsafe(48); // 64 characters
    let state = random_urlsafe(16);
    let url = build_auth_url(
        provider,
        &client.client_id,
        &redirect_uri,
        &state,
        &challenge_for(&verifier),
        login_hint,
    );

    webbrowser::open(&url).map_err(|e| OAuthError::Browser(e.to_string()))?;

    let code = loopback::wait_for_code(listener, &state, AUTHORIZE_TIMEOUT).await?;

    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client.client_id.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(secret) = &client.client_secret {
        form.push(("client_secret", secret));
    }
    post_token(provider, &form).await
}

/// Gets a new access token from a refresh token. `Tokens::refresh_token` is
/// `Some` only when the provider rotated it.
pub async fn refresh(
    provider: Provider,
    client: &ClientConfig,
    refresh_token: &str,
) -> Result<Tokens, OAuthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client.client_id.as_str()),
    ];
    if provider == Provider::Microsoft {
        // Microsoft expects the scopes again; Google keeps the granted ones.
        form.push(("scope", provider.scope()));
    }
    if let Some(secret) = &client.client_secret {
        form.push(("client_secret", secret));
    }
    post_token(provider, &form).await
}

async fn post_token(provider: Provider, form: &[(&str, &str)]) -> Result<Tokens, OAuthError> {
    let network = |e: reqwest::Error| OAuthError::Network(e.without_url().to_string());
    let http = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(network)?;
    let response = http
        .post(provider.token_endpoint())
        .form(form)
        .send()
        .await
        .map_err(network)?;
    let status = response.status().as_u16();
    let body = response.text().await.map_err(network)?;
    token::parse_token_response(provider, status, &body, now())
}

/// Builds the authorization URL opened in the browser.
pub(crate) fn build_auth_url(
    provider: Provider,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    login_hint: Option<&str>,
) -> String {
    let mut params = vec![
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", provider.scope()),
        ("state", state),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
    ];
    if provider == Provider::Google {
        params.push(("access_type", "offline"));
        params.push(("prompt", "consent"));
    }
    if let Some(hint) = login_hint.filter(|h| !h.is_empty()) {
        params.push(("login_hint", hint));
    }
    url::Url::parse_with_params(provider.auth_endpoint(), &params)
        .expect("static endpoint is a valid URL")
        .into()
}

/// PKCE S256 challenge: base64url(sha256(verifier)) without padding.
pub(crate) fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `len` random bytes from the OS, base64url-encoded without padding.
fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("OS random generator unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(url: &str) -> Vec<(String, String)> {
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn rfc7636_appendix_b_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape() {
        let v = random_urlsafe(48);
        assert_eq!(v.len(), 64);
        assert!(
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(v, random_urlsafe(48));
    }

    #[test]
    fn google_auth_url() {
        let url = build_auth_url(
            Provider::Google,
            "cid.apps.googleusercontent.com",
            &Provider::Google.redirect_uri(5555),
            "st",
            "ch",
            Some("ana@gmail.com"),
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        let q = query(&url);
        assert_eq!(get(&q, "client_id"), Some("cid.apps.googleusercontent.com"));
        assert_eq!(get(&q, "redirect_uri"), Some("http://127.0.0.1:5555"));
        assert_eq!(get(&q, "response_type"), Some("code"));
        assert_eq!(
            get(&q, "scope"),
            Some("https://mail.google.com/ openid email")
        );
        assert_eq!(get(&q, "state"), Some("st"));
        assert_eq!(get(&q, "code_challenge"), Some("ch"));
        assert_eq!(get(&q, "code_challenge_method"), Some("S256"));
        assert_eq!(get(&q, "access_type"), Some("offline"));
        assert_eq!(get(&q, "prompt"), Some("consent"));
        assert_eq!(get(&q, "login_hint"), Some("ana@gmail.com"));
    }

    #[test]
    fn microsoft_auth_url() {
        let url = build_auth_url(
            Provider::Microsoft,
            "mid",
            &Provider::Microsoft.redirect_uri(4242),
            "st",
            "ch",
            None,
        );
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
        let q = query(&url);
        assert_eq!(get(&q, "redirect_uri"), Some("http://localhost:4242"));
        assert_eq!(
            get(&q, "scope"),
            Some(
                "https://outlook.office.com/IMAP.AccessAsUser.All \
                 https://outlook.office.com/SMTP.Send offline_access openid email"
            )
        );
        assert_eq!(get(&q, "code_challenge_method"), Some("S256"));
        assert_eq!(get(&q, "access_type"), None);
        assert_eq!(get(&q, "prompt"), None);
        assert_eq!(get(&q, "login_hint"), None);
    }

    #[test]
    fn tokens_debug_is_masked() {
        let tokens = Tokens {
            access_token: "ya29.secret-access".into(),
            refresh_token: Some("1//secret-refresh".into()),
            expires_at: 42,
            email: Some("ana@gmail.com".into()),
        };
        let dbg = format!("{tokens:?}");
        assert!(!dbg.contains("secret"));
        assert!(dbg.contains("***"));
        assert!(dbg.contains("ana@gmail.com"));
    }
}
