//! Parser for Mozilla autoconfig `config-v1.1.xml` documents.

use roxmltree::{Document, Node};

use crate::{AuthMethod, OAuthProvider, ProviderConfig, Security, Server, UsernameKind};

fn text<'a>(node: Node<'a, '_>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn parse_server(node: Node, domain: &str) -> Option<(Server, Vec<AuthMethod>)> {
    let host = text(node, "hostname")?
        .replace("%EMAILDOMAIN%", domain)
        .to_ascii_lowercase();
    let port: u16 = text(node, "port")?.parse().ok()?;
    let security = match text(node, "socketType")?.to_ascii_uppercase().as_str() {
        "SSL" => Security::Tls,
        "STARTTLS" => Security::StartTls,
        _ => return None,
    };
    let username = match text(node, "username")? {
        "%EMAILADDRESS%" => UsernameKind::Email,
        "%EMAILLOCALPART%" => UsernameKind::LocalPart,
        _ => return None,
    };

    let mut auth = Vec::new();
    for n in node.children().filter(|n| n.has_tag_name("authentication")) {
        let method = match n.text().map(|t| t.trim().to_ascii_lowercase()).as_deref() {
            Some("oauth2") => oauth_for_host(&host).map(AuthMethod::OAuth),
            Some("password-cleartext" | "password-encrypted" | "plain") => {
                Some(AuthMethod::Password)
            }
            _ => None,
        };
        if let Some(m) = method
            && !auth.contains(&m)
        {
            auth.push(m);
        }
    }
    // OAuth first, then password.
    auth.sort_by_key(|m| matches!(m, AuthMethod::Password));
    Some((
        Server {
            host,
            port,
            security,
            username,
        },
        auth,
    ))
}

fn oauth_for_host(host: &str) -> Option<OAuthProvider> {
    let ends = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));
    if ends("gmail.com") || ends("googlemail.com") {
        Some(OAuthProvider::Google)
    } else if ends("office365.com") || ends("outlook.com") {
        Some(OAuthProvider::Microsoft)
    } else {
        None
    }
}

/// Parse an autoconfig document. Returns `None` when no acceptable
/// IMAP (implicit TLS) and SMTP (TLS or STARTTLS) pair is found.
pub fn parse_autoconfig(xml: &str, domain: &str) -> Option<ProviderConfig> {
    let doc = Document::parse(xml).ok()?;
    let root = doc.root_element();
    if !root.has_tag_name("clientConfig") {
        return None;
    }
    let domain = domain.trim().to_ascii_lowercase();

    for provider in root.children().filter(|n| n.has_tag_name("emailProvider")) {
        let servers = |kind: &str, tag: &str| -> Vec<(Server, Vec<AuthMethod>)> {
            provider
                .children()
                .filter(|n| n.has_tag_name(tag) && n.attribute("type") == Some(kind))
                .filter_map(|n| parse_server(n, &domain))
                .collect()
        };
        let imap = servers("imap", "incomingServer")
            .into_iter()
            .find(|(s, _)| s.security == Security::Tls);
        let mut smtps = servers("smtp", "outgoingServer");
        let smtp = match smtps.iter().position(|(s, _)| s.security == Security::Tls) {
            Some(i) => Some(smtps.swap_remove(i)),
            None => smtps.into_iter().next(),
        };
        let (Some((imap, mut auth)), Some((smtp, _))) = (imap, smtp) else {
            continue;
        };
        if auth.is_empty() {
            auth.push(AuthMethod::Password);
        }
        let display_name = text(provider, "displayName")
            .map(str::to_string)
            .unwrap_or_else(|| domain.clone());
        return Some(ProviderConfig {
            display_name,
            imap,
            smtp,
            auth,
            password_help: None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(imap_socket: &str, imap_auth: &str, imap_host: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.org">
    <displayName>Example Mail</displayName>
    <incomingServer type="imap">
      <hostname>{imap_host}</hostname><port>993</port>
      <socketType>{imap_socket}</socketType>
      <username>%EMAILLOCALPART%</username>
      <authentication>{imap_auth}</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.org</hostname><port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#
        )
    }

    #[test]
    fn ssl_imap_starttls_smtp() {
        let c = parse_autoconfig(
            &xml("SSL", "password-cleartext", "imap.example.org"),
            "example.org",
        )
        .unwrap();
        assert_eq!(c.display_name, "Example Mail");
        assert_eq!(c.imap.security, Security::Tls);
        assert_eq!(c.imap.username, UsernameKind::LocalPart);
        assert_eq!(c.smtp.security, Security::StartTls);
        assert_eq!(c.smtp.port, 587);
        assert_eq!(c.smtp.username, UsernameKind::Email);
        assert_eq!(c.auth, vec![AuthMethod::Password]);
    }

    #[test]
    fn starttls_only_imap_rejected() {
        assert!(
            parse_autoconfig(
                &xml("STARTTLS", "password-cleartext", "imap.example.org"),
                "example.org"
            )
            .is_none()
        );
    }

    #[test]
    fn gmail_oauth() {
        let c = parse_autoconfig(&xml("SSL", "OAuth2", "imap.gmail.com"), "example.org").unwrap();
        assert_eq!(c.auth, vec![AuthMethod::OAuth(OAuthProvider::Google)]);
    }

    #[test]
    fn garbage_and_plain_smtp() {
        assert!(parse_autoconfig("not xml", "x.org").is_none());
        let plain =
            xml("SSL", "password-cleartext", "imap.example.org").replace("STARTTLS", "plain");
        assert!(parse_autoconfig(&plain, "example.org").is_none());
    }
}
