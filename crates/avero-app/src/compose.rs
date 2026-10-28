//! Building messages: reply and forward drafts, quoting, address lists,
//! attachment limits and the final `Outgoing` sent by SMTP.

use avero_sync::parse::{Address, ParsedBody};
use avero_sync::smtp::{Outgoing, OutgoingAttachment};
use chrono::{Local, TimeZone};
use std::sync::Arc;

/// Total size allowed for attachments (most servers refuse more).
pub const MAX_ATTACHMENTS: usize = 25 * 1024 * 1024;

/// How a draft was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Reply,
    ReplyAll,
    Forward,
}

/// A file attached to a draft. The bytes are shared, not copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub name: String,
    pub mime: String,
    pub data: Arc<Vec<u8>>,
}

/// The text fields of a draft, before the body editor exists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// `Re: subject`, without stacking prefixes.
pub fn reply_subject(subject: &str) -> String {
    prefixed(subject, "Re: ", &["re:", "ré:", "aw:"])
}

/// `Fwd: subject`, without stacking prefixes.
pub fn forward_subject(subject: &str) -> String {
    prefixed(subject, "Fwd: ", &["fwd:", "fw:", "tr:"])
}

fn prefixed(subject: &str, prefix: &str, known: &[&str]) -> String {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    if known.iter().any(|k| lower.starts_with(k)) {
        trimmed.to_string()
    } else {
        format!("{prefix}{trimmed}")
    }
}

/// `Name <addr>`, quoting the name when it holds a comma or quote.
pub fn format_address(address: &Address) -> String {
    match address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(name) if name.contains([',', '"', ';', '<', '>']) => {
            format!("\"{}\" <{}>", name.replace('"', "'"), address.addr)
        }
        Some(name) => format!("{name} <{}>", address.addr),
        None => address.addr.clone(),
    }
}

/// Splits `a@x, "Dupont, Jean" <j@y>; b@z` into addresses, honouring
/// quotes. Empty entries are dropped.
pub fn split_addresses(input: &str) -> Vec<String> {
    let mut list = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            ',' | ';' | '\n' if !quoted => {
                push_trimmed(&mut list, &current);
                current.clear();
            }
            _ => current.push(c),
        }
    }
    push_trimmed(&mut list, &current);
    list
}

fn push_trimmed(list: &mut Vec<String>, entry: &str) {
    let entry = entry.trim();
    if !entry.is_empty() {
        list.push(entry.to_string());
    }
}

/// The bare address inside `Name <addr>` (or the entry itself).
pub fn bare_address(entry: &str) -> String {
    match (entry.rfind('<'), entry.rfind('>')) {
        (Some(start), Some(end)) if start < end => entry[start + 1..end].trim().to_string(),
        _ => entry.trim().to_string(),
    }
}

/// Whether an address looks deliverable (`x@y.z`, no spaces).
pub fn looks_valid(entry: &str) -> bool {
    let addr = bare_address(entry);
    match addr.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !addr.contains(char::is_whitespace)
                && !domain.contains('@')
        }
        None => false,
    }
}

/// "Le 28/10/2026 à 14:32, Marie a écrit :" followed by `> ` lines.
pub fn quote(text: &str, date: i64, author: &str) -> String {
    let when = match Local.timestamp_opt(date, 0).single() {
        Some(d) if date > 0 => d.format("Le %d/%m/%Y à %H:%M, ").to_string(),
        _ => "Le ".to_string(),
    };
    let mut out = format!("{when}{author} a écrit :\n");
    for line in text.trim_end().lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The name shown for a sender: display name, else address.
fn author(parsed: &ParsedBody) -> String {
    match &parsed.from {
        Some(Address {
            name: Some(name), ..
        }) if !name.trim().is_empty() => name.trim().to_string(),
        Some(address) => address.addr.clone(),
        None => "quelqu'un".to_string(),
    }
}

/// `In-Reply-To` and `References` for an answer to `parsed`.
pub fn thread_ids(parsed: &ParsedBody) -> (Option<String>, Option<String>) {
    let Some(id) = parsed.message_id.clone() else {
        return (None, parsed.references.clone());
    };
    let references = match &parsed.references {
        Some(previous) if !previous.trim().is_empty() => format!("{} {id}", previous.trim()),
        _ => id.clone(),
    };
    (Some(id), Some(references))
}

/// Fields of a reply, reply-all or forward of `parsed`. `own` holds the
/// user's addresses, removed from a reply-all.
pub fn answer(parsed: &ParsedBody, mode: Mode, own: &[String]) -> Fields {
    let is_own = |a: &Address| own.iter().any(|o| o.eq_ignore_ascii_case(&a.addr));
    match mode {
        Mode::Forward => Fields {
            subject: forward_subject(&parsed.subject),
            body: format!("\n\n{}", forwarded_block(parsed)),
            ..Fields::default()
        },
        Mode::Reply | Mode::ReplyAll => {
            let primary: Vec<&Address> = if parsed.reply_to.is_empty() {
                parsed.from.iter().collect()
            } else {
                parsed.reply_to.iter().collect()
            };
            let mut to: Vec<&Address> = primary.clone();
            let mut cc: Vec<&Address> = Vec::new();
            if mode == Mode::ReplyAll {
                to.extend(parsed.to.iter().filter(|a| !is_own(a)));
                cc.extend(parsed.cc.iter().filter(|a| !is_own(a)));
            }
            // Answering one's own message: keep the original recipients.
            if to.is_empty() || to.iter().all(|a| is_own(a)) {
                to = parsed.to.iter().filter(|a| !is_own(a)).collect();
                if to.is_empty() {
                    to = parsed.to.iter().collect();
                }
            }
            let (in_reply_to, references) = thread_ids(parsed);
            Fields {
                to: join_unique(&to, &[]),
                cc: join_unique(&cc, &to),
                subject: reply_subject(&parsed.subject),
                body: format!("\n\n{}", quote(&parsed.text, parsed.date, &author(parsed))),
                in_reply_to,
                references,
            }
        }
    }
}

/// Addresses joined with ", ", without duplicates or those in `skip`.
fn join_unique(list: &[&Address], skip: &[&Address]) -> String {
    let mut seen: Vec<String> = skip.iter().map(|a| a.addr.to_lowercase()).collect();
    let mut out = Vec::new();
    for address in list {
        let key = address.addr.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            out.push(format_address(address));
        }
    }
    out.join(", ")
}

/// The header block put above a forwarded message.
fn forwarded_block(parsed: &ParsedBody) -> String {
    let date = match Local.timestamp_opt(parsed.date, 0).single() {
        Some(d) if parsed.date > 0 => d.format("%d/%m/%Y %H:%M").to_string(),
        _ => String::new(),
    };
    let from = parsed.from.as_ref().map(format_address).unwrap_or_default();
    let to: Vec<String> = parsed.to.iter().map(format_address).collect();
    format!(
        "---------- Message transféré ----------\nDe : {from}\nDate : {date}\nObjet : {}\nÀ : {}\n\n{}",
        parsed.subject,
        to.join(", "),
        parsed.text.trim_end()
    )
}

/// A MIME type guessed from the file extension.
pub fn mime_for(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Size for people: "820 o", "14 Ko", "2,4 Mo".
pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} o")
    } else if b < KB * KB {
        format!("{} Ko", (b / KB).round() as u64)
    } else {
        format!("{:.1} Mo", b / KB / KB).replace('.', ",")
    }
}

/// Total size of the attachments.
pub fn total_size(attachments: &[Attached]) -> usize {
    attachments.iter().map(|a| a.data.len()).sum()
}

/// What the composer holds when "Envoyer" is pressed.
#[derive(Debug, Clone)]
pub struct Draft<'a> {
    pub from_name: &'a str,
    pub from_addr: &'a str,
    pub to: &'a str,
    pub cc: &'a str,
    pub bcc: &'a str,
    pub subject: &'a str,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: &'a [Attached],
}

/// Checks a draft and builds the message to send, or explains in French
/// what is wrong.
pub fn build(draft: Draft<'_>) -> Result<Outgoing, String> {
    let to = split_addresses(draft.to);
    let cc = split_addresses(draft.cc);
    let bcc = split_addresses(draft.bcc);
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err("Ajoutez au moins un destinataire.".to_string());
    }
    let invalid: Vec<&String> = to
        .iter()
        .chain(&cc)
        .chain(&bcc)
        .filter(|a| !looks_valid(a))
        .collect();
    if let Some(first) = invalid.first() {
        return Err(format!("Adresse invalide : {first}"));
    }
    if total_size(draft.attachments) > MAX_ATTACHMENTS {
        return Err(format!(
            "Les pièces jointes dépassent {} au total.",
            human_size(MAX_ATTACHMENTS)
        ));
    }
    Ok(Outgoing {
        from_name: Some(draft.from_name.to_string()).filter(|n| !n.trim().is_empty()),
        from_addr: draft.from_addr.to_string(),
        to,
        cc,
        bcc,
        subject: draft.subject.trim().to_string(),
        body: draft.body,
        in_reply_to: draft.in_reply_to,
        references: draft.references,
        attachments: draft
            .attachments
            .iter()
            .map(|a| OutgoingAttachment {
                name: a.name.clone(),
                mime: a.mime.clone(),
                data: a.data.as_ref().clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: Option<&str>, addr: &str) -> Address {
        Address {
            name: name.map(str::to_string),
            addr: addr.to_string(),
        }
    }

    fn original() -> ParsedBody {
        ParsedBody {
            from: Some(addr(Some("Marie Durand"), "marie@ex.com")),
            to: vec![addr(None, "moi@ex.com"), addr(Some("Paul"), "paul@ex.com")],
            cc: vec![addr(None, "zoe@ex.com"), addr(None, "MOI@ex.com")],
            reply_to: Vec::new(),
            date: 0,
            subject: "Réunion jeudi".to_string(),
            message_id: Some("<b@ex.com>".to_string()),
            references: Some("<a@ex.com>".to_string()),
            text: "Bonjour,\n\nÀ jeudi.".to_string(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn subjects_are_not_prefixed_twice() {
        assert_eq!(reply_subject("Bonjour"), "Re: Bonjour");
        assert_eq!(reply_subject("RE: Bonjour"), "RE: Bonjour");
        assert_eq!(reply_subject("re: x"), "re: x");
        assert_eq!(forward_subject("Bonjour"), "Fwd: Bonjour");
        assert_eq!(forward_subject("TR: Bonjour"), "TR: Bonjour");
        assert_eq!(forward_subject("Fwd: x"), "Fwd: x");
    }

    #[test]
    fn quote_prefixes_every_line() {
        let q = quote("Ligne 1\n\nLigne 3\n", 0, "Marie");
        assert_eq!(q, "Le Marie a écrit :\n> Ligne 1\n>\n> Ligne 3\n");
        let dated = quote("x", 1_793_000_000, "Marie");
        assert!(dated.starts_with("Le 26/10/2026 à "), "{dated}");
        assert!(dated.contains(", Marie a écrit :\n> x\n"), "{dated}");
    }

    #[test]
    fn reply_goes_to_sender_and_keeps_thread() {
        let fields = answer(&original(), Mode::Reply, &["moi@ex.com".into()]);
        assert_eq!(fields.to, "Marie Durand <marie@ex.com>");
        assert_eq!(fields.cc, "");
        assert_eq!(fields.subject, "Re: Réunion jeudi");
        assert_eq!(fields.in_reply_to.as_deref(), Some("<b@ex.com>"));
        assert_eq!(fields.references.as_deref(), Some("<a@ex.com> <b@ex.com>"));
        assert!(fields.body.contains("Marie Durand a écrit :\n> Bonjour,"));
    }

    #[test]
    fn reply_uses_reply_to_when_present() {
        let mut parsed = original();
        parsed.reply_to = vec![addr(None, "liste@ex.com")];
        let fields = answer(&parsed, Mode::Reply, &[]);
        assert_eq!(fields.to, "liste@ex.com");
    }

    #[test]
    fn reply_all_adds_recipients_without_own_address() {
        let fields = answer(&original(), Mode::ReplyAll, &["moi@ex.com".into()]);
        assert_eq!(fields.to, "Marie Durand <marie@ex.com>, Paul <paul@ex.com>");
        assert_eq!(fields.cc, "zoe@ex.com");
    }

    #[test]
    fn answering_own_message_targets_original_recipients() {
        let mut parsed = original();
        parsed.from = Some(addr(None, "moi@ex.com"));
        let fields = answer(&parsed, Mode::Reply, &["moi@ex.com".into()]);
        assert_eq!(fields.to, "Paul <paul@ex.com>");
    }

    #[test]
    fn forward_has_header_block_and_no_thread() {
        let fields = answer(&original(), Mode::Forward, &[]);
        assert_eq!(fields.subject, "Fwd: Réunion jeudi");
        assert_eq!(fields.to, "");
        assert!(fields.in_reply_to.is_none());
        assert!(fields.body.contains("Message transféré"));
        assert!(fields.body.contains("De : Marie Durand <marie@ex.com>"));
        assert!(fields.body.ends_with("À jeudi."));
    }

    #[test]
    fn thread_ids_without_references() {
        let mut parsed = original();
        parsed.references = None;
        assert_eq!(
            thread_ids(&parsed),
            (Some("<b@ex.com>".into()), Some("<b@ex.com>".into()))
        );
        parsed.message_id = None;
        assert_eq!(thread_ids(&parsed), (None, None));
    }

    #[test]
    fn addresses_split_on_commas_outside_quotes() {
        let list = split_addresses(" a@x.fr, \"Dupont, Jean\" <j@y.fr>; ,b@z.fr ");
        assert_eq!(list, vec!["a@x.fr", "\"Dupont, Jean\" <j@y.fr>", "b@z.fr"]);
        assert!(split_addresses("  ").is_empty());
    }

    #[test]
    fn names_with_commas_are_quoted() {
        let formatted = format_address(&addr(Some("Dupont, Jean"), "j@y.fr"));
        assert_eq!(formatted, "\"Dupont, Jean\" <j@y.fr>");
        assert_eq!(split_addresses(&formatted), vec![formatted.clone()]);
    }

    #[test]
    fn address_validation() {
        assert!(looks_valid("a@x.fr"));
        assert!(looks_valid("Jean <j@y.fr>"));
        assert!(!looks_valid("jean"));
        assert!(!looks_valid("a@x"));
        assert!(!looks_valid("a b@x.fr"));
        assert_eq!(bare_address("Jean <j@y.fr>"), "j@y.fr");
    }

    fn draft<'a>(to: &'a str, attachments: &'a [Attached]) -> Draft<'a> {
        Draft {
            from_name: "Moi",
            from_addr: "moi@ex.com",
            to,
            cc: "",
            bcc: "",
            subject: " Salut ",
            body: "Texte".into(),
            in_reply_to: None,
            references: None,
            attachments,
        }
    }

    #[test]
    fn build_checks_recipients() {
        assert_eq!(
            build(draft("", &[])).unwrap_err(),
            "Ajoutez au moins un destinataire."
        );
        assert_eq!(
            build(draft("a@x.fr, oops", &[])).unwrap_err(),
            "Adresse invalide : oops"
        );
        let out = build(draft("a@x.fr", &[])).unwrap();
        assert_eq!(out.to, vec!["a@x.fr"]);
        assert_eq!(out.subject, "Salut");
        assert_eq!(out.from_name.as_deref(), Some("Moi"));
    }

    #[test]
    fn build_refuses_more_than_25_mb() {
        let big = Attached {
            name: "gros.bin".into(),
            mime: "application/octet-stream".into(),
            data: Arc::new(vec![0; MAX_ATTACHMENTS + 1]),
        };
        let attachments = [big];
        let error = build(draft("a@x.fr", &attachments)).unwrap_err();
        assert!(error.contains("25,0 Mo"), "{error}");
    }

    #[test]
    fn sizes_and_mime_types() {
        assert_eq!(human_size(820), "820 o");
        assert_eq!(human_size(14 * 1024), "14 Ko");
        assert_eq!(human_size(2_516_582), "2,4 Mo");
        assert_eq!(mime_for("Rapport.PDF"), "application/pdf");
        assert_eq!(mime_for("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_for("sans-extension"), "application/octet-stream");
    }
}
