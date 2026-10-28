//! User preferences kept in `settings.json` in the config folder.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Light, dark, or whatever the system uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// Reads `AVERO_THEME` (`light`, `dark` or `system`), if set.
    pub fn from_env_value(value: &str) -> Option<ThemeChoice> {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" | "clair" => Some(ThemeChoice::Light),
            "dark" | "sombre" => Some(ThemeChoice::Dark),
            "system" | "systeme" | "système" => Some(ThemeChoice::System),
            _ => None,
        }
    }

    /// Whether the app is dark, given the system preference.
    pub fn is_dark(self, system_is_dark: bool) -> bool {
        match self {
            ThemeChoice::System => system_is_dark,
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        }
    }
}

/// Everything stored in `settings.json`. Unknown or missing fields fall
/// back to their defaults, so old files keep working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// A system notification for each new unread message of an inbox.
    pub notifications: bool,
    /// One line per conversation in the list (off: one per message).
    pub group_threads: bool,
    /// The inbox is sorted in tabs: Important, Newsletters,
    /// Notifications (off: one list).
    pub inbox_tabs: bool,
    /// The local spam filter moves new mail it is sure about to the
    /// spam folder.
    pub spam_filter: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: ThemeChoice::default(),
            notifications: true,
            group_threads: true,
            inbox_tabs: true,
            spam_filter: true,
        }
    }
}

/// Name of the file inside the config folder.
pub const FILE: &str = "settings.json";

impl Settings {
    /// Reads the settings; a missing or broken file gives the defaults.
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the settings, creating the folder if needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// Path of the settings file in `config_dir`.
pub fn path_in(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("avero-settings-{}-{tag}", std::process::id()))
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = temp_dir("missing").join(FILE);
        assert_eq!(Settings::load(&path), Settings::default());
        assert_eq!(Settings::default().theme, ThemeChoice::System);
        assert!(Settings::default().notifications);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = temp_dir("roundtrip");
        let path = path_in(&dir);
        let settings = Settings {
            theme: ThemeChoice::Dark,
            notifications: false,
            group_threads: false,
            inbox_tabs: false,
            spam_filter: false,
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"dark\""), "{text}");
        assert!(text.contains("\"notifications\": false"), "{text}");
        assert!(text.contains("\"group_threads\": false"), "{text}");
        assert!(text.contains("\"inbox_tabs\": false"), "{text}");
        assert!(text.contains("\"spam_filter\": false"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn older_files_get_notifications_and_conversations_on() {
        let dir = temp_dir("older");
        std::fs::create_dir_all(&dir).unwrap();
        let path = path_in(&dir);
        std::fs::write(&path, "{\"theme\": \"light\"}").unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.theme, ThemeChoice::Light);
        assert!(settings.notifications);
        assert!(settings.group_threads);
        assert!(settings.inbox_tabs);
        assert!(settings.spam_filter);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn broken_or_partial_file_is_tolerated() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = path_in(&dir);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, "{\"other\": 1}").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_choice_resolves_against_system() {
        assert!(ThemeChoice::System.is_dark(true));
        assert!(!ThemeChoice::System.is_dark(false));
        assert!(ThemeChoice::Dark.is_dark(false));
        assert!(!ThemeChoice::Light.is_dark(true));
        assert_eq!(ThemeChoice::from_env_value("Dark"), Some(ThemeChoice::Dark));
        assert_eq!(ThemeChoice::from_env_value("nope"), None);
    }
}
