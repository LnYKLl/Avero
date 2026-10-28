//! The real IMAP client: TLS on port 993 (or whatever the account says).

use crate::source::{FetchedHeader, FolderStatus, MailSource, RemoteFolder, SyncError};
use async_imap::error::Error as ImapError;
use async_imap::imap_proto::{AttributeValue, MessageSection, Response, SectionPath, Status};
use async_imap::types::NameAttribute;
use async_imap::{Client, Session};
use avero_core::model::ServerConfig;
use futures::TryStreamExt;
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

/// Maximum time for TCP connect + TLS handshake + server greeting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum time for LOGIN, LIST and SELECT.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time for one UID FETCH (one batch of up to 500 headers).
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

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
        let session = login(client, &cfg.username, password).await?;
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

/// Runs `work`, but gives up with a connection error after `limit`.
async fn with_timeout<T>(
    limit: Duration,
    command: &str,
    work: impl Future<Output = Result<T, SyncError>>,
) -> Result<T, SyncError> {
    match timeout(limit, work).await {
        Ok(result) => result,
        Err(_) => Err(SyncError::Connect(format!(
            "délai dépassé ({command}, {} s)",
            limit.as_secs()
        ))),
    }
}

/// Sends LOGIN. Works on any stream, so tests can use an in-memory one.
async fn login<T>(
    client: Client<T>,
    username: &str,
    password: &str,
) -> Result<Session<T>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let logging_in = async {
        // The error only holds what the server answered, never our password.
        client
            .login(username, password)
            .await
            .map_err(|(e, _client)| login_error(e))
    };
    with_timeout(COMMAND_TIMEOUT, "LOGIN", logging_in).await
}

/// Only a NO or BAD answer from the server means "wrong credentials";
/// anything else (network, unreadable answer, ...) is a connection problem.
fn login_error(error: ImapError) -> SyncError {
    match error {
        ImapError::No(_) | ImapError::Bad(_) => SyncError::Auth(error.to_string()),
        other => SyncError::Connect(other.to_string()),
    }
}

/// A lost or broken connection is a connection error; anything else
/// (the server refused the command, an answer we cannot read) is a
/// protocol error, which only skips the current folder.
fn command_error(error: ImapError) -> SyncError {
    match error {
        ImapError::Io(_) | ImapError::ConnectionLost => SyncError::Connect(error.to_string()),
        other => SyncError::Protocol(other.to_string()),
    }
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

/// Sends `UID FETCH` and reads every answer up to the one tagged for it.
///
/// We do not use async-imap's `uid_fetch`: its stream simply ends when the
/// server answers NO or BAD, so a refused fetch would look like an empty
/// folder. Here a refusal is a `SyncError::Protocol`.
async fn fetch_headers_from<T>(
    session: &mut Session<T>,
    from_uid: u32,
    to_uid: Option<u32>,
) -> Result<Vec<FetchedHeader>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let range = match to_uid {
        Some(to) => format!("{from_uid}:{to}"),
        None => format!("{from_uid}:*"),
    };
    let command = format!("UID FETCH {range} (UID FLAGS BODY.PEEK[HEADER])");
    let tag = session.run_command(&command).await.map_err(command_error)?;

    let mut headers = Vec::new();
    loop {
        let response = match session.read_response().await {
            Ok(Some(response)) => response,
            Ok(None) => return Err(SyncError::Connect("connection lost".to_string())),
            Err(e) => return Err(SyncError::Connect(e.to_string())),
        };
        match response.parsed() {
            // The end of our command: did the server accept it?
            Response::Done {
                tag: done_tag,
                status,
                outcome,
            } if *done_tag == tag => {
                if *status == Status::Ok {
                    return Ok(headers);
                }
                let reason = outcome.information.as_deref().unwrap_or("");
                return Err(SyncError::Protocol(format!(
                    "UID FETCH refused: {status:?} {reason}"
                )));
            }
            Response::Fetch(_, attributes) => {
                if let Some(header) = header_from_fetch(attributes, from_uid) {
                    headers.push(header);
                }
            }
            // Other answers (EXISTS, EXPUNGE, ...) are not needed here.
            _ => {}
        }
    }
}

/// Builds a `FetchedHeader` from the attributes of one FETCH answer.
/// Returns `None` if the UID or the header block is missing.
fn header_from_fetch(attributes: &[AttributeValue<'_>], from_uid: u32) -> Option<FetchedHeader> {
    let mut uid = None;
    let mut seen = false;
    let mut raw = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(value) => uid = Some(*value),
            AttributeValue::Flags(flags) => {
                seen = flags.iter().any(|flag| flag.eq_ignore_ascii_case(r"\Seen"));
            }
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data: Some(data),
                ..
            }
            | AttributeValue::Rfc822Header(Some(data)) => raw = Some(data.to_vec()),
            _ => {}
        }
    }

    let (Some(uid), Some(raw)) = (uid, raw) else {
        return None;
    };
    // `N:*` always returns the last message, even below N.
    if uid < from_uid {
        return None;
    }
    Some(FetchedHeader { uid, seen, raw })
}

impl MailSource for ImapSource {
    async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError> {
        let listing = async {
            self.session
                .list(Some(""), Some("*"))
                .await
                .map_err(command_error)?
                .try_collect::<Vec<_>>()
                .await
                .map_err(command_error)
        };
        let names = with_timeout(COMMAND_TIMEOUT, "LIST", listing).await?;

        Ok(names
            .iter()
            .map(|name| RemoteFolder {
                path: name.name().to_string(),
                attributes: name.attributes().iter().map(attribute_text).collect(),
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SyncError> {
        let selecting = async { self.session.select(path).await.map_err(command_error) };
        let mailbox = with_timeout(COMMAND_TIMEOUT, "SELECT", selecting).await?;
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
        let fetching = fetch_headers_from(&mut self.session, from_uid, to_uid);
        with_timeout(FETCH_TIMEOUT, "UID FETCH", fetching).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    /// A tiny scripted IMAP server. For each command line the client sends,
    /// it writes the next reply (`{tag}` becomes the command's tag).
    /// Then it keeps the connection open until the client goes away.
    async fn fake_server(stream: DuplexStream, replies: Vec<String>) {
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();
        write
            .write_all(b"* OK fake server ready\r\n")
            .await
            .unwrap();
        for reply in replies {
            let Ok(Some(line)) = lines.next_line().await else {
                return;
            };
            let tag = line.split(' ').next().unwrap_or("");
            write
                .write_all(reply.replace("{tag}", tag).as_bytes())
                .await
                .unwrap();
        }
        let _ = lines.next_line().await;
    }

    /// A client connected to a fake server, greeting already read.
    async fn client_for(replies: Vec<String>) -> Client<DuplexStream> {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(fake_server(server_side, replies));
        let mut client = Client::new(client_side);
        client.read_response().await.unwrap().unwrap();
        client
    }

    /// One FETCH answer carrying `header` as a literal.
    fn fetch_line(seq: u32, uid: u32, flags: &str, header: &str) -> String {
        format!(
            "* {seq} FETCH (UID {uid} FLAGS ({flags}) BODY[HEADER] {{{}}}\r\n{header})\r\n",
            header.len()
        )
    }

    const LOGIN_OK: &str = "{tag} OK logged in\r\n";

    #[tokio::test]
    async fn login_refused_is_auth_error() {
        let client = client_for(vec!["{tag} NO [AUTHENTICATIONFAILED] nope\r\n".into()]).await;
        let result = login(client, "user", "pw").await;
        assert!(matches!(result, Err(SyncError::Auth(_))));
    }

    #[tokio::test]
    async fn login_connection_lost_is_connect_error() {
        // The server reads LOGIN, then hangs up without answering.
        let client = client_for(vec![]).await;
        let result = login(client, "user", "pw").await;
        assert!(matches!(result, Err(SyncError::Connect(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn login_without_answer_times_out() {
        // The server reads LOGIN and never answers (time is simulated).
        let client = client_for(vec![String::new()]).await;
        let result = login(client, "user", "pw").await;
        match result {
            Err(SyncError::Connect(text)) => assert!(text.contains("délai dépassé"), "{text}"),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("login should time out"),
        }
    }

    #[tokio::test]
    async fn fetch_reads_headers_flags_and_skips_lower_uids() {
        let header = "Subject: hello\r\n\r\n";
        let fetch_reply = format!(
            "{}* 9 EXISTS\r\n{}{}{{tag}} OK done\r\n",
            fetch_line(1, 3, "", header),
            fetch_line(2, 5, r"\Seen", header),
            fetch_line(3, 6, "", header),
        );
        let client = client_for(vec![LOGIN_OK.into(), fetch_reply]).await;
        let mut session = login(client, "user", "pw").await.unwrap();

        let headers = fetch_headers_from(&mut session, 5, None).await.unwrap();

        let uids: Vec<u32> = headers.iter().map(|h| h.uid).collect();
        assert_eq!(uids, vec![5, 6]);
        assert!(headers[0].seen);
        assert!(!headers[1].seen);
        assert_eq!(headers[0].raw, header.as_bytes());
    }

    #[tokio::test]
    async fn tagged_no_on_fetch_is_protocol_error() {
        let fetch_reply = format!(
            "{}{{tag}} NO server busy\r\n",
            fetch_line(1, 1, "", "Subject: a\r\n\r\n")
        );
        let client = client_for(vec![LOGIN_OK.into(), fetch_reply]).await;
        let mut session = login(client, "user", "pw").await.unwrap();

        let result = fetch_headers_from(&mut session, 1, Some(500)).await;

        assert!(matches!(result, Err(SyncError::Protocol(_))), "{result:?}");
    }

    #[test]
    fn only_no_and_bad_are_auth_errors() {
        assert!(matches!(
            login_error(ImapError::No("x".into())),
            SyncError::Auth(_)
        ));
        assert!(matches!(
            login_error(ImapError::Bad("x".into())),
            SyncError::Auth(_)
        ));
        assert!(matches!(
            login_error(ImapError::ConnectionLost),
            SyncError::Connect(_)
        ));
        assert!(matches!(
            command_error(ImapError::No("x".into())),
            SyncError::Protocol(_)
        ));
        assert!(matches!(
            command_error(ImapError::ConnectionLost),
            SyncError::Connect(_)
        ));
    }
}
