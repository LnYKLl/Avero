//! Parsing of raw messages (headers for the list, full bodies for reading)
//! and detection of folder roles.

use avero_core::model::{FolderRole, MessageHeaders, NewMessage};
use mail_parser::{MessageParser, MimeHeaders};

/// The header fields the sync asks for (`BODY.PEEK[HEADER.FIELDS (...)]`):
/// what `parse_headers` reads, and nothing else (no `Received`, no DKIM
/// signatures), so each message costs a few hundred bytes.
pub const HEADER_FIELDS: &str = "MESSAGE-ID SUBJECT FROM DATE CONTENT-TYPE TO IN-REPLY-TO \
     REFERENCES DELIVERED-TO LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST LIST-ID PRECEDENCE \
     AUTO-SUBMITTED";

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
        ..Default::default()
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

    msg.headers = MessageHeaders {
        in_reply_to: ids(parsed.in_reply_to()).into_iter().next(),
        references: ids(parsed.references()),
        to: parsed
            .to()
            .map(|list| addresses(list).into_iter().map(|a| a.addr).collect())
            .unwrap_or_default(),
        delivered_to: raw_header(&parsed, "Delivered-To"),
        list_unsubscribe: raw_header(&parsed, "List-Unsubscribe"),
        list_unsubscribe_post: raw_header(&parsed, "List-Unsubscribe-Post"),
        list_id: raw_header(&parsed, "List-Id"),
        precedence: raw_header(&parsed, "Precedence"),
        auto_submitted: raw_header(&parsed, "Auto-Submitted"),
    };

    msg
}

/// The message ids of a header, with angle brackets.
fn ids(value: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    value
        .as_text_list()
        .unwrap_or_default()
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| format!("<{id}>"))
        .collect()
}

/// The first header called `name` (any case), as written, on one line
/// (folding removed). `None` when missing or empty.
fn raw_header(message: &mail_parser::Message<'_>, name: &str) -> Option<String> {
    let header = message
        .headers()
        .iter()
        .find(|h| h.name.as_str().eq_ignore_ascii_case(name))?;
    let start = header.offset_start as usize;
    let end = header.offset_end as usize;
    let bytes = message.raw_message.get(start..end)?;
    let value = String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!value.is_empty()).then_some(value)
}

/// Width given to the HTML converter: wide enough that it never wraps a
/// paragraph itself (the reader wraps to the pane).
const HTML_TEXT_WIDTH: usize = 10_000;

/// A piece of a line as the reader shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    /// Where a click goes: only `http` and `https` addresses.
    pub link: Option<String>,
    pub strong: bool,
}

/// One line (paragraph) of the message as the reader shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
    /// A title (`<h1>` to `<h6>`).
    pub heading: bool,
}

impl Line {
    fn plain(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

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
    /// Only a draft has one (a sent message never shows its Bcc).
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// Unix time in seconds (0 if unknown).
    pub date: i64,
    pub subject: String,
    /// With angle brackets, like `NewMessage::message_id`.
    pub message_id: Option<String>,
    /// The parent's Message-ID, with angle brackets.
    pub in_reply_to: Option<String>,
    /// The References header (or In-Reply-To when absent), ids with angle
    /// brackets separated by spaces: what a reply's References starts with.
    pub references: Option<String>,
    /// Readable text: the text/plain part, else the HTML converted to text.
    /// What replies quote and the search reads.
    pub text: String,
    /// What the reader shows: the HTML part with its links and bold text
    /// when there is one (it is what the sender meant to be seen), else
    /// the text with its web addresses made links.
    pub lines: Vec<Line>,
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
        bcc: Vec::new(),
        reply_to: Vec::new(),
        date: 0,
        subject: "(sans objet)".to_string(),
        message_id: None,
        in_reply_to: None,
        references: None,
        text: String::new(),
        lines: Vec::new(),
        attachments: Vec::new(),
    }
}

fn body_of(message: &mail_parser::Message<'_>) -> ParsedBody {
    let mut body = empty_body();
    body.from = message.from().and_then(|a| addresses(a).into_iter().next());
    body.to = message.to().map(addresses).unwrap_or_default();
    body.cc = message.cc().map(addresses).unwrap_or_default();
    body.bcc = message.bcc().map(addresses).unwrap_or_default();
    body.reply_to = message.reply_to().map(addresses).unwrap_or_default();
    body.date = message.date().map_or(0, |d| d.to_timestamp());
    if let Some(subject) = message.subject() {
        body.subject = subject.to_string();
    }
    body.message_id = message.message_id().map(|id| format!("<{id}>"));
    body.in_reply_to = id_list(message.in_reply_to());
    body.references = id_list(message.references()).or_else(|| body.in_reply_to.clone());
    let html = html_lines(message);
    body.text = match plain_text(message) {
        Some(plain) => normalize_text(&plain),
        None => normalize_text(&lines_text(&html)),
    };
    body.lines = if html.is_empty() {
        body.text.lines().map(linkify).collect()
    } else {
        html
    };
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

/// The message's text/plain parts, if it has any.
fn plain_text(message: &mail_parser::Message<'_>) -> Option<String> {
    let plain: Vec<&str> = message
        .text_body
        .iter()
        .filter_map(|&id| message.part(id))
        .filter(|part| !part.is_text_html())
        .filter_map(|part| part.text_contents())
        .collect();
    (!plain.is_empty()).then(|| plain.join("\n\n"))
}

/// The message's HTML parts as lines, entirely offline (nothing is
/// fetched; an image only keeps its text when it is a link). Empty when
/// the message has no HTML.
fn html_lines(message: &mail_parser::Message<'_>) -> Vec<Line> {
    let mut lines = Vec::new();
    for part in message.html_body.iter().filter_map(|&id| message.part(id)) {
        if !part.is_text_html() {
            continue;
        }
        if let Some(html) = part.text_contents() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(html_to_lines(html));
        }
    }
    tidy(lines)
}

/// HTML to lines. Layout tables (most newsletters) are read as blocks,
/// one cell after the other, instead of being drawn as a grid.
fn html_to_lines(html: &str) -> Vec<Line> {
    use html2text::render::RichAnnotation;
    let Ok(rendered) = html2text::config::rich()
        .raw_mode(true)
        .link_footnotes(false)
        .lines_from_read(html.as_bytes(), HTML_TEXT_WIDTH)
    else {
        return Vec::new();
    };
    rendered
        .into_iter()
        .map(|rendered| {
            let mut line = Line::default();
            for piece in rendered.tagged_strings() {
                let link = piece.tag.iter().find_map(|tag| match tag {
                    RichAnnotation::Link(url) => Some(url.as_str()),
                    _ => None,
                });
                let image = piece
                    .tag
                    .iter()
                    .any(|tag| matches!(tag, RichAnnotation::Image(_)));
                // An image's alternative text says little on its own
                // ("logo", "spacer"); inside a link it names the link.
                if image && link.is_none() {
                    continue;
                }
                let strong = piece
                    .tag
                    .iter()
                    .any(|tag| matches!(tag, RichAnnotation::Strong));
                push_span(&mut line, &piece.s, link.and_then(web_link), strong);
            }
            heading_of(&mut line);
            line
        })
        .collect()
}

/// Adds a piece to the line, joined to the previous one when it looks
/// the same.
fn push_span(line: &mut Line, text: &str, link: Option<String>, strong: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = line.spans.last_mut()
        && last.link == link
        && last.strong == strong
    {
        last.text.push_str(text);
        return;
    }
    line.spans.push(Span {
        text: text.to_string(),
        link,
        strong,
    });
}

/// The converter writes titles as `# Title`: the marks go, the line is
/// a heading.
fn heading_of(line: &mut Line) {
    let Some(first) = line.spans.first_mut() else {
        return;
    };
    let marks = first.text.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&marks) && first.text[marks..].starts_with(' ') {
        first.text.drain(..=marks);
        line.heading = true;
    }
}

/// The address a link may open: `http` and `https` only (never
/// `javascript:`, `file:` or the like).
fn web_link(url: &str) -> Option<String> {
    let url = url.trim();
    let scheme = url.split_once(':')?.0;
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        .then(|| url.to_string())
}

/// A line of plain text, its web addresses made links.
fn linkify(text: &str) -> Line {
    let mut line = Line::default();
    let mut rest = text;
    while let Some(start) = find_url(rest) {
        let (before, from) = rest.split_at(start);
        let end = from
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .unwrap_or(from.len());
        let url = url_end(&from[..end]);
        push_span(&mut line, before, None, false);
        push_span(&mut line, url, web_link(url), false);
        rest = &from[url.len()..];
    }
    push_span(&mut line, rest, None, false);
    line
}

/// An address without the punctuation that ends the sentence around it
/// (a closing bracket stays when the address opened one).
fn url_end(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().last() else {
            return url;
        };
        let opened = url.matches('(').count() > url.matches(')').count() - usize::from(last == ')');
        let trailing = matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ']' | '\'')
            || (last == ')' && !opened);
        if !trailing {
            return url;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
}

/// Where the next `http://` or `https://` address starts.
fn find_url(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("http") {
        let at = from + found;
        let after = &lower[at..];
        let starts_word = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
        let scheme = if after.starts_with("https://") {
            8
        } else if after.starts_with("http://") {
            7
        } else {
            0
        };
        let named = after[scheme.max(1)..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        if starts_word && scheme > 0 && named {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

/// Lines as plain text.
fn lines_text(lines: &[Line]) -> String {
    lines.iter().map(Line::plain).collect::<Vec<_>>().join("\n")
}

/// Like `normalize_text` for lines: no trailing spaces, at most one blank
/// line in a row, none at the start or the end.
fn tidy(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut blank = false;
    for mut line in lines {
        if let Some(last) = line.spans.last_mut() {
            let kept = last.text.trim_end().len();
            last.text.truncate(kept);
        }
        line.spans.retain(|span| !span.text.is_empty());
        if line.plain().trim().is_empty() {
            blank = true;
            continue;
        }
        if blank && !out.is_empty() {
            out.push(Line::default());
        }
        blank = false;
        out.push(line);
    }
    out
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
    fn reads_threading_list_and_recipient_headers() {
        let raw = "Message-ID: <n@ex.com>\r\n\
Delivered-To: moi@ex.com\r\n\
Delivered-To: alias@ex.com\r\n\
To: Moi <moi@ex.com>, autre@ex.com\r\n\
In-Reply-To: <p@ex.com>\r\n\
References: <root@ex.com>\r\n <p@ex.com>\r\n\
List-Unsubscribe: <mailto:u@list.ex.com?subject=unsub>,\r\n <https://list.ex.com/u/42>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
List-Id: Nouvelles <news.list.ex.com>\r\n\
Precedence: bulk\r\n\
Auto-Submitted: auto-generated\r\n\
\r\n";
        let h = parse_headers(1, false, raw.as_bytes()).headers;
        assert_eq!(h.in_reply_to.as_deref(), Some("<p@ex.com>"));
        assert_eq!(h.references, vec!["<root@ex.com>", "<p@ex.com>"]);
        assert_eq!(h.to, vec!["moi@ex.com", "autre@ex.com"]);
        assert_eq!(h.delivered_to.as_deref(), Some("moi@ex.com"));
        assert_eq!(
            h.list_unsubscribe.as_deref(),
            Some("<mailto:u@list.ex.com?subject=unsub>, <https://list.ex.com/u/42>")
        );
        assert_eq!(
            h.list_unsubscribe_post.as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
        assert_eq!(h.list_id.as_deref(), Some("Nouvelles <news.list.ex.com>"));
        assert_eq!(h.precedence.as_deref(), Some("bulk"));
        assert_eq!(h.auto_submitted.as_deref(), Some("auto-generated"));

        let bare = parse_headers(1, false, b"Subject: a\r\n\r\n").headers;
        assert_eq!(bare, MessageHeaders::default());
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
        // The reader shows the HTML, with its bold text.
        assert_eq!(
            body.lines,
            vec![Line {
                spans: vec![span("Version ", None), strong("HTML")],
                heading: false,
            }]
        );
    }

    fn span(text: &str, link: Option<&str>) -> Span {
        Span {
            text: text.into(),
            link: link.map(str::to_string),
            strong: false,
        }
    }

    fn strong(text: &str) -> Span {
        Span {
            strong: true,
            ..span(text, None)
        }
    }

    fn html_message(html: &str) -> String {
        format!("From: a@b.c\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n")
    }

    #[test]
    fn a_newsletter_reads_as_lines_with_links() {
        let raw = html_message(
            "<table width=\"100%\"><tr><td><img src=\"https://cdn.ex/logo.png\" alt=\"Logo\"></td>\
             <td><a href=\"https://shop.ex/view\">Voir en ligne</a></td></tr>\
             <tr><td colspan=\"2\"><h1>Les soldes</h1><p>Profitez de <strong>-30 %</strong> \
             jusqu'à dimanche.</p><p><a href=\"https://shop.ex/p/1\"><img src=\"https://cdn.ex/1.jpg\" \
             alt=\"Pull\"></a></p></td></tr></table>",
        );
        let body = parse_body(raw.as_bytes());
        let link = |text: &str, url: &str| Line {
            spans: vec![span(text, Some(url))],
            heading: false,
        };
        assert_eq!(
            body.lines,
            vec![
                link("Voir en ligne", "https://shop.ex/view"),
                Line {
                    spans: vec![span("Les soldes", None)],
                    heading: true,
                },
                Line::default(),
                Line {
                    spans: vec![
                        span("Profitez de ", None),
                        strong("-30 %"),
                        span(" jusqu'à dimanche.", None),
                    ],
                    heading: false,
                },
                Line::default(),
                link("Pull", "https://shop.ex/p/1"),
            ]
        );
        // No grid, no numbered references, no addresses at the end.
        for drawn in ['─', '│', '['] {
            assert!(!body.text.contains(drawn), "{}", body.text);
        }
        assert!(!body.text.contains("https://"), "{}", body.text);
    }

    #[test]
    fn only_web_links_can_be_clicked() {
        let raw = html_message(
            "<a href=\"javascript:alert(1)\">a</a> <a href=\"file:///C:/x\">b</a> \
             <a href=\"HTTPS://ex.com/c\">c</a>",
        );
        let body = parse_body(raw.as_bytes());
        let links: Vec<_> = body.lines[0]
            .spans
            .iter()
            .filter_map(|s| s.link.as_deref())
            .collect();
        assert_eq!(links, ["HTTPS://ex.com/c"]);
    }

    #[test]
    fn web_addresses_in_plain_text_become_links() {
        let line = linkify("Voir https://ex.com/a, ou (http://ex.com/b_(c)). Fin");
        assert_eq!(
            line.spans,
            vec![
                span("Voir ", None),
                span("https://ex.com/a", Some("https://ex.com/a")),
                span(", ou (", None),
                span("http://ex.com/b_(c)", Some("http://ex.com/b_(c)")),
                span("). Fin", None),
            ]
        );
        assert_eq!(linkify("rien ici http:// shttps://x").spans.len(), 1);
        let body = parse_body(b"From: a@b.c\r\n\r\nLigne 1\r\n\r\nhttps://ex.com\r\n");
        assert_eq!(body.lines.len(), 3);
        assert_eq!(
            body.lines[2].spans[0].link.as_deref(),
            Some("https://ex.com")
        );
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
