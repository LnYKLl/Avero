//! Parsing of raw IMAP headers and detection of folder roles.

use avero_core::model::{FolderRole, NewMessage};
use mail_parser::MessageParser;

/// Builds a `NewMessage` from raw header bytes. Never panics: anything
/// missing or unreadable falls back to a default value.
pub fn parse_headers(uid: u32, seen: bool, raw: &[u8]) -> NewMessage {
    let mut msg = NewMessage {
        uid,
        message_id: None,
        subject: "(sans objet)".to_string(),
        from_name: None,
        from_addr: String::new(),
        date: 0,
        seen,
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

    msg
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

    // Last segment, whatever the delimiter ('/' or '.').
    let name = path.rsplit(['/', '.']).next().unwrap_or(path);
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
}
