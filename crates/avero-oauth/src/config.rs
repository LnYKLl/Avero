//! OAuth client identifiers: runtime `oauth.json` first, then compile-time
//! environment variables.
//!
//! Desktop ("installed app") client ids and secrets are not confidential: they
//! ship inside the binary. They are still masked in `Debug` output out of
//! caution.

use std::fmt;
use std::io;
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

/// The configuration saved in `{config_dir}/oauth.json` only, ignoring the
/// values compiled in. Tells "saved by the user" from "built in".
pub fn saved_client_config(provider: Provider, config_dir: &Path) -> Option<ClientConfig> {
    from_file(provider, &config_dir.join(CONFIG_FILE))
}

/// Whether this build has a client id compiled in for `provider`.
pub fn has_compiled_client(provider: Provider) -> bool {
    compiled(provider).is_some()
}

/// Saves the client of `provider` in `{config_dir}/oauth.json`, keeping the
/// other provider's entry (and any other key). Creates the folder if needed
/// and writes through a temporary file, so a crash never leaves half a
/// file. An unreadable existing file is replaced: it was ignored anyway.
pub fn save_client_config(
    provider: Provider,
    config_dir: &Path,
    client: &ClientConfig,
) -> io::Result<()> {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "client_id".into(),
        serde_json::Value::String(client.client_id.trim().to_owned()),
    );
    if let Some(secret) = non_empty(client.client_secret.as_deref()) {
        entry.insert("client_secret".into(), serde_json::Value::String(secret));
    }
    update_file(config_dir, |root| {
        root.insert(
            provider_key(provider).into(),
            serde_json::Value::Object(entry),
        );
    })
}

/// Removes the client of `provider` from `{config_dir}/oauth.json`, keeping
/// the other provider's entry. A missing file is not an error.
pub fn remove_client_config(provider: Provider, config_dir: &Path) -> io::Result<()> {
    if !config_dir.join(CONFIG_FILE).exists() {
        return Ok(());
    }
    update_file(config_dir, |root| {
        root.remove(provider_key(provider));
    })
}

fn update_file(
    config_dir: &Path,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> io::Result<()> {
    let path = config_dir.join(CONFIG_FILE);
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    change(&mut root);
    let mut text =
        serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(io::Error::other)?;
    text.push('\n');
    std::fs::create_dir_all(config_dir)?;
    let temp = config_dir.join(format!("{CONFIG_FILE}.tmp"));
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &path)
}

/// Suffix of every Google OAuth client id.
pub const GOOGLE_CLIENT_SUFFIX: &str = ".apps.googleusercontent.com";

/// Checks the shape of a client id pasted from the provider's console and
/// returns it trimmed, or a French explanation of what is wrong.
///
/// Google ids look like `1234567890-abc123.apps.googleusercontent.com`;
/// Microsoft "application (client) ids" are GUIDs.
pub fn check_client_id(provider: Provider, client_id: &str) -> Result<String, &'static str> {
    let id = client_id.trim();
    if id.is_empty() {
        return Err("Collez l'ID client");
    }
    let valid = match provider {
        Provider::Google => is_google_client_id(id),
        Provider::Microsoft => is_guid(id),
    };
    if valid {
        return Ok(id.to_owned());
    }
    Err(match provider {
        Provider::Google => "Un ID client Google se termine par .apps.googleusercontent.com",
        Provider::Microsoft => {
            "Un ID d'application Microsoft a la forme 00000000-0000-0000-0000-000000000000"
        }
    })
}

/// Checks a Google client secret ("GOCSPX-…") and returns it trimmed.
pub fn check_client_secret(secret: &str) -> Result<String, &'static str> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Err("Collez le code secret du client");
    }
    if secret.ends_with(GOOGLE_CLIENT_SUFFIX) {
        return Err("C'est l'ID client : collez ici le code secret (GOCSPX-…)");
    }
    if secret.len() < 10
        || !secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Ce code secret ne semble pas complet");
    }
    Ok(secret.to_owned())
}

fn is_google_client_id(id: &str) -> bool {
    id.strip_suffix(GOOGLE_CLIENT_SUFFIX).is_some_and(|prefix| {
        !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// `8-4-4-4-12` hexadecimal digits.
fn is_guid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A client id shortened for display: `123456…tent.com`.
pub fn mask_client_id(client_id: &str) -> String {
    let chars: Vec<char> = client_id.trim().chars().collect();
    if chars.len() <= 14 {
        return chars.iter().collect();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 8..].iter().collect();
    format!("{head}…{tail}")
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

    fn google() -> ClientConfig {
        ClientConfig {
            client_id: "123-abc.apps.googleusercontent.com".into(),
            client_secret: Some("GOCSPX-secret".into()),
        }
    }

    #[test]
    fn save_creates_the_folder_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("a").join("b");
        save_client_config(Provider::Google, &config_dir, &google()).unwrap();
        assert_eq!(client_config(Provider::Google, &config_dir), Some(google()));
        assert_eq!(
            saved_client_config(Provider::Google, &config_dir),
            Some(google())
        );
        let text = std::fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap();
        assert!(text.contains("\n  \"google\""), "pretty JSON: {text}");
        assert!(!config_dir.join("oauth.json.tmp").exists());
    }

    #[test]
    fn save_keeps_the_other_provider() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"{"microsoft": {"client_id": "m-id"}, "comment": "keep me"}"#,
        );
        save_client_config(Provider::Google, dir.path(), &google()).unwrap();
        assert_eq!(
            saved_client_config(Provider::Microsoft, dir.path()).map(|c| c.client_id),
            Some("m-id".into())
        );
        let microsoft = ClientConfig {
            client_id: " 00000000-0000-0000-0000-000000000000 ".into(),
            client_secret: None,
        };
        save_client_config(Provider::Microsoft, dir.path(), &microsoft).unwrap();
        assert_eq!(client_config(Provider::Google, dir.path()), Some(google()));
        assert_eq!(
            client_config(Provider::Microsoft, dir.path()),
            Some(ClientConfig {
                client_id: "00000000-0000-0000-0000-000000000000".into(),
                client_secret: None,
            })
        );
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("keep me"));
        assert!(!text.contains("m-id"));
        assert!(!text.contains("null"));
    }

    #[test]
    fn save_replaces_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "not json");
        save_client_config(Provider::Google, dir.path(), &google()).unwrap();
        assert_eq!(client_config(Provider::Google, dir.path()), Some(google()));
    }

    #[test]
    fn remove_keeps_the_other_provider() {
        let dir = tempfile::tempdir().unwrap();
        remove_client_config(Provider::Google, dir.path()).unwrap();
        assert!(!dir.path().join(CONFIG_FILE).exists());
        write(
            dir.path(),
            r#"{"google": {"client_id": "g-id"}, "microsoft": {"client_id": "m-id"}}"#,
        );
        remove_client_config(Provider::Google, dir.path()).unwrap();
        assert_eq!(saved_client_config(Provider::Google, dir.path()), None);
        assert_eq!(
            saved_client_config(Provider::Microsoft, dir.path()).map(|c| c.client_id),
            Some("m-id".into())
        );
    }

    #[test]
    fn google_client_ids() {
        assert_eq!(
            check_client_id(
                Provider::Google,
                " 1234567890-abc123def.apps.googleusercontent.com\n"
            ),
            Ok("1234567890-abc123def.apps.googleusercontent.com".into())
        );
        for bad in [
            "",
            "   ",
            ".apps.googleusercontent.com",
            "1234567890-abc123def",
            "1234 567.apps.googleusercontent.com",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert!(check_client_id(Provider::Google, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn microsoft_client_ids() {
        assert!(
            check_client_id(Provider::Microsoft, "3fa85f64-5717-4562-B3FC-2c963f66afa6").is_ok()
        );
        for bad in [
            "",
            "3fa85f64-5717-4562-b3fc-2c963f66afa",
            "3fa85f64-5717-4562-b3fc-2c963f66afag",
            "3fa85f6405717-4562-b3fc-2c963f66afa6",
            "{3fa85f64-5717-4562-b3fc-2c963f66afa6}",
            "123-abc.apps.googleusercontent.com",
        ] {
            assert!(
                check_client_id(Provider::Microsoft, bad).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn client_secrets() {
        assert_eq!(
            check_client_secret(" GOCSPX-AbC_dEf-123456 "),
            Ok("GOCSPX-AbC_dEf-123456".into())
        );
        assert!(check_client_secret("").is_err());
        assert!(check_client_secret("GOCSPX").is_err());
        assert!(check_client_secret("GOCSPX abc def ghi").is_err());
        let err = check_client_secret("123-abc.apps.googleusercontent.com").unwrap_err();
        assert!(err.contains("ID client"));
    }

    #[test]
    fn masked_ids() {
        assert_eq!(
            mask_client_id("1234567890-abc.apps.googleusercontent.com"),
            "123456…tent.com"
        );
        assert_eq!(mask_client_id("short-id"), "short-id");
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
