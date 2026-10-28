//! Sorting the inbox in tabs: Important, Newsletters, Notifications.
//!
//! Local rules on a few headers, no AI: what the message says about
//! itself (mailing-list headers, `Auto-Submitted`) and who sent it
//! (`noreply@`, `notifications@`). Everything else is Important. A
//! category chosen by hand for a sender wins over these rules (see
//! `Store::set_sender_category`).

use crate::MessageHeaders;

/// The tab of the inbox a message goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Category {
    /// Mail from people (and anything the rules below do not catch).
    #[default]
    Important,
    /// Mailing lists, newsletters, promotions.
    Newsletter,
    /// Automatic messages: alerts, receipts, password resets...
    Notification,
}

impl Category {
    /// Every category, in the order of the tabs.
    pub const ALL: [Category; 3] = [
        Category::Important,
        Category::Newsletter,
        Category::Notification,
    ];

    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Important => "important",
            Category::Newsletter => "newsletter",
            Category::Notification => "notification",
        }
    }

    /// Parses the text form; unknown text gives `Important`.
    pub fn parse(s: &str) -> Category {
        match s {
            "newsletter" => Category::Newsletter,
            "notification" => Category::Notification,
            _ => Category::Important,
        }
    }
}

/// Local parts of automatic senders that only send notifications.
const NOTIFY_WORDS: [&str; 4] = ["notification", "notify", "alert", "mailer-daemon"];

/// Local parts of senders that cannot be answered.
const NOREPLY_WORDS: [&str; 4] = ["noreply", "no-reply", "no_reply", "donotreply"];

/// The tab a message goes to, from its sender address and headers.
///
/// In this order:
/// 1. `Auto-Submitted` other than `no` (RFC 3834), or a sender such as
///    `notifications@` or `alerts@`: Notification (GitHub's notifications
///    carry list headers, but are notifications first).
/// 2. `List-Unsubscribe`, `List-Id` or `Precedence: bulk/list/junk`:
///    Newsletter (promotions sent from `noreply@` land here).
/// 3. A `noreply@`-like sender: Notification (receipts, codes).
/// 4. Everything else: Important.
pub fn classify(from_addr: &str, headers: &MessageHeaders) -> Category {
    let local = from_addr
        .trim()
        .rsplit_once('@')
        .map_or(from_addr.trim(), |(local, _)| local)
        .to_lowercase();
    let auto_submitted = headers
        .auto_submitted
        .as_deref()
        .map(|value| value.trim().to_lowercase())
        .is_some_and(|value| !value.is_empty() && !value.starts_with("no"));
    if auto_submitted || NOTIFY_WORDS.iter().any(|word| local.contains(word)) {
        return Category::Notification;
    }
    let bulk = headers
        .precedence
        .as_deref()
        .map(|value| value.trim().to_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "bulk" | "list" | "junk"));
    let present = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.trim().is_empty());
    if bulk || present(&headers.list_unsubscribe) || present(&headers.list_id) {
        return Category::Newsletter;
    }
    if NOREPLY_WORDS.iter().any(|word| local.contains(word)) {
        return Category::Notification;
    }
    Category::Important
}

/// The form a sender address is remembered in: trimmed, lower case.
pub fn sender_key(addr: &str) -> String {
    addr.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> MessageHeaders {
        MessageHeaders::default()
    }

    #[test]
    fn plain_mail_is_important() {
        assert_eq!(
            classify("marie@exemple.fr", &headers()),
            Category::Important
        );
        assert_eq!(classify("", &headers()), Category::Important);
        // "no" alone is not a no-reply sender.
        assert_eq!(classify("nora@exemple.fr", &headers()), Category::Important);
    }

    #[test]
    fn list_headers_make_a_newsletter() {
        let mut h = headers();
        h.list_unsubscribe = Some("<https://news.example/u>".into());
        assert_eq!(classify("news@shop.fr", &h), Category::Newsletter);

        let mut h = headers();
        h.list_id = Some("Club <club.lists.example>".into());
        assert_eq!(classify("membre@club.fr", &h), Category::Newsletter);

        for value in ["bulk", " List ", "junk"] {
            let mut h = headers();
            h.precedence = Some(value.into());
            assert_eq!(classify("a@b.fr", &h), Category::Newsletter, "{value}");
        }
        // An empty header counts for nothing.
        let mut h = headers();
        h.list_id = Some("  ".into());
        h.precedence = Some("first-class".into());
        assert_eq!(classify("a@b.fr", &h), Category::Important);
    }

    #[test]
    fn automatic_mail_is_a_notification() {
        let mut h = headers();
        h.auto_submitted = Some("auto-generated".into());
        assert_eq!(classify("robot@b.fr", &h), Category::Notification);
        // `Auto-Submitted: no` is a person.
        h.auto_submitted = Some("No".into());
        assert_eq!(classify("robot@b.fr", &h), Category::Important);

        for sender in [
            "noreply@banque.fr",
            "No-Reply@Shop.com",
            "ne-pas-repondre.donotreply@x.fr",
            "notifications@github.com",
            "alerts@monitoring.io",
            "MAILER-DAEMON@mx.example",
        ] {
            assert_eq!(
                classify(sender, &headers()),
                Category::Notification,
                "{sender}"
            );
        }
    }

    #[test]
    fn notifications_win_over_list_headers_but_noreply_does_not() {
        let mut h = headers();
        h.list_id = Some("<repo.github.com>".into());
        h.list_unsubscribe = Some("<mailto:u@github.com>".into());
        assert_eq!(
            classify("notifications@github.com", &h),
            Category::Notification
        );
        // A promotion sent from noreply@ is a newsletter.
        assert_eq!(classify("noreply@shop.fr", &h), Category::Newsletter);
    }

    #[test]
    fn category_text_roundtrip() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), c);
        }
        assert_eq!(Category::parse("?"), Category::Important);
        assert_eq!(sender_key("  Marie@Exemple.FR "), "marie@exemple.fr");
    }
}
