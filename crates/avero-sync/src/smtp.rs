//! Sending mail over SMTP: implicit TLS (port 465) or required STARTTLS
//! (port 587), never in clear.

use avero_core::model::{Credentials, Security, ServerConfig};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials as SmtpCredentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

/// Maximum time for each step of the SMTP exchange.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// One recipient: a bare address and an optional display name, kept
/// apart so a name holding `@`, `:`, `(` or a comma is never re-parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub name: Option<String>,
    pub addr: String,
}

impl Recipient {
    pub fn new(name: Option<&str>, addr: &str) -> Recipient {
        Recipient {
            name: name.map(str::to_string),
            addr: addr.to_string(),
        }
    }
}

/// A message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub from_name: Option<String>,
    pub from_addr: String,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    /// Plain text.
    pub body: String,
    /// Message-ID of the message answered, with angle brackets.
    pub in_reply_to: Option<String>,
    /// Message-IDs of the thread, with angle brackets, separated by spaces.
    pub references: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
}

/// A file attached to an `Outgoing` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    pub name: String,
    /// For example `application/pdf`; anything unreadable is sent as
    /// `application/octet-stream`.
    pub mime: String,
    pub data: Vec<u8>,
}

/// Why a message could not be sent. Never holds a password or token.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("adresse invalide : {0}")]
    InvalidAddress(String),
    #[error("connexion impossible : {0}")]
    Connect(String),
    #[error("identifiants refusés : {0}")]
    Auth(String),
    #[error("envoi refusé par le serveur : {0}")]
    Rejected(String),
}

/// Sends `msg` through the account's SMTP server and returns the raw
/// message as sent (without its Bcc header), ready to be appended to the
/// Sent folder.
pub async fn send(
    cfg: &ServerConfig,
    creds: &Credentials,
    msg: &Outgoing,
) -> Result<Vec<u8>, SendError> {
    let message = build_message(msg)?;
    let raw = message.formatted();
    let transport = transport(cfg, creds)?;
    transport.send(message).await.map_err(send_error)?;
    Ok(raw)
}

/// Connects to the SMTP server and logs in, without sending anything:
/// checks the settings of a new account.
pub async fn check(cfg: &ServerConfig, creds: &Credentials) -> Result<(), SendError> {
    let transport = transport(cfg, creds)?;
    match transport.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(SendError::Connect(
            "le serveur ne répond plus après la connexion".to_string(),
        )),
        Err(error) => Err(send_error(error)),
    }
}

/// The SMTP connection settings: TLS from the first byte, or STARTTLS
/// required before any credential is sent.
fn transport(
    cfg: &ServerConfig,
    creds: &Credentials,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
    let builder = match cfg.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host),
        Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host),
    }
    .map_err(|e| SendError::Connect(e.to_string()))?;

    let (credentials, mechanisms) = match creds {
        Credentials::Password(password) => (
            SmtpCredentials::new(cfg.username.clone(), password.clone()),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        Credentials::OAuth2 { user, access_token } => (
            SmtpCredentials::new(user.clone(), access_token.clone()),
            vec![Mechanism::Xoauth2],
        ),
    };
    Ok(builder
        .port(cfg.port)
        .timeout(Some(SMTP_TIMEOUT))
        .credentials(credentials)
        .authentication(mechanisms)
        .build())
}

/// Sorts a transport error: an answer 534/535 is a refused login, any
/// other answer a refusal of the message; no answer at all (network,
/// TLS, timeout, missing STARTTLS...) is a connection problem.
fn send_error(error: lettre::transport::smtp::Error) -> SendError {
    match error.status().map(|code| code.to_string()) {
        Some(code) if code == "534" || code == "535" => SendError::Auth(error.to_string()),
        Some(_) => SendError::Rejected(error.to_string()),
        None if is_rejected_token(&error) => SendError::Auth(error.to_string()),
        None => SendError::Connect(error.to_string()),
    }
}

/// When it refuses an XOAUTH2 token, the server answers with a 334
/// challenge (a JSON error) before the final 535; lettre does not expect
/// that challenge and gives up with this client error. Checked against
/// smtp.gmail.com (tests/live_refusals.rs).
fn is_rejected_token(error: &lettre::transport::smtp::Error) -> bool {
    error.is_client() && error.to_string().contains("does not expect a challenge")
}

/// Builds the MIME message: plain text alone, or text followed by the
/// attachments in a multipart/mixed.
pub fn build_message(msg: &Outgoing) -> Result<Message, SendError> {
    let from_addr = msg
        .from_addr
        .trim()
        .parse()
        .map_err(|_| SendError::InvalidAddress(msg.from_addr.clone()))?;
    let from = Mailbox::new(msg.from_name.clone(), from_addr);

    let mut builder = Message::builder()
        .message_id(Some(new_message_id(&msg.from_addr)))
        .from(from)
        .subject(msg.subject.clone());
    for to in &msg.to {
        builder = builder.to(mailbox(to)?);
    }
    for cc in &msg.cc {
        builder = builder.cc(mailbox(cc)?);
    }
    for bcc in &msg.bcc {
        builder = builder.bcc(mailbox(bcc)?);
    }
    if let Some(id) = &msg.in_reply_to {
        builder = builder.in_reply_to(id.clone());
    }
    if let Some(ids) = &msg.references {
        builder = builder.references(ids.clone());
    }

    let text = SinglePart::plain(msg.body.clone());
    let built = if msg.attachments.is_empty() {
        builder.singlepart(text)
    } else {
        let mut parts = MultiPart::mixed().singlepart(text);
        for attachment in &msg.attachments {
            let mime = ContentType::parse(&attachment.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            parts = parts.singlepart(
                Attachment::new(attachment.name.clone()).body(attachment.data.clone(), mime),
            );
        }
        builder.multipart(parts)
    };
    built.map_err(|e| SendError::InvalidAddress(e.to_string()))
}

/// A lettre mailbox built from the name and address as they are (the
/// name is encoded by lettre, never parsed).
fn mailbox(recipient: &Recipient) -> Result<Mailbox, SendError> {
    let addr = recipient
        .addr
        .trim()
        .parse()
        .map_err(|_| SendError::InvalidAddress(recipient.addr.clone()))?;
    let name = recipient
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    Ok(Mailbox::new(name, addr))
}

/// A unique `<random@domain>` Message-ID, on the sender's domain (the
/// computer's name is never revealed).
fn new_message_id(from_addr: &str) -> String {
    let domain = from_addr
        .rsplit_once('@')
        .map_or("avero.invalid", |(_, domain)| domain.trim());
    // `RandomState` is seeded randomly for each instance.
    let random = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.finish()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!("<{:016x}{:016x}@{domain}>", random(nanos), random(!nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_body;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn outgoing() -> Outgoing {
        Outgoing {
            from_name: Some("Marie Durand".into()),
            from_addr: "marie@ex.com".into(),
            to: vec![
                Recipient::new(Some("Paul"), "paul@ex.com"),
                Recipient::new(None, "zoe@ex.com"),
            ],
            cc: vec![Recipient::new(None, "chef@ex.com")],
            bcc: vec![Recipient::new(None, "secret@ex.com")],
            subject: "Réunion de jeudi".into(),
            body: "Bonjour,\n\nÀ jeudi.".into(),
            in_reply_to: None,
            references: None,
            attachments: Vec::new(),
        }
    }

    fn formatted(msg: &Outgoing) -> Vec<u8> {
        build_message(msg).unwrap().formatted()
    }

    #[test]
    fn builds_headers_and_text() {
        let raw = formatted(&outgoing());
        let parsed = parse_body(&raw);
        let from = parsed.from.unwrap();
        assert_eq!(from.addr, "marie@ex.com");
        assert_eq!(from.name.as_deref(), Some("Marie Durand"));
        let to: Vec<&str> = parsed.to.iter().map(|a| a.addr.as_str()).collect();
        assert_eq!(to, vec!["paul@ex.com", "zoe@ex.com"]);
        assert_eq!(parsed.cc[0].addr, "chef@ex.com");
        assert_eq!(parsed.subject, "Réunion de jeudi");
        assert_eq!(parsed.text, "Bonjour,\n\nÀ jeudi.");
        assert!(parsed.date > 0);
        let id = parsed.message_id.unwrap();
        assert!(id.ends_with("@ex.com>"), "{id}");
        // Bcc recipients get the mail but are not written in it.
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("secret@ex.com"), "{text}");
        assert_eq!(parsed.references, None);
    }

    #[test]
    fn bcc_is_in_the_envelope() {
        let message = build_message(&outgoing()).unwrap();
        let recipients: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert!(recipients.contains(&"secret@ex.com".to_string()));
        assert_eq!(recipients.len(), 4);
    }

    #[test]
    fn reply_headers_are_set() {
        let mut msg = outgoing();
        msg.in_reply_to = Some("<parent@ex.com>".into());
        msg.references = Some("<root@ex.com> <parent@ex.com>".into());
        let raw = formatted(&msg);
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("In-Reply-To: <parent@ex.com>"), "{text}");
        let parsed = parse_body(&raw);
        assert_eq!(
            parsed.references.as_deref(),
            Some("<root@ex.com> <parent@ex.com>")
        );
    }

    #[test]
    fn attachments_make_a_mixed_message() {
        let mut msg = outgoing();
        msg.attachments = vec![
            OutgoingAttachment {
                name: "facture.pdf".into(),
                mime: "application/pdf".into(),
                data: b"%PDF-1.4 fake".to_vec(),
            },
            OutgoingAttachment {
                name: "bizarre.bin".into(),
                mime: "pas un type".into(),
                data: vec![0, 1, 2, 255],
            },
        ];
        let raw = formatted(&msg);
        assert!(String::from_utf8_lossy(&raw).contains("multipart/mixed"));
        let parsed = parse_body(&raw);
        assert_eq!(parsed.text, "Bonjour,\n\nÀ jeudi.");
        assert_eq!(parsed.attachments.len(), 2);
        assert_eq!(parsed.attachments[0].name, "facture.pdf");
        assert_eq!(parsed.attachments[0].mime, "application/pdf");
        assert_eq!(parsed.attachments[1].mime, "application/octet-stream");
        assert_eq!(
            crate::parse::attachment_data(&raw, 1).unwrap(),
            vec![0, 1, 2, 255]
        );
    }

    #[test]
    fn invalid_addresses_are_errors() {
        let mut msg = outgoing();
        msg.to.push(Recipient::new(None, "pas une adresse"));
        let error = build_message(&msg).unwrap_err();
        assert!(matches!(&error, SendError::InvalidAddress(a) if a == "pas une adresse"));

        let mut msg = outgoing();
        msg.from_addr = "moi".into();
        assert!(matches!(
            build_message(&msg),
            Err(SendError::InvalidAddress(_))
        ));

        let mut msg = outgoing();
        msg.to.clear();
        msg.cc.clear();
        msg.bcc.clear();
        assert!(matches!(
            build_message(&msg),
            Err(SendError::InvalidAddress(_))
        ));
    }

    #[test]
    fn names_with_special_characters_are_kept_whole() {
        let mut msg = outgoing();
        msg.to = vec![
            Recipient::new(Some("Dupont, Jean (RH): jean@old.fr"), "j@ex.com"),
            Recipient::new(Some("Zoé \"la chef\""), "zoe@ex.com"),
        ];
        let raw = formatted(&msg);
        let parsed = parse_body(&raw);
        let to: Vec<(Option<&str>, &str)> = parsed
            .to
            .iter()
            .map(|a| (a.name.as_deref(), a.addr.as_str()))
            .collect();
        assert_eq!(
            to,
            vec![
                (Some("Dupont, Jean (RH): jean@old.fr"), "j@ex.com"),
                (Some("Zoé \"la chef\""), "zoe@ex.com"),
            ]
        );
    }

    #[test]
    fn message_ids_are_unique() {
        let a = new_message_id("a@ex.com");
        let b = new_message_id("a@ex.com");
        assert_ne!(a, b);
        assert!(new_message_id("bad").ends_with("@avero.invalid>"));
    }

    #[tokio::test]
    async fn unreachable_server_is_connect_error() {
        let cfg = ServerConfig {
            host: "127.0.0.1".into(),
            port: 1,
            username: "u".into(),
            security: Security::Tls,
        };
        let result = send(&cfg, &Credentials::Password("hunter2".into()), &outgoing()).await;
        match result {
            Err(SendError::Connect(text)) => assert!(!text.contains("hunter2"), "{text}"),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_reports_an_unreachable_server() {
        let cfg = ServerConfig {
            host: "127.0.0.1".into(),
            port: 1,
            username: "u".into(),
            security: Security::Tls,
        };
        let result = check(&cfg, &Credentials::Password("hunter2".into())).await;
        match result {
            Err(SendError::Connect(text)) => assert!(!text.contains("hunter2"), "{text}"),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    /// A server that does not offer STARTTLS: nothing secret may be sent.
    #[tokio::test]
    async fn starttls_is_required_before_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            let mut received = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply: &[u8] = if line.starts_with("EHLO") {
                    b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                } else {
                    b"221 bye\r\n"
                };
                received.push(line);
                if write.write_all(reply).await.is_err() {
                    break;
                }
            }
            received
        });
        let cfg = ServerConfig {
            host: "127.0.0.1".into(),
            port,
            username: "u".into(),
            security: Security::StartTls,
        };

        let result = send(&cfg, &Credentials::Password("hunter2".into()), &outgoing()).await;

        assert!(matches!(result, Err(SendError::Connect(_))), "{result:?}");
        let received = server.await.unwrap();
        assert!(
            received
                .iter()
                .all(|l| !l.starts_with("AUTH") && !l.starts_with("MAIL")),
            "{received:?}"
        );
    }
}
