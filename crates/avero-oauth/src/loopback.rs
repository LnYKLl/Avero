//! Loopback redirect receiver (RFC 8252 §7.3).
//!
//! The browser is sent back to `http://127.0.0.1:{port}/?code=..&state=..`.
//! We answer with a small French HTML page and hand the code to the caller.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::OAuthError;

/// Outcome of parsing one HTTP request line received on the loopback port.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RedirectOutcome {
    /// The authorization code (state verified).
    Code(String),
    /// Not the redirect (other path, other method, garbage): answer 404 and
    /// keep waiting.
    Ignore,
    /// The redirect carried an error, a wrong state or no code.
    Failed(OAuthError),
}

/// Parses a request line such as `GET /?code=abc&state=xyz HTTP/1.1`.
pub(crate) fn parse_redirect(request_line: &str, expected_state: &str) -> RedirectOutcome {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return RedirectOutcome::Ignore;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return RedirectOutcome::Ignore;
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        return RedirectOutcome::Failed(if error == "access_denied" {
            OAuthError::Cancelled
        } else {
            // Only the error code: error_description is free text from the
            // server and may echo request details.
            OAuthError::Server(sanitize_code(&error))
        });
    }
    if code.is_none() && state.is_none() {
        // A bare `GET /` (e.g. the user reloading the page) is not the redirect.
        return RedirectOutcome::Ignore;
    }
    if state.as_deref() != Some(expected_state) {
        return RedirectOutcome::Failed(OAuthError::InvalidResponse(
            "state mismatch in redirect".into(),
        ));
    }
    match code {
        Some(code) if !code.is_empty() => RedirectOutcome::Code(code),
        _ => RedirectOutcome::Failed(OAuthError::InvalidResponse(
            "no authorization code in redirect".into(),
        )),
    }
}

/// Keeps only characters expected in an OAuth error code.
pub(crate) fn sanitize_code(error: &str) -> String {
    error
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .take(64)
        .collect()
}

/// Maximum size of a request head we are willing to read.
const MAX_REQUEST: usize = 16 * 1024;
/// How long one connection may take to send its request line.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Accepts connections on `listener` until the redirect arrives, then stops
/// listening. Each connection is served in its own task so that idle
/// speculative connections opened by browsers cannot block the real one.
pub(crate) async fn wait_for_code(
    listener: TcpListener,
    state: &str,
    timeout: Duration,
) -> Result<String, OAuthError> {
    let (tx, mut rx) = mpsc::channel::<Result<String, OAuthError>>(4);
    let mut tasks = JoinSet::new();

    let wait = async {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let tx = tx.clone();
                    let state = state.to_owned();
                    tasks.spawn(async move {
                        if let Some(result) = serve(stream, &state).await {
                            let _ = tx.send(result).await;
                        }
                    });
                }
                Some(result) = rx.recv() => return result,
            }
        }
    };

    let result = tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or(Err(OAuthError::Timeout));
    // Dropping the listener and the JoinSet stops listening and aborts any
    // connection still being served.
    drop(listener);
    tasks.shutdown().await;
    result
}

/// Serves one connection. Returns `None` when it was not the redirect.
async fn serve(mut stream: TcpStream, state: &str) -> Option<Result<String, OAuthError>> {
    let line = tokio::time::timeout(READ_TIMEOUT, read_request_line(&mut stream))
        .await
        .ok()??;
    match parse_redirect(&line, state) {
        RedirectOutcome::Ignore => {
            respond(&mut stream, "404 Not Found", "").await;
            None
        }
        RedirectOutcome::Code(code) => {
            respond(&mut stream, "200 OK", &page(true)).await;
            Some(Ok(code))
        }
        RedirectOutcome::Failed(err) => {
            respond(&mut stream, "200 OK", &page(false)).await;
            Some(Err(err))
        }
    }
}

/// Reads up to the end of the first line of the request.
async fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.contains(&b'\n') || buf.len() >= MAX_REQUEST {
            break;
        }
    }
    let end = buf.iter().position(|&b| b == b'\n')?;
    let line = String::from_utf8_lossy(&buf[..end]);
    Some(line.trim_end_matches('\r').to_owned())
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let content_type = if body.is_empty() {
        "text/plain; charset=utf-8"
    } else {
        "text/html; charset=utf-8"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Self-contained result page shown in the browser tab.
fn page(success: bool) -> String {
    let (title, message) = if success {
        (
            "Avero est connecté",
            "Avero est connecté. Vous pouvez fermer cet onglet et revenir à l'application.",
        )
    } else {
        (
            "Connexion échouée",
            "La connexion n'a pas abouti. Vous pouvez fermer cet onglet et réessayer depuis Avero.",
        )
    };
    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><style>\
         body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         font-family:system-ui,-apple-system,'Segoe UI',sans-serif;background:#F7F7FB;color:#1F1D2B}}\
         main{{max-width:28rem;margin:1rem;padding:2rem;background:#fff;border-radius:16px;\
         border-top:4px solid #5B4FE8;box-shadow:0 4px 24px rgba(31,29,43,.08)}}\
         h1{{margin:0 0 .75rem;font-size:1.25rem;color:#5B4FE8}}p{{margin:0;line-height:1.5}}\
         </style></head><body><main><h1>{title}</h1><p>{message}</p></main></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "s-123";

    #[test]
    fn parses_code_and_decodes_percent_escapes() {
        assert_eq!(
            parse_redirect("GET /?code=4%2F0Ab%2Bc&state=s-123 HTTP/1.1", STATE),
            RedirectOutcome::Code("4/0Ab+c".into())
        );
        assert_eq!(
            parse_redirect(
                "GET /?state=s-123&code=abc&scope=email%20openid HTTP/1.1",
                STATE
            ),
            RedirectOutcome::Code("abc".into())
        );
    }

    #[test]
    fn wrong_or_missing_state_fails() {
        assert!(matches!(
            parse_redirect("GET /?code=abc&state=other HTTP/1.1", STATE),
            RedirectOutcome::Failed(OAuthError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_redirect("GET /?code=abc HTTP/1.1", STATE),
            RedirectOutcome::Failed(OAuthError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_redirect("GET /?state=s-123 HTTP/1.1", STATE),
            RedirectOutcome::Failed(OAuthError::InvalidResponse(_))
        ));
    }

    #[test]
    fn errors_are_mapped() {
        assert_eq!(
            parse_redirect("GET /?error=access_denied&state=s-123 HTTP/1.1", STATE),
            RedirectOutcome::Failed(OAuthError::Cancelled)
        );
        assert_eq!(
            parse_redirect(
                "GET /?error=invalid_scope&error_description=bad%20thing HTTP/1.1",
                STATE
            ),
            RedirectOutcome::Failed(OAuthError::Server("invalid_scope".into()))
        );
    }

    #[test]
    fn other_requests_are_ignored() {
        for line in [
            "GET /favicon.ico HTTP/1.1",
            "GET /other?code=abc&state=s-123 HTTP/1.1",
            "POST /?code=abc&state=s-123 HTTP/1.1",
            "GET / HTTP/1.1",
            "",
            "garbage",
        ] {
            assert_eq!(
                parse_redirect(line, STATE),
                RedirectOutcome::Ignore,
                "{line}"
            );
        }
    }

    async fn send(port: u16, request_line: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("{request_line}\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    async fn listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[tokio::test]
    async fn good_request_yields_code() {
        let (listener, port) = listener().await;
        let waiter =
            tokio::spawn(
                async move { wait_for_code(listener, STATE, Duration::from_secs(10)).await },
            );
        let response = send(port, "GET /?code=the-code&state=s-123 HTTP/1.1").await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("Avero est connecté"));
        assert_eq!(waiter.await.unwrap(), Ok("the-code".into()));
        // Listening has stopped.
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn wrong_state_is_an_error() {
        let (listener, port) = listener().await;
        let waiter =
            tokio::spawn(
                async move { wait_for_code(listener, STATE, Duration::from_secs(10)).await },
            );
        let response = send(port, "GET /?code=the-code&state=evil HTTP/1.1").await;
        assert!(response.contains("La connexion n'a pas abouti"));
        assert!(matches!(
            waiter.await.unwrap(),
            Err(OAuthError::InvalidResponse(_))
        ));
    }

    #[tokio::test]
    async fn access_denied_is_cancelled() {
        let (listener, port) = listener().await;
        let waiter =
            tokio::spawn(
                async move { wait_for_code(listener, STATE, Duration::from_secs(10)).await },
            );
        send(port, "GET /?error=access_denied&state=s-123 HTTP/1.1").await;
        assert_eq!(waiter.await.unwrap(), Err(OAuthError::Cancelled));
    }

    #[tokio::test]
    async fn favicon_is_ignored_then_code_arrives() {
        let (listener, port) = listener().await;
        let waiter =
            tokio::spawn(
                async move { wait_for_code(listener, STATE, Duration::from_secs(10)).await },
            );
        // An idle connection (browser preconnect) must not block anything.
        let _idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let response = send(port, "GET /favicon.ico HTTP/1.1").await;
        assert!(response.starts_with("HTTP/1.1 404"));
        send(port, "GET /?code=c2&state=s-123 HTTP/1.1").await;
        assert_eq!(waiter.await.unwrap(), Ok("c2".into()));
    }

    #[tokio::test]
    async fn times_out() {
        let (listener, _port) = listener().await;
        let result = wait_for_code(listener, STATE, Duration::from_millis(100)).await;
        assert_eq!(result, Err(OAuthError::Timeout));
    }
}
