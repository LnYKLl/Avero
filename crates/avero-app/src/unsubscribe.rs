//! "Se désinscrire": reading `List-Unsubscribe` (RFC 2369) and
//! `List-Unsubscribe-Post` (RFC 8058), choosing how to unsubscribe, and
//! the one-click HTTPS request.
//!
//! Only the unsubscribe address is contacted, and nothing of the mail is
//! sent: a one-click request carries exactly `List-Unsubscribe=One-Click`
//! (no cookie, no referrer, no proxy, never redirected to plain `http:`);
//! a mail carries only the subject and body the `mailto:` link asks for.
//!
//! A one-click request only goes to the public Internet: never to this
//! computer or the local network (a spam could otherwise make Avero post
//! to a service of the intranet), whether the address names one, a name
//! leads to one, or a redirect does.

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// The body of a one-click request (RFC 8058 §3.2).
pub const ONE_CLICK_BODY: &str = "List-Unsubscribe=One-Click";

/// How long a one-click request may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// At most this many HTTPS redirects are followed.
const MAX_REDIRECTS: usize = 5;

/// How to unsubscribe, in the order Avero prefers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// RFC 8058: a POST to this HTTPS address, nothing to open.
    OneClick(Url),
    /// A mail to `to`, sent from the account that received the list.
    Mail {
        to: String,
        subject: String,
        body: String,
    },
    /// An HTTPS page to open in the browser.
    Web(Url),
    /// A plain `http:` page: opened only after a warning that it is not
    /// encrypted.
    InsecureWeb(Url),
}

/// The addresses of a `List-Unsubscribe` header, in order.
///
/// They are written between angle brackets and separated by commas;
/// spaces inside the brackets (left by line folding) are dropped and
/// comments in parentheses outside them are skipped. A header without
/// any bracket (seen in the wild) is read as comma-separated addresses.
pub fn parse_header(value: &str) -> Vec<String> {
    let clean = |uri: &str| -> Option<String> {
        let uri: String = uri.chars().filter(|c| !c.is_whitespace()).collect();
        (!uri.is_empty()).then_some(uri)
    };
    if !value.contains('<') {
        return value.split(',').filter_map(clean).collect();
    }
    let mut uris = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        // A comment before the next bracket: skip it whole (it may hold
        // a `<`).
        if let Some(open) = rest[..start].find('(') {
            match rest[open..].find(')') {
                Some(close) => {
                    rest = &rest[open + close + 1..];
                    continue;
                }
                None => break,
            }
        }
        let after = &rest[start + 1..];
        let (uri, next) = match after.find('>') {
            Some(end) => (&after[..end], &after[end + 1..]),
            // Never closed: what is left is the address.
            None => (after, ""),
        };
        uris.extend(clean(uri));
        rest = next;
    }
    uris
}

/// Whether `List-Unsubscribe-Post` offers one-click unsubscribe.
fn offers_one_click(post: Option<&str>) -> bool {
    post.is_some_and(|value| {
        value
            .split(['&', ';'])
            .any(|part| part.trim().eq_ignore_ascii_case(ONE_CLICK_BODY))
    })
}

/// The best way to unsubscribe from what the headers offer: one-click
/// (an HTTPS address and `List-Unsubscribe-Post`), else a mail, else an
/// HTTPS page, else a plain `http:` page. `None` when nothing usable is
/// offered (other schemes, broken addresses).
pub fn choose(list_unsubscribe: &str, post: Option<&str>) -> Option<Method> {
    let uris = parse_header(list_unsubscribe);
    let web = |scheme: &str| {
        uris.iter()
            .filter_map(|uri| Url::parse(uri).ok())
            .find(|url| url.scheme() == scheme && url.host_str().is_some_and(|h| !h.is_empty()))
    };
    let https = web("https");
    if offers_one_click(post)
        && let Some(url) = &https
        && host_allowed(url)
    {
        return Some(Method::OneClick(url.clone()));
    }
    if let Some(mail) = uris.iter().find_map(|uri| parse_mailto(uri)) {
        return Some(mail);
    }
    if let Some(url) = https {
        return Some(Method::Web(url));
    }
    web("http").map(Method::InsecureWeb)
}

/// A `mailto:` address (RFC 6068) as a `Method::Mail`: the first
/// recipient, and the subject and body it asks for (others fields such as
/// `cc` are ignored). `None` when it is not a usable address.
fn parse_mailto(uri: &str) -> Option<Method> {
    let scheme = uri.get(..7)?;
    if !scheme.eq_ignore_ascii_case("mailto:") {
        return None;
    }
    let rest = &uri[7..];
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let to = percent_decode(to.split(',').next()?.trim());
    let valid = to.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    }) && !to.chars().any(|c| c.is_whitespace() || c.is_control());
    if !valid {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    for field in query.split('&') {
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        if name.eq_ignore_ascii_case("subject") {
            // One line: no header can be slipped in.
            let text = percent_decode(value);
            let lines: Vec<&str> = text.split(['\r', '\n']).filter(|l| !l.is_empty()).collect();
            subject = Some(lines.join(" "));
        } else if name.eq_ignore_ascii_case("body") {
            body = Some(percent_decode(value));
        }
    }
    let subject = subject
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unsubscribe".to_string());
    Some(Method::Mail {
        to,
        subject,
        body: body.unwrap_or_else(|| "Unsubscribe".to_string()),
    })
}

/// Decodes `%XX` escapes (UTF-8); a broken escape is kept as it is. A
/// `+` stays a `+` (it is not a space in a `mailto:` link).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a redirect of a one-click request to `next` is followed:
/// HTTPS only, not to a local address, and at most [`MAX_REDIRECTS`] of
/// them (`followed` so far).
pub fn redirect_allowed(next: &Url, followed: usize) -> bool {
    next.scheme() == "https" && host_allowed(next) && followed < MAX_REDIRECTS
}

/// Whether an address is on the public Internet: not this computer
/// (loopback), nor a private, shared (CGNAT), link-local or unique local
/// network, nor a special one (unspecified, multicast, broadcast,
/// documentation, benchmarking, reserved). An IPv4 address written as
/// IPv6 (mapped, or NAT64) is judged as IPv4.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = s[6].to_be_bytes();
                let [c, d] = s[7].to_be_bytes();
                return is_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7: unique local.
                || (s[0] & 0xfe00) == 0xfc00
                // fe80::/10: link-local; fec0::/10: old site-local.
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                // 2001:db8::/32: documentation.
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                // ::/96 (IPv4-compatible, deprecated).
                || s[..6] == [0; 6])
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0.0.0.0/8 ("this network"), 100.64.0.0/10 (shared, CGNAT),
        // 192.0.0.0/24 (protocol assignments), 198.18.0.0/15
        // (benchmarking), 240.0.0.0/4 (reserved).
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

/// Whether the host of `url` may be contacted, as far as it can tell
/// without the DNS: an address must be public, and `localhost` is
/// refused. A name is checked when it is resolved (see `PublicOnly`).
fn host_allowed(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // An IPv6 address is written between brackets.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_public(ip);
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    !name.is_empty() && name != "localhost" && !name.ends_with(".localhost")
}

/// Resolves names like the system, keeping only the public addresses
/// (see `is_public`); a name that has none is refused. reqwest connects
/// only to the addresses it gives, at the first request and after each
/// redirect, so a name cannot lead elsewhere between the check and the
/// connection.
struct PublicOnly;

/// The error of a name that only leads to the local network.
const LOCAL_ADDRESS: &str = "l'adresse mène au réseau local";

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let found = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public: Vec<SocketAddr> = found.filter(|addr| is_public(addr.ip())).collect();
            if public.is_empty() {
                return Err(LOCAL_ADDRESS.into());
            }
            let addrs: Addrs = Box::new(public.into_iter());
            Ok(addrs)
        })
    }
}

/// The HTTP client of one-click requests: no cookie store (reqwest keeps
/// none unless asked), no `Referer`, no proxy (not even the system's),
/// public addresses only, HTTPS-only redirects.
fn client() -> Result<reqwest::Client, String> {
    // Same crypto backend as the rest of the app (see avero-oauth).
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .referer(false)
        .no_proxy()
        .dns_resolver(Arc::new(PublicOnly))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if redirect_allowed(attempt.url(), attempt.previous().len()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| e.without_url().to_string())
}

/// Sends the one-click request (RFC 8058) to an HTTPS address on the
/// public Internet.
pub async fn one_click(url: Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Adresse de désinscription non chiffrée".to_string());
    }
    if !host_allowed(&url) {
        return Err(refused_local());
    }
    // Asked here too, for a clear message (the client checks again at
    // the connection).
    if let Some(host) = url.host_str()
        && let Ok(name) = host.parse::<Name>()
        && PublicOnly.resolve(name).await.is_err()
    {
        return Err(refused_local());
    }
    post(&client()?, url).await
}

/// Why a one-click request to the local network was not sent.
fn refused_local() -> String {
    "Adresse de désinscription refusée : elle mène à cet ordinateur ou au réseau local".to_string()
}

/// The POST itself, on any client (tests use plain HTTP on 127.0.0.1).
async fn post(client: &reqwest::Client, url: Url) -> Result<(), String> {
    let response = client
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(ONE_CLICK_BODY)
        .send()
        .await
        .map_err(|e| {
            format!(
                "Le site de désinscription ne répond pas ({})",
                e.without_url()
            )
        })?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else if status.is_redirection() {
        Err("Le site de désinscription redirige vers une adresse non chiffrée".to_string())
    } else {
        Err(format!(
            "Le site de désinscription a refusé la demande (erreur {})",
            status.as_u16()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn header_addresses_are_read_in_order() {
        assert_eq!(
            parse_header("<mailto:u@list.example?subject=stop>, <https://list.example/u?id=1>"),
            vec![
                "mailto:u@list.example?subject=stop",
                "https://list.example/u?id=1"
            ]
        );
        // Folding left spaces inside the brackets; comments are skipped.
        assert_eq!(
            parse_header(
                " (lien <ancien>) <https://list.example/\r\n  unsub?a=1&b=2> ,\t(mail) <mailto:x@y.z>"
            ),
            vec!["https://list.example/unsub?a=1&b=2", "mailto:x@y.z"]
        );
        // No brackets at all, an unclosed one, empty ones.
        assert_eq!(
            parse_header("https://a.example/u, mailto:b@c.d"),
            vec!["https://a.example/u", "mailto:b@c.d"]
        );
        assert_eq!(
            parse_header("<https://a.example/u>, <mailto:b@c.d"),
            vec!["https://a.example/u", "mailto:b@c.d"]
        );
        assert!(parse_header("<>, < >").is_empty());
        assert!(parse_header("").is_empty());
        // A comment that never ends hides the rest.
        assert!(parse_header("(oops <https://a.example>").is_empty());
    }

    #[test]
    fn one_click_needs_https_and_the_post_header() {
        let header = "<mailto:u@list.example>, <https://list.example/u>";
        assert_eq!(
            choose(header, Some("List-Unsubscribe=One-Click")),
            Some(Method::OneClick(url("https://list.example/u")))
        );
        assert_eq!(
            choose(header, Some(" list-unsubscribe=one-click ")),
            Some(Method::OneClick(url("https://list.example/u")))
        );
        // Without the header, or with something else, the mail wins.
        for post in [None, Some("List-Unsubscribe=Later")] {
            assert!(
                matches!(choose(header, post), Some(Method::Mail { .. })),
                "{post:?}"
            );
        }
        // One-click never goes over plain HTTP.
        assert_eq!(
            choose(
                "<http://list.example/u>",
                Some("List-Unsubscribe=One-Click")
            ),
            Some(Method::InsecureWeb(url("http://list.example/u")))
        );
    }

    #[test]
    fn mail_then_https_then_http() {
        assert_eq!(
            choose("<https://a.example/u>, <mailto:stop@a.example>", None),
            Some(Method::Mail {
                to: "stop@a.example".into(),
                subject: "Unsubscribe".into(),
                body: "Unsubscribe".into(),
            })
        );
        assert_eq!(
            choose("<http://a.example/u>, <https://a.example/s>", None),
            Some(Method::Web(url("https://a.example/s")))
        );
        assert_eq!(
            choose("<http://a.example/u>", None),
            Some(Method::InsecureWeb(url("http://a.example/u")))
        );
        // Nothing usable.
        for header in [
            "<ftp://a.example/u>",
            "<mailto:>",
            "<https://>",
            "<javascript:alert(1)>",
        ] {
            assert_eq!(choose(header, None), None, "{header}");
        }
    }

    #[test]
    fn mailto_subject_and_body_are_decoded() {
        let method = choose(
            "<MAILTO:liste%2Bstop@a.example,autre@a.example?Subject=D%C3%A9sinscription%0D%0ABcc:%20x@evil&body=stop+now%20please&cc=boss@a.example>",
            None,
        );
        assert_eq!(
            method,
            Some(Method::Mail {
                to: "liste+stop@a.example".into(),
                subject: "Désinscription Bcc: x@evil".into(),
                body: "stop+now please".into(),
            })
        );
        // A broken escape is kept, an address with spaces is refused.
        assert_eq!(percent_decode("100%zz%4"), "100%zz%4");
        assert_eq!(choose("<mailto:a%20b@c.d>", None), None);
        assert_eq!(choose("<mailto:a@b@c.d>", None), None);
    }

    #[test]
    fn redirects_stay_on_https() {
        assert!(redirect_allowed(&url("https://a.example/done"), 0));
        assert!(!redirect_allowed(&url("http://a.example/done"), 0));
        assert!(!redirect_allowed(
            &url("https://a.example/done"),
            MAX_REDIRECTS
        ));
    }

    #[tokio::test]
    async fn one_click_refuses_plain_http() {
        let result = one_click(url("http://127.0.0.1:9/u")).await;
        assert!(result.is_err());
    }

    #[test]
    fn only_public_addresses_are_contacted() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for public in [
            "93.184.216.34",
            "1.1.1.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd12:3456::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(ip(local)), "{local}");
        }
        assert!(host_allowed(&url("https://list.example/u")));
        for local in [
            "https://127.0.0.1/u",
            "https://[::1]/u",
            "https://10.0.0.8/admin",
            "https://localhost/u",
            "https://app.localhost./u",
        ] {
            assert!(!host_allowed(&url(local)), "{local}");
            assert!(!redirect_allowed(&url(local), 0), "{local}");
        }
    }

    #[test]
    fn one_click_is_not_offered_for_a_local_address() {
        let post = Some("List-Unsubscribe=One-Click");
        assert_eq!(
            choose("<https://192.168.1.10/u>, <mailto:u@list.example>", post),
            Some(Method::Mail {
                to: "u@list.example".into(),
                subject: "Unsubscribe".into(),
                body: "Unsubscribe".into(),
            })
        );
    }

    #[tokio::test]
    async fn a_name_that_leads_to_this_computer_is_refused() {
        // `localhost` resolves without any network.
        let name: Name = "localhost".parse().unwrap();
        assert!(PublicOnly.resolve(name).await.is_err());
        let refused = one_click(url("https://localhost:9/u")).await.unwrap_err();
        assert!(refused.contains("réseau local"), "{refused}");
        // The client resolves the same way (also after a redirect): a
        // server listening on this computer gets nothing.
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let (served, request) = serve_once(ok.into()).await;
        let mut by_name = served.clone();
        by_name.set_host(Some("localhost")).unwrap();
        let error = post(&client().unwrap(), by_name).await.unwrap_err();
        assert!(error.contains("ne répond pas"), "{error}");
        // The server is still waiting: reach it directly to end it.
        post(&reqwest::Client::new(), served).await.unwrap();
        assert!(request.await.unwrap().starts_with("POST "));
    }

    /// A one-request HTTP server on 127.0.0.1: answers `reply` and hands
    /// back the request it read.
    async fn serve_once(reply: String) -> (Url, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = reply.replace("{port}", &port.to_string());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            // Headers, then the announced body.
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| {
                            let (name, value) = l.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length || read == 0 {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
            String::from_utf8_lossy(&request).to_string()
        });
        (url(&format!("http://127.0.0.1:{port}/unsub?id=42")), handle)
    }

    #[tokio::test]
    async fn one_click_posts_exactly_the_rfc_body() {
        let (address, server) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into()).await;
        post(&client().unwrap(), address).await.unwrap();
        let request = server.await.unwrap();
        let lower = request.to_lowercase();
        assert!(
            request.starts_with("POST /unsub?id=42 HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.ends_with("\r\n\r\nList-Unsubscribe=One-Click"),
            "{request}"
        );
        assert!(lower.contains("content-type: application/x-www-form-urlencoded"));
        assert!(!lower.contains("cookie"), "{request}");
        assert!(!lower.contains("referer"), "{request}");
    }

    #[tokio::test]
    async fn redirect_to_plain_http_is_not_followed() {
        let (address, server) = serve_once(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{port}/elsewhere\r\nContent-Length: 0\r\n\r\n"
                .into(),
        )
        .await;
        let result = post(&client().unwrap(), address).await;
        assert!(result.unwrap_err().contains("non chiffrée"));
        // Only one request reached the server.
        server.await.unwrap();
    }

    #[tokio::test]
    async fn refused_request_says_so() {
        let (address, server) =
            serve_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into()).await;
        let error = post(&client().unwrap(), address).await.unwrap_err();
        assert!(error.contains("404"), "{error}");
        server.await.unwrap();
    }
}
