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
    /// Number of messages in the folder (EXISTS).
    pub exists: u32,
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
    /// A command got no answer in time. The server may still have done
    /// it (an APPEND, for example), so it is not blindly retried.
    #[error("{0}")]
    Timeout(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A mail server we can read from and act on (the real IMAP one, or a fake
/// in tests).
///
/// Every method but `list_folders`, `select` and `append` acts on the
/// folder chosen by the last `select(path)`: call it first.
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

    /// Headers of the messages with sequence numbers `from_seq..=to_seq`
    /// (1 is the oldest message, EXISTS the newest).
    async fn fetch_headers_by_seq(
        &mut self,
        from_seq: u32,
        to_seq: u32,
    ) -> Result<Vec<FetchedHeader>, SyncError>;

    /// UIDs of the messages from `from_uid` up, in increasing order.
    async fn fetch_uids(&mut self, from_uid: u32) -> Result<Vec<u32>, SyncError>;

    /// `(uid, seen)` of every message with a UID in `from_uid..=to_uid`.
    async fn fetch_flags(
        &mut self,
        from_uid: u32,
        to_uid: u32,
    ) -> Result<Vec<(u32, bool)>, SyncError>;

    /// The full raw message `uid` of the selected folder, without marking it
    /// as read. `None` when the server has no such message.
    async fn fetch_body(&mut self, uid: u32) -> Result<Option<Vec<u8>>, SyncError>;

    /// Marks messages of the selected folder as read or unread.
    async fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), SyncError>;

    /// Moves messages of the selected folder to the folder at `dest`.
    async fn move_to(&mut self, uids: &[u32], dest: &str) -> Result<(), SyncError>;

    /// Deletes messages of the selected folder for good (`\Deleted`, then
    /// UID EXPUNGE when the server has UIDPLUS, else EXPUNGE).
    async fn delete_permanently(&mut self, uids: &[u32]) -> Result<(), SyncError>;

    /// Adds a raw message to `folder` (no selection needed).
    async fn append(&mut self, folder: &str, raw: &[u8], seen: bool) -> Result<(), SyncError>;
}
