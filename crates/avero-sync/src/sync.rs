//! Incremental synchronisation of one account.

use crate::parse::{detect_role, parse_headers};
use crate::source::{FetchedHeader, MailSource, SyncError};
use avero_core::model::{AccountId, FolderId, NewMessage};
use avero_core::store::Store;

/// Maximum number of UIDs asked from the server in one request.
pub const BATCH: u32 = 500;

/// What a synchronisation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub folders: usize,
    pub new_messages: usize,
}

/// Brings the local copy of every selectable folder up to date.
pub async fn sync_account<S: MailSource>(
    store: &Store,
    source: &mut S,
    account: AccountId,
) -> Result<SyncReport, SyncError> {
    let mut report = SyncReport {
        folders: 0,
        new_messages: 0,
    };

    for remote in source.list_folders().await? {
        let noselect = remote
            .attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Noselect"));
        if noselect {
            continue;
        }

        let role = detect_role(&remote.path, &remote.attributes);
        let folder = store.upsert_folder(account, &remote.path, role)?;
        let status = source.select(&remote.path).await?;

        // A new UIDVALIDITY (or none stored yet) means our UIDs mean nothing.
        let mut last_seen = folder.last_seen_uid;
        if folder.uid_validity != Some(status.uid_validity) {
            store.reset_folder(folder.id, status.uid_validity)?;
            last_seen = None;
        }

        let mut start = last_seen.map_or(1, |uid| uid + 1);
        match status.uid_next {
            Some(uid_next) => {
                while start < uid_next {
                    let end = start.saturating_add(BATCH - 1);
                    let headers = source.fetch_headers(start, Some(end)).await?;
                    report.new_messages += save_batch(store, folder.id, headers)?;
                    start = end.saturating_add(1);
                }
            }
            None => {
                let headers = source.fetch_headers(start, None).await?;
                report.new_messages += save_batch(store, folder.id, headers)?;
            }
        }
        report.folders += 1;
    }

    Ok(report)
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

    struct FakeFolder {
        path: String,
        attributes: Vec<String>,
        uid_validity: u32,
        uids: Vec<u32>,
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
            self.selected = Some(index);
            let folder = &self.folders[index];
            Ok(FolderStatus {
                uid_validity: folder.uid_validity,
                uid_next: Some(folder.uids.iter().max().map_or(1, |m| m + 1)),
            })
        }

        async fn fetch_headers(
            &mut self,
            from_uid: u32,
            to_uid: Option<u32>,
        ) -> Result<Vec<FetchedHeader>, SyncError> {
            self.fetches.push((from_uid, to_uid));
            let folder = &self.folders[self.selected.expect("select first")];
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
                },
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
}
