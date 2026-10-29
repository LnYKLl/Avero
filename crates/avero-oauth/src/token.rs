//! Token endpoint responses and id_token claims.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT;
use serde_json::Value;

use crate::loopback::sanitize_code;
use crate::{OAuthError, Provider, Tokens};

/// Lifetime assumed when the server omits `expires_in`.
const DEFAULT_EXPIRES_IN: i64 = 3600;

/// Parses a token endpoint response body.
///
/// Error messages never include the body: it may contain tokens.
pub(crate) fn parse_token_response(
    provider: Provider,
    status: u16,
    body: &str,
    now: i64,
) -> Result<Tokens, OAuthError> {
    let json: Option<Value> = serde_json::from_str(body).ok();
    if let Some(error) = json
        .as_ref()
        .and_then(|j| j.get("error"))
        .and_then(Value::as_str)
    {
        return Err(OAuthError::Server(sanitize_code(error)));
    }
    if !(200..300).contains(&status) {
        return Err(OAuthError::Server(format!("HTTP {status}")));
    }
    let json =
        json.ok_or_else(|| OAuthError::InvalidResponse("token response is not JSON".into()))?;

    let access_token = json
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::InvalidResponse("missing access_token".into()))?
        .to_owned();
    let refresh_token = json
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // Some servers send expires_in as a string.
    let expires_in = match json.get("expires_in") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
    .unwrap_or(DEFAULT_EXPIRES_IN);
    let email = json
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(|t| email_from_id_token(provider, t));

    Ok(Tokens {
        access_token,
        refresh_token,
        expires_at: now.saturating_add(expires_in),
        email,
    })
}

/// Reads the account address from an id_token (JWT) payload.
///
/// The signature is deliberately not verified: the id_token comes straight
/// from the provider's token endpoint over TLS in response to our own
/// request (OpenID Connect Core §3.1.3.7, point 6), so its origin is already
/// established. The address is only used to prefill the account.
pub(crate) fn email_from_id_token(provider: Provider, id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD_INDIFFERENT.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let claim = |name: &str| {
        claims
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| s.contains('@'))
            .map(str::to_owned)
    };
    claim("email").or_else(|| match provider {
        Provider::Microsoft => claim("preferred_username"),
        Provider::Google => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            enc.encode(r#"{"alg":"RS256"}"#),
            enc.encode(claims)
        )
    }

    #[test]
    fn email_claim() {
        let token = jwt(r#"{"sub":"1","email":"ana@gmail.com"}"#);
        assert_eq!(
            email_from_id_token(Provider::Google, &token),
            Some("ana@gmail.com".into())
        );
    }

    #[test]
    fn microsoft_falls_back_to_preferred_username() {
        let token = jwt(r#"{"preferred_username":"bob@outlook.com"}"#);
        assert_eq!(
            email_from_id_token(Provider::Microsoft, &token),
            Some("bob@outlook.com".into())
        );
        assert_eq!(email_from_id_token(Provider::Google, &token), None);
    }

    #[test]
    fn malformed_id_token() {
        assert_eq!(email_from_id_token(Provider::Google, "nodots"), None);
        assert_eq!(email_from_id_token(Provider::Google, "a.!!!.c"), None);
        assert_eq!(email_from_id_token(Provider::Google, &jwt("[1]")), None);
    }

    #[test]
    fn full_response() {
        let body = format!(
            r#"{{"access_token":"at","refresh_token":"rt","expires_in":3599,
                "token_type":"Bearer","id_token":"{}"}}"#,
            jwt(r#"{"email":"ana@gmail.com"}"#)
        );
        let tokens = parse_token_response(Provider::Google, 200, &body, 1_000).unwrap();
        assert_eq!(
            tokens,
            Tokens {
                access_token: "at".into(),
                refresh_token: Some("rt".into()),
                expires_at: 4_599,
                email: Some("ana@gmail.com".into()),
            }
        );
    }

    #[test]
    fn response_without_refresh_token() {
        let body = r#"{"access_token":"at","expires_in":"60","token_type":"Bearer"}"#;
        let tokens = parse_token_response(Provider::Microsoft, 200, body, 10).unwrap();
        assert_eq!(tokens.refresh_token, None);
        assert_eq!(tokens.email, None);
        assert_eq!(tokens.expires_at, 70);
    }

    #[test]
    fn error_responses() {
        let body =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        assert_eq!(
            parse_token_response(Provider::Google, 400, body, 0),
            Err(OAuthError::Server("invalid_grant".into()))
        );
        assert_eq!(
            parse_token_response(Provider::Google, 502, "<html>bad gateway</html>", 0),
            Err(OAuthError::Server("HTTP 502".into()))
        );
        assert!(matches!(
            parse_token_response(Provider::Google, 200, r#"{"token_type":"Bearer"}"#, 0),
            Err(OAuthError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_token_response(Provider::Google, 200, "nope", 0),
            Err(OAuthError::InvalidResponse(_))
        ));
    }
}
