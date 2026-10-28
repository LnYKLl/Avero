//! Runs one account's synchronisation on its own thread.

use avero_core::store::Store;
use avero_core::{Account, AccountId, Credentials, ServerConfig};
use avero_sync::imap::ImapSource;
use avero_sync::source::SyncError;
use avero_sync::sync::{SyncReport, sync_account};
use std::future::Future;
use std::path::{Path, PathBuf};

/// Synchronises one account on a dedicated thread.
///
/// The thread has its own tokio runtime and its own `Store` (a SQLite
/// connection cannot be shared between threads). The result comes back
/// through a oneshot channel, so the returned future is `Send + 'static`
/// and the UI never blocks.
pub fn spawn_sync(
    db_path: PathBuf,
    account: Account,
    creds: Credentials,
) -> impl Future<Output = (AccountId, Result<SyncReport, String>)> + Send + 'static {
    let id = account.id;
    let (sender, receiver) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let result = run_sync(&db_path, &account, &creds);
        // The receiver may be gone if the UI gave up; nothing to do then.
        let _ = sender.send(result);
    });

    async move {
        let result = match receiver.await {
            Ok(result) => result,
            // The thread panicked: the sender was dropped without sending.
            Err(_) => Err("La synchronisation s'est arrêtée de façon inattendue".to_string()),
        };
        (id, result)
    }
}

/// Runs on the sync thread: builds a runtime and does the actual work.
fn run_sync(db_path: &Path, account: &Account, creds: &Credentials) -> Result<SyncReport, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Erreur interne : {e}"))?;

    runtime.block_on(async {
        let result = async {
            let store = Store::open(db_path)?;
            let mut source = ImapSource::connect(&account.imap, creds).await?;
            sync_account(&store, &mut source, account.id, |_| {}).await
        }
        .await;
        result.map_err(|e: SyncError| user_message(&e, &account.imap))
    })
}

/// Turns a sync error into a message for the user (never includes the password).
fn user_message(error: &SyncError, cfg: &ServerConfig) -> String {
    match error {
        SyncError::Connect(detail) => format!("Connexion impossible à {} : {detail}", cfg.host),
        SyncError::Auth(_) => format!("Identifiants refusés pour {}", cfg.username),
        SyncError::Protocol(detail) => format!("Erreur du serveur {} : {detail}", cfg.host),
        SyncError::Store(detail) => format!("Erreur de la base locale : {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::{AccountColor, NewAccount};

    fn temp_db(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("avero-test-{}-{tag}.db", std::process::id()))
    }

    fn account_at(host: &str, port: u16) -> Account {
        let store = Store::open_in_memory().unwrap();
        let mut account = store
            .add_account(NewAccount {
                name: "Test".into(),
                email: "a@x".into(),
                imap: ServerConfig {
                    host: host.into(),
                    port,
                    username: "a@x".into(),
                    security: avero_core::model::Security::Tls,
                },
                smtp: ServerConfig {
                    host: host.into(),
                    port,
                    username: "a@x".into(),
                    security: avero_core::model::Security::Tls,
                },
                auth: avero_core::model::AuthKind::Password,
            })
            .unwrap();
        account.color = AccountColor::palette(0);
        account
    }

    #[tokio::test]
    async fn unreachable_server_reports_error_without_panicking() {
        let account = account_at("127.0.0.1", 1);
        let (id, result) = spawn_sync(
            temp_db("one"),
            account.clone(),
            Credentials::Password("pw".into()),
        )
        .await;
        assert_eq!(id, account.id);
        let msg = result.unwrap_err();
        assert!(msg.contains("Connexion impossible"), "{msg}");
    }

    #[tokio::test]
    async fn two_accounts_fail_independently() {
        let a = spawn_sync(
            temp_db("a"),
            account_at("invalid.invalid", 993),
            Credentials::Password("pw".into()),
        );
        let b = spawn_sync(
            temp_db("b"),
            account_at("127.0.0.1", 1),
            Credentials::Password("pw".into()),
        );
        let ((_, ra), (_, rb)) = futures::join!(a, b);
        assert!(ra.is_err());
        assert!(rb.is_err());
    }

    #[test]
    fn user_message_maps_each_error_to_french() {
        let cfg = ServerConfig {
            host: "imap.ex.com".into(),
            port: 993,
            username: "a@x".into(),
            security: avero_core::model::Security::Tls,
        };
        let connect = user_message(&SyncError::Connect("timeout".into()), &cfg);
        assert_eq!(connect, "Connexion impossible à imap.ex.com : timeout");
        let auth = user_message(&SyncError::Auth("NO secret-detail".into()), &cfg);
        assert_eq!(auth, "Identifiants refusés pour a@x");
        let proto = user_message(&SyncError::Protocol("bad".into()), &cfg);
        assert_eq!(proto, "Erreur du serveur imap.ex.com : bad");
        // Opening a folder as a database gives a real StoreError.
        let store_err = Store::open(&std::env::temp_dir()).err().unwrap();
        let store = user_message(&SyncError::Store(store_err), &cfg);
        assert!(store.starts_with("Erreur de la base locale : "), "{store}");
    }
}
