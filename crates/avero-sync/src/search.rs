//! "Chercher aussi sur le serveur": an IMAP search in the folders that
//! matter, adding to the local copy the messages found that it lacked.

use crate::source::{FetchedHeader, MailSource, SyncError};
use avero_core::model::{AccountId, Folder, FolderRole, NewMessage};
use avero_core::store::Store;

/// Most messages one search adds per account.
pub const SEARCH_LIMIT: usize = 200;

/// What a search on the server found for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerHits {
    /// Messages that were not on this computer and now are.
    pub added: usize,
    /// The rows of every message found (added or already stored), newest
    /// first, at most [`SEARCH_LIMIT`] per folder: the local search may
    /// not find them (their text is not on this computer).
    pub rows: Vec<i64>,
}

/// Searches the inbox, the sent folder and the archive (already synced),
/// then the folder holding all the messages when the server has one
/// (Gmail's "All Mail", never synced as a whole), for what the user
/// typed. The headers of the messages found that are not stored yet are
/// fetched and stored, newest first, at most `limit` for the account.
///
/// In a synced folder, only messages older than its last sync are added
/// (the next sync brings the newer ones; adding them here would make it
/// skip the ones in between). A synced folder whose UIDs changed meaning
/// since its last sync is left to the sync. In the "all messages"
/// folder, a message the account already has in another folder (same
/// Message-ID) is not added again.
///
/// A folder the server refuses to open or search is skipped; connection
/// and database errors stop the search.
pub async fn search_server<S: MailSource>(
    store: &Store,
    source: &mut S,
    account: AccountId,
    query: &str,
    limit: usize,
) -> Result<ServerHits, SyncError> {
    let mut folders: Vec<Folder> = Vec::new();
    for role in [FolderRole::Inbox, FolderRole::Sent, FolderRole::Archive] {
        if let Some(folder) = store.folder_by_role(account, role)?
            && folder.uid_validity.is_some()
        {
            folders.push(folder);
        }
    }
    let all_mail = source.list_folders().await?.into_iter().find(|remote| {
        remote
            .attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\All"))
            && !remote
                .attributes
                .iter()
                .any(|a| a.eq_ignore_ascii_case("\\Noselect"))
    });
    if let Some(remote) = all_mail
        && !folders.iter().any(|f| f.path == remote.path)
    {
        // Known to the sync under another role: keep that role.
        let role = store
            .folders(account)?
            .into_iter()
            .find(|f| f.path == remote.path)
            .map_or(FolderRole::Other, |f| f.role);
        folders.push(store.upsert_folder(account, &remote.path, role)?);
    }

    let mut hits = ServerHits::default();
    for folder in folders {
        let budget = limit.saturating_sub(hits.added);
        match search_folder(store, source, account, &folder, query, budget, &mut hits).await {
            Ok(()) | Err(SyncError::Protocol(_)) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(hits)
}

/// Searches one folder; adds at most `budget` messages.
async fn search_folder<S: MailSource>(
    store: &Store,
    source: &mut S,
    account: AccountId,
    folder: &Folder,
    query: &str,
    budget: usize,
    hits: &mut ServerHits,
) -> Result<(), SyncError> {
    // Only the "all messages" folder is not synced.
    let synced = folder.role != FolderRole::Other;
    let status = source.select(&folder.path).await?;
    if folder.uid_validity != Some(status.uid_validity) {
        if synced {
            return Ok(());
        }
        store.reset_folder(folder.id, status.uid_validity)?;
    }
    let mut found = source.search_text(query).await?;
    // Newest first.
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.dedup();
    found.truncate(SEARCH_LIMIT);

    let known = store.known_uids(folder.id, &found)?;
    let newest_synced = folder.last_seen_uid.unwrap_or(0);
    let missing: Vec<u32> = found
        .iter()
        .copied()
        .filter(|uid| !known.contains(uid))
        .filter(|&uid| !synced || uid <= newest_synced)
        .take(budget)
        .collect();
    if !missing.is_empty() {
        let mut messages: Vec<NewMessage> = source
            .fetch_headers_of(&missing)
            .await?
            .iter()
            .map(FetchedHeader::parse)
            .collect();
        if !synced {
            let mut fresh = Vec::new();
            for message in messages {
                let elsewhere = match &message.message_id {
                    Some(id) => store.has_message_id(account, id)?,
                    None => false,
                };
                if !elsewhere {
                    fresh.push(message);
                }
            }
            messages = fresh;
        }
        store.insert_messages(folder.id, &messages)?;
        hits.added += messages.len();
    }
    hits.rows.extend(store.rows_of_uids(folder.id, &found)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::{FakeSource, setup, sync};
    use avero_core::model::AccountFilter;

    fn stored(store: &Store, account: AccountId, role: FolderRole) -> Vec<String> {
        let mut subjects: Vec<String> = store
            .unified(role, AccountFilter::Only(account), 1000, 0)
            .unwrap()
            .into_iter()
            .map(|item| item.subject)
            .collect();
        subjects.sort();
        subjects
    }

    #[tokio::test]
    async fn old_messages_found_on_the_server_are_added_once() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, (1..=5).collect());
        source.add_folder("Sent", &["\\Sent"], 1, vec![1, 2]);
        sync(&store, &mut source, account).await.unwrap();
        // The first sync took everything; forget the oldest inbox ones as
        // if they were out of its window.
        let inbox = store
            .folder_by_role(account, FolderRole::Inbox)
            .unwrap()
            .unwrap();
        let rows = store.rows_of_uids(inbox.id, &[1, 2]).unwrap();
        store.delete_messages(&rows).unwrap();
        source.folder("INBOX").found = vec![1, 2, 4];
        source.folder("Sent").found = vec![2];

        let hits = search_server(&store, &mut source, account, "m", SEARCH_LIMIT)
            .await
            .unwrap();
        assert_eq!(hits.added, 2);
        // Every message found, stored before or not.
        assert_eq!(hits.rows.len(), 4);
        assert_eq!(
            stored(&store, account, FolderRole::Inbox),
            vec!["m1", "m2", "m3", "m4", "m5"]
        );
        // Only the missing UIDs were fetched, in one request.
        assert_eq!(source.uid_fetches, vec![vec![2, 1]]);
        assert_eq!(source.searches, vec!["m", "m"]);

        // Again: nothing new, no duplicate.
        let again = search_server(&store, &mut source, account, "m", SEARCH_LIMIT)
            .await
            .unwrap();
        assert_eq!(again.added, 0);
        assert_eq!(again.rows.len(), 4);
        assert_eq!(stored(&store, account, FolderRole::Inbox).len(), 5);
    }

    #[tokio::test]
    async fn the_limit_keeps_the_newest_and_new_mail_is_left_to_the_sync() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![10]);
        sync(&store, &mut source, account).await.unwrap();
        // Older messages the first sync did not take, and one arrived
        // since the sync.
        source.folder("INBOX").uids = vec![1, 2, 3, 10, 11];
        source.folder("INBOX").found = vec![1, 2, 3, 11];
        let hits = search_server(&store, &mut source, account, "m", 2)
            .await
            .unwrap();
        assert_eq!(hits.added, 2);
        assert_eq!(
            stored(&store, account, FolderRole::Inbox),
            vec!["m10", "m2", "m3"]
        );
        // The sync still brings message 11.
        sync(&store, &mut source, account).await.unwrap();
        assert!(stored(&store, account, FolderRole::Inbox).contains(&"m11".to_string()));
    }

    #[tokio::test]
    async fn all_mail_is_searched_without_copying_known_messages() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![1]);
        source.add_folder("[Gmail]/Tous les messages", &["\\All"], 9, vec![1, 2, 3]);
        sync(&store, &mut source, account).await.unwrap();
        // The sync leaves "All Mail" aside.
        assert_eq!(store.folders(account).unwrap().len(), 1);
        // Its message 3 is the inbox's message 1 (same Message-ID).
        source.folder("[Gmail]/Tous les messages").same_as =
            vec![(3, "<1.INBOX@fake>".to_string())];
        source.folder("[Gmail]/Tous les messages").found = vec![2, 3];

        let hits = search_server(&store, &mut source, account, "m", SEARCH_LIMIT)
            .await
            .unwrap();
        assert_eq!(hits.added, 1);
        let all = store
            .folders(account)
            .unwrap()
            .into_iter()
            .find(|f| f.path == "[Gmail]/Tous les messages")
            .unwrap();
        assert_eq!(all.role, FolderRole::Other);
        assert_eq!(all.uid_validity, Some(9));
        // The new one is searchable and can be opened (its folder is known).
        let found = store
            .search_with("m2", None, AccountFilter::All, &hits.rows, 10)
            .unwrap();
        assert_eq!(found.len(), 1);
        let refs = store.message_refs(&found[0].row_ids).unwrap();
        assert_eq!(refs[0].folder_path, "[Gmail]/Tous les messages");
        assert_eq!(refs[0].uid, 2);
        // The sync still leaves it aside, and keeps what the search added.
        sync(&store, &mut source, account).await.unwrap();
        assert_eq!(stored(&store, account, FolderRole::Other), vec!["m2"]);
    }

    #[tokio::test]
    async fn a_refusing_folder_is_skipped() {
        let (store, account) = setup();
        let mut source = FakeSource::default();
        source.add_folder("INBOX", &[], 1, vec![5]);
        source.add_folder("Archives", &["\\Archive"], 1, vec![1, 9]);
        sync(&store, &mut source, account).await.unwrap();
        let archive = store
            .folder_by_role(account, FolderRole::Archive)
            .unwrap()
            .unwrap();
        let rows = store.rows_of_uids(archive.id, &[1]).unwrap();
        store.delete_messages(&rows).unwrap();
        source.folder("INBOX").search_fails = true;
        source.folder("Archives").found = vec![1];
        let hits = search_server(&store, &mut source, account, "m", SEARCH_LIMIT)
            .await
            .unwrap();
        assert_eq!(hits.added, 1);
    }
}
