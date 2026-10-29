//! What the sync logic needs from a mail server, as a trait.

use crate::parse::parse_headers;
use avero_core::model::{NewMessage, ServerFlags};
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
    /// Starred (`\Flagged`).
    pub flagged: bool,
    /// RFC822.SIZE: the size of the whole message, in bytes.
    pub size: Option<u32>,
    pub raw: Vec<u8>,
}

impl FetchedHeader {
    /// The message these headers describe, with its flags.
    pub fn parse(&self) -> NewMessage {
        let mut message = parse_headers(self.uid, self.seen, &self.raw);
        message.flagged = self.flagged;
        message.size = self.size;
        message
    }
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
    /// The folder got a new UIDVALIDITY since its messages were read: the
    /// UIDs known here name other messages now (or none).
    #[error("the folder {0} changed on the server")]
    FolderChanged(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A mail server we can read from and act on (the real IMAP one, or a fake
/// in tests).
///
/// Every method but `list_folders`, `select`, `append` and `append_draft`
/// acts on the
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

    /// The flags of every message with a UID in `from_uid..=to_uid`.
    async fn fetch_flags(
        &mut self,
        from_uid: u32,
        to_uid: u32,
    ) -> Result<Vec<ServerFlags>, SyncError>;

    /// The size (RFC822.SIZE) of every message with a UID in
    /// `from_uid..=to_uid`, as `(uid, bytes)`.
    async fn fetch_sizes(
        &mut self,
        from_uid: u32,
        to_uid: u32,
    ) -> Result<Vec<(u32, u32)>, SyncError>;

    /// The full raw message `uid` of the selected folder, without marking it
    /// as read. `None` when the server has no such message.
    async fn fetch_body(&mut self, uid: u32) -> Result<Option<Vec<u8>>, SyncError>;

    /// Marks messages of the selected folder as read or unread.
    async fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), SyncError>;

    /// Stars or unstars (`\Flagged`) messages of the selected folder.
    async fn set_flagged(&mut self, uids: &[u32], flagged: bool) -> Result<(), SyncError>;

    /// Moves messages of the selected folder to the folder at `dest`.
    async fn move_to(&mut self, uids: &[u32], dest: &str) -> Result<(), SyncError>;

    /// Deletes messages of the selected folder for good (`\Deleted`, then
    /// UID EXPUNGE when the server has UIDPLUS, else EXPUNGE).
    async fn delete_permanently(&mut self, uids: &[u32]) -> Result<(), SyncError>;

    /// Deletes messages of the selected folder without ever touching
    /// others: `\Deleted`, then UID EXPUNGE when the server has UIDPLUS;
    /// without it, they stay marked `\Deleted` (the next EXPUNGE, by any
    /// client, removes them), since a plain EXPUNGE would also remove
    /// the messages another client marked `\Deleted` and meant to keep
    /// a little longer. For what Avero deletes on its own, often (the
    /// previous copies of a draft). By default, `delete_permanently`.
    async fn discard(&mut self, uids: &[u32]) -> Result<(), SyncError> {
        self.delete_permanently(uids).await
    }

    /// Adds a raw message to `folder` (no selection needed).
    async fn append(&mut self, folder: &str, raw: &[u8], seen: bool) -> Result<(), SyncError>;

    /// Adds a draft to `folder`, flagged `\Seen` and `\Draft` (no
    /// selection needed).
    async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<(), SyncError>;

    /// UIDs of the messages of the selected folder whose Message-ID
    /// header is `message_id` (with its angle brackets).
    async fn search_message_id(&mut self, message_id: &str) -> Result<Vec<u32>, SyncError>;

    /// UIDs of the messages of the selected folder whose sender, subject
    /// or text holds every word of `query` (what the user typed), in
    /// increasing order.
    async fn search_text(&mut self, query: &str) -> Result<Vec<u32>, SyncError>;

    /// Headers of the messages of the selected folder with these UIDs
    /// (gone ones are left out).
    async fn fetch_headers_of(&mut self, uids: &[u32]) -> Result<Vec<FetchedHeader>, SyncError>;
}
