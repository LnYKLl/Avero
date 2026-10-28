//! Waiting for the inbox to change: IMAP IDLE (RFC 2177) when the server
//! has it, polling otherwise. The real watcher is
//! [`ImapWatch`](crate::imap::ImapWatch); tests use fakes.

use crate::source::SyncError;
use std::time::Duration;

/// How long one IDLE lasts before it is ended and sent again. RFC 2177
/// asks clients to renew it before the server's 30-minute timeout.
pub const IDLE_RENEW: Duration = Duration::from_secs(25 * 60);
/// How often the inbox is checked when the server has no IDLE.
pub const POLL_EVERY: Duration = Duration::from_secs(2 * 60);

/// What one wait found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Something happened in the inbox (new message, deletion, flags).
    Changed,
    /// Nothing within one round (an IDLE renewal or one poll).
    Quiet,
}

/// A connection that watches the inbox.
// Only used with generics, never as `dyn`, and never needs `Send`.
#[allow(async_fn_in_trait)]
pub trait InboxWatch {
    /// Waits until the inbox changes, or until one round ends without
    /// news. If the future is dropped before it ends (the watch is
    /// cancelled), the connection is given up: later calls fail with
    /// [`SyncError::Connect`] and a new watcher must be opened.
    async fn wait(&mut self) -> Result<Change, SyncError>;
}
