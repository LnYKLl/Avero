//! Incremental synchronisation of one account.

use crate::parse::{detect_role, parse_headers};
use crate::source::{FetchedHeader, MailSource, SyncError};
use avero_core::model::{AccountId, Folder, FolderId, NewMessage};
use avero_core::store::Store;

/// Maximum number of UIDs asked from the server in one request.
pub const BATCH: u32 = 500;

/// What a synchronisation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Folders brought up to date.
    pub folders: usize,
    pub new_messages: usize,
    /// Folders left aside because the server refused a command on them.
    pub skipped_folders: usize,
}

/// Brings the local copy of every selectable folder up to date.
///
/// A protocol error on one folder (the server refuses SELECT or FETCH)
/// only skips that folder. Connection, login and database errors stop
/// the whole account: the next folders would fail the same way.
pub async fn sync_account<S: MailSource>(
    store: &Store,
    source: &mut S,
    account: AccountId,
) -> Result<SyncReport, SyncError> {
    let mut report = SyncReport {
        folders: 0,
        new_messages: 0,
        skipped_folders: 0,
    };

    for remote in source.list_folders().await? {
        if !is_selectable(&remote.attributes) {
            continue;
        }

        let role = detect_role(&remote.path, &remote.attributes);
        let folder = store.upsert_folder(account, &remote.path, role)?;
        match sync_folder(store, source, &folder, &mut report).await {
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

/// Fetches the new messages of one folder. The batches already saved
/// stay saved even if a later batch fails.
async fn sync_folder<S: MailSource>(
    store: &Store,
    source: &mut S,
    folder: &Folder,
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    let status = source.select(&folder.path).await?;

    // A new UIDVALIDITY (or none stored yet) means our UIDs mean nothing.
    let mut last_seen = folder.last_seen_uid;
    if folder.uid_validity != Some(status.uid_validity) {
        store.reset_folder(folder.id, status.uid_validity)?;
        last_seen = None;
    }

    let mut start = last_seen.map_or(1, |uid| uid.saturating_add(1));
    match status.uid_next {
        Some(uid_next) => {
            while start < uid_next {
                let end = start.saturating_add(BATCH - 1);
                let headers = source.fetch_headers(start, Some(end)).await?;
                report.new_messages += save_batch(store, folder.id, headers)?;
                start = end.saturating_add(1);
            }
            // Every UID below `uid_next` has now been asked for. If the
            // highest ones were deleted, remember it so we don't ask again.
            if uid_next > 0 {
                store.set_last_seen_uid(folder.id, uid_next - 1)?;
            }
        }
        None => {
            let headers = source.fetch_headers(start, None).await?;
            report.new_messages += save_batch(store, folder.id, headers)?;
        }
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
    use crate::source::{FetchedHeader, FolderStatus, RemoteFolder};
    use avero_core::model::{AccountFilter, FolderRole, NewAccount, ServerConfig};

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
    }

    impl FakeSource {
        fn add_folder(&mut self, path: &str, attrs: &[&str], validity: u32, uids: Vec<u32>) {
            self.folders.push(FakeFolder {
                path: path.to_string(),
                attributes: attrs.iter().map(|a| a.to_string()).collect(),
                uid_validity: validity,
                uids,
                uid_next: None,
                select_fails: None,
                fetch_fails_from: None,
            });
        }
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
            })
        }

        async fn fetch_headers(
            &mut self,
            from_uid: u32,
            to_uid: Option<u32>,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            self.fetches.push((from_uid, to_uid));
            let folder = &self.folders[self.selected.expect("select first")];
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
                    seen: false,
                    raw: format!(
                        "Message-ID: <{}@fake>\r\nFrom: a@b.c\r\nSubject: m{}\r\nDate: Mon, 1 Jan 2024 10:00:00 +0000\r\n\r\n",
                        uid, uid
                    )
                    .into_bytes(),
                })
                .collect())
        }
    }

    fn setup() -> (Store, AccountId) {
        let store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(NewAccount {
                name: "Test".into(),
                email: "t@example.com".into(),
                imap: ServerConfig {
                    host: "imap.example.com".into(),
                    port: 993,
                    username: "t".into(),
                    security: avero_core::model::Security::Tls,
                },
                smtp: ServerConfig {
                    host: "smtp.example.com".into(),
                    port: 993,
                    username: "t".into(),
                    security: avero_core::model::Security::Tls,
                },
                auth: avero_core::model::AuthKind::Password,
            })
            .unwrap();
        (store, account.id)
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

    #[tokio::test]
    async fn first_sync_imports_all_folders_and_roles() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.add_folder("Sent", &["\\Sent"], 1, vec![1]);

        let report = sync_account(&store, &mut source, account).await.unwrap();

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
        sync_account(&store, &mut source, account).await.unwrap();

        source.folders[0].uids.push(4);
        source.fetches.clear();
        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 1);
        assert_eq!(source.fetches[0].0, 4);
        assert_eq!(total_rows(&store, account), 4);
    }

    #[tokio::test]
    async fn uidvalidity_change_resets_folder() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        sync_account(&store, &mut source, account).await.unwrap();

        source.folders[0].uid_validity = 2;
        source.folders[0].uids = vec![10, 11];
        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 2);
        assert_eq!(total_rows(&store, account), 2);
    }

    #[tokio::test]
    async fn large_folder_is_fetched_in_batches() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=1203).collect());

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 1203);
        assert_eq!(total_rows(&store, account), 1203);
        assert!(source.fetches.len() >= 3);
        for (from, to) in &source.fetches {
            let to = to.expect("a bounded range");
            assert!(to - from < BATCH);
        }
    }

    #[tokio::test]
    async fn noselect_folders_are_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("[Gmail]", &["\\NoSelect", "\\HasChildren"], 1, vec![]);

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(source.selects, vec!["INBOX".to_string()]);
    }

    #[tokio::test]
    async fn sparse_uids_are_not_missed() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1, 2, 900, 1700]);

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.new_messages, 4);
        assert_eq!(total_rows(&store, account), 4);
    }

    #[tokio::test]
    async fn nonexistent_folders_are_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("Old", &["\\nonexistent"], 1, vec![]);

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(report.skipped_folders, 0);
        assert_eq!(source.selects, vec!["INBOX".to_string()]);
    }

    #[tokio::test]
    async fn protocol_error_on_select_skips_only_that_folder() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("Broken", &[], 1, vec![1, 2]);
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.folders[0].select_fails = Some(Fail::Protocol);

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.folders, 1);
        assert_eq!(report.skipped_folders, 1);
        assert_eq!(report.new_messages, 3);
        assert_eq!(total_rows(&store, account), 3);
    }

    #[tokio::test]
    async fn connect_error_still_aborts_the_account() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("Broken", &[], 1, vec![1]);
        source.add_folder("INBOX", &[], 1, vec![1, 2, 3]);
        source.folders[0].select_fails = Some(Fail::Connect);

        let result = sync_account(&store, &mut source, account).await;

        assert!(matches!(result, Err(SyncError::Connect(_))));
        assert_eq!(source.selects, vec!["Broken".to_string()]);
        assert_eq!(total_rows(&store, account), 0);
    }

    #[tokio::test]
    async fn empty_folder_with_high_uid_next_is_not_rescanned() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![]);
        source.folders[0].uid_next = Some(1000);
        sync_account(&store, &mut source, account).await.unwrap();

        source.fetches.clear();
        let report = sync_account(&store, &mut source, account).await.unwrap();

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
        sync_account(&store, &mut source, account).await.unwrap();
        assert_eq!(store.folders(account).unwrap()[0].last_seen_uid, Some(899));

        source.fetches.clear();
        sync_account(&store, &mut source, account).await.unwrap();

        assert!(source.fetches.is_empty(), "{:?}", source.fetches);
        assert_eq!(total_rows(&store, account), 3);
    }

    #[tokio::test]
    async fn failed_batch_keeps_earlier_batches_and_is_retried() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=700).collect());
        // The first batch is 1..=500; the second one (from 501) fails.
        source.folders[0].fetch_fails_from = Some((501, Fail::Protocol));

        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(report.skipped_folders, 1);
        assert_eq!(report.new_messages, 500);
        assert_eq!(total_rows(&store, account), 500);
        assert_eq!(store.folders(account).unwrap()[0].last_seen_uid, Some(500));

        // Next time the server answers: the failed range is fetched again.
        source.folders[0].fetch_fails_from = None;
        source.fetches.clear();
        let report = sync_account(&store, &mut source, account).await.unwrap();

        assert_eq!(source.fetches[0].0, 501);
        assert_eq!(report.new_messages, 200);
        assert_eq!(total_rows(&store, account), 700);
    }
}
