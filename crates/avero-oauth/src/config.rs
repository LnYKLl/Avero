//! OAuth client identifiers: runtime `oauth.json` first, then compile-time
//! environment variables.
//!
//! Desktop ("installed app") client ids and secrets are not confidential: they
//! ship inside the binary. They are still masked in `Debug` output out of
//! caution.

use std::fmt;
use std::path::Path;

use crate::Provider;

/// Identifiers of the OAuth client registered with a provider.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Name of the runtime configuration file inside the config directory.
pub const CONFIG_FILE: &str = "oauth.json";

/// Returns the client configuration for `provider`, or `None` when no client
/// id is configured.
///
/// Looks at `{config_dir}/oauth.json` first, then at the values baked in at
/// compile time (`AVERO_GOOGLE_CLIENT_ID`, `AVERO_GOOGLE_CLIENT_SECRET`,
/// `AVERO_MICROSOFT_CLIENT_ID`). Empty strings count as missing.
pub fn client_config(provider: Provider, config_dir: &Path) -> Option<ClientConfig> {
    from_file(provider, &config_dir.join(CONFIG_FILE)).or_else(|| compiled(provider))
}

fn from_file(provider: Provider, path: &Path) -> Option<ClientConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = json.get(provider_key(provider))?;
    let client_id = non_empty(entry.get("client_id")?.as_str())?;
    let client_secret = non_empty(entry.get("client_secret").and_then(|v| v.as_str()));
    Some(ClientConfig {
        client_id,
        client_secret,
    })
}

fn compiled(provider: Provider) -> Option<ClientConfig> {
    let (id, secret) = match provider {
        Provider::Google => (
            option_env!("AVERO_GOOGLE_CLIENT_ID"),
            option_env!("AVERO_GOOGLE_CLIENT_SECRET"),
        ),
        Provider::Microsoft => (option_env!("AVERO_MICROSOFT_CLIENT_ID"), None),
    };
    Some(ClientConfig {
        client_id: non_empty(id)?,
        client_secret: non_empty(secret),
    })
}

fn provider_key(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "google",
        Provider::Microsoft => "microsoft",
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, content: &str) {
        std::fs::write(dir.join(CONFIG_FILE), content).unwrap();
    }

    #[test]
    fn reads_both_providers_from_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"{"google": {"client_id": "g-id", "client_secret": "g-secret"},
                "microsoft": {"client_id": "m-id"}}"#,
        );
        assert_eq!(
            client_config(Provider::Google, dir.path()),
            Some(ClientConfig {
                client_id: "g-id".into(),
                client_secret: Some("g-secret".into()),
            })
        );
        assert_eq!(
            client_config(Provider::Microsoft, dir.path()),
            Some(ClientConfig {
                client_id: "m-id".into(),
                client_secret: None,
            })
        );
    }

    #[test]
    fn empty_values_count_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"{"google": {"client_id": "g-id", "client_secret": ""},
                "microsoft": {"client_id": ""}}"#,
        );
        assert_eq!(
            from_file(Provider::Google, &dir.path().join(CONFIG_FILE)),
            Some(ClientConfig {
                client_id: "g-id".into(),
                client_secret: None,
            })
        );
        assert_eq!(
            from_file(Provider::Microsoft, &dir.path().join(CONFIG_FILE)),
            None
        );
    }

    #[test]
    fn missing_or_invalid_file_falls_back_to_compile_time() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            client_config(Provider::Google, dir.path()),
            compiled(Provider::Google)
        );
        write(dir.path(), "not json");
        assert_eq!(
            client_config(Provider::Microsoft, dir.path()),
            compiled(Provider::Microsoft)
        );
    }

    #[test]
    fn debug_masks_secret() {
        let cfg = ClientConfig {
            client_id: "id".into(),
            client_secret: Some("very-secret".into()),
        };
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("id"));
        assert!(!dbg.contains("very-secret"));
        assert!(dbg.contains("***"));
    }
}
