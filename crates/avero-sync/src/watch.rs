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
    /// New mail in the inbox: it holds more messages than before (or more
    /// recent ones). Worth a sync at once.
    Arrived,
    /// Only flags or deletions changed (FETCH, EXPUNGE): often the echo
    /// of what the user just did in Avero; worth a sync of the inbox, not
    /// right away.
    Updated,
    /// Nothing within one round (an IDLE renewal or one poll).
    Quiet,
}

/// One piece of news the server sent about the watched inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxNews {
    /// `* n EXISTS`: the inbox holds `n` messages.
    Exists(u32),
    /// `* n RECENT`: `n` messages are new since the last look.
    Recent(u32),
    /// `* n EXPUNGE`: one message went.
    Expunge,
    /// `* n FETCH (FLAGS ...)`: a message's flags changed.
    Fetch,
    /// Anything else (says nothing about the messages).
    Other,
}

/// The size of the inbox as a watcher last knew it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxCount {
    pub exists: u32,
    pub recent: u32,
}

impl InboxCount {
    /// Takes the news into account, in order, and says what they mean:
    /// `Arrived` if the inbox grew (or holds more recent messages),
    /// else `Updated` if a message went or changed, else `Quiet`.
    pub fn change(&mut self, news: &[InboxNews]) -> Change {
        let (mut arrived, mut updated) = (false, false);
        for item in news {
            match *item {
                InboxNews::Exists(n) => {
                    if n > self.exists {
                        arrived = true;
                    } else if n < self.exists {
                        updated = true;
                    }
                    self.exists = n;
                }
                InboxNews::Recent(n) => {
                    arrived |= n > self.recent;
                    self.recent = n;
                }
                InboxNews::Expunge => {
                    self.exists = self.exists.saturating_sub(1);
                    updated = true;
                }
                InboxNews::Fetch => updated = true,
                InboxNews::Other => {}
            }
        }
        if arrived {
            Change::Arrived
        } else if updated {
            Change::Updated
        } else {
            Change::Quiet
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use InboxNews::*;

    #[test]
    fn only_a_bigger_inbox_is_new_mail() {
        let mut count = InboxCount {
            exists: 10,
            recent: 0,
        };
        // Marked read or starred elsewhere (or by Avero itself).
        assert_eq!(count.change(&[Fetch]), Change::Updated);
        // Moved out: one less.
        assert_eq!(count.change(&[Expunge]), Change::Updated);
        assert_eq!(count.exists, 9);
        assert_eq!(count.change(&[Exists(9)]), Change::Quiet);
        // One went, another came: still new mail.
        assert_eq!(count.change(&[Expunge, Exists(9)]), Change::Arrived);
        assert_eq!(count.change(&[Exists(10), Recent(1)]), Change::Arrived);
        assert_eq!(count.exists, 10);
        assert_eq!(count.change(&[Recent(1), Other]), Change::Quiet);
        assert_eq!(count.change(&[Recent(2)]), Change::Arrived);
        assert_eq!(count.change(&[Exists(8)]), Change::Updated);
        assert_eq!(count.change(&[]), Change::Quiet);
    }
}
