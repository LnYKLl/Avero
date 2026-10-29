//! What a link of a message really points to, checked before it opens:
//! the site it leads to, and what looks wrong about it (a text that
//! shows another site, a numeric address, a name that imitates another
//! one…). Nothing is fetched: only the address is read.

use reqwest::Url;
use std::net::IpAddr;

/// Longest address shown in full in the dialog.
const SHOWN_URL: usize = 300;

/// Two-level endings under which a site is one label further left
/// (`exemple.co.uk`, not `co.uk`). Not the whole public suffix list:
/// enough for the comparison of a link's text with its target.
const TWO_LEVEL: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "com.au", "net.au", "org.au", "co.nz", "co.jp", "co.kr",
    "co.in", "com.br", "com.mx", "com.cn", "com.tr", "co.za", "gouv.fr", "asso.fr", "com.fr",
    "gc.ca", "qc.ca",
];

/// Something that looks wrong about a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The text of the link shows another site than the one it leads to.
    Mismatch { shown: String },
    /// `https://banque.fr@autre.site/`: what comes before `@` is not the
    /// site.
    UserInfo,
    /// A numeric address instead of a site name.
    Numeric,
    /// A name with letters of another alphabet, which can imitate a
    /// known site (`xn--` in its ASCII form).
    Lookalike,
    /// Plain http: what is typed on the page travels unencrypted.
    Unencrypted,
}

impl Warning {
    /// What the dialog says about it.
    pub fn describe(&self, host: &str) -> String {
        match self {
            Warning::Mismatch { shown } => format!(
                "Le texte du lien affiche « {shown} », mais il mène en réalité vers « {host} »."
            ),
            Warning::UserInfo => format!(
                "L'adresse contient un « @ » : ce qui le précède n'est pas le site. \
                 Le vrai site est « {host} »."
            ),
            Warning::Numeric => {
                "Le lien mène vers une adresse numérique au lieu d'un nom de site.".to_string()
            }
            Warning::Lookalike => "Le nom du site contient des lettres d'un autre alphabet, \
                 qui peuvent imiter un site connu."
                .to_string(),
            Warning::Unencrypted => "La connexion à ce site n'est pas chiffrée (http) : \
                 n'y saisissez aucun mot de passe."
                .to_string(),
        }
    }
}

/// A link clicked in a message, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The address that opens.
    pub url: String,
    /// The site, as the browser will reach it (ASCII form).
    pub host: String,
    pub warnings: Vec<Warning>,
}

impl Checked {
    /// The address for the dialog, cut when it is very long (trackers).
    pub fn shown_url(&self) -> String {
        match self.url.char_indices().nth(SHOWN_URL) {
            Some((cut, _)) => format!("{}…", &self.url[..cut]),
            None => self.url.clone(),
        }
    }
}

/// Checks `url`, whose text in the message is `label`. `None` when it is
/// not a web address that a browser can open.
pub fn check(url: &str, label: &str) -> Option<Checked> {
    let parsed = Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let mut warnings = Vec::new();
    if let Some(shown) = shown_host(label)
        && site(&shown) != site(&host)
    {
        warnings.push(Warning::Mismatch { shown });
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        warnings.push(Warning::UserInfo);
    }
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        warnings.push(Warning::Numeric);
    } else if host.split('.').any(|label| label.starts_with("xn--")) {
        warnings.push(Warning::Lookalike);
    }
    if parsed.scheme() == "http" {
        warnings.push(Warning::Unencrypted);
    }
    Some(Checked {
        url: parsed.to_string(),
        host,
        warnings,
    })
}

/// The site a link's text shows, when the text looks like an address
/// (`www.banque.fr`, `https://banque.fr/compte`), not like words.
fn shown_host(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty() || label.contains(char::is_whitespace) {
        return None;
    }
    let with_scheme = if label.contains("://") {
        label.to_string()
    } else {
        format!("https://{label}")
    };
    let parsed = Url::parse(&with_scheme).ok()?;
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // "Cliquez.ici" is not an address: a site ends with letters.
    let ending = host.rsplit('.').next()?;
    let looks_like_a_site =
        host.contains('.') && ending.len() >= 2 && ending.chars().all(|c| c.is_ascii_alphabetic());
    looks_like_a_site.then_some(host)
}

/// The site of a host: its last two labels, three under a two-level
/// ending. `www.banque.fr` and `client.banque.fr` are the same site.
fn site(host: &str) -> &str {
    let labels: Vec<&str> = host.split('.').collect();
    let keep = if labels.len() >= 3
        && TWO_LEVEL.contains(&labels[labels.len() - 2..].join(".").as_str())
    {
        3
    } else {
        2
    };
    if labels.len() <= keep {
        return host;
    }
    let skipped: usize = labels[..labels.len() - keep]
        .iter()
        .map(|l| l.len() + 1)
        .sum();
    &host[skipped..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warnings(url: &str, label: &str) -> Vec<Warning> {
        check(url, label).unwrap().warnings
    }

    #[test]
    fn a_plain_link_has_no_warning() {
        let checked = check("https://www.example.org/news?id=3", "Lire la suite").unwrap();
        assert_eq!(checked.host, "www.example.org");
        assert!(checked.warnings.is_empty());
        // Its own address as text, or the same site under another name.
        assert!(warnings("https://example.org/a", "https://example.org/a").is_empty());
        assert!(warnings("https://client.banque.fr/", "www.banque.fr").is_empty());
        assert!(warnings("https://shop.example.co.uk/", "example.co.uk").is_empty());
    }

    #[test]
    fn a_text_showing_another_site_is_flagged() {
        assert_eq!(
            warnings("https://evil.example/login", "www.banque.fr"),
            vec![Warning::Mismatch {
                shown: "www.banque.fr".into()
            }]
        );
        assert_eq!(
            warnings("https://a.example.co.uk/", "https://b.example2.co.uk/x").len(),
            1
        );
        // Words, a file name or a version number are not addresses.
        assert!(warnings("https://evil.example/", "Mon compte").is_empty());
        assert!(warnings("https://evil.example/", "Version 2.0").is_empty());
        assert!(warnings("https://evil.example/", "1.5").is_empty());
    }

    #[test]
    fn tricks_in_the_address_are_flagged() {
        let checked = check("https://banque.fr@evil.example/", "Banque").unwrap();
        assert_eq!(checked.host, "evil.example");
        assert_eq!(checked.warnings, vec![Warning::UserInfo]);
        assert_eq!(
            warnings("https://192.168.1.4/x", "Lien"),
            vec![Warning::Numeric]
        );
        assert_eq!(warnings("https://[::1]/x", "Lien"), vec![Warning::Numeric]);
        // A Cyrillic "а" in place of the Latin one.
        let lookalike = check("https://pаypal.com/", "PayPal").unwrap();
        assert!(lookalike.host.starts_with("xn--"), "{}", lookalike.host);
        assert_eq!(lookalike.warnings, vec![Warning::Lookalike]);
        assert_eq!(
            warnings("http://example.org/", "Lien"),
            vec![Warning::Unencrypted]
        );
    }

    #[test]
    fn only_web_addresses_can_open() {
        assert!(check("javascript:alert(1)", "x").is_none());
        assert!(check("file:///C:/Windows", "x").is_none());
        assert!(check("pas une adresse", "x").is_none());
    }

    #[test]
    fn a_very_long_address_is_cut_for_the_dialog() {
        let long = format!("https://example.org/{}", "a".repeat(1000));
        let shown = check(&long, "x").unwrap().shown_url();
        assert_eq!(shown.chars().count(), SHOWN_URL + 1);
        assert!(shown.ends_with('…'));
    }
}
