//! Parsing of raw messages (headers for the list, full bodies for reading)
//! and detection of folder roles.

use avero_core::model::{FolderRole, NewMessage};
use mail_parser::{MessageParser, MimeHeaders};

/// Builds a `NewMessage` from raw header bytes. Never panics: anything
/// missing or unreadable falls back to a default value.
///
/// `has_attachments` is a guess from the header block alone: a top-level
/// `Content-Type: multipart/mixed` almost always means attachments. It can
/// be wrong (a mixed message with only text parts, or an attachment inside
/// another multipart type); the flag is corrected once the body is read.
pub fn parse_headers(uid: u32, seen: bool, raw: &[u8]) -> NewMessage {
    let mut msg = NewMessage {
        uid,
        message_id: None,
        subject: "(sans objet)".to_string(),
        from_name: None,
        from_addr: String::new(),
        date: 0,
        seen,
        has_attachments: false,
    };

    let Some(parsed) = MessageParser::new().parse_headers(raw) else {
        return msg;
    };

    // mail-parser strips the angle brackets; we store them.
    msg.message_id = parsed.message_id().map(|id| format!("<{id}>"));

    if let Some(subject) = parsed.subject() {
        msg.subject = subject.to_string();
    }

    if let Some(sender) = parsed.from().and_then(|from| from.first()) {
        msg.from_name = sender.name().map(|n| n.to_string());
        msg.from_addr = sender.address().unwrap_or("").to_string();
    }

    if let Some(date) = parsed.date() {
        msg.date = date.to_timestamp();
    }

    msg.has_attachments = parsed.content_type().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("multipart")
            && ct
                .subtype()
                .is_some_and(|sub| sub.eq_ignore_ascii_case("mixed"))
    });

    msg
}

/// Width used to wrap text converted from HTML.
const HTML_TEXT_WIDTH: usize = 100;

/// An e-mail address with its optional display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub addr: String,
}

/// An attachment as listed by `parse_body`; its bytes come from
/// `attachment_data(raw, index)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    /// For example `application/pdf`.
    pub mime: String,
    /// Decoded size in bytes.
    pub size: usize,
    /// Position among the message's attachments.
    pub index: usize,
}

/// A full message, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBody {
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// Unix time in seconds (0 if unknown).
    pub date: i64,
    pub subject: String,
    /// With angle brackets, like `NewMessage::message_id`.
    pub message_id: Option<String>,
    /// The References header (or In-Reply-To when absent), ids with angle
    /// brackets separated by spaces: what a reply's References starts with.
    pub references: Option<String>,
    /// Readable text: the text/plain part, else the HTML converted to text.
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Parses a raw message (as fetched with `fetch_body`). Never panics:
/// unreadable input gives an empty body.
///
/// HTML is turned into plain text locally; nothing is ever fetched from
/// the network (no images, no style sheets).
pub fn parse_body(raw: &[u8]) -> ParsedBody {
    match MessageParser::default().parse(raw) {
        Some(message) => body_of(&message),
        None => empty_body(),
    }
}

/// Like [`parse_body`], and also decodes every attachment in the same
/// pass: `data[i]` holds the bytes of `body.attachments[i]`.
pub fn parse_with_attachments(raw: &[u8]) -> (ParsedBody, Vec<Vec<u8>>) {
    let Some(message) = MessageParser::default().parse(raw) else {
        return (empty_body(), Vec::new());
    };
    let body = body_of(&message);
    let data = message
        .attachments()
        .map(|part| part.contents().to_vec())
        .collect();
    (body, data)
}

/// Runs a parsing job, turning a panic of the MIME or HTML libraries
/// into a readable error instead of bringing the caller down.
pub fn guarded<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .map_err(|_| "Ce message n'a pas pu être analysé".to_string())
}

fn empty_body() -> ParsedBody {
    ParsedBody {
        from: None,
        to: Vec::new(),
        cc: Vec::new(),
        reply_to: Vec::new(),
        date: 0,
        subject: "(sans objet)".to_string(),
        message_id: None,
        references: None,
        text: String::new(),
        attachments: Vec::new(),
    }
}

fn body_of(message: &mail_parser::Message<'_>) -> ParsedBody {
    let mut body = empty_body();
    body.from = message.from().and_then(|a| addresses(a).into_iter().next());
    body.to = message.to().map(addresses).unwrap_or_default();
    body.cc = message.cc().map(addresses).unwrap_or_default();
    body.reply_to = message.reply_to().map(addresses).unwrap_or_default();
    body.date = message.date().map_or(0, |d| d.to_timestamp());
    if let Some(subject) = message.subject() {
        body.subject = subject.to_string();
    }
    body.message_id = message.message_id().map(|id| format!("<{id}>"));
    body.references = id_list(message.references()).or_else(|| id_list(message.in_reply_to()));
    body.text = normalize_text(&readable_text(message));
    body.attachments = message
        .attachments()
        .enumerate()
        .map(|(index, part)| Attachment {
            name: part
                .attachment_name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("piece-jointe-{}", index + 1)),
            mime: mime_type(part),
            size: part.contents().len(),
            index,
        })
        .collect();
    body
}

/// The decoded bytes of attachment `index` (see `Attachment::index`).
pub fn attachment_data(raw: &[u8], index: usize) -> Option<Vec<u8>> {
    let message = MessageParser::default().parse(raw)?;
    let part = message.attachments().nth(index)?;
    Some(part.contents().to_vec())
}

/// Every plain address of an address header (groups are flattened).
fn addresses(list: &mail_parser::Address<'_>) -> Vec<Address> {
    list.iter()
        .filter_map(|a| {
            Some(Address {
                name: a.name().map(str::to_string),
                addr: a.address()?.to_string(),
            })
        })
        .collect()
}

/// `<a> <b>` from a header holding message ids (stored without brackets
/// by mail-parser). `None` when the header is missing or empty.
fn id_list(value: &mail_parser::HeaderValue<'_>) -> Option<String> {
    let ids = value.as_text_list()?;
    let joined = ids
        .iter()
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// The message's text: its text/plain parts if it has any, else its HTML
/// parts converted to text.
fn readable_text(message: &mail_parser::Message<'_>) -> String {
    let parts: Vec<&mail_parser::MessagePart<'_>> = message
        .text_body
        .iter()
        .filter_map(|&id| message.part(id))
        .collect();
    let plain: Vec<&str> = parts
        .iter()
        .filter(|part| !part.is_text_html())
        .filter_map(|part| part.text_contents())
        .collect();
    if !plain.is_empty() {
        return plain.join("\n\n");
    }
    message
        .html_body
        .iter()
        .filter_map(|&id| message.part(id))
        .filter_map(|part| part.text_contents())
        .map(html_to_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// HTML to readable text, entirely offline.
fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), HTML_TEXT_WIDTH).unwrap_or_default()
}

/// LF line ends, no trailing spaces, at most one blank line in a row, no
/// blank lines at the start or the end.
fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank_run > 0 { "\n\n" } else { "\n" });
        }
        blank_run = 0;
        out.push_str(line);
    }
    out
}

/// `type/subtype` of a part, `application/octet-stream` if unknown.
fn mime_type(part: &mail_parser::MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub).to_lowercase(),
            None => ct.ctype().to_lowercase(),
        },
        None => "application/octet-stream".to_string(),
    }
}

/// Guesses the role of a folder from its SPECIAL-USE attributes, then
/// from its path: `INBOX` (any case) is the inbox, otherwise the last
/// segment of the path is compared to well-known names.
pub fn detect_role(path: &str, attributes: &[String]) -> FolderRole {
    for attribute in attributes {
        match attribute.to_lowercase().as_str() {
            "\\sent" => return FolderRole::Sent,
            "\\drafts" => return FolderRole::Drafts,
            "\\archive" | "\\all" => return FolderRole::Archive,
            "\\trash" => return FolderRole::Trash,
            "\\junk" => return FolderRole::Junk,
            _ => {}
        }
    }

    // Only the real INBOX is the inbox, not a sub-folder named "Inbox".
    if path.eq_ignore_ascii_case("INBOX") {
        return FolderRole::Inbox;
    }

    // Last segment, whatever the delimiter ('/' or '.'), as the user
    // reads it (servers send non-ASCII names in modified UTF-7).
    let decoded = decode_folder_name(path);
    let name = decoded.rsplit(['/', '.']).next().unwrap_or(&decoded);
    match name.to_lowercase().as_str() {
        "sent" | "sent items" | "sent messages" | "envoyés" | "éléments envoyés" => {
            FolderRole::Sent
        }
        "drafts" | "brouillons" => FolderRole::Drafts,
        "archive" | "archives" => FolderRole::Archive,
        "trash" | "deleted items" | "corbeille" | "bin" => FolderRole::Trash,
        "junk" | "spam" | "courrier indésirable" => FolderRole::Junk,
        _ => FolderRole::Other,
    }
}

/// Decodes a folder name as servers send it, in modified UTF-7 (RFC 3501
/// 5.1.3): `Envoy&AOk-s` is "Envoyés", `&-` is "&". Anything that is not
/// valid modified UTF-7 comes back unchanged. Only for reading: commands
/// sent to the server use the name exactly as listed.
pub fn decode_folder_name(name: &str) -> String {
    decode_mutf7(name).unwrap_or_else(|| name.to_string())
}

fn decode_mutf7(name: &str) -> Option<String> {
    let mut decoded = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('-')?;
        let encoded = &after[..end];
        if encoded.is_empty() {
            decoded.push('&');
        } else {
            // Base64 of UTF-16BE, with ',' instead of '/' and no padding.
            let bytes = mutf7_base64(encoded)?;
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes
                .chunks(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            decoded.push_str(&String::from_utf16(&units).ok()?);
        }
        rest = &after[end + 1..];
    }
    decoded.push_str(rest);
    Some(decoded)
}

/// The modified base64 of RFC 3501; `None` on any other character.
fn mutf7_base64(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0u32);
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b',' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_headers() {
        let m = parse_headers(7, true, include_bytes!("../tests/fixtures/normal.eml"));
        assert_eq!(m.uid, 7);
        assert!(m.seen);
        assert_eq!(m.message_id.as_deref(), Some("<abc@ex.com>"));
        assert_eq!(m.subject, "Réunion jeudi");
        assert_eq!(m.from_name.as_deref(), Some("Marie D."));
        assert_eq!(m.from_addr, "marie@ex.com");
        // Mon, 26 Oct 2026 07:33:20 +0000
        assert_eq!(m.date, 1_793_000_000);
    }

    #[test]
    fn decodes_encoded_word_subject() {
        let raw = b"Subject: =?UTF-8?B?w4l0w6k=?=\r\n\r\n";
        assert_eq!(parse_headers(1, false, raw).subject, "Été");
    }

    #[test]
    fn missing_headers_use_fallbacks() {
        let m = parse_headers(1, false, b"X-Foo: bar\r\n\r\n");
        assert_eq!(m.subject, "(sans objet)");
        assert_eq!(m.from_addr, "");
        assert_eq!(m.from_name, None);
        assert_eq!(m.date, 0);
        assert_eq!(m.message_id, None);
    }

    #[test]
    fn garbage_bytes_do_not_panic() {
        let m = parse_headers(1, false, &[0xff, 0xfe, 0x00, 0x80]);
        assert_eq!(m.uid, 1);
        assert_eq!(m.subject, "(sans objet)");
    }

    #[test]
    fn multipart_mixed_header_means_attachments() {
        let mixed = b"Subject: a
Content-Type: multipart/Mixed; boundary=x

";
        assert!(parse_headers(1, false, mixed).has_attachments);
        let alternative = b"Content-Type: multipart/alternative; boundary=x

";
        assert!(!parse_headers(1, false, alternative).has_attachments);
        assert!(
            !parse_headers(
                1,
                false,
                b"Subject: a

"
            )
            .has_attachments
        );
    }

    #[test]
    fn role_from_special_use() {
        assert_eq!(
            detect_role("[Gmail]/Messages envoyés", &["\\Sent".into()]),
            FolderRole::Sent
        );
        assert_eq!(detect_role("Tout", &["\\All".into()]), FolderRole::Archive);
    }

    #[test]
    fn role_from_name() {
        assert_eq!(detect_role("INBOX.Corbeille", &[]), FolderRole::Trash);
        assert_eq!(detect_role("[Gmail]/Corbeille", &[]), FolderRole::Trash);
        assert_eq!(detect_role("inbox", &[]), FolderRole::Inbox);
        assert_eq!(detect_role("Éléments envoyés", &[]), FolderRole::Sent);
    }

    /// Servers send non-ASCII folder names in modified UTF-7 (RFC 3501
    /// 5.1.3): "Envoyés" arrives as `Envoy&AOk-s`. Seen against GreenMail
    /// (tests/e2e_local.rs).
    #[test]
    fn role_from_name_in_modified_utf7() {
        assert_eq!(detect_role("Envoy&AOk-s", &[]), FolderRole::Sent);
        assert_eq!(
            detect_role("&AMk-l&AOk-ments envoy&AOk-s", &[]),
            FolderRole::Sent
        );
        assert_eq!(
            detect_role("INBOX.Courrier ind&AOk-sirable", &[]),
            FolderRole::Junk
        );
    }

    #[test]
    fn decodes_modified_utf7_folder_names() {
        assert_eq!(decode_folder_name("Envoy&AOk-s"), "Envoyés");
        assert_eq!(decode_folder_name("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(decode_folder_name("&ZeVnLIqe-"), "日本語");
        // A character outside the BMP (surrogate pair).
        assert_eq!(decode_folder_name("&2D3eAA-"), "😀");
        assert_eq!(decode_folder_name("Plain/ASCII.name"), "Plain/ASCII.name");
        // Broken input comes back unchanged.
        assert_eq!(decode_folder_name("a&AOk"), "a&AOk");
        assert_eq!(decode_folder_name("a&A*k-"), "a&A*k-");
        assert_eq!(decode_folder_name("&2D0-"), "&2D0-");
    }

    #[test]
    fn only_the_real_inbox_is_inbox() {
        assert_eq!(detect_role("INBOX", &[]), FolderRole::Inbox);
        assert_eq!(detect_role("Inbox", &[]), FolderRole::Inbox);
        assert_eq!(detect_role("Archives/2023/Inbox", &[]), FolderRole::Other);
        assert_eq!(detect_role("INBOX.Inbox", &[]), FolderRole::Other);
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(detect_role("Factures", &[]), FolderRole::Other);
    }

    const PLAIN: &str = "From: \"Marie D.\" <marie@ex.com>\r\n\
To: Paul <paul@ex.com>, zoe@ex.com\r\n\
Cc: chef@ex.com\r\n\
Reply-To: equipe@ex.com\r\n\
Subject: =?UTF-8?B?w4l0w6k=?=\r\n\
Date: Mon, 26 Oct 2026 07:33:20 +0000\r\n\
Message-ID: <abc@ex.com>\r\n\
In-Reply-To: <parent@ex.com>\r\n\
References: <root@ex.com> <parent@ex.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Bonjour,   \r\n\
\r\n\
\r\n\
\r\n\
On se voit jeudi.\r\n\
\r\n";

    #[test]
    fn plain_message_headers_and_text() {
        let body = parse_body(PLAIN.as_bytes());
        assert_eq!(
            body.from,
            Some(Address {
                name: Some("Marie D.".into()),
                addr: "marie@ex.com".into()
            })
        );
        let to: Vec<&str> = body.to.iter().map(|a| a.addr.as_str()).collect();
        assert_eq!(to, vec!["paul@ex.com", "zoe@ex.com"]);
        assert_eq!(body.to[0].name.as_deref(), Some("Paul"));
        assert_eq!(body.cc[0].addr, "chef@ex.com");
        assert_eq!(body.reply_to[0].addr, "equipe@ex.com");
        assert_eq!(body.subject, "Été");
        assert_eq!(body.date, 1_793_000_000);
        assert_eq!(body.message_id.as_deref(), Some("<abc@ex.com>"));
        assert_eq!(
            body.references.as_deref(),
            Some("<root@ex.com> <parent@ex.com>")
        );
        // CRLF gone, trailing spaces trimmed, blank lines collapsed.
        assert_eq!(body.text, "Bonjour,\n\nOn se voit jeudi.");
        assert!(body.attachments.is_empty());
    }

    #[test]
    fn in_reply_to_is_used_without_references() {
        let raw = "From: a@b.c\r\nIn-Reply-To: <parent@ex.com>\r\n\r\nok\r\n";
        let body = parse_body(raw.as_bytes());
        assert_eq!(body.references.as_deref(), Some("<parent@ex.com>"));
        let raw = "From: a@b.c\r\n\r\nok\r\n";
        assert_eq!(parse_body(raw.as_bytes()).references, None);
    }

    #[test]
    fn html_only_message_is_converted_to_text() {
        let raw = "From: a@b.c\r\n\
Subject: html\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><head><style>p { color: red }</style></head><body>\
<p>Bonjour <b>Paul</b>,</p><p>Voici la <a href=\"https://ex.com/f\">facture</a>.</p>\
<img src=\"https://tracker.example/pixel.gif\"></body></html>\r\n";
        let body = parse_body(raw.as_bytes());
        assert!(body.text.contains("Bonjour"), "{}", body.text);
        assert!(body.text.contains("Paul"), "{}", body.text);
        assert!(body.text.contains("facture"), "{}", body.text);
        assert!(!body.text.contains("<p>"), "{}", body.text);
        assert!(!body.text.contains("color: red"), "{}", body.text);
    }

    #[test]
    fn alternative_prefers_the_plain_part() {
        let raw = "From: a@b.c\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Version texte\r\n\
--alt\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Version <b>HTML</b></p>\r\n\
--alt--\r\n";
        let body = parse_body(raw.as_bytes());
        assert_eq!(body.text, "Version texte");
        assert!(body.attachments.is_empty());
    }

    const MIXED_WITH_PDF: &str = "From: a@b.c\r\n\
Subject: Facture\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Ci-joint la facture.\r\n\
--mix\r\n\
Content-Type: application/pdf; name=\"facture.pdf\"\r\n\
Content-Disposition: attachment; filename=\"facture.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQKJcOkw7zDtsOfCg==\r\n\
--mix--\r\n";

    #[test]
    fn mixed_message_lists_its_pdf_attachment() {
        let body = parse_body(MIXED_WITH_PDF.as_bytes());
        assert_eq!(body.text, "Ci-joint la facture.");
        let pdf_bytes = b"%PDF-1.4\n%\xc3\xa4\xc3\xbc\xc3\xb6\xc3\x9f\n";
        assert_eq!(
            body.attachments,
            vec![Attachment {
                name: "facture.pdf".into(),
                mime: "application/pdf".into(),
                size: pdf_bytes.len(),
                index: 0,
            }]
        );
        assert_eq!(
            attachment_data(MIXED_WITH_PDF.as_bytes(), 0).as_deref(),
            Some(&pdf_bytes[..])
        );
        assert_eq!(attachment_data(MIXED_WITH_PDF.as_bytes(), 1), None);

        let (again, data) = parse_with_attachments(MIXED_WITH_PDF.as_bytes());
        assert_eq!(again, body);
        assert_eq!(data, vec![pdf_bytes.to_vec()]);
    }

    #[test]
    fn guarded_turns_a_panic_into_an_error() {
        assert_eq!(guarded(|| 2 + 2), Ok(4));
        let error = guarded(|| -> u8 { panic!("html2text exploded") }).unwrap_err();
        assert_eq!(error, "Ce message n'a pas pu être analysé");
    }

    #[test]
    fn garbage_body_does_not_panic() {
        for raw in [
            &[0xff, 0xfe, 0x00, 0x80][..],
            b"",
            b"\r\n\r\n",
            b"Content-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\n--x",
            b"Content-Type: text/html\r\n\r\n<<<</p></div>&#xffffff;",
        ] {
            let body = parse_body(raw);
            assert!(body.attachments.len() <= 1);
            let _ = attachment_data(raw, 0);
        }
        assert_eq!(parse_body(b"").subject, "(sans objet)");
    }

    #[test]
    fn normalize_text_collapses_blank_lines() {
        assert_eq!(
            normalize_text("\r\n\r\na  \r\n\r\n\r\n\r\nb\rc\n\n"),
            "a\n\nb\nc"
        );
        assert_eq!(normalize_text("  indenté\n> cité"), "  indenté\n> cité");
    }
}
