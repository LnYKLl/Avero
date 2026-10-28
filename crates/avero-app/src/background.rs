//! One-off network jobs that do not belong to an account worker yet:
//! server detection, the IMAP login test before saving an account and
//! the OAuth sign-in in the browser.
//!
//! Each job runs on its own thread with a small tokio runtime, so the
//! futures returned here are `Send + 'static` whatever the network code
//! looks like, and the UI thread never waits.

use crate::auth::{AccessToken, OAuthGrant};
use avero_core::{Credentials, ServerConfig};
use avero_oauth::{ClientConfig, OAuthError, Provider};
use avero_sync::imap::ImapSource;
use avero_sync::source::SyncError;
use std::future::Future;
use tokio::sync::oneshot;

/// Message used when a background thread dies without answering.
pub const CRASHED: &str = "La tâche s'est arrêtée de façon inattendue";

/// Runs the future built by `make` on a new thread and returns its output.
pub fn run<T, F>(make: impl FnOnce() -> F + Send + 'static) -> impl Future<Output = Option<T>>
where
    T: Send + 'static,
    F: Future<Output = T> + 'static,
{
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        let output = runtime.block_on(make());
        let _ = sender.send(output);
    });
    async move { receiver.await.ok() }
}

/// Why a login test failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// The server answered and refused the credentials.
    Refused,
    /// The server could not be reached (or TLS failed).
    Unreachable(String),
    /// Anything else.
    Other(String),
}

/// Logs in to the IMAP server once to check the settings, then hangs up.
pub async fn check_login(imap: ServerConfig, creds: Credentials) -> Result<(), LoginError> {
    let result = run(move || async move {
        match ImapSource::connect(&imap, &creds).await {
            Ok(_) => Ok(()),
            Err(SyncError::Auth(_)) => Err(LoginError::Refused),
            Err(SyncError::Connect(detail)) => Err(LoginError::Unreachable(detail)),
            Err(other) => Err(LoginError::Other(other.to_string())),
        }
    })
    .await;
    result.unwrap_or_else(|| Err(LoginError::Other(CRASHED.to_string())))
}

/// Finds the servers of an address (table, ISPDB, autoconfig, guess).
pub async fn discover(email: String) -> Option<avero_autoconfig::ProviderConfig> {
    run(move || async move { avero_autoconfig::discover(&email).await.ok() })
        .await
        .flatten()
}

/// Opens the browser for `provider`, waits for the user (at most five
/// minutes, or until `cancel` fires), then checks the IMAP login with the
/// new token. `login_hint` pre-fills the address on the provider's page.
pub async fn sign_in(
    provider: Provider,
    client: ClientConfig,
    login_hint: Option<String>,
    cancel: oneshot::Receiver<()>,
) -> Result<OAuthGrant, String> {
    let result = run(move || async move {
        let tokens = tokio::select! {
            tokens = avero_oauth::authorize(provider, &client, login_hint.as_deref()) => tokens,
            _ = cancel => return Err(String::new()),
        };
        let tokens = tokens.map_err(|e| oauth_message(&e))?;
        let email = tokens.email.clone().or(login_hint);
        let Some(user) = email.clone() else {
            return Err("Le fournisseur n'a pas indiqué l'adresse du compte.".to_string());
        };
        let (imap, _) = crate::auth::oauth_servers(provider, &user);
        let creds = Credentials::OAuth2 {
            user,
            access_token: tokens.access_token.clone(),
        };
        match ImapSource::connect(&imap, &creds).await {
            Ok(_) => {}
            Err(SyncError::Auth(_)) => {
                return Err(
                    "Le serveur de messagerie a refusé l'accès. Vérifiez que l'accès IMAP est activé sur ce compte."
                        .to_string(),
                );
            }
            Err(other) => return Err(format!("Connexion au serveur impossible : {other}")),
        }
        Ok(OAuthGrant {
            provider,
            email,
            refresh_token: tokens.refresh_token,
            access: AccessToken {
                token: tokens.access_token,
                expires_at: tokens.expires_at,
            },
        })
    })
    .await;
    result.unwrap_or_else(|| Err(CRASHED.to_string()))
}

/// French text for a sign-in failure (never contains a token).
pub fn oauth_message(error: &OAuthError) -> String {
    match error {
        OAuthError::Cancelled => "Connexion annulée dans le navigateur.".to_string(),
        OAuthError::Timeout => {
            "Délai dépassé : la connexion n'a pas été terminée à temps.".to_string()
        }
        OAuthError::Browser(detail) => format!("Impossible d'ouvrir le navigateur : {detail}"),
        OAuthError::Network(detail) => format!("Erreur réseau : {detail}"),
        OAuthError::Server(code) => format!("Le fournisseur a refusé la connexion ({code})."),
        OAuthError::InvalidResponse(detail) => {
            format!("Réponse inattendue du fournisseur : {detail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::Security;

    #[tokio::test]
    async fn run_returns_the_output() {
        assert_eq!(run(|| async { 21 * 2 }).await, Some(42));
    }

    #[tokio::test]
    async fn run_survives_a_panicking_job() {
        let out: Option<u8> = run(|| async { panic!("boom") }).await;
        assert_eq!(out, None);
    }

    #[tokio::test]
    async fn unreachable_server_is_reported_as_such() {
        let imap = ServerConfig {
            host: "127.0.0.1".into(),
            port: 1,
            username: "a@x".into(),
            security: Security::Tls,
        };
        let result = check_login(imap, Credentials::Password("pw".into())).await;
        assert!(
            matches!(result, Err(LoginError::Unreachable(_))),
            "{result:?}"
        );
    }

    #[test]
    fn oauth_messages_are_french_and_carry_codes() {
        let text = oauth_message(&OAuthError::Server("invalid_grant".into()));
        assert!(text.contains("invalid_grant"));
        assert!(oauth_message(&OAuthError::Timeout).starts_with("Délai"));
    }
}
