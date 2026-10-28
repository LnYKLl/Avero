//! Only one Avero at a time for a user. Two would share one database: both
//! would sync, apply the rules, reopen the same draft and could send the
//! same scheduled mail; the second one, at start, would also report the
//! sending the first one is doing as cut short.
//!
//! On Windows a named mutex says that Avero runs; a second Avero brings
//! the first one's window to the front and quits. Elsewhere, a lock on a
//! file next to the database; a second Avero says that one runs already
//! and quits.

use std::fs::File;
use std::path::Path;

/// What the user reads when Avero runs already and its window could not
/// be shown.
pub const ALREADY_RUNNING: &str =
    "Avero est déjà ouvert. Retrouvez sa fenêtre dans la barre des tâches.";

/// Proof that this Avero is the only one: keep it until the end.
pub struct InstanceGuard {
    #[cfg(windows)]
    _mutex: Option<windows::Mutex>,
    #[cfg(not(windows))]
    _file: Option<File>,
}

/// Whether this Avero may run.
pub enum Claim {
    /// It is the only one.
    First(InstanceGuard),
    /// Another one runs.
    Running,
}

/// Claims the right to run for this user. `database` is the database
/// path (the lock file sits next to it, outside Windows). When the check
/// itself fails, Avero runs: refusing to start would be worse.
pub fn claim(database: &Path) -> Claim {
    #[cfg(windows)]
    {
        let _ = database;
        match windows::Mutex::create(windows::MUTEX_NAME) {
            Some(Some(mutex)) => Claim::First(InstanceGuard {
                _mutex: Some(mutex),
            }),
            Some(None) => Claim::Running,
            None => Claim::First(InstanceGuard { _mutex: None }),
        }
    }
    #[cfg(not(windows))]
    {
        match lock_file(&database.with_extension("lock")) {
            Ok(Some(file)) => Claim::First(InstanceGuard { _file: Some(file) }),
            Ok(None) => Claim::Running,
            Err(_) => Claim::First(InstanceGuard { _file: None }),
        }
    }
}

/// Brings the window of the Avero already running to the front. Returns
/// `false` when it could not be found (it may still be opening).
pub fn show_running() -> bool {
    #[cfg(windows)]
    {
        windows::show_window("Avero")
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Locks `path` (created if needed) for this process: `Ok(None)` when
/// another process holds it. The lock goes with the returned file (or
/// with the process).
#[cfg_attr(windows, allow(dead_code))]
fn lock_file(path: &Path) -> std::io::Result<Option<File>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    /// In the session's namespace (`Local\`): one Avero per signed-in
    /// user.
    pub const MUTEX_NAME: &str = "Local\\Avero.SingleInstance";

    /// The class winit gives the windows it opens (iced uses winit).
    const WINDOW_CLASS: &str = "Window Class";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// A named mutex, closed when dropped (Windows closes it at the end
    /// of the process anyway).
    pub struct Mutex(HANDLE);

    impl Mutex {
        /// Creates the mutex `name`: `Some(None)` if it exists already
        /// (another Avero holds it), `None` if it cannot be created.
        pub fn create(name: &str) -> Option<Option<Mutex>> {
            let name = wide(name);
            // SAFETY: `name` is a NUL-terminated UTF-16 string that lives
            // until the call returns; no security attributes (defaults).
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            // Read at once: the next call could change it.
            // SAFETY: no argument.
            let error = unsafe { GetLastError() };
            if handle.is_null() {
                return None;
            }
            let mutex = Mutex(handle);
            if error == ERROR_ALREADY_EXISTS {
                return Some(None);
            }
            Some(Some(mutex))
        }
    }

    impl Drop for Mutex {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateMutexW` and is closed
            // only here.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Restores and brings to the front the window titled `title` that
    /// winit opened, if there is one.
    pub fn show_window(title: &str) -> bool {
        let (class, title) = (wide(WINDOW_CLASS), wide(title));
        // SAFETY: both strings are NUL-terminated and live until the
        // calls return; the window handle is only passed back to Windows,
        // which checks it.
        unsafe {
            let window = FindWindowW(class.as_ptr(), title.as_ptr());
            if window.is_null() {
                return false;
            }
            if IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            SetForegroundWindow(window);
        }
        true
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_second_claim_finds_the_first() {
            let name = format!("Local\\Avero.Test.{}", std::process::id());
            let first = Mutex::create(&name).unwrap();
            assert!(first.is_some());
            assert!(matches!(Mutex::create(&name), Some(None)));
            drop(first);
            // Released: the next Avero may run.
            assert!(matches!(Mutex::create(&name), Some(Some(_))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_file_cannot_be_locked_twice() {
        let path = std::env::temp_dir().join(format!("avero-lock-{}.lock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let first = lock_file(&path).unwrap();
        assert!(first.is_some());
        assert!(lock_file(&path).unwrap().is_none());
        drop(first);
        assert!(lock_file(&path).unwrap().is_some());
        let _ = std::fs::remove_file(&path);
    }
}
