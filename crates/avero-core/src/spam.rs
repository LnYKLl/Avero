//! The local spam filter: a naive Bayesian filter, in the manner of Paul
//! Graham's "A Plan for Spam" (2002).
//!
//! A message is cut into tokens (words of its subject, its sender and
//! its domain, words of its body when it is cached). The model counts,
//! for each token, how many messages learned as spam and as wanted mail
//! ("ham") held it; it learns from the "Spam" and "Pas spam" buttons and,
//! once, from what the spam and inbox folders already hold. Everything
//! stays in the local database: nothing is ever sent anywhere.
//!
//! The filter is prudent on purpose: wanted mail counts double, a token
//! needs to have been seen a few times to count, and a message only goes
//! to the spam folder above [`THRESHOLD`], once [`MIN_LEARNED`] messages
//! of each kind were learned.

use std::collections::BTreeSet;

/// Above this probability a new message is spam.
pub const THRESHOLD: f64 = 0.99;

/// Messages of each kind (spam and ham) to learn before classifying
/// anything.
pub const MIN_LEARNED: u64 = 20;

/// How many of the most telling tokens decide.
const INTERESTING: usize = 15;

/// A token counts once `2 × ham + spam` reaches this.
const MIN_SEEN: u64 = 5;

/// How sure a single token may make the filter.
const CLAMP: f64 = 0.01;

/// Characters of the body read.
const BODY_CHARS: usize = 3000;

/// At most this many tokens per message.
const MAX_TOKENS: usize = 400;

/// Word lengths kept, in characters.
const WORD_LEN: std::ops::RangeInclusive<usize> = 2..=24;

/// What the filter reads of a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpamInput<'a> {
    pub from_addr: &'a str,
    pub from_name: Option<&'a str>,
    pub subject: &'a str,
    /// The cached body text, if any.
    pub body: Option<&'a str>,
}

/// How often a token was seen in learned messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCount {
    pub spam: u64,
    pub ham: u64,
}

/// How many messages were learned of each kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Learned {
    pub spam: u64,
    pub ham: u64,
}

impl Learned {
    /// Whether the filter learned enough to classify.
    pub fn is_enough(&self) -> bool {
        self.spam >= MIN_LEARNED && self.ham >= MIN_LEARNED
    }
}

/// The words of a text, lower case: letters and digits, other
/// characters cut words. Numbers alone and words too short or too long
/// are dropped.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| WORD_LEN.contains(&w.chars().count()))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
}

/// The tokens of a message, each once, sorted. Header tokens carry a
/// prefix (`s:` subject, `n:` sender's name, `f:` sender's address, `d:`
/// its domain) so that "promo" in a subject and in a body count apart.
pub fn tokens(input: &SpamInput) -> Vec<String> {
    let mut found = BTreeSet::new();
    let addr = input.from_addr.trim().to_lowercase();
    if !addr.is_empty() {
        found.insert(format!("f:{addr}"));
    }
    if let Some((_, domain)) = addr.rsplit_once('@') {
        found.insert(format!("d:{domain}"));
        // The registered part ("example.com" of "news.example.com").
        let labels: Vec<&str> = domain.split('.').collect();
        if labels.len() > 2 {
            found.insert(format!("d:{}", labels[labels.len() - 2..].join(".")));
        }
    }
    for word in words(input.subject) {
        found.insert(format!("s:{word}"));
    }
    for word in words(input.from_name.unwrap_or("")) {
        found.insert(format!("n:{word}"));
    }
    if let Some(body) = input.body {
        let start: String = body.chars().take(BODY_CHARS).collect();
        for word in words(&start) {
            if found.len() >= MAX_TOKENS {
                break;
            }
            found.insert(word);
        }
    }
    found.into_iter().take(MAX_TOKENS).collect()
}

/// How much one token says "spam" (0.01 to 0.99), or `None` when it was
/// not seen often enough. Wanted mail counts double: a token must be
/// clearly more frequent in spam to count against a message.
fn token_probability(count: TokenCount, learned: Learned) -> Option<f64> {
    if learned.spam == 0 || learned.ham == 0 || 2 * count.ham + count.spam < MIN_SEEN {
        return None;
    }
    let bad = (count.spam as f64 / learned.spam as f64).min(1.0);
    let good = (2.0 * count.ham as f64 / learned.ham as f64).min(1.0);
    if bad + good == 0.0 {
        return None;
    }
    Some((bad / (bad + good)).clamp(CLAMP, 1.0 - CLAMP))
}

/// The probability that a message is spam, from the counts of its
/// tokens: the [`INTERESTING`] tokens farthest from 0.5 are combined
/// (Bayes, tokens taken as independent). `None` before the filter
/// learned enough ([`Learned::is_enough`]); 0.5 when no token says
/// anything.
pub fn probability(counts: &[TokenCount], learned: Learned) -> Option<f64> {
    if !learned.is_enough() {
        return None;
    }
    let mut telling: Vec<f64> = counts
        .iter()
        .filter_map(|&count| token_probability(count, learned))
        .collect();
    // Farthest from 0.5 first; on a tie the wanted-mail side first (the
    // prudent choice).
    telling.sort_by(|a, b| {
        (b - 0.5)
            .abs()
            .total_cmp(&(a - 0.5).abs())
            .then(a.total_cmp(b))
    });
    telling.truncate(INTERESTING);
    if telling.is_empty() {
        return Some(0.5);
    }
    // Π p / (Π p + Π (1 - p)), in logarithms (no underflow).
    let spam: f64 = telling.iter().map(|p| p.ln()).sum();
    let ham: f64 = telling.iter().map(|p| (1.0 - p).ln()).sum();
    Some(1.0 / (1.0 + (ham - spam).exp()))
}

/// Whether a probability is high enough to move a message to the spam
/// folder.
pub fn is_spam(probability: Option<f64>) -> bool {
    probability.is_some_and(|p| p > THRESHOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(from: &'a str, subject: &'a str, body: Option<&'a str>) -> SpamInput<'a> {
        SpamInput {
            from_addr: from,
            from_name: Some("Super Promo"),
            subject,
            body,
        }
    }

    #[test]
    fn tokens_mark_where_words_come_from() {
        let t = tokens(&input(
            "Deals@News.Casino-Gagnant.com",
            "GAGNEZ 1000 € maintenant !",
            Some("Cliquez ici, gagnez"),
        ));
        for expected in [
            "f:deals@news.casino-gagnant.com",
            "d:news.casino-gagnant.com",
            "d:casino-gagnant.com",
            "s:gagnez",
            "s:maintenant",
            "n:super",
            "n:promo",
            "cliquez",
            "ici",
            "gagnez",
        ] {
            assert!(t.contains(&expected.to_string()), "{expected} in {t:?}");
        }
        assert!(!t.iter().any(|w| w.ends_with("1000")), "numbers dropped");
        let mut sorted = t.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, t, "each once");
    }

    #[test]
    fn tokens_are_bounded() {
        let body: String = (0..2000).map(|i| format!("mot{i} ")).collect();
        let t = tokens(&input("a@b.c", "x", Some(&body)));
        assert!(t.len() <= MAX_TOKENS, "{}", t.len());
    }

    const ENOUGH: Learned = Learned { spam: 40, ham: 40 };

    #[test]
    fn nothing_is_decided_before_enough_learning() {
        let spammy = vec![TokenCount { spam: 30, ham: 0 }; 10];
        let few = Learned { spam: 40, ham: 5 };
        assert_eq!(probability(&spammy, few), None);
        assert!(!is_spam(probability(&spammy, few)));
        assert!(is_spam(probability(&spammy, ENOUGH)));
    }

    #[test]
    fn spam_tokens_win_and_ham_tokens_protect() {
        let spammy = vec![TokenCount { spam: 30, ham: 0 }; 10];
        let hammy = vec![TokenCount { spam: 0, ham: 30 }; 10];
        assert!(probability(&spammy, ENOUGH).unwrap() > THRESHOLD);
        assert!(probability(&hammy, ENOUGH).unwrap() < 0.01);
        // Half and half: not spam.
        let mixed: Vec<TokenCount> = spammy.iter().chain(&hammy).copied().collect();
        assert!(!is_spam(probability(&mixed, ENOUGH)));
    }

    #[test]
    fn rare_or_unknown_tokens_say_nothing() {
        let rare = vec![TokenCount { spam: 2, ham: 0 }; 10];
        assert_eq!(probability(&rare, ENOUGH), Some(0.5));
        assert_eq!(probability(&[], ENOUGH), Some(0.5));
    }

    #[test]
    fn one_telling_token_is_not_enough() {
        // A single token is clamped to 0.99: never above the threshold.
        let one = vec![TokenCount { spam: 40, ham: 0 }];
        assert!(!is_spam(probability(&one, ENOUGH)));
    }

    #[test]
    fn wanted_mail_counts_double() {
        // Seen in as many spam as ham messages: leans to ham.
        let even = vec![TokenCount { spam: 10, ham: 10 }; 10];
        assert!(probability(&even, ENOUGH).unwrap() < 0.5);
    }
}
