//! Building messages: reply and forward drafts, quoting, address lists,
//! attachment limits and the final `Outgoing` sent by SMTP.

use avero_core::DraftFields;
use avero_sync::parse::{Address, ParsedBody};
use avero_sync::smtp::{Outgoing, OutgoingAttachment, Recipient};
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

/// Characters that force a display name into quotes (RFC 5322 specials).
const NAME_SPECIALS: [char; 13] = [
    ',', '"', ';', '<', '>', '@', ':', '(', ')', '[', ']', '\\', '.',
];

/// `Name <addr>` as shown in the address fields, quoting the name (with
/// `\"` and `\\` escapes) when it holds a special character, so that
/// [`split_addresses`] and [`parse_recipient`] give it back unchanged.
pub fn format_address(address: &Address) -> String {
    match address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(name) if name.contains(NAME_SPECIALS) => {
            let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\" <{}>", address.addr)
        }
        Some(name) => format!("{name} <{}>", address.addr),
        None => address.addr.clone(),
    }
}

/// Splits `a@x, "Dupont, Jean" <j@y>; b@z` into addresses, honouring
/// quotes (and `\"` inside them). Empty entries are dropped.
pub fn split_addresses(input: &str) -> Vec<String> {
    let mut list = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            escaped = false;
            current.push(c);
            continue;
        }
        match c {
            '\\' if quoted => {
                escaped = true;
                current.push(c);
            }
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

/// Reads one entry of an address field: `addr`, `Name <addr>` or
/// `"Quoted, Name" <addr>`. The name is unquoted and unescaped; it is
/// never parsed again (lettre encodes it as it is).
pub fn parse_recipient(entry: &str) -> Recipient {
    let entry = entry.trim();
    let (Some(start), true) = (entry.rfind('<'), entry.ends_with('>')) else {
        return Recipient {
            name: None,
            addr: entry.to_string(),
        };
    };
    let addr = entry[start + 1..entry.len() - 1].trim().to_string();
    let raw_name = entry[..start].trim();
    let name = match raw_name
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(quoted) => unescape(quoted),
        None => raw_name.to_string(),
    };
    Recipient {
        name: Some(name).filter(|n| !n.trim().is_empty()),
        addr,
    }
}

/// Removes the backslashes of a quoted string (`\"` → `"`).
fn unescape(quoted: &str) -> String {
    let mut out = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            _ => out.push(c),
        }
    }
    out
}

/// The bare address inside `Name <addr>` (or the entry itself).
pub fn bare_address(entry: &str) -> String {
    parse_recipient(entry).addr
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

/// The fields of a quick reply ("Répondre à tous…" under a
/// conversation): a reply to all of `parsed` whose body is only what was
/// typed, then the signature; no quote (the conversation is there).
pub fn quick_reply(parsed: &ParsedBody, own: &[String], text: &str, signature: &str) -> Fields {
    let fields = answer(parsed, Mode::ReplyAll, own);
    let text = text.trim_end();
    let block = signature_block(signature);
    let body = if block.is_empty() {
        text.to_string()
    } else {
        format!("{text}\n\n{block}")
    };
    Fields { body, ..fields }
}

/// The signature as a draft shows it: the `-- ` line, then the text
/// (empty when the account has none).
pub fn signature_block(signature: &str) -> String {
    let text = signature.trim_end();
    if text.trim().is_empty() {
        String::new()
    } else {
        format!("-- \n{text}")
    }
}

/// A draft body with the signature: a blank line to write in, the
/// signature, then what the body already held (the quote of a reply, the
/// forwarded message).
pub fn with_signature(body: &str, signature: &str) -> String {
    let block = signature_block(signature);
    if block.is_empty() {
        return body.to_string();
    }
    match body.trim_start_matches('\n') {
        "" => format!("\n\n{block}"),
        rest => format!("\n\n{block}\n\n{rest}"),
    }
}

/// Whether a saved draft is a reply or a forward nobody wrote in: its
/// body is still what "Répondre" or "Transférer" put there (the blank
/// lines to write in, the signature `signature`, then the quote or the
/// forwarded message), a forward has no recipient yet, and a reply has
/// no Bcc and no file. Older versions of Avero kept such a draft when its
/// composer was closed untouched (and saved it on the server at the next
/// start): they are thrown away at start.
pub fn untouched_answer(fields: &DraftFields, signature: &str, has_files: bool) -> bool {
    let block = signature_block(signature);
    let rest = if block.is_empty() {
        fields.body.strip_prefix(
            "

",
        )
    } else {
        fields.body.strip_prefix(&format!(
            "

{block}

"
        ))
    };
    let Some(rest) = rest else {
        return false;
    };
    let subject = fields.subject.trim();
    if rest.starts_with(
        "---------- Message transféré ----------
",
    ) {
        return fields.to.trim().is_empty()
            && fields.cc.trim().is_empty()
            && fields.bcc.trim().is_empty()
            && forward_subject(subject) == subject;
    }
    let mut lines = rest.lines();
    let quote_only = lines
        .next()
        .is_some_and(|first| first.starts_with("Le ") && first.ends_with(" a écrit :"))
        && lines.all(|line| line.starts_with('>'));
    quote_only && !has_files && fields.bcc.trim().is_empty() && reply_subject(subject) == subject
}

/// The body once the sending account changed from one with signature
/// `old` to one with `new`; `base` is the body the draft started from,
/// without signature. `None` when the signature cannot be swapped: the
/// user changed it (or wrote around a missing one), so the body stays.
pub fn swap_signature(body: &str, base: &str, old: &str, new: &str) -> Option<String> {
    // Nothing touched: the draft is built again.
    if body == with_signature(base, old) {
        return Some(with_signature(base, new));
    }
    let old_block = signature_block(old);
    if old_block.is_empty() {
        return None;
    }
    // The old signature as it was inserted, followed by nothing or by
    // the blank line before the quote.
    let at = body
        .match_indices(&old_block)
        .map(|(at, _)| at)
        .find(|&at| {
            let after = &body[at + old_block.len()..];
            after.is_empty() || after.starts_with("\n\n")
        })?;
    let before = &body[..at];
    let after = &body[at + old_block.len()..];
    let new_block = signature_block(new);
    if !new_block.is_empty() {
        return Some(format!("{before}{new_block}{after}"));
    }
    // No signature any more: its blank line goes with it.
    Some(match after.strip_prefix("\n\n") {
        Some(rest) => format!("{before}{rest}"),
        None => before.strip_suffix("\n\n").unwrap_or(before).to_string(),
    })
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
    let recipients = |list: Vec<String>| list.iter().map(|e| parse_recipient(e)).collect();
    Ok(Outgoing {
        from_name: Some(draft.from_name.to_string()).filter(|n| !n.trim().is_empty()),
        from_addr: draft.from_addr.to_string(),
        to: recipients(to),
        cc: recipients(cc),
        bcc: recipients(bcc),
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

    #[test]
    fn signature_goes_before_the_quote() {
        assert_eq!(with_signature("", "Marie\n"), "\n\n-- \nMarie");
        assert_eq!(
            with_signature("\n\nPaul a écrit :\n> Salut", "Marie"),
            "\n\n-- \nMarie\n\nPaul a écrit :\n> Salut"
        );
        // No signature: the body stays as it is.
        assert_eq!(with_signature("\n\n> Salut", "  \n"), "\n\n> Salut");
    }

    #[test]
    fn untouched_signature_is_swapped() {
        let base = "\n\n> Salut";
        let body = with_signature(base, "Marie");
        assert_eq!(
            swap_signature(&body, base, "Marie", "M. Durand\nPro").as_deref(),
            Some("\n\n-- \nM. Durand\nPro\n\n> Salut")
        );
        // From an account without signature to one with.
        assert_eq!(
            swap_signature(base, base, "", "Marie").as_deref(),
            Some(body.as_str())
        );
    }

    #[test]
    fn signature_is_swapped_under_text_typed_above() {
        let typed = "Bonjour,\n\n-- \nMarie\n\n> Salut";
        assert_eq!(
            swap_signature(typed, "\n\n> Salut", "Marie", "Paul").as_deref(),
            Some("Bonjour,\n\n-- \nPaul\n\n> Salut")
        );
        // The new account has none: the signature and its blank line go.
        assert_eq!(
            swap_signature(typed, "\n\n> Salut", "Marie", "").as_deref(),
            Some("Bonjour,\n\n> Salut")
        );
        assert_eq!(
            swap_signature("Bonjour,\n\n-- \nMarie", "", "Marie", "").as_deref(),
            Some("Bonjour,")
        );
    }

    #[test]
    fn edited_signature_is_kept() {
        let edited = "\n\n-- \nMarie\nà bientôt\n\n> Salut";
        assert_eq!(swap_signature(edited, "\n\n> Salut", "Marie", "Paul"), None);
        let gone = "Bonjour\n\n> Salut";
        assert_eq!(swap_signature(gone, "\n\n> Salut", "Marie", "Paul"), None);
        // Text written in a draft that had no signature stays as it is.
        assert_eq!(swap_signature("Bonjour", "", "", "Paul"), None);
    }

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
            bcc: Vec::new(),
            reply_to: Vec::new(),
            date: 0,
            subject: "Réunion jeudi".to_string(),
            message_id: Some("<b@ex.com>".to_string()),
            in_reply_to: Some("<a@ex.com>".to_string()),
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
    fn quick_reply_answers_everyone_without_quote() {
        let own = ["moi@ex.com".to_string()];
        let fields = quick_reply(&original(), &own, "D'accord, à jeudi !\n\n", "Camille");
        assert_eq!(fields.to, "Marie Durand <marie@ex.com>, Paul <paul@ex.com>");
        assert_eq!(fields.cc, "zoe@ex.com");
        assert_eq!(fields.subject, "Re: Réunion jeudi");
        assert_eq!(fields.in_reply_to.as_deref(), Some("<b@ex.com>"));
        assert_eq!(fields.references.as_deref(), Some("<a@ex.com> <b@ex.com>"));
        assert_eq!(fields.body, "D'accord, à jeudi !\n\n-- \nCamille");
        assert!(!fields.body.contains("a écrit"));
        assert!(!fields.body.contains("> "));
        // No signature: just the text.
        let plain = quick_reply(&original(), &own, "Merci", "  ");
        assert_eq!(plain.body, "Merci");
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

    /// The draft a composer opened with `fields` and signed with
    /// `signature` keeps, untouched.
    fn kept(fields: &Fields, signature: &str) -> DraftFields {
        DraftFields {
            account: avero_core::AccountId(1),
            to: fields.to.clone(),
            cc: fields.cc.clone(),
            bcc: String::new(),
            subject: fields.subject.clone(),
            body: with_signature(&fields.body, signature),
            in_reply_to: fields.in_reply_to.clone(),
            references: fields.references.clone(),
            message_id: "<d@ex.com>".to_string(),
        }
    }

    #[test]
    fn untouched_replies_and_forwards_are_recognised() {
        let own = ["moi@ex.com".to_string()];
        for signature in ["", "Camille"] {
            let reply = kept(&answer(&original(), Mode::ReplyAll, &own), signature);
            assert!(untouched_answer(&reply, signature, false));
            let forward = kept(&answer(&original(), Mode::Forward, &own), signature);
            assert!(untouched_answer(&forward, signature, true));

            // Anything written keeps the draft.
            let mut typed = reply.clone();
            typed.body = format!("Bonjour,{}", reply.body);
            assert!(!untouched_answer(&typed, signature, false));
            let mut sent_to = forward.clone();
            sent_to.to = "paul@ex.com".into();
            assert!(!untouched_answer(&sent_to, signature, true));
            let mut subject = reply.clone();
            subject.subject = "Compte rendu".into();
            assert!(!untouched_answer(&subject, signature, false));
            assert!(!untouched_answer(&reply, signature, true));
        }
        // A new message, even empty, is not an answer.
        let new = kept(&Fields::default(), "Camille");
        assert!(!untouched_answer(&new, "Camille", false));
        // Signed with a signature changed since: kept, to be safe.
        let reply = kept(&answer(&original(), Mode::Reply, &own), "Camille");
        assert!(!untouched_answer(&reply, "C. Martin", false));
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
    fn odd_names_survive_the_address_field() {
        for name in [
            "Dupont, Jean",
            "jean@old.fr",
            "Service: RH (Paris)",
            "Zoé \"la chef\"",
            "C:\\dossier",
            "J. Martin",
        ] {
            let formatted = format_address(&addr(Some(name), "j@y.fr"));
            let typed = format!("a@x.fr, {formatted}; b@z.fr");
            let entries = split_addresses(&typed);
            assert_eq!(entries.len(), 3, "{typed}");
            assert_eq!(
                parse_recipient(&entries[1]),
                Recipient::new(Some(name), "j@y.fr"),
                "{formatted}"
            );
            assert!(looks_valid(&entries[1]));
        }
    }

    #[test]
    fn typed_recipients_are_read_robustly() {
        assert_eq!(
            parse_recipient("  Jean Dupont <j@y.fr> "),
            Recipient::new(Some("Jean Dupont"), "j@y.fr")
        );
        assert_eq!(parse_recipient("<j@y.fr>"), Recipient::new(None, "j@y.fr"));
        assert_eq!(parse_recipient("j@y.fr"), Recipient::new(None, "j@y.fr"));
        assert_eq!(
            split_addresses("\"a, b\" <a@x.fr>, \"c \\\" d, e\" <c@x.fr>"),
            vec!["\"a, b\" <a@x.fr>", "\"c \\\" d, e\" <c@x.fr>"]
        );
        assert_eq!(
            parse_recipient("\"c \\\" d, e\" <c@x.fr>").name.as_deref(),
            Some("c \" d, e")
        );
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
        let out = build(draft("a@x.fr, \"Dupont, Jean\" <j@y.fr>", &[])).unwrap();
        assert_eq!(
            out.to,
            vec![
                Recipient::new(None, "a@x.fr"),
                Recipient::new(Some("Dupont, Jean"), "j@y.fr")
            ]
        );
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
