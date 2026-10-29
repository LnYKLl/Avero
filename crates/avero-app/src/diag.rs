//! A small log of what Avero does in the background (connections, inbox
//! watching, syncs), in `avero.log` beside the database, to understand a
//! problem afterwards. It holds events, account numbers and error
//! messages (which name a server or a user name at most): never a secret
//! nor anything of a mail.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Above this size, the log starts again (the previous one is kept as
/// `avero.log.old`).
const MAX_SIZE: u64 = 1024 * 1024;

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

/// Opens the log in `dir`. Without it (demo mode, tests, a folder that
/// cannot be written), `log` does nothing.
pub fn open(dir: &Path) {
    let path = dir.join("avero.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        let _ = std::fs::rename(&path, dir.join("avero.log.old"));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = LOG.set(Mutex::new(file));
    }
}

/// Writes one line, with the time.
pub fn log(what: std::fmt::Arguments<'_>) {
    let Some(file) = LOG.get() else {
        return;
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut file) = file.lock() {
        let _ = writeln!(file, "{now} {what}");
    }
}

/// `diag!("compte {} : ...", id)`: a line of the log.
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::diag::log(format_args!($($arg)*))
    };
}
pub(crate) use diag;
