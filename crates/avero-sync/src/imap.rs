//! The real IMAP client: TLS on port 993 (or whatever the account says).

use crate::source::{FetchedHeader, FolderStatus, MailSource, RemoteFolder, SyncError};
use async_imap::types::{Flag, NameAttribute};
use async_imap::{Client, Session};
use avero_core::model::ServerConfig;
use futures::TryStreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

/// Maximum time for TCP connect + TLS handshake + server greeting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A logged-in connection to an IMAP server.
pub struct ImapSource {
    session: Session<TlsStream<TcpStream>>,
}

impl ImapSource {
    /// Connects over TLS and logs in.
    pub async fn connect(cfg: &ServerConfig, password: &str) -> Result<ImapSource, SyncError> {
        let connecting = open_client(cfg);
        let client = match timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(result) => result?,
            Err(_) => return Err(SyncError::Connect("connection timed out".to_string())),
        };

        // The error only holds what the server answered, never our password.
        let session = client
            .login(&cfg.username, password)
            .await
            .map_err(|(e, _client)| SyncError::Auth(e.to_string()))?;
        Ok(ImapSource { session })
    }
}

/// TCP + TLS handshake + reading the server greeting.
async fn open_client(cfg: &ServerConfig) -> Result<Client<TlsStream<TcpStream>>, SyncError> {
    let connect_err = |e: &dyn std::fmt::Display| SyncError::Connect(e.to_string());

    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    // Some certificates may be unreadable; we keep the ones that are fine.
    roots.add_parsable_certificates(native.certs);

    let tls_config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| connect_err(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(tls_config));
    let server_name = ServerName::try_from(cfg.host.clone()).map_err(|e| connect_err(&e))?;

    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port))
        .await
        .map_err(|e| connect_err(&e))?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| connect_err(&e))?;

    let mut client = Client::new(tls);
    match client.read_response().await {
        Ok(Some(_greeting)) => Ok(client),
        Ok(None) => Err(SyncError::Connect("no greeting from server".to_string())),
        Err(e) => Err(connect_err(&e)),
    }
}

fn protocol_err(e: impl std::fmt::Display) -> SyncError {
    SyncError::Protocol(e.to_string())
}

/// Text form of a LIST attribute, as `detect_role` expects it (for example `\Sent`).
fn attribute_text(attribute: &NameAttribute<'_>) -> String {
    match attribute {
        NameAttribute::NoInferiors => r"\Noinferiors".to_string(),
        NameAttribute::NoSelect => r"\Noselect".to_string(),
        NameAttribute::Marked => r"\Marked".to_string(),
        NameAttribute::Unmarked => r"\Unmarked".to_string(),
        NameAttribute::All => r"\All".to_string(),
        NameAttribute::Archive => r"\Archive".to_string(),
        NameAttribute::Drafts => r"\Drafts".to_string(),
        NameAttribute::Flagged => r"\Flagged".to_string(),
        NameAttribute::Junk => r"\Junk".to_string(),
        NameAttribute::Sent => r"\Sent".to_string(),
        NameAttribute::Trash => r"\Trash".to_string(),
        NameAttribute::Extension(text) => text.to_string(),
        // The enum may grow in future versions of the library.
        _ => String::new(),
    }
}

impl MailSource for ImapSource {
    async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError> {
        let names: Vec<_> = self
            .session
            .list(Some(""), Some("*"))
            .await
            .map_err(protocol_err)?
            .try_collect()
            .await
            .map_err(protocol_err)?;

        Ok(names
            .iter()
            .map(|name| RemoteFolder {
                path: name.name().to_string(),
                attributes: name.attributes().iter().map(attribute_text).collect(),
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SyncError> {
        let mailbox = self.session.select(path).await.map_err(protocol_err)?;
        let uid_validity = mailbox
            .uid_validity
            .ok_or_else(|| SyncError::Protocol("server sent no UIDVALIDITY".to_string()))?;
        Ok(FolderStatus {
            uid_validity,
            uid_next: mailbox.uid_next,
        })
    }

    async fn fetch_headers(
        &mut self,
        from_uid: u32,
        to_uid: Option<u32>,
    ) -> Result<Vec<FetchedHeader>, SyncError> {
        let range = match to_uid {
            Some(to) => format!("{from_uid}:{to}"),
            None => format!("{from_uid}:*"),
        };
        let fetches: Vec<_> = self
            .session
            .uid_fetch(range, "(UID FLAGS BODY.PEEK[HEADER])")
            .await
            .map_err(protocol_err)?
            .try_collect()
            .await
            .map_err(protocol_err)?;

        let mut headers = Vec::new();
        for fetch in &fetches {
            // Skip answers that miss the UID or the header block.
            let (Some(uid), Some(raw)) = (fetch.uid, fetch.header()) else {
                continue;
            };
            // `N:*` always returns the last message, even below N.
            if uid < from_uid {
                continue;
            }
            headers.push(FetchedHeader {
                uid,
                seen: fetch.flags().any(|flag| flag == Flag::Seen),
                raw: raw.to_vec(),
            });
        }
        Ok(headers)
    }
}
