//! Incremental synchronisation of one account.

use crate::parse::{detect_role, parse_headers};
use crate::source::{FetchedHeader, FolderStatus, MailSource, RemoteFolder, SyncError};
use avero_core::model::{AccountId, Folder, FolderId, FolderRole, NewMessage};
use avero_core::store::Store;

/// Maximum number of messages asked from the server in one request.
pub const BATCH: u32 = 500;
/// On the first sync of a folder, only its newest messages are fetched.
pub const FIRST_SYNC: u32 = 2000;
/// Maximum number of stored messages whose flags are asked in one request.
pub const FLAG_CHUNK: usize = 2000;

/// What a synchronisation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Folders brought up to date.
    pub folders: usize,
    pub new_messages: usize,
    /// Folders left aside because the server refused a command on them.
    pub skipped_folders: usize,
}

/// Brings the local copy of the special folders (inbox, sent, drafts,
/// archive, trash, junk) up to date, INBOX first, then resynchronises the
/// INBOX's flags and deletions.
///
/// Other folders (user folders, Gmail labels) are left aside for now: on
/// a big account they would make the first sync very long. Folders marked
/// `\All`, `\Flagged` or `\Important` are skipped too: on Gmail they only
/// hold copies of messages found in other folders.
///
/// `on_progress` is called after every saved batch (and after the INBOX
/// flags are applied), so the UI can refresh the list while syncing.
///
/// A protocol error on one folder (the server refuses SELECT or FETCH)
/// only skips that folder. Connection, login and database errors stop
/// the whole account: the next folders would fail the same way.
pub async fn sync_account<S: MailSource>(
    store: &Store,
    source: &mut S,
    account: AccountId,
    mut on_progress: impl FnMut(&SyncReport),
) -> Result<SyncReport, SyncError> {
    let mut report = SyncReport {
        folders: 0,
        new_messages: 0,
        skipped_folders: 0,
    };

    let mut remotes: Vec<(RemoteFolder, FolderRole)> = source
        .list_folders()
        .await?
        .into_iter()
        .filter(|remote| {
            is_selectable(&remote.attributes) && !is_duplicate_view(&remote.attributes)
        })
        .map(|remote| {
            let role = detect_role(&remote.path, &remote.attributes);
            (remote, role)
        })
        .filter(|(_, role)| *role != FolderRole::Other)
        .collect();
    // Stable sort: INBOX first, the others keep the server's order.
    remotes.sort_by_key(|(remote, _)| !remote.path.eq_ignore_ascii_case("INBOX"));

    for (remote, role) in remotes {
        let folder = store.upsert_folder(account, &remote.path, role)?;
        match sync_folder(store, source, &folder, &mut report, &mut on_progress).await {
            Ok(()) => report.folders += 1,
            Err(SyncError::Protocol(_)) => report.skipped_folders += 1,
            Err(other) => return Err(other),
        }
    }

    Ok(report)
}

/// `\Noselect` and `\NonExistent` folders cannot be opened.
fn is_selectable(attributes: &[String]) -> bool {
    let cannot_open = |a: &String| {
        a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent")
    };
    !attributes.iter().any(cannot_open)
}

/// `\All`, `\Flagged` and `\Important` folders are views over other
/// folders (Gmail's "All Mail", "Starred", "Important").
fn is_duplicate_view(attributes: &[String]) -> bool {
    attributes.iter().any(|a| {
        ["\\All", "\\Flagged", "\\Important"]
            .iter()
            .any(|view| a.eq_ignore_ascii_case(view))
    })
}

/// Fetches the new messages of one folder; for the inbox, also brings
/// flags and deletions up to date. The batches already saved stay saved
/// even if a later batch fails.
///
/// The first time, only the newest [`FIRST_SYNC`] messages are fetched,
/// by sequence number (so sparse UIDs cost nothing). After that, new
/// messages are found by UID above the highest one seen.
async fn sync_folder<S: MailSource>(
    store: &Store,
    source: &mut S,
    folder: &Folder,
    report: &mut SyncReport,
    on_progress: &mut impl FnMut(&SyncReport),
) -> Result<(), SyncError> {
    let status = source.select(&folder.path).await?;

    // A new UIDVALIDITY (or none stored yet) means our UIDs mean nothing.
    let mut last_seen = folder.last_seen_uid;
    if folder.uid_validity != Some(status.uid_validity) {
        store.reset_folder(folder.id, status.uid_validity)?;
        last_seen = None;
    }

    match last_seen {
        None => {
            // Newest batch first, so the list fills from the top.
            let oldest = status.exists.saturating_sub(FIRST_SYNC - 1).max(1);
            let mut to = status.exists;
            while to >= oldest && to > 0 {
                let from = to.saturating_sub(BATCH - 1).max(oldest);
                let headers = source.fetch_headers_by_seq(from, to).await?;
                report.new_messages += save_batch(store, folder.id, headers)?;
                on_progress(report);
                to = from - 1;
            }
        }
        Some(last) => {
            let start = last.saturating_add(1);
            let nothing_new = status.uid_next.is_some_and(|next| start >= next);
            if !nothing_new {
                let uids = source.fetch_uids(start).await?;
                for chunk in uids.chunks(BATCH as usize) {
                    let (first, last) = (chunk[0], chunk[chunk.len() - 1]);
                    let headers = source.fetch_headers(first, Some(last)).await?;
                    report.new_messages += save_batch(store, folder.id, headers)?;
                    on_progress(report);
                }
            }
        }
    }
    // Every UID below `uid_next` is now known (or deliberately left aside,
    // when older than the first sync's window). If the highest ones were
    // deleted, remember it so we don't ask again.
    if let Some(uid_next) = status.uid_next.filter(|&n| n > 0) {
        store.set_last_seen_uid(folder.id, uid_next - 1)?;
    }

    if folder.role == FolderRole::Inbox {
        resync_flags(store, source, folder, &status).await?;
        on_progress(report);
    }
    Ok(())
}

/// Copies the server's seen flags to the stored messages of the selected
/// folder and removes the ones expunged on the server. Only the UID span
/// of the stored messages is asked, [`FLAG_CHUNK`] messages at a time.
async fn resync_flags<S: MailSource>(
    store: &Store,
    source: &mut S,
    folder: &Folder,
    status: &FolderStatus,
) -> Result<(), SyncError> {
    let stored: Vec<u32> = store
        .inbox_uids(folder.id)?
        .into_iter()
        .map(|(uid, _)| uid)
        .collect();
    if stored.is_empty() {
        return Ok(());
    }
    if status.exists == 0 {
        // Emptied on the server. `UID FETCH 1:*` would be refused by some
        // servers, so forget everything (what is left to fetch stays known).
        store.reset_folder(folder.id, status.uid_validity)?;
        if let Some(uid_next) = status.uid_next.filter(|&n| n > 0) {
            store.set_last_seen_uid(folder.id, uid_next - 1)?;
        }
        return Ok(());
    }
    let mut chunks = Vec::new();
    for chunk in stored.chunks(FLAG_CHUNK) {
        let (from, to) = (chunk[0], chunk[chunk.len() - 1]);
        let present = source.fetch_flags(from, to).await?;
        chunks.push((from, to, present));
    }
    // A server answering nothing at all for a non-empty folder is more
    // likely confused than emptied: keep what we have.
    if chunks.iter().all(|(_, _, present)| present.is_empty()) {
        return Ok(());
    }
    for (from, to, present) in chunks {
        store.apply_flags(folder.id, from, to, &present)?;
    }
    Ok(())
}

/// Parses and stores one batch; returns how many headers it held.
fn save_batch(
    store: &Store,
    folder: FolderId,
    headers: Vec<FetchedHeader>,
) -> Result<usize, SyncError> {
    let messages: Vec<NewMessage> = headers
        .iter()
        .map(|h| parse_headers(h.uid, h.seen, &h.raw))
        .collect();
    store.insert_messages(folder, &messages)?;
    Ok(messages.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::model::{AccountFilter, AuthKind, NewAccount, Security, ServerConfig};

    /// Kind of error a fake folder can be told to return.
    #[derive(Clone, Copy)]
    enum Fail {
        Protocol,
        Connect,
    }

    impl Fail {
        fn error(self) -> SyncError {
            match self {
                Fail::Protocol => SyncError::Protocol("refused".into()),
                Fail::Connect => SyncError::Connect("lost".into()),
            }
        }
    }

    struct FakeFolder {
        path: String,
        attributes: Vec<String>,
        uid_validity: u32,
        uids: Vec<u32>,
        /// UIDs carrying `\Seen`.
        seen: Vec<u32>,
        /// UIDNEXT to announce; `None` means "highest UID + 1".
        uid_next: Option<u32>,
        /// SELECT on this folder fails.
        select_fails: Option<Fail>,
        /// A fetch starting at or after this UID fails.
        fetch_fails_from: Option<(u32, Fail)>,
    }

    #[derive(Default)]
    struct FakeSource {
        folders: Vec<FakeFolder>,
        selected: Option<usize>,
        selects: Vec<String>,
        /// (from, to) of every fetch_headers call.
        fetches: Vec<(u32, Option<u32>)>,
        /// (from, to) of every fetch_headers_by_seq call.
        seq_fetches: Vec<(u32, u32)>,
        /// Folder path of every fetch_flags call.
        flag_fetches: Vec<String>,
        /// (from, to) of every fetch_flags call.
        flag_ranges: Vec<(u32, u32)>,
    }

    impl FakeSource {
        fn add_folder(&mut self, path: &str, attrs: &[&str], validity: u32, uids: Vec<u32>) {
            self.folders.push(FakeFolder {
                path: path.to_string(),
                attributes: attrs.iter().map(|a| a.to_string()).collect(),
                uid_validity: validity,
                uids,
                seen: Vec::new(),
                uid_next: None,
                select_fails: None,
                fetch_fails_from: None,
            });
        }

        fn folder(&mut self, path: &str) -> &mut FakeFolder {
            self.folders.iter_mut().find(|f| f.path == path).unwrap()
        }

        fn selected(&mut self) -> &mut FakeFolder {
            let index = self.selected.expect("select first");
            &mut self.folders[index]
        }
    }

    fn fake_raw(uid: u32) -> Vec<u8> {
        format!(
            "Message-ID: <{uid}@fake>\r\nFrom: a@b.c\r\nSubject: m{uid}\r\nDate: Mon, 1 Jan 2024 10:00:00 +0000\r\n\r\n"
        )
        .into_bytes()
    }

    impl MailSource for FakeSource {
        async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError> {
            Ok(self
                .folders
                .iter()
                .map(|f| RemoteFolder {
                    path: f.path.clone(),
                    attributes: f.attributes.clone(),
                })
                .collect())
        }

        async fn select(&mut self, path: &str) -> Result<FolderStatus, SyncError> {
            self.selects.push(path.to_string());
            let index = self
                .folders
                .iter()
                .position(|f| f.path == path)
                .ok_or_else(|| SyncError::Protocol("no such folder".into()))?;
            let folder = &self.folders[index];
            if let Some(fail) = folder.select_fails {
                return Err(fail.error());
            }
            self.selected = Some(index);
            let highest_plus_one = folder.uids.iter().max().map_or(1, |m| m + 1);
            Ok(FolderStatus {
                uid_validity: folder.uid_validity,
                uid_next: Some(folder.uid_next.unwrap_or(highest_plus_one)),
                exists: folder.uids.len() as u32,
            })
        }

        async fn fetch_headers(
            &mut self,
            from_uid: u32,
            to_uid: Option<u32>,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            self.fetches.push((from_uid, to_uid));
            let folder = self.selected();
            if let Some((fail_uid, fail)) = folder.fetch_fails_from
                && from_uid >= fail_uid
            {
                return Err(fail.error());
            }
            Ok(folder
                .uids
                .iter()
                .filter(|&&uid| uid >= from_uid && to_uid.is_none_or(|to| uid <= to))
                .map(|&uid| FetchedHeader {
                    uid,
                    seen: folder.seen.contains(&uid),
                    raw: fake_raw(uid),
                })
                .collect())
        }

        async fn fetch_headers_by_seq(
            &mut self,
            from_seq: u32,
            to_seq: u32,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            self.seq_fetches.push((from_seq, to_seq));
            let folder = self.selected();
            let mut uids = folder.uids.clone();
            uids.sort_unstable();
            let range: Vec<u32> = uids
                .iter()
                .enumerate()
                .filter(|(i, _)| (from_seq..=to_seq).contains(&(*i as u32 + 1)))
                .map(|(_, &uid)| uid)
                .collect();
            if let Some((fail_uid, fail)) = folder.fetch_fails_from
                && range.iter().any(|&uid| uid >= fail_uid)
            {
                return Err(fail.error());
            }
            Ok(range
                .into_iter()
                .map(|uid| FetchedHeader {
                    uid,
                    seen: folder.seen.contains(&uid),
                    raw: fake_raw(uid),
                })
                .collect())
        }

        async fn fetch_uids(&mut self, from_uid: u32) -> Result<Vec<u32>, SyncError> {
            let mut uids: Vec<u32> = self
                .selected()
                .uids
                .iter()
                .copied()
                .filter(|&uid| uid >= from_uid)
                .collect();
            uids.sort_unstable();
            Ok(uids)
        }

        async fn fetch_flags(
            &mut self,
            from_uid: u32,
            to_uid: u32,
        ) -> Result<Vec<(u32, bool)>, SyncError> {
            let folder = self.selected();
            let path = folder.path.clone();
            let flags = folder
                .uids
                .iter()
                .filter(|&&uid| (from_uid..=to_uid).contains(&uid))
                .map(|&uid| (uid, folder.seen.contains(&uid)))
                .collect();
            self.flag_fetches.push(path);
            self.flag_ranges.push((from_uid, to_uid));
            Ok(flags)
        }

        async fn delete_permanently(&mut self, uids: &[u32]) -> Result<(), SyncError> {
            self.selected().uids.retain(|uid| !uids.contains(uid));
            Ok(())
        }

        async fn fetch_body(&mut self, uid: u32) -> Result<Option<Vec<u8>>, SyncError> {
            let folder = self.selected();
            Ok(folder.uids.contains(&uid).then(|| fake_raw(uid)))
        }

        async fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), SyncError> {
            let folder = self.selected();
            folder.seen.retain(|uid| !uids.contains(uid));
            if seen {
                folder.seen.extend(uids);
            }
            Ok(())
        }

        async fn move_to(&mut self, uids: &[u32], dest: &str) -> Result<(), SyncError> {
            let moved: Vec<u32> = uids
                .iter()
                .copied()
                .filter(|uid| self.selected().uids.contains(uid))
                .collect();
            self.selected().uids.retain(|uid| !moved.contains(uid));
            let target = self.folder(dest);
            for _ in moved {
                let next = target.uids.iter().max().map_or(1, |m| m + 1);
                target.uids.push(next);
            }
            Ok(())
        }

        async fn append(&mut self, folder: &str, _raw: &[u8], seen: bool) -> Result<(), SyncError> {
            let target = self.folder(folder);
            let next = target.uids.iter().max().map_or(1, |m| m + 1);
            target.uids.push(next);
            if seen {
                target.seen.push(next);
            }
            Ok(())
        }
    }

    fn setup() -> (Store, AccountId) {
        let store = Store::open_in_memory().unwrap();
        let server = |host: &str| ServerConfig {
            host: host.into(),
            port: 993,
            username: "t".into(),
            security: Security::Tls,
        };
        let account = store
            .add_account(NewAccount {
                name: "Test".into(),
                email: "t@example.com".into(),
                imap: server("imap.example.com"),
                smtp: server("smtp.example.com"),
                auth: AuthKind::Password,
            })
            .unwrap();
        (store, account.id)
    }

    /// Synchronises without watching progress.
    async fn sync(
        store: &Store,
        source: &mut FakeSource,
        account: AccountId,
    ) -> Result<SyncReport, SyncError> {
        sync_account(store, source, account, |_| {}).await
    }

    /// Counts stored rows (no merging of duplicates across folders: each
    /// test uses one Message-ID per row, so ids must be distinct per folder).
    fn total_rows(store: &Store, account: AccountId) -> usize {
        let mut ids: Vec<i64> = Vec::new();
        for role in [FolderRole::Inbox, FolderRole::Sent, FolderRole::Other] {
            for item in store
                .unified(role, AccountFilter::Only(account), 10_000, 0)
                .unwrap()
            {
                ids.extend(item.row_ids);
            }
        }
        ids.len()
    }

    fn inbox_id(store: &Store, account: AccountId) -> FolderId {
        store
            .folder_by_role(account, FolderRole::Inbox)
            .unwrap()
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn first_sync_imports_all_folders_and_roles() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.add_folder("Sent", &["\\Sent"], 1, vec![1]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 2);
        assert_eq!(report.new_messages, 4);
        let roles: Vec<FolderRole> = store
            .folders(account)
            .unwrap()
            .iter()
            .map(|f| f.role)
            .collect();
        assert_eq!(roles, vec![FolderRole::Inbox, FolderRole::Sent]);
        assert_eq!(total_rows(&store, account), 4);
    }

    #[tokio::test]
    async fn second_sync_only_fetches_new_uids() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        sync(&store, &mut source, account).await.unwrap();

        source.folders[0].uids.push(4);
        source.fetches.clear();
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 1);
        assert_eq!(source.fetches[0].0, 4);
        assert_eq!(total_rows(&store, account), 4);
    }

    #[tokio::test]
    async fn uidvalidity_change_resets_folder() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        sync(&store, &mut source, account).await.unwrap();

        source.folders[0].uid_validity = 2;
        source.folders[0].uids = vec![10, 11];
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 2);
        assert_eq!(total_rows(&store, account), 2);
    }

    #[tokio::test]
    async fn large_folder_is_fetched_in_batches() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=1203).collect());

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 1203);
        assert_eq!(total_rows(&store, account), 1203);
        // Newest batch first, by sequence number, no UID windows.
        assert_eq!(source.seq_fetches, vec![(704, 1203), (204, 703), (1, 203)]);
        assert!(source.fetches.is_empty());

        // Next time, only what is above the highest UID is asked.
        source.folder("INBOX").uids.extend([1204, 1205]);
        sync(&store, &mut source, account).await.unwrap();
        assert_eq!(source.fetches, vec![(1204, Some(1205))]);
        assert_eq!(total_rows(&store, account), 1205);
    }

    #[tokio::test]
    async fn first_sync_takes_only_the_newest_messages() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=5000).collect());

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, FIRST_SYNC as usize);
        let stored = store.inbox_uids(inbox_id(&store, account)).unwrap();
        assert_eq!(stored.first().map(|s| s.0), Some(3001));
        assert_eq!(stored.last().map(|s| s.0), Some(5000));
        assert_eq!(source.seq_fetches.last(), Some(&(3001, 3500)));

        // Older messages are not fetched later either.
        source.seq_fetches.clear();
        let report = sync(&store, &mut source, account).await.unwrap();
        assert_eq!(report.new_messages, 0);
        assert!(source.seq_fetches.is_empty() && source.fetches.is_empty());
    }

    #[tokio::test]
    async fn sparse_new_uids_cost_no_empty_requests() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2]);
        sync(&store, &mut source, account).await.unwrap();

        // Gmail-like gaps: thousands of UIDs between two messages.
        source.folder("INBOX").uids.extend([50_000, 900_000]);
        source.fetches.clear();
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 2);
        assert_eq!(source.fetches, vec![(50_000, Some(900_000))]);
    }

    #[tokio::test]
    async fn other_folders_are_not_synced() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("Factures", &[], 1, vec![1, 2]);
        source.add_folder("[Gmail]/Envoyés", &["\\Sent"], 1, vec![1]);
        source.add_folder("Perso/2019", &["\\HasNoChildren"], 1, vec![1]);
        source.add_folder("Spam", &[], 1, vec![1]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 3);
        assert_eq!(source.selects, vec!["INBOX", "[Gmail]/Envoyés", "Spam"]);
        assert!(
            store
                .folders(account)
                .unwrap()
                .iter()
                .all(|f| f.role != FolderRole::Other)
        );
    }

    #[tokio::test]
    async fn inbox_flags_are_asked_in_chunks_of_stored_uids() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=2500).map(|u| u * 10).collect());
        // The first sync keeps the newest 2000: UIDs 5010 to 25000.
        sync(&store, &mut source, account).await.unwrap();

        assert_eq!(source.flag_ranges, vec![(5010, 25_000)]);

        // Ten more: 2010 stored messages need two requests.
        source
            .folder("INBOX")
            .uids
            .extend((2501..=2510).map(|u| u * 10));
        source.flag_ranges.clear();
        sync(&store, &mut source, account).await.unwrap();
        assert_eq!(source.flag_ranges, vec![(5010, 25_000), (25_010, 25_100)]);
    }

    #[tokio::test]
    async fn local_read_waiting_for_the_server_is_not_undone_by_a_sync() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2]);
        sync(&store, &mut source, account).await.unwrap();
        let rows: Vec<i64> = store
            .unified(FolderRole::Inbox, AccountFilter::All, 10, 0)
            .unwrap()
            .into_iter()
            .flat_map(|i| i.row_ids)
            .collect();

        // The user reads uid 2; its SetSeen job waits behind a sync.
        store.set_seen(&rows[..1], true).unwrap();
        sync(&store, &mut source, account).await.unwrap();
        let inbox = inbox_id(&store, account);
        assert_eq!(
            store.inbox_uids(inbox).unwrap(),
            vec![(1, false), (2, true)]
        );

        // The job ran: the server agrees, and later changes apply again.
        source.select("INBOX").await.unwrap();
        source.set_seen(&[2], true).await.unwrap();
        store.confirm_seen(&rows[..1]).unwrap();
        source.folder("INBOX").seen.clear();
        sync(&store, &mut source, account).await.unwrap();
        assert_eq!(
            store.inbox_uids(inbox).unwrap(),
            vec![(1, false), (2, false)]
        );
    }

    #[tokio::test]
    async fn progress_is_reported_after_every_batch() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=1203).collect());
        source.add_folder("Sent", &["\\Sent"], 1, vec![1]);
        let mut seen: Vec<usize> = Vec::new();

        let report = sync_account(&store, &mut source, account, |r| seen.push(r.new_messages))
            .await
            .unwrap();

        // Three INBOX batches, the INBOX flags, then the Sent batch.
        assert_eq!(seen, vec![500, 1000, 1203, 1203, 1204]);
        assert_eq!(report.new_messages, 1204);
    }

    #[tokio::test]
    async fn inbox_is_synced_first() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("Archive", &[], 1, vec![1]);
        source.add_folder("Drafts", &[], 1, vec![1]);
        source.add_folder("INBOX", &[], 1, vec![1]);

        sync(&store, &mut source, account).await.unwrap();

        assert_eq!(source.selects, vec!["INBOX", "Archive", "Drafts"]);
    }

    #[tokio::test]
    async fn gmail_duplicate_views_are_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder(
            "[Gmail]/Tous les messages",
            &["\\HasNoChildren", "\\All"],
            1,
            vec![1],
        );
        source.add_folder("[Gmail]/Suivis", &["\\flagged"], 1, vec![1]);
        source.add_folder("[Gmail]/Important", &["\\Important"], 1, vec![1]);
        source.add_folder("[Gmail]/Corbeille", &["\\Trash"], 1, vec![1]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 2);
        assert_eq!(source.selects, vec!["INBOX", "[Gmail]/Corbeille"]);
    }

    #[tokio::test]
    async fn inbox_flags_and_deletions_are_resynced() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        sync(&store, &mut source, account).await.unwrap();

        // Elsewhere: 1 was read, 2 was deleted, 4 arrived already read.
        let inbox = source.folder("INBOX");
        inbox.seen = vec![1, 4];
        inbox.uids = vec![1, 3, 4];
        sync(&store, &mut source, account).await.unwrap();

        let stored = store.inbox_uids(inbox_id(&store, account)).unwrap();
        assert_eq!(stored, vec![(1, true), (3, false), (4, true)]);
    }

    #[tokio::test]
    async fn only_the_inbox_gets_its_flags_resynced() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("Sent", &["\\Sent"], 1, vec![1]);
        source.add_folder("Empty", &[], 1, vec![]);

        sync(&store, &mut source, account).await.unwrap();

        assert_eq!(source.flag_fetches, vec!["INBOX"]);
    }

    #[tokio::test]
    async fn emptied_inbox_is_cleared_without_fetching_flags() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2]);
        sync(&store, &mut source, account).await.unwrap();

        source.folder("INBOX").uids.clear();
        source.folder("INBOX").uid_next = Some(3);
        source.flag_fetches.clear();
        source.fetches.clear();
        sync(&store, &mut source, account).await.unwrap();

        assert!(source.flag_fetches.is_empty());
        assert_eq!(total_rows(&store, account), 0);
        let inbox = store
            .folder_by_role(account, FolderRole::Inbox)
            .unwrap()
            .unwrap();
        assert_eq!(inbox.last_seen_uid, Some(2));

        // Next time nothing is fetched again.
        source.fetches.clear();
        sync(&store, &mut source, account).await.unwrap();
        assert!(source.fetches.is_empty(), "{:?}", source.fetches);
    }

    #[tokio::test]
    async fn noselect_folders_are_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("[Gmail]", &["\\NoSelect", "\\HasChildren"], 1, vec![]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(source.selects, vec!["INBOX".to_string()]);
    }

    #[tokio::test]
    async fn sparse_uids_are_not_missed() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 900, 1700]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 4);
        assert_eq!(total_rows(&store, account), 4);
    }

    #[tokio::test]
    async fn nonexistent_folders_are_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("Old", &["\\nonexistent"], 1, vec![]);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(report.skipped_folders, 0);
        assert_eq!(source.selects, vec!["INBOX".to_string()]);
    }

    #[tokio::test]
    async fn protocol_error_on_select_skips_only_that_folder() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("Broken", &["\\Junk"], 1, vec![1, 2]);
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.folders[0].select_fails = Some(Fail::Protocol);

        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(report.skipped_folders, 1);
        assert_eq!(report.new_messages, 3);
        assert_eq!(total_rows(&store, account), 3);
    }

    #[tokio::test]
    async fn connect_error_still_aborts_the_account() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.add_folder("Broken", &["\\Trash"], 1, vec![1]);
        source.add_folder("Later", &["\\Sent"], 1, vec![1]);
        source.folders[1].select_fails = Some(Fail::Connect);

        let result = sync(&store, &mut source, account).await;

        assert!(matches!(result, Err(SyncError::Connect(_))));
        assert_eq!(source.selects, vec!["INBOX", "Broken"]);
        // What was saved before the error stays saved.
        assert_eq!(total_rows(&store, account), 3);
    }

    #[tokio::test]
    async fn empty_folder_with_high_uid_next_is_not_rescanned() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![]);
        source.folders[0].uid_next = Some(1000);
        sync(&store, &mut source, account).await.unwrap();

        source.fetches.clear();
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 0);
        assert!(source.fetches.is_empty(), "{:?}", source.fetches);
    }

    #[tokio::test]
    async fn expunged_top_uids_are_not_refetched() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        // UIDs 4 to 899 existed once and were deleted.
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.folders[0].uid_next = Some(900);
        sync(&store, &mut source, account).await.unwrap();
        assert_eq!(store.folders(account).unwrap()[0].last_seen_uid, Some(899));

        source.fetches.clear();
        sync(&store, &mut source, account).await.unwrap();

        assert!(source.fetches.is_empty(), "{:?}", source.fetches);
        assert_eq!(total_rows(&store, account), 3);
    }

    #[tokio::test]
    async fn failed_batch_keeps_earlier_batches_and_is_retried() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=100).collect());
        sync(&store, &mut source, account).await.unwrap();

        // 700 new messages: 101..=600 come, the batch from 601 fails.
        source.folder("INBOX").uids.extend(101..=800);
        source.folders[0].fetch_fails_from = Some((601, Fail::Protocol));
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(report.skipped_folders, 1);
        assert_eq!(report.new_messages, 500);
        assert_eq!(total_rows(&store, account), 600);
        assert_eq!(store.folders(account).unwrap()[0].last_seen_uid, Some(600));

        // Next time the server answers: the failed range is fetched again.
        source.folders[0].fetch_fails_from = None;
        source.fetches.clear();
        let report = sync(&store, &mut source, account).await.unwrap();

        assert_eq!(source.fetches[0].0, 601);
        assert_eq!(report.new_messages, 200);
        assert_eq!(total_rows(&store, account), 800);
    }

    #[tokio::test]
    async fn fake_source_actions_behave_like_a_server() {
        // Keeps the fake honest for the app's tests of read/move/append.
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2]);
        source.add_folder("Trash", &["\\Trash"], 1, vec![]);
        source.select("INBOX").await.unwrap();

        assert!(source.fetch_body(1).await.unwrap().is_some());
        assert!(source.fetch_body(9).await.unwrap().is_none());
        source.set_seen(&[1], true).await.unwrap();
        assert_eq!(
            source.fetch_flags(1, 2).await.unwrap(),
            vec![(1, true), (2, false)]
        );
        source.move_to(&[2], "Trash").await.unwrap();
        source.append("Trash", b"raw", true).await.unwrap();

        assert_eq!(source.folder("INBOX").uids, vec![1]);
        assert_eq!(source.folder("Trash").uids, vec![1, 2]);
        assert_eq!(source.folder("Trash").seen, vec![2]);
    }
}
