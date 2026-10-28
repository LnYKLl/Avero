//! System notifications for new mail. They show the sender and the
//! subject, nothing else of the message.
//!
//! On Windows a notification belongs to an AppUserModelID. The installer
//! (`packaging/windows/avero.iss`) gives [`APP_USER_MODEL_ID`] to the
//! Start menu shortcut: notifications then show « Avero » and its icon.
//! Without that shortcut (`cargo run`, portable zip) Windows would drop
//! them silently, so Avero falls back to notify-rust's default id (the
//! one of PowerShell): they still appear, under « Windows PowerShell ».

use avero_sync::sync::Arrival;
#[cfg(windows)]
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;

/// Avero's AppUserModelID on Windows. Must stay equal to the one in
/// `packaging/windows/avero.iss`.
#[cfg_attr(not(windows), allow(dead_code))]
pub const APP_USER_MODEL_ID: &str = "Avero.Avero";

/// Name of the Start menu shortcut created by the installer.
#[cfg(any(windows, test))]
const SHORTCUT: &str = "Avero.lnk";

/// Above this many new messages at once, one notification counts them
/// instead of one each.
pub const GROUP_ABOVE: usize = 3;

/// One notification: a title and a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// The notifications for the new messages of one account: one per
/// message (sender, then subject), or a single « 12 nouveaux messages »
/// when there are many.
pub fn notices(arrivals: &[Arrival], account_name: &str) -> Vec<Notice> {
    if arrivals.len() > GROUP_ABOVE {
        return vec![Notice {
            title: format!("{} nouveaux messages", arrivals.len()),
            body: account_name.to_string(),
        }];
    }
    arrivals
        .iter()
        .map(|arrival| Notice {
            title: sender(arrival),
            body: match arrival.subject.trim() {
                "" => "(sans objet)".to_string(),
                subject => subject.to_string(),
            },
        })
        .collect()
}

/// The sender's name, else its address.
fn sender(arrival: &Arrival) -> String {
    match arrival.from_name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => arrival.from_addr.clone(),
    }
}

/// Shows the notifications from a thread of their own, never from the
/// interface's. A system that refuses them (or a backend that panics)
/// only leaves a line in the log, without the message's details.
pub fn show(notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("avero-notify".to_string())
        .spawn(move || {
            for notice in notices {
                let shown = std::panic::catch_unwind(|| {
                    let mut notification = notify_rust::Notification::new();
                    notification
                        .appname("Avero")
                        .summary(&notice.title)
                        .body(&notice.body);
                    #[cfg(windows)]
                    if let Some(id) = toast_app_id() {
                        notification.app_id(id);
                    }
                    notification.show().map(drop)
                });
                match shown {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("Notification refusée par le système : {e}"),
                    Err(_) => eprintln!("Notification impossible : erreur du système"),
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("Notification impossible : {e}");
    }
}

/// Windows: gives this process Avero's AppUserModelID, so the taskbar
/// groups its windows under the Start menu shortcut. Called once, at
/// start-up, before any window opens. Does nothing elsewhere.
pub fn set_process_app_id() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let wide: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that lives
        // until the call returns; Windows copies it.
        let result = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
        if result < 0 {
            eprintln!("Identifiant d'application Windows refusé : {result:#x}");
        }
    }
}

/// The AppUserModelID for the notifications: Avero's when the installer
/// created its Start menu shortcut, else `None` (notify-rust's default).
/// Looked up once per run.
#[cfg(windows)]
fn toast_app_id() -> Option<&'static str> {
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let installed = *INSTALLED.get_or_init(|| has_shortcut(&start_menu_dirs()));
    installed.then_some(APP_USER_MODEL_ID)
}

/// The Start menu "Programs" folders: the user's, then everyone's.
#[cfg(windows)]
fn start_menu_dirs() -> Vec<PathBuf> {
    ["APPDATA", "ProgramData"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| {
            Path::new(&base)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
        })
        .collect()
}

/// Whether one of these folders holds the installer's shortcut.
#[cfg(any(windows, test))]
fn has_shortcut(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| dir.join(SHORTCUT).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrival(uid: u32, name: Option<&str>, subject: &str) -> Arrival {
        Arrival {
            uid,
            from_name: name.map(String::from),
            from_addr: format!("n{uid}@exemple.fr"),
            subject: subject.to_string(),
        }
    }

    #[test]
    fn few_messages_get_one_notice_each() {
        let notices = notices(
            &[
                arrival(1, Some("Alice Martin"), "Réunion"),
                arrival(2, None, "  "),
            ],
            "Perso",
        );
        assert_eq!(
            notices,
            vec![
                Notice {
                    title: "Alice Martin".into(),
                    body: "Réunion".into(),
                },
                Notice {
                    title: "n2@exemple.fr".into(),
                    body: "(sans objet)".into(),
                },
            ]
        );
    }

    #[test]
    fn many_messages_are_grouped() {
        let arrivals: Vec<Arrival> = (1..=12).map(|uid| arrival(uid, None, "s")).collect();
        let notices = notices(&arrivals, "Perso");
        assert_eq!(
            notices,
            vec![Notice {
                title: "12 nouveaux messages".into(),
                body: "Perso".into(),
            }]
        );
        // Right at the limit, still one each.
        assert_eq!(super::notices(&arrivals[..GROUP_ABOVE], "Perso").len(), 3);
    }

    #[test]
    fn nothing_new_shows_nothing() {
        assert!(notices(&[], "Perso").is_empty());
    }

    #[test]
    fn installed_shortcut_is_found() {
        let base = std::env::temp_dir().join(format!("avero-notify-{}", std::process::id()));
        let empty = base.join("empty");
        let programs = base.join("programs");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&programs).unwrap();
        assert!(!has_shortcut(&[empty.clone(), programs.clone()]));
        std::fs::write(programs.join(SHORTCUT), b"").unwrap();
        assert!(has_shortcut(&[empty.clone(), programs.clone()]));
        assert!(!has_shortcut(&[empty]));
        assert!(!has_shortcut(&[]));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn app_id_matches_the_installer() {
        let script = include_str!("../../../packaging/windows/avero.iss");
        let line = format!("#define AppUserModelID \"{APP_USER_MODEL_ID}\"");
        assert!(script.contains(&line), "avero.iss must contain: {line}");
    }
}
