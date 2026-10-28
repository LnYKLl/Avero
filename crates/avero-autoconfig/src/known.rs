//! Built-in provider table (no network).
//!
//! Proton Mail is deliberately absent: it only works through the local
//! Bridge, which speaks IMAP with STARTTLS on localhost and therefore
//! violates the "IMAP is implicit TLS only" rule.

use crate::{
    AuthMethod, OAuthProvider, PasswordHelp, ProviderConfig, Security, Server, UsernameKind,
    domain_of,
};

fn server(host: &str, port: u16, security: Security) -> Server {
    Server {
        host: host.to_string(),
        port,
        security,
        username: UsernameKind::Email,
    }
}

fn help(text: &str, url: &str) -> Option<PasswordHelp> {
    Some(PasswordHelp {
        text: text.to_string(),
        url: url.to_string(),
    })
}

fn simple(
    name: &str,
    imap: &str,
    smtp: &str,
    password_help: Option<PasswordHelp>,
) -> ProviderConfig {
    ProviderConfig {
        display_name: name.to_string(),
        imap: server(imap, 993, Security::Tls),
        smtp: server(smtp, 465, Security::Tls),
        auth: vec![AuthMethod::Password],
        password_help,
    }
}

const APP_PASSWORD: &str = "Ce service demande un mot de passe d'application, différent de votre mot de passe habituel. Créez-en un dans les paramètres de sécurité de votre compte, puis collez-le ici.";

/// Look the address's domain up in the built-in table.
pub fn known(email: &str) -> Option<ProviderConfig> {
    let domain = domain_of(email)?;
    let config = match domain.as_str() {
        "gmail.com" | "googlemail.com" => ProviderConfig {
            auth: vec![
                AuthMethod::OAuth(OAuthProvider::Google),
                AuthMethod::Password,
            ],
            ..simple(
                "Gmail",
                "imap.gmail.com",
                "smtp.gmail.com",
                help(
                    "Activez la validation en deux étapes sur votre compte Google, puis créez un mot de passe d'application (16 lettres, à saisir sans espaces).",
                    "https://myaccount.google.com/apppasswords",
                ),
            )
        },
        "outlook.com" | "outlook.fr" | "hotmail.com" | "hotmail.fr" | "live.com" | "live.fr"
        | "msn.com" => ProviderConfig {
            display_name: "Outlook.com".to_string(),
            imap: server("outlook.office365.com", 993, Security::Tls),
            smtp: server("smtp-mail.outlook.com", 587, Security::StartTls),
            auth: vec![AuthMethod::OAuth(OAuthProvider::Microsoft)],
            password_help: None,
        },
        "yahoo.com" | "yahoo.fr" | "ymail.com" => simple(
            "Yahoo",
            "imap.mail.yahoo.com",
            "smtp.mail.yahoo.com",
            help(
                "Yahoo exige un mot de passe d'application : générez-en un dans la sécurité du compte, puis saisissez-le ici.",
                "https://login.yahoo.com/account/security",
            ),
        ),
        "icloud.com" | "me.com" | "mac.com" => ProviderConfig {
            display_name: "iCloud".to_string(),
            imap: server("imap.mail.me.com", 993, Security::Tls),
            smtp: server("smtp.mail.me.com", 587, Security::StartTls),
            auth: vec![AuthMethod::Password],
            password_help: help(
                "iCloud exige un mot de passe spécifique à l'app : créez-en un dans votre compte Apple (section Connexion et sécurité), puis saisissez-le ici.",
                "https://account.apple.com",
            ),
        },
        "aol.com" | "aol.fr" => simple(
            "AOL",
            "imap.aol.com",
            "smtp.aol.com",
            help(APP_PASSWORD, "https://login.aol.com/account/security"),
        ),
        "gmx.com" | "gmx.fr" | "gmx.de" | "gmx.net" => {
            simple("GMX", "imap.gmx.com", "mail.gmx.com", None)
        }
        "orange.fr" | "wanadoo.fr" => simple("Orange", "imap.orange.fr", "smtp.orange.fr", None),
        "free.fr" => simple("Free", "imap.free.fr", "smtp.free.fr", None),
        "sfr.fr" | "neuf.fr" => simple("SFR", "imap.sfr.fr", "smtp.sfr.fr", None),
        "laposte.net" => simple("La Poste", "imap.laposte.net", "smtp.laposte.net", None),
        "fastmail.com" | "fastmail.fm" => simple(
            "Fastmail",
            "imap.fastmail.com",
            "smtp.fastmail.com",
            help(
                "Fastmail demande un mot de passe d'application : créez-en un dans Réglages, Confidentialité et sécurité, Mots de passe d'application.",
                "https://app.fastmail.com/settings/security/devicekeys",
            ),
        ),
        "zoho.com" | "zohomail.com" => simple("Zoho", "imap.zoho.com", "smtp.zoho.com", None),
        "yandex.com" | "yandex.ru" => simple("Yandex", "imap.yandex.com", "smtp.yandex.com", None),
        "mail.ru" | "inbox.ru" | "list.ru" | "bk.ru" => {
            simple("Mail.ru", "imap.mail.ru", "smtp.mail.ru", None)
        }
        _ => return None,
    };
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail() {
        let c = known("Me@GoogleMail.com").unwrap();
        assert_eq!(c.display_name, "Gmail");
        assert_eq!(c.imap.host, "imap.gmail.com");
        assert_eq!(c.smtp.port, 465);
        assert_eq!(
            c.auth,
            vec![
                AuthMethod::OAuth(OAuthProvider::Google),
                AuthMethod::Password
            ]
        );
        assert_eq!(
            c.password_help.unwrap().url,
            "https://myaccount.google.com/apppasswords"
        );
    }

    #[test]
    fn outlook() {
        let c = known("a@hotmail.fr").unwrap();
        assert_eq!(c.smtp.port, 587);
        assert_eq!(c.smtp.security, Security::StartTls);
        assert_eq!(c.auth, vec![AuthMethod::OAuth(OAuthProvider::Microsoft)]);
    }

    #[test]
    fn orange() {
        let c = known("a@wanadoo.fr").unwrap();
        assert_eq!(c.imap.host, "imap.orange.fr");
        assert_eq!(c.smtp.host, "smtp.orange.fr");
        assert_eq!(c.auth, vec![AuthMethod::Password]);
    }

    #[test]
    fn unknown_and_invalid() {
        assert!(known("a@example.org").is_none());
        assert!(known("garbage").is_none());
    }

    #[test]
    fn imap_always_implicit_tls() {
        for d in [
            "gmail.com",
            "outlook.com",
            "yahoo.fr",
            "icloud.com",
            "aol.com",
            "gmx.fr",
            "orange.fr",
            "free.fr",
            "sfr.fr",
            "laposte.net",
            "fastmail.fm",
            "zoho.com",
            "yandex.com",
            "mail.ru",
        ] {
            let c = known(&format!("a@{d}")).unwrap();
            assert_eq!(c.imap.security, Security::Tls, "{d}");
            assert_eq!(c.imap.port, 993, "{d}");
        }
    }
}
