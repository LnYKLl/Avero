//! Real servers, fake credentials: TLS, greeting and the login dialogue
//! must work, and the server's refusal must come back as an auth error.
//! Needs the network: `cargo test -p avero-sync --test live_refusals -- --ignored`.

use avero_core::model::{Credentials, Security, ServerConfig};
use avero_sync::imap::ImapSource;
use avero_sync::smtp::{Outgoing, SendError, send};
use avero_sync::source::SyncError;

fn server(host: &str, port: u16, security: Security) -> ServerConfig {
    ServerConfig {
        host: host.to_string(),
        port,
        username: "avero.test.nobody@gmail.com".to_string(),
        security,
    }
}

fn oauth() -> Credentials {
    Credentials::OAuth2 {
        user: "avero.test.nobody@gmail.com".to_string(),
        access_token: "ya29.not-a-real-token".to_string(),
    }
}

fn message() -> Outgoing {
    Outgoing {
        from_name: None,
        from_addr: "avero.test.nobody@gmail.com".to_string(),
        to: vec!["avero.test.nobody@gmail.com".to_string()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "test".to_string(),
        body: "test".to_string(),
        in_reply_to: None,
        references: None,
        attachments: Vec::new(),
    }
}

#[tokio::test]
#[ignore]
async fn gmail_imap_refuses_wrong_password() {
    let cfg = server("imap.gmail.com", 993, Security::Tls);
    let result = ImapSource::connect(&cfg, &Credentials::Password("wrong".into())).await;
    assert!(
        matches!(result, Err(SyncError::Auth(_))),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore]
async fn gmail_imap_refuses_bad_oauth_token() {
    let cfg = server("imap.gmail.com", 993, Security::Tls);
    let result = ImapSource::connect(&cfg, &oauth()).await;
    assert!(
        matches!(result, Err(SyncError::Auth(_))),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore]
async fn outlook_imap_refuses_bad_oauth_token() {
    let cfg = server("outlook.office365.com", 993, Security::Tls);
    let result = ImapSource::connect(&cfg, &oauth()).await;
    assert!(
        matches!(result, Err(SyncError::Auth(_))),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore]
async fn gmail_smtp_tls_refuses_wrong_password() {
    let cfg = server("smtp.gmail.com", 465, Security::Tls);
    let result = send(&cfg, &Credentials::Password("wrong".into()), &message()).await;
    assert!(
        matches!(result, Err(SendError::Auth(_))),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore]
async fn gmail_smtp_starttls_refuses_bad_oauth_token() {
    let cfg = server("smtp.gmail.com", 587, Security::StartTls);
    let result = send(&cfg, &oauth(), &message()).await;
    assert!(
        matches!(result, Err(SendError::Auth(_))),
        "{:?}",
        result.err()
    );
}
