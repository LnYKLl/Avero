//! What the sync logic needs from a mail server, as a trait.

use avero_core::store::StoreError;

/// A folder as listed by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    pub path: String,
    pub attributes: Vec<String>,
}

/// Result of selecting a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStatus {
    pub uid_validity: u32,
    pub uid_next: Option<u32>,
}

/// The raw header block of one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedHeader {
    pub uid: u32,
    pub seen: bool,
    pub raw: Vec<u8>,
}

/// Everything that can go wrong while synchronising.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A mail server we can read from (the real IMAP one, or a fake in tests).
// We only use this trait with generics, never as `dyn`, and never need `Send`.
#[allow(async_fn_in_trait)]
pub trait MailSource {
    async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError>;
    async fn select(&mut self, path: &str) -> Result<FolderStatus, SyncError>;
    /// Headers for UIDs in `from_uid..=to_uid`; `to_uid = None` means `*`.
    async fn fetch_headers(
        &mut self,
        from_uid: u32,
        to_uid: Option<u32>,
    ) -> Result<Vec<FetchedHeader>, SyncError>;
}
