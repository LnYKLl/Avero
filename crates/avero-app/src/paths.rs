//! Where Avero keeps its files on disk.

use directories::ProjectDirs;
use std::path::PathBuf;

/// Path of the SQLite database, in the user's data folder.
/// The folder is created if it does not exist yet.
pub fn database_path() -> PathBuf {
    let dir = match ProjectDirs::from("org", "Avero", "Avero") {
        Some(dirs) => dirs.data_dir().to_path_buf(),
        // No home directory found: fall back to the current folder.
        None => PathBuf::from("."),
    };
    // If this fails, opening the database will report a clear error.
    let _ = std::fs::create_dir_all(&dir);
    dir.join("avero.db")
}
