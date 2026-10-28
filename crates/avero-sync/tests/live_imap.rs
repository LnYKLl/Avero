//! Tests of the real IMAP client.

use avero_core::model::{FolderRole, NewAccount, ServerConfig};
use avero_core::store::Store;
use avero_sync::imap::ImapSource;
use avero_sync::source::SyncError;
use avero_sync::sync::sync_account;
use std::time::{Duration, Instant};

#[tokio::test]
async fn bad_host_is_connect_error() {
    let cfg = ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 1,
        username: "nobody".to_string(),
    };
    let start = Instant::now();
    let result = ImapSource::connect(&cfg, "secret").await;
    assert!(matches!(result, Err(SyncError::Connect(_))));
    assert!(start.elapsed() < Duration::from_secs(10));
}

/// Needs a real account: set AVERO_TEST_IMAP_HOST, AVERO_TEST_IMAP_USER and
/// AVERO_TEST_IMAP_PASS, then run `cargo test -p avero-sync -- --ignored live_sync`.
#[tokio::test]
#[ignore]
async fn live_sync_against_real_server() {
    let host = std::env::var("AVERO_TEST_IMAP_HOST").expect("AVERO_TEST_IMAP_HOST");
    let user = std::env::var("AVERO_TEST_IMAP_USER").expect("AVERO_TEST_IMAP_USER");
    let pass = std::env::var("AVERO_TEST_IMAP_PASS").expect("AVERO_TEST_IMAP_PASS");
    let imap = ServerConfig {
        host,
        port: 993,
        username: user.clone(),
    };

    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(NewAccount {
            name: "live".to_string(),
            email: user,
            imap: imap.clone(),
        })
        .unwrap();

    let mut source = ImapSource::connect(&imap, &pass).await.unwrap();
    let report = sync_account(&store, &mut source, account.id).await.unwrap();

    assert!(report.folders >= 1);
    let folders = store.folders(account.id).unwrap();
    assert!(folders.iter().any(|f| f.role == FolderRole::Inbox));
}
