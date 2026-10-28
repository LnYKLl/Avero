//! Requests against Google's and Microsoft's real OAuth endpoints, with
//! made-up clients and codes. They check that our URLs and forms are
//! accepted up to the client check and that the errors the providers
//! really send are parsed into something the app can explain.
//!
//! They need the network, so they are ignored by default:
//!
//! ```sh
//! cargo test -p avero-oauth --test live_endpoints -- --ignored --test-threads 1
//! ```

use avero_oauth::{ClientConfig, OAuthError, Provider, build_auth_url, challenge_for};

/// Well-formed but unregistered Google client.
const GOOGLE_CLIENT: &str =
    "123456789012-averolivetestnotaclient00000000.apps.googleusercontent.com";
/// Well-formed but unregistered Microsoft application id.
const MICROSOFT_CLIENT: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk-averolive";

fn http() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap()
}

fn google() -> ClientConfig {
    ClientConfig {
        client_id: GOOGLE_CLIENT.into(),
        client_secret: Some("GOCSPX-averolivetestnotasecret".into()),
    }
}

fn microsoft() -> ClientConfig {
    ClientConfig {
        client_id: MICROSOFT_CLIENT.into(),
        client_secret: None,
    }
}

#[tokio::test]
#[ignore = "network: Google authorization endpoint"]
async fn google_auth_url_reaches_the_client_check() {
    let url = build_auth_url(
        Provider::Google,
        GOOGLE_CLIENT,
        "http://127.0.0.1:53682",
        "state123",
        &challenge_for(VERIFIER),
        Some("someone@gmail.com"),
    );
    let response = http().get(&url).send().await.unwrap();
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let body = response.text().await.unwrap();
    println!("status {status}, final url {final_url}");
    // Google renders an error page ("Erreur 401 : invalid_client — The
    // OAuth client was not found."). A malformed request (bad scope,
    // redirect or PKCE parameter) would be reported before the client.
    assert!(
        body.contains("invalid_client") || body.contains("OAuth client was not found"),
        "unexpected page (status {status}): {}",
        &body[..body.len().min(2000)]
    );
    assert!(
        !body.contains("invalid_request"),
        "request rejected as malformed"
    );
    assert!(!body.contains("invalid_scope"), "scope rejected");
}

#[tokio::test]
#[ignore = "network: Microsoft authorization endpoint"]
async fn microsoft_auth_url_reaches_the_client_check() {
    let url = build_auth_url(
        Provider::Microsoft,
        MICROSOFT_CLIENT,
        "http://localhost:53682",
        "state123",
        &challenge_for(VERIFIER),
        None,
    );
    let response = http().get(&url).send().await.unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();
    println!("status {status}");
    // On the multi-tenant `common` endpoint Microsoft cannot look the
    // application up before the user picks an account: a well-formed
    // request gets the sign-in page, a malformed one an AADSTS error page
    // straight away (e.g. AADSTS900144 for a missing parameter).
    assert_eq!(status, 200);
    assert!(
        body.contains(r#"content="ConvergedSignIn""#),
        "not the sign-in page: {}",
        &body[..body.len().min(2000)]
    );
    assert!(!body.contains("AADSTS"), "request rejected");
}

#[tokio::test]
#[ignore = "network: Microsoft token endpoint"]
async fn microsoft_token_error_details() {
    // What `exchange_code` gets, read raw to record the AADSTS code: the
    // application id is unknown (AADSTS700016) and maps to
    // `unauthorized_client`.
    let response = http()
        .post("https://login.microsoftonline.com/common/oauth2/v2.0/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", "M.C123_BAY.2.U.bogus-code"),
            ("redirect_uri", "http://localhost:53682"),
            ("client_id", MICROSOFT_CLIENT),
            ("code_verifier", VERIFIER),
        ])
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let json: serde_json::Value = response.json().await.unwrap();
    println!(
        "status {status}: error {:?}, codes {:?}",
        json["error"], json["error_codes"]
    );
    assert_eq!(status, 400);
    assert_eq!(json["error"], "unauthorized_client");
    assert_eq!(json["error_codes"][0], 700016);
}

#[tokio::test]
#[ignore = "network: Google token endpoint"]
async fn google_code_exchange_with_unknown_client() {
    let result = avero_oauth::exchange_code(
        Provider::Google,
        &google(),
        "4/0AVeroBogusAuthorizationCode",
        "http://127.0.0.1:53682",
        VERIFIER,
    )
    .await;
    println!("{result:?}");
    assert_eq!(result, Err(OAuthError::Server("invalid_client".into())));
}

#[tokio::test]
#[ignore = "network: Google token endpoint"]
async fn google_refresh_with_unknown_client() {
    let result = avero_oauth::refresh(Provider::Google, &google(), "1//0averoBogusRefresh").await;
    println!("{result:?}");
    assert_eq!(result, Err(OAuthError::Server("invalid_client".into())));
}

#[tokio::test]
#[ignore = "network: Microsoft token endpoint"]
async fn microsoft_code_exchange_with_unknown_client() {
    let result = avero_oauth::exchange_code(
        Provider::Microsoft,
        &microsoft(),
        "M.C123_BAY.2.U.bogus-code",
        "http://localhost:53682",
        VERIFIER,
    )
    .await;
    println!("{result:?}");
    assert_eq!(
        result,
        Err(OAuthError::Server("unauthorized_client".into()))
    );
}

#[tokio::test]
#[ignore = "network: Microsoft token endpoint"]
async fn microsoft_refresh_with_unknown_client() {
    let result = avero_oauth::refresh(Provider::Microsoft, &microsoft(), "M.C123.bogus").await;
    println!("{result:?}");
    assert_eq!(
        result,
        Err(OAuthError::Server("unauthorized_client".into()))
    );
}
