//! The user's rules: "if the sender contains X, archive it".
//!
//! A rule has conditions (sender, recipient or subject contains a text,
//! or the sender's address is exactly one, case ignored), checked all
//! together or at least one of them, and
//! actions: mark as read, and at most one move (archive, trash, spam).
//! It applies to one account or to all of them. Rules stay on this
//! computer: they act on the new messages of the inbox after each sync
//! (so only while Avero runs), and on the inbox on demand.

use crate::AccountId;
use crate::category::sender_key;

/// What a condition looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleField {
    /// The sender's address and name.
    From,
    /// The recipients (To) and the mailbox that received the message.
    To,
    Subject,
    /// The sender's address, compared whole (not "contains"): what
    /// "Bloquer" uses, so that blocking `jo@gmail.com` spares
    /// `marjo@gmail.com`.
    FromAddress,
}

impl RuleField {
    pub const ALL: [RuleField; 4] = [
        RuleField::From,
        RuleField::To,
        RuleField::Subject,
        RuleField::FromAddress,
    ];

    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleField::From => "from",
            RuleField::To => "to",
            RuleField::Subject => "subject",
            RuleField::FromAddress => "from_address",
        }
    }

    /// Parses the text form; unknown text gives `Subject`.
    pub fn parse(s: &str) -> RuleField {
        match s {
            "from" => RuleField::From,
            "to" => RuleField::To,
            "from_address" => RuleField::FromAddress,
            _ => RuleField::Subject,
        }
    }
}

/// "`field` contains `value`" (case ignored), or for `FromAddress`,
/// "the sender's address is `value`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub field: RuleField,
    pub value: String,
}

/// Where a rule moves a message on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleMove {
    Archive,
    Trash,
    Junk,
}

impl RuleMove {
    pub const ALL: [RuleMove; 3] = [RuleMove::Archive, RuleMove::Trash, RuleMove::Junk];

    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleMove::Archive => "archive",
            RuleMove::Trash => "trash",
            RuleMove::Junk => "junk",
        }
    }

    /// Parses the text form; unknown text gives `None` (no move).
    pub fn parse(s: &str) -> Option<RuleMove> {
        match s {
            "archive" => Some(RuleMove::Archive),
            "trash" => Some(RuleMove::Trash),
            "junk" => Some(RuleMove::Junk),
            _ => None,
        }
    }
}

/// What a rule says, without its place in the list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuleDef {
    pub name: String,
    pub conditions: Vec<Condition>,
    /// All conditions must hold (else at least one).
    pub match_all: bool,
    pub mark_read: bool,
    pub move_to: Option<RuleMove>,
    /// The account it applies to; `None`: every account.
    pub account: Option<AccountId>,
}

/// A saved rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: i64,
    pub enabled: bool,
    pub def: RuleDef,
}

/// What a rule can see of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFacts {
    /// The stored row (`messages.id`).
    pub row: i64,
    /// Its UID in its folder.
    pub uid: u32,
    pub account: AccountId,
    pub from_addr: String,
    pub from_name: Option<String>,
    /// To addresses, and the Delivered-To mailbox.
    pub to: Vec<String>,
    pub subject: String,
    pub seen: bool,
}

/// What the rules decided for one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuleOutcome {
    pub mark_read: bool,
    pub move_to: Option<RuleMove>,
}

impl RuleOutcome {
    /// Whether the rules change anything for this message (marking an
    /// already read message as read changes nothing).
    pub fn changes(&self, facts: &RuleFacts) -> bool {
        self.move_to.is_some() || (self.mark_read && !facts.seen)
    }
}

/// Lower case, without the spaces around: what "contains" compares.
fn folded(text: &str) -> String {
    text.trim().to_lowercase()
}

impl Condition {
    /// Whether the message matches. An empty value matches nothing (a
    /// half-filled condition must not catch every message).
    pub fn matches(&self, facts: &RuleFacts) -> bool {
        let wanted = folded(&self.value);
        if wanted.is_empty() {
            return false;
        }
        let has = |text: &str| text.to_lowercase().contains(&wanted);
        match self.field {
            RuleField::From => has(&facts.from_addr) || facts.from_name.as_deref().is_some_and(has),
            RuleField::To => facts.to.iter().any(|addr| has(addr)),
            RuleField::Subject => has(&facts.subject),
            RuleField::FromAddress => sender_key(&facts.from_addr) == wanted,
        }
    }
}

impl RuleDef {
    /// Whether the rule applies to the message: right account, and its
    /// conditions hold (all of them, or at least one). A rule without
    /// any condition matches nothing.
    pub fn matches(&self, facts: &RuleFacts) -> bool {
        if self.account.is_some_and(|account| account != facts.account) {
            return false;
        }
        let conditions: Vec<&Condition> = self
            .conditions
            .iter()
            .filter(|c| !c.value.trim().is_empty())
            .collect();
        if conditions.is_empty() {
            return false;
        }
        if self.match_all {
            conditions.iter().all(|c| c.matches(facts))
        } else {
            conditions.iter().any(|c| c.matches(facts))
        }
    }

    /// Whether the rule does something when it matches.
    pub fn has_action(&self) -> bool {
        self.mark_read || self.move_to.is_some()
    }
}

/// What the enabled rules, in their order, do to a message: each one
/// that matches can mark it read; the first one that moves it decides
/// where it goes (the later moves are ignored).
pub fn outcome(rules: &[Rule], facts: &RuleFacts) -> RuleOutcome {
    let mut result = RuleOutcome::default();
    for rule in rules.iter().filter(|r| r.enabled) {
        if !rule.def.matches(facts) {
            continue;
        }
        result.mark_read |= rule.def.mark_read;
        if result.move_to.is_none() {
            result.move_to = rule.def.move_to;
        }
    }
    result
}

/// The rule "Bloquer" makes for a sender: their future mail goes to the
/// trash, on every account. Only this exact address is caught (not the
/// addresses or names that contain it).
pub fn block_rule(addr: &str, name: &str) -> RuleDef {
    let shown = if name.trim().is_empty() {
        addr.trim()
    } else {
        name.trim()
    };
    RuleDef {
        name: format!("Bloquer {shown}"),
        conditions: vec![Condition {
            field: RuleField::FromAddress,
            value: sender_key(addr),
        }],
        match_all: true,
        mark_read: false,
        move_to: Some(RuleMove::Trash),
        account: None,
    }
}

/// Whether `rule` is the block rule of `addr` (see [`block_rule`]).
pub fn blocks(rule: &RuleDef, addr: &str) -> bool {
    rule.move_to == Some(RuleMove::Trash)
        && rule.account.is_none()
        && matches!(rule.conditions.as_slice(),
            [c] if c.field == RuleField::FromAddress && folded(&c.value) == folded(addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(from: &str, subject: &str) -> RuleFacts {
        RuleFacts {
            row: 1,
            uid: 1,
            account: AccountId(1),
            from_addr: from.to_string(),
            from_name: Some("La Boutique".to_string()),
            to: vec!["moi@example.com".to_string()],
            subject: subject.to_string(),
            seen: false,
        }
    }

    fn cond(field: RuleField, value: &str) -> Condition {
        Condition {
            field,
            value: value.to_string(),
        }
    }

    fn rule(id: i64, conditions: Vec<Condition>, match_all: bool, to: Option<RuleMove>) -> Rule {
        Rule {
            id,
            enabled: true,
            def: RuleDef {
                name: format!("règle {id}"),
                conditions,
                match_all,
                mark_read: false,
                move_to: to,
                account: None,
            },
        }
    }

    #[test]
    fn conditions_ignore_case_and_look_at_the_right_field() {
        let f = facts("Promo@Boutique.fr", "Soldes d'HIVER");
        assert!(cond(RuleField::From, "boutique.FR").matches(&f));
        assert!(cond(RuleField::From, "la boutique").matches(&f), "name");
        assert!(cond(RuleField::Subject, "hiver").matches(&f));
        assert!(cond(RuleField::To, "moi@").matches(&f));
        assert!(!cond(RuleField::Subject, "boutique").matches(&f));
        assert!(!cond(RuleField::From, "  ").matches(&f), "empty value");
    }

    #[test]
    fn all_or_at_least_one() {
        let f = facts("promo@boutique.fr", "Soldes");
        let both = vec![
            cond(RuleField::From, "boutique"),
            cond(RuleField::Subject, "facture"),
        ];
        assert!(!rule(1, both.clone(), true, None).def.matches(&f));
        assert!(rule(1, both, false, None).def.matches(&f));
        assert!(!rule(1, Vec::new(), false, None).def.matches(&f));
    }

    #[test]
    fn scope_is_one_account_or_all() {
        let f = facts("promo@boutique.fr", "Soldes");
        let mut r = rule(1, vec![cond(RuleField::From, "boutique")], true, None);
        assert!(r.def.matches(&f));
        r.def.account = Some(AccountId(2));
        assert!(!r.def.matches(&f));
        r.def.account = Some(AccountId(1));
        assert!(r.def.matches(&f));
    }

    #[test]
    fn first_move_wins_and_read_adds_up() {
        let f = facts("promo@boutique.fr", "Soldes");
        let mut reader = rule(1, vec![cond(RuleField::Subject, "soldes")], true, None);
        reader.def.mark_read = true;
        let archive = rule(
            2,
            vec![cond(RuleField::From, "boutique")],
            true,
            Some(RuleMove::Archive),
        );
        let trash = rule(
            3,
            vec![cond(RuleField::From, "promo")],
            true,
            Some(RuleMove::Trash),
        );
        let got = outcome(&[reader.clone(), archive.clone(), trash.clone()], &f);
        assert_eq!(
            got,
            RuleOutcome {
                mark_read: true,
                move_to: Some(RuleMove::Archive)
            }
        );
        // In the other order, the trash rule comes first.
        let got = outcome(&[trash, archive.clone()], &f);
        assert_eq!(got.move_to, Some(RuleMove::Trash));
        // A disabled rule does nothing.
        let mut off = archive;
        off.enabled = false;
        assert_eq!(outcome(&[off], &f), RuleOutcome::default());
    }

    #[test]
    fn marking_read_mail_read_changes_nothing() {
        let mut f = facts("a@b.c", "x");
        let read = RuleOutcome {
            mark_read: true,
            move_to: None,
        };
        assert!(read.changes(&f));
        f.seen = true;
        assert!(!read.changes(&f));
    }

    #[test]
    fn block_rule_sends_the_sender_to_the_trash() {
        let def = block_rule(" Promo@Boutique.fr ", "La Boutique");
        assert_eq!(def.name, "Bloquer La Boutique");
        assert_eq!(def.move_to, Some(RuleMove::Trash));
        assert_eq!(def.account, None);
        assert!(def.matches(&facts("promo@boutique.fr", "x")));
        assert!(!def.matches(&facts("info@ailleurs.fr", "x")));
        assert!(blocks(&def, "PROMO@boutique.fr"));
        assert!(!blocks(&def, "info@ailleurs.fr"));
    }

    #[test]
    fn blocking_a_sender_spares_the_addresses_that_contain_it() {
        let def = block_rule("jo@gmail.com", "");
        assert!(def.matches(&facts(" JO@gmail.com ", "x")));
        assert!(!def.matches(&facts("marjo@gmail.com", "x")));
        assert!(!def.matches(&facts("jo@gmail.com.evil.fr", "x")));
        let mut named = facts("autre@site.fr", "x");
        named.from_name = Some("jo@gmail.com".to_string());
        assert!(!def.matches(&named));

        // A rule written by hand still means "contains".
        let by_hand = rule(1, vec![cond(RuleField::From, "jo@gmail.com")], true, None);
        assert!(by_hand.def.matches(&facts("marjo@gmail.com", "x")));
    }

    #[test]
    fn text_forms_roundtrip() {
        for field in RuleField::ALL {
            assert_eq!(RuleField::parse(field.as_str()), field);
        }
        for to in RuleMove::ALL {
            assert_eq!(RuleMove::parse(to.as_str()), Some(to));
        }
        assert_eq!(RuleMove::parse(""), None);
    }
}
