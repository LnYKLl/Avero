//! The real IMAP client: implicit TLS on port 993 (or whatever the account
//! says), logged in with a password (LOGIN) or an OAuth2 token
//! (AUTHENTICATE XOAUTH2).

use crate::source::{FetchedHeader, FolderStatus, MailSource, RemoteFolder, SyncError};
use async_imap::error::Error as ImapError;
use async_imap::imap_proto::{AttributeValue, MessageSection, Response, SectionPath, Status};
use async_imap::types::NameAttribute;
use async_imap::{Authenticator, Client, Session};
use avero_core::model::{Credentials, Security, ServerConfig};
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
/// Maximum time for LOGIN, LIST, SELECT, STORE, MOVE...
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time for one UID FETCH of headers or flags (one batch).
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Maximum time to download or upload one full message (attachments
/// included).
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(180);

/// Optional server features we use, read once after login.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ServerCaps {
    /// `UID MOVE` (RFC 6851).
    move_command: bool,
    /// `UID EXPUNGE` (RFC 4315).
    uidplus: bool,
}

/// A logged-in connection to an IMAP server.
///
/// Folder-scoped operations (`fetch_headers`, `fetch_flags`, `fetch_body`,
/// `set_seen`, `move_to`) act on the folder chosen by the last `select`.
pub struct ImapSource {
    session: Session<TlsStream<TcpStream>>,
    caps: ServerCaps,
}

impl ImapSource {
    /// Connects over implicit TLS, logs in and reads the server's
    /// capabilities. IMAP over STARTTLS is refused.
    pub async fn connect(cfg: &ServerConfig, creds: &Credentials) -> Result<ImapSource, SyncError> {
        if cfg.security == Security::StartTls {
            return Err(SyncError::Connect(
                "IMAP exige le TLS implicite (STARTTLS n'est pas pris en charge)".to_string(),
            ));
        }
        let connecting = open_client(cfg);
        let client = match timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(result) => result?,
            Err(_) => return Err(SyncError::Connect("connection timed out".to_string())),
        };
        let mut session = authenticate(client, &cfg.username, creds).await?;
        let caps = read_caps(&mut session).await?;
        Ok(ImapSource { session, caps })
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

/// Runs `work`, but gives up with a timeout error after `limit`.
async fn with_timeout<T>(
    limit: Duration,
    command: &str,
    work: impl Future<Output = Result<T, SyncError>>,
) -> Result<T, SyncError> {
    match timeout(limit, work).await {
        Ok(result) => result,
        Err(_) => Err(SyncError::Timeout(format!(
            "délai dépassé ({command}, {} s)",
            limit.as_secs()
        ))),
    }
}

/// Logs in with whatever the credentials hold: LOGIN with `username` for
/// a password, AUTHENTICATE XOAUTH2 with the token's own user for OAuth2.
async fn authenticate<T>(
    client: Client<T>,
    username: &str,
    creds: &Credentials,
) -> Result<Session<T>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    match creds {
        Credentials::Password(password) => login(client, username, password).await,
        Credentials::OAuth2 { user, access_token } => {
            authenticate_xoauth2(client, user, access_token).await
        }
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

/// The SASL XOAUTH2 exchange. The first challenge (empty) gets the token
/// string; any later one is the server's error report (JSON), which must
/// be answered with an empty line before the server sends its tagged NO.
struct XOAuth2 {
    initial: Option<String>,
}

impl XOAuth2 {
    fn new(user: &str, access_token: &str) -> XOAuth2 {
        XOAuth2 {
            initial: Some(xoauth2_string(user, access_token)),
        }
    }
}

impl Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        self.initial.take().unwrap_or_default()
    }
}

/// `user={user}^Aauth=Bearer {token}^A^A` (RFC 7628 style, used by Google
/// and Microsoft). The library base64-encodes it.
fn xoauth2_string(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// Sends AUTHENTICATE XOAUTH2; errors are classified like LOGIN's.
async fn authenticate_xoauth2<T>(
    client: Client<T>,
    user: &str,
    access_token: &str,
) -> Result<Session<T>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let authenticating = async {
        // As for LOGIN, the error only holds the server's answer.
        client
            .authenticate("XOAUTH2", XOAuth2::new(user, access_token))
            .await
            .map_err(|(e, _client)| login_error(e))
    };
    with_timeout(COMMAND_TIMEOUT, "AUTHENTICATE", authenticating).await
}

/// Asks for CAPABILITY once, to know whether MOVE and UIDPLUS exist.
async fn read_caps<T>(session: &mut Session<T>) -> Result<ServerCaps, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let asking = async { session.capabilities().await.map_err(command_error) };
    let caps = with_timeout(COMMAND_TIMEOUT, "CAPABILITY", asking).await?;
    Ok(ServerCaps {
        move_command: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
    })
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

/// `1,5,9` for a UID STORE/COPY/MOVE/EXPUNGE.
fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Sends a `UID FETCH` command and reads every answer up to the one tagged
/// for it, passing the attributes of each FETCH answer to `on_fetch`.
///
/// We do not use async-imap's `uid_fetch`: its stream simply ends when the
/// server answers NO or BAD, so a refused fetch would look like an empty
/// folder. Here a refusal is a `SyncError::Protocol`.
async fn run_uid_fetch<T>(
    session: &mut Session<T>,
    command: &str,
    mut on_fetch: impl FnMut(&[AttributeValue<'_>]),
) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let tag = session.run_command(command).await.map_err(command_error)?;
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
                    return Ok(());
                }
                let reason = outcome.information.as_deref().unwrap_or("");
                return Err(SyncError::Protocol(format!(
                    "UID FETCH refused: {status:?} {reason}"
                )));
            }
            Response::Fetch(_, attributes) => on_fetch(attributes),
            // Other answers (EXISTS, EXPUNGE, ...) are not needed here.
            _ => {}
        }
    }
}

/// Headers (with UID and flags) of the messages in `from_uid..=to_uid`.
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
    let mut headers = Vec::new();
    run_uid_fetch(session, &command, |attributes| {
        headers.extend(header_from_fetch(attributes, from_uid));
    })
    .await?;
    Ok(headers)
}

/// Headers (with UID and flags) of the messages with sequence numbers
/// `from_seq..=to_seq`. A plain FETCH with `UID` in the items returns the
/// real UIDs, so no request is spent on UID ranges that hold nothing.
async fn fetch_headers_seq<T>(
    session: &mut Session<T>,
    from_seq: u32,
    to_seq: u32,
) -> Result<Vec<FetchedHeader>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let command = format!("FETCH {from_seq}:{to_seq} (UID FLAGS BODY.PEEK[HEADER])");
    let mut headers = Vec::new();
    run_uid_fetch(session, &command, |attributes| {
        headers.extend(header_from_fetch(attributes, 0));
    })
    .await?;
    Ok(headers)
}

/// UIDs of the messages from `from_uid` up, in increasing order.
async fn fetch_uids_from<T>(session: &mut Session<T>, from_uid: u32) -> Result<Vec<u32>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let command = format!("UID FETCH {from_uid}:* (UID)");
    let mut uids = Vec::new();
    run_uid_fetch(session, &command, |attributes| {
        let (uid, _, _) = read_fetch(attributes);
        // `N:*` always returns the last message, even below N.
        if let Some(uid) = uid.filter(|&uid| uid >= from_uid) {
            uids.push(uid);
        }
    })
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// `(uid, seen)` of every message with a UID in `from_uid..=to_uid`.
async fn fetch_flags_range<T>(
    session: &mut Session<T>,
    from_uid: u32,
    to_uid: u32,
) -> Result<Vec<(u32, bool)>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let command = format!("UID FETCH {from_uid}:{to_uid} (UID FLAGS)");
    let mut flags = Vec::new();
    run_uid_fetch(session, &command, |attributes| {
        let (uid, seen, _) = read_fetch(attributes);
        if let Some(uid) = uid.filter(|uid| (from_uid..=to_uid).contains(uid)) {
            flags.push((uid, seen));
        }
    })
    .await?;
    Ok(flags)
}

/// The full raw message, without setting `\Seen` (BODY.PEEK).
/// `None` when the server has no message with this UID.
async fn fetch_body_of<T>(session: &mut Session<T>, uid: u32) -> Result<Option<Vec<u8>>, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let command = format!("UID FETCH {uid} (UID BODY.PEEK[])");
    let mut body = None;
    run_uid_fetch(session, &command, |attributes| {
        let (fetched_uid, _, data) = read_fetch(attributes);
        if fetched_uid == Some(uid) && data.is_some() {
            body = data;
        }
    })
    .await?;
    Ok(body)
}

/// UID, seen flag and body section (whole message or header) of one
/// FETCH answer.
fn read_fetch(attributes: &[AttributeValue<'_>]) -> (Option<u32>, bool, Option<Vec<u8>>) {
    let mut uid = None;
    let mut seen = false;
    let mut data = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(value) => uid = Some(*value),
            AttributeValue::Flags(flags) => {
                seen = flags.iter().any(|flag| flag.eq_ignore_ascii_case(r"\Seen"));
            }
            AttributeValue::BodySection {
                section: None | Some(SectionPath::Full(MessageSection::Header)),
                data: Some(bytes),
                ..
            }
            | AttributeValue::Rfc822(Some(bytes))
            | AttributeValue::Rfc822Header(Some(bytes)) => data = Some(bytes.to_vec()),
            _ => {}
        }
    }
    (uid, seen, data)
}

/// Builds a `FetchedHeader` from the attributes of one FETCH answer.
/// Returns `None` if the UID or the header block is missing.
fn header_from_fetch(attributes: &[AttributeValue<'_>], from_uid: u32) -> Option<FetchedHeader> {
    let (Some(uid), seen, Some(raw)) = read_fetch(attributes) else {
        return None;
    };
    // `N:*` always returns the last message, even below N.
    if uid < from_uid {
        return None;
    }
    Some(FetchedHeader { uid, seen, raw })
}

/// Runs a command that only needs a tagged OK (STORE, COPY, EXPUNGE...).
async fn run_checked<T>(session: &mut Session<T>, command: &str) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let name = command.split(' ').take(2).collect::<Vec<_>>().join(" ");
    let running = async {
        session
            .run_command_and_check_ok(command)
            .await
            .map_err(command_error)
    };
    with_timeout(COMMAND_TIMEOUT, &name, running).await
}

/// Adds or removes `\Seen` on the given messages of the selected folder.
async fn store_seen<T>(session: &mut Session<T>, uids: &[u32], seen: bool) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    if uids.is_empty() {
        return Ok(());
    }
    let sign = if seen { '+' } else { '-' };
    let command = format!("UID STORE {} {sign}FLAGS.SILENT (\\Seen)", uid_set(uids));
    run_checked(session, &command).await
}

/// Moves messages of the selected folder to `dest`: UID MOVE when the
/// server has it, else COPY, mark `\Deleted` and expunge (only those UIDs
/// with UIDPLUS; a plain EXPUNGE also removes other messages already
/// marked `\Deleted` in the folder).
async fn move_uids<T>(
    session: &mut Session<T>,
    caps: ServerCaps,
    uids: &[u32],
    dest: &str,
) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    let dest = quote_mailbox(dest)?;
    if caps.move_command {
        return run_checked(session, &format!("UID MOVE {set} {dest}")).await;
    }
    run_checked(session, &format!("UID COPY {set} {dest}")).await?;
    expunge_uids(session, caps, uids).await
}

/// Marks messages `\Deleted` and expunges them: only those UIDs with
/// UIDPLUS; a plain EXPUNGE also removes other messages already marked
/// `\Deleted` in the folder.
async fn expunge_uids<T>(
    session: &mut Session<T>,
    caps: ServerCaps,
    uids: &[u32],
) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    run_checked(
        session,
        &format!("UID STORE {set} +FLAGS.SILENT (\\Deleted)"),
    )
    .await?;
    if caps.uidplus {
        run_checked(session, &format!("UID EXPUNGE {set}")).await
    } else {
        run_checked(session, "EXPUNGE").await
    }
}

/// Adds a raw message to `folder`, flagged `\Seen` if asked.
async fn append_to<T>(
    session: &mut Session<T>,
    folder: &str,
    raw: &[u8],
    seen: bool,
) -> Result<(), SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let flags = if seen { Some(r"(\Seen)") } else { None };
    let appending = async {
        session
            .append(folder, flags, None, raw)
            .await
            .map_err(command_error)
    };
    with_timeout(TRANSFER_TIMEOUT, "APPEND", appending).await
}

/// A mailbox name as an IMAP quoted string. Line breaks cannot be quoted.
fn quote_mailbox(name: &str) -> Result<String, SyncError> {
    if name.contains(['\r', '\n']) {
        return Err(SyncError::Protocol("invalid folder name".to_string()));
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
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
            exists: mailbox.exists,
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

    async fn fetch_headers_by_seq(
        &mut self,
        from_seq: u32,
        to_seq: u32,
    ) -> Result<Vec<FetchedHeader>, SyncError> {
        let fetching = fetch_headers_seq(&mut self.session, from_seq, to_seq);
        with_timeout(FETCH_TIMEOUT, "FETCH", fetching).await
    }

    async fn fetch_uids(&mut self, from_uid: u32) -> Result<Vec<u32>, SyncError> {
        let fetching = fetch_uids_from(&mut self.session, from_uid);
        with_timeout(FETCH_TIMEOUT, "UID FETCH", fetching).await
    }

    async fn fetch_flags(
        &mut self,
        from_uid: u32,
        to_uid: u32,
    ) -> Result<Vec<(u32, bool)>, SyncError> {
        let fetching = fetch_flags_range(&mut self.session, from_uid, to_uid);
        with_timeout(FETCH_TIMEOUT, "UID FETCH", fetching).await
    }

    async fn fetch_body(&mut self, uid: u32) -> Result<Option<Vec<u8>>, SyncError> {
        let fetching = fetch_body_of(&mut self.session, uid);
        with_timeout(TRANSFER_TIMEOUT, "UID FETCH", fetching).await
    }

    async fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), SyncError> {
        store_seen(&mut self.session, uids, seen).await
    }

    async fn move_to(&mut self, uids: &[u32], dest: &str) -> Result<(), SyncError> {
        move_uids(&mut self.session, self.caps, uids, dest).await
    }

    async fn delete_permanently(&mut self, uids: &[u32]) -> Result<(), SyncError> {
        expunge_uids(&mut self.session, self.caps, uids).await
    }

    async fn append(&mut self, folder: &str, raw: &[u8], seen: bool) -> Result<(), SyncError> {
        append_to(&mut self.session, folder, raw, seen).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
    use tokio::task::JoinHandle;

    /// A tiny scripted IMAP server. For each line the client sends, it
    /// writes the next reply (`{tag}` becomes the tag of the last command;
    /// a line without a space, such as a SASL answer or an APPEND literal,
    /// keeps the previous tag). Then it reads one more line and hangs up,
    /// returning every line it received.
    async fn fake_server(stream: DuplexStream, replies: Vec<String>) -> Vec<String> {
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();
        let mut received = Vec::new();
        write
            .write_all(b"* OK fake server ready\r\n")
            .await
            .unwrap();
        let mut tag = String::new();
        for reply in replies {
            let Ok(Some(line)) = lines.next_line().await else {
                return received;
            };
            if let Some((first, _)) = line.split_once(' ') {
                tag = first.to_string();
            }
            received.push(line);
            write
                .write_all(reply.replace("{tag}", &tag).as_bytes())
                .await
                .unwrap();
        }
        // One more line (or the end of the connection), then hang up.
        if let Ok(Some(line)) = lines.next_line().await {
            received.push(line);
        }
        received
    }

    /// A client connected to a fake server, greeting already read; the
    /// handle gives the lines the server received once the client is gone.
    async fn client_recording(
        replies: Vec<String>,
    ) -> (Client<DuplexStream>, JoinHandle<Vec<String>>) {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(fake_server(server_side, replies));
        let mut client = Client::new(client_side);
        client.read_response().await.unwrap().unwrap();
        (client, server)
    }

    /// A client connected to a fake server, greeting already read.
    async fn client_for(replies: Vec<String>) -> Client<DuplexStream> {
        client_recording(replies).await.0
    }

    /// A logged-in session on a fake server whose next replies are
    /// `replies`, and the handle to the lines it received.
    async fn session_recording(
        replies: Vec<&str>,
    ) -> (Session<DuplexStream>, JoinHandle<Vec<String>>) {
        let mut script = vec![LOGIN_OK.to_string()];
        script.extend(replies.into_iter().map(String::from));
        let (client, server) = client_recording(script).await;
        let session = login(client, "user", "pw").await.unwrap();
        (session, server)
    }

    /// Commands the server received after LOGIN, without their tags.
    async fn commands(
        session: Session<DuplexStream>,
        server: JoinHandle<Vec<String>>,
    ) -> Vec<String> {
        drop(session);
        server
            .await
            .unwrap()
            .into_iter()
            .skip(1)
            .map(|line| match line.split_once(' ') {
                Some((_tag, rest)) => rest.to_string(),
                _ => line,
            })
            .collect()
    }

    /// One FETCH answer carrying `header` as a literal.
    fn fetch_line(seq: u32, uid: u32, flags: &str, header: &str) -> String {
        format!(
            "* {seq} FETCH (UID {uid} FLAGS ({flags}) BODY[HEADER] {{{}}}\r\n{header})\r\n",
            header.len()
        )
    }

    const LOGIN_OK: &str = "{tag} OK logged in\r\n";
    const OK: &str = "{tag} OK done\r\n";

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
            Err(SyncError::Timeout(text)) => assert!(text.contains("délai dépassé"), "{text}"),
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

    #[test]
    fn xoauth2_string_has_sasl_layout() {
        assert_eq!(
            xoauth2_string("a@x.com", "tok"),
            "user=a@x.com\x01auth=Bearer tok\x01\x01"
        );
        let mut auth = XOAuth2::new("a@x.com", "tok");
        assert_eq!(auth.process(b""), "user=a@x.com\x01auth=Bearer tok\x01\x01");
        // The error report that follows a refusal gets an empty answer.
        assert_eq!(auth.process(b"{\"status\":\"401\"}"), "");
    }

    #[tokio::test]
    async fn xoauth2_sends_the_token_and_logs_in() {
        let (client, server) =
            client_recording(vec!["+ \r\n".into(), "{tag} OK welcome\r\n".into()]).await;
        let creds = Credentials::OAuth2 {
            user: "a@x.com".into(),
            access_token: "tok".into(),
        };

        let session = authenticate(client, "ignored", &creds).await.unwrap();

        drop(session);
        let lines = server.await.unwrap();
        assert!(lines[0].ends_with("AUTHENTICATE XOAUTH2"), "{lines:?}");
        // base64("user=a@x.com\x01auth=Bearer tok\x01\x01")
        assert_eq!(lines[1], "dXNlcj1hQHguY29tAWF1dGg9QmVhcmVyIHRvawEB");
    }

    #[tokio::test]
    async fn xoauth2_refused_is_auth_error() {
        let (client, _server) = client_recording(vec![
            "+ \r\n".into(),
            "+ eyJzdGF0dXMiOiI0MDEifQ==\r\n".into(),
            "{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n".into(),
        ])
        .await;
        let creds = Credentials::OAuth2 {
            user: "a@x.com".into(),
            access_token: "expired".into(),
        };
        let result = authenticate(client, "a@x.com", &creds).await;
        match result {
            Err(SyncError::Auth(text)) => assert!(!text.contains("expired"), "{text}"),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("authentication should fail"),
        }
    }

    #[tokio::test]
    async fn password_credentials_use_login() {
        let (client, server) = client_recording(vec![LOGIN_OK.into()]).await;
        let creds = Credentials::Password("pw".into());
        let session = authenticate(client, "user", &creds).await.unwrap();
        drop(session);
        let lines = server.await.unwrap();
        assert!(lines[0].contains("LOGIN \"user\" \"pw\""), "{lines:?}");
    }

    #[tokio::test]
    async fn starttls_imap_is_refused_before_connecting() {
        let cfg = ServerConfig {
            host: "imap.example.com".into(),
            port: 143,
            username: "u".into(),
            security: Security::StartTls,
        };
        let result = ImapSource::connect(&cfg, &Credentials::Password("pw".into())).await;
        assert!(matches!(result, Err(SyncError::Connect(_))));
    }

    #[tokio::test]
    async fn capabilities_are_read_once() {
        let (mut session, _server) = session_recording(vec![
            "* CAPABILITY IMAP4rev1 UIDPLUS MOVE IDLE\r\n{tag} OK\r\n",
        ])
        .await;
        let caps = read_caps(&mut session).await.unwrap();
        assert_eq!(
            caps,
            ServerCaps {
                move_command: true,
                uidplus: true
            }
        );
    }

    #[tokio::test]
    async fn fetch_flags_reads_seen_in_the_range_only() {
        let reply = "* 1 FETCH (UID 3 FLAGS (\\Seen))\r\n\
                     * 2 FETCH (UID 7 FLAGS (\\Answered \\Seen))\r\n\
                     * 3 FETCH (UID 9 FLAGS ())\r\n\
                     * 4 FETCH (UID 12 FLAGS ())\r\n\
                     {tag} OK done\r\n";
        let (mut session, server) = session_recording(vec![reply]).await;

        let flags = fetch_flags_range(&mut session, 5, 10).await.unwrap();

        assert_eq!(flags, vec![(7, true), (9, false)]);
        let sent = commands(session, server).await;
        assert_eq!(sent, vec!["UID FETCH 5:10 (UID FLAGS)"]);
    }

    #[tokio::test]
    async fn fetch_by_sequence_returns_real_uids() {
        let header = "Subject: hello\r\n\r\n";
        let reply = format!(
            "{}{}{{tag}} OK done\r\n",
            fetch_line(1999, 40_017, "", header),
            fetch_line(2000, 90_123, r"\Seen", header),
        );
        let (mut session, server) = session_recording(vec![&reply]).await;

        let headers = fetch_headers_seq(&mut session, 1999, 2000).await.unwrap();

        let uids: Vec<u32> = headers.iter().map(|h| h.uid).collect();
        assert_eq!(uids, vec![40_017, 90_123]);
        assert!(headers[1].seen);
        let sent = commands(session, server).await;
        assert_eq!(sent, vec!["FETCH 1999:2000 (UID FLAGS BODY.PEEK[HEADER])"]);
    }

    #[tokio::test]
    async fn fetch_uids_lists_existing_uids_only() {
        let reply = "* 1 FETCH (UID 3)\r\n\
                     * 2 FETCH (UID 900)\r\n\
                     * 3 FETCH (UID 70000)\r\n\
                     {tag} OK done\r\n";
        let (mut session, server) = session_recording(vec![reply]).await;
        let uids = fetch_uids_from(&mut session, 5).await.unwrap();
        assert_eq!(uids, vec![900, 70000]);
        let sent = commands(session, server).await;
        assert_eq!(sent, vec!["UID FETCH 5:* (UID)"]);
    }

    #[tokio::test]
    async fn permanent_delete_flags_and_expunges_only_those_uids() {
        let (mut session, server) = session_recording(vec![OK, OK]).await;
        let caps = ServerCaps {
            move_command: true,
            uidplus: true,
        };
        expunge_uids(&mut session, caps, &[4, 8]).await.unwrap();
        let sent = commands(session, server).await;
        assert_eq!(
            sent,
            vec!["UID STORE 4,8 +FLAGS.SILENT (\\Deleted)", "UID EXPUNGE 4,8"]
        );
    }

    #[tokio::test]
    async fn fetch_flags_refused_is_protocol_error() {
        let (mut session, _server) = session_recording(vec!["{tag} BAD invalid set\r\n"]).await;
        let result = fetch_flags_range(&mut session, 1, 9).await;
        assert!(matches!(result, Err(SyncError::Protocol(_))), "{result:?}");
    }

    #[tokio::test]
    async fn fetch_body_peeks_whole_message() {
        let raw = "Subject: a\r\n\r\nbody\r\n";
        let reply = format!(
            "* 1 FETCH (UID 9 BODY[] {{{}}}\r\n{raw})\r\n{{tag}} OK done\r\n",
            raw.len()
        );
        let (mut session, server) = session_recording(vec![&reply, OK]).await;

        let body = fetch_body_of(&mut session, 9).await.unwrap();
        assert_eq!(body.as_deref(), Some(raw.as_bytes()));
        // No FETCH answer: the message is gone.
        assert_eq!(fetch_body_of(&mut session, 10).await.unwrap(), None);

        let sent = commands(session, server).await;
        assert_eq!(sent[0], "UID FETCH 9 (UID BODY.PEEK[])");
    }

    #[tokio::test]
    async fn set_seen_stores_silently() {
        let (mut session, server) = session_recording(vec![OK, OK]).await;
        store_seen(&mut session, &[1, 4], true).await.unwrap();
        store_seen(&mut session, &[2], false).await.unwrap();
        // Nothing to do: no command sent.
        store_seen(&mut session, &[], true).await.unwrap();
        let sent = commands(session, server).await;
        assert_eq!(
            sent,
            vec![
                "UID STORE 1,4 +FLAGS.SILENT (\\Seen)",
                "UID STORE 2 -FLAGS.SILENT (\\Seen)"
            ]
        );
    }

    #[tokio::test]
    async fn refused_store_is_protocol_error() {
        let (mut session, _server) = session_recording(vec!["{tag} NO read-only\r\n"]).await;
        let result = store_seen(&mut session, &[1], true).await;
        assert!(matches!(result, Err(SyncError::Protocol(_))), "{result:?}");
    }

    #[tokio::test]
    async fn move_uses_move_when_announced() {
        let (mut session, server) = session_recording(vec![OK]).await;
        let caps = ServerCaps {
            move_command: true,
            uidplus: true,
        };
        move_uids(&mut session, caps, &[3, 5], "[Gmail]/Corbeille")
            .await
            .unwrap();
        let sent = commands(session, server).await;
        assert_eq!(sent, vec!["UID MOVE 3,5 \"[Gmail]/Corbeille\""]);
    }

    #[tokio::test]
    async fn move_without_move_copies_deletes_and_expunges() {
        let (mut session, server) = session_recording(vec![OK, OK, OK]).await;
        let caps = ServerCaps {
            move_command: false,
            uidplus: true,
        };
        move_uids(&mut session, caps, &[3], "Trash").await.unwrap();
        let sent = commands(session, server).await;
        assert_eq!(
            sent,
            vec![
                "UID COPY 3 \"Trash\"",
                "UID STORE 3 +FLAGS.SILENT (\\Deleted)",
                "UID EXPUNGE 3"
            ]
        );
    }

    #[tokio::test]
    async fn move_without_uidplus_uses_plain_expunge() {
        let (mut session, server) = session_recording(vec![OK, OK, OK]).await;
        move_uids(&mut session, ServerCaps::default(), &[3], "Trash")
            .await
            .unwrap();
        let sent = commands(session, server).await;
        assert_eq!(sent[2], "EXPUNGE");
    }

    #[tokio::test]
    async fn failed_copy_stops_the_move() {
        let (mut session, server) =
            session_recording(vec!["{tag} NO [TRYCREATE] no such folder\r\n"]).await;
        let result = move_uids(&mut session, ServerCaps::default(), &[3], "Nope").await;
        assert!(matches!(result, Err(SyncError::Protocol(_))), "{result:?}");
        let sent = commands(session, server).await;
        assert_eq!(sent.len(), 1);
    }

    #[tokio::test]
    async fn append_sends_the_message_as_a_literal() {
        let (mut session, server) = session_recording(vec!["+ go ahead\r\n", OK]).await;
        append_to(&mut session, "Sent", b"hello", true)
            .await
            .unwrap();
        let sent = commands(session, server).await;
        assert_eq!(sent[0], "APPEND \"Sent\" (\\Seen) {5}");
        assert_eq!(sent[1], "hello");
    }

    #[test]
    fn mailbox_names_are_quoted() {
        assert_eq!(quote_mailbox("a \"b\"").unwrap(), "\"a \\\"b\\\"\"");
        assert!(quote_mailbox("a\r\nb").is_err());
        assert_eq!(uid_set(&[1, 2, 30]), "1,2,30");
    }
}
