//! End to end against a real IMAP/SMTP server running locally (GreenMail),
//! through the same code paths as the app: SMTP sending, APPEND to Sent,
//! sync into a store, body download and parsing, flags, moves, deletions,
//! and IDLE.
//!
//! Run everything with `scripts/e2e-local.ps1` (see `docs/e2e-local.md`),
//! which starts the server and sets these variables:
//!
//! - `AVERO_TEST_CA_PEM`: PEM file of the test authority that signed the
//!   server's certificate (read by the `test-ca` feature);
//! - `AVERO_E2E_HOST`: server host (default `127.0.0.1`);
//! - `AVERO_E2E_IMAP_PORT`: IMAP over implicit TLS (default 3993);
//! - `AVERO_E2E_SMTP_PORT`: SMTP over implicit TLS (default 3465);
//! - `AVERO_E2E_ALICE`, `AVERO_E2E_ALICE_PASSWORD`: the sender's account;
//! - `AVERO_E2E_BOB`, `AVERO_E2E_BOB_PASSWORD`: the recipient's account.
//!
//! By hand: `cargo test -p avero-sync --features test-ca --test e2e_local
//! -- --ignored --test-threads=1`. Passwords are never printed.

#![cfg(feature = "test-ca")]

use avero_core::model::{
    AccountFilter, AccountId, AuthKind, Credentials, FolderRole, NewAccount, Security,
    ServerConfig, UnifiedItem,
};
use avero_core::store::Store;
use avero_sync::imap::{ImapSource, ImapWatch};
use avero_sync::parse::{attachment_data, parse_body};
use avero_sync::smtp::{self, Outgoing, OutgoingAttachment, Recipient, SendError};
use avero_sync::source::{MailSource, SyncError};
use avero_sync::sync::sync_account;
use avero_sync::watch::{Change, InboxWatch};
use std::time::Duration;

/// One test account: its address (also its login) and password.
struct User {
    addr: String,
    password: String,
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (see docs/e2e-local.md)"))
}

fn port(name: &str, default: u16) -> u16 {
    std::env::var(name).map_or(default, |p| p.parse().expect(name))
}

fn host() -> String {
    std::env::var("AVERO_E2E_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn alice() -> User {
    User {
        addr: var("AVERO_E2E_ALICE"),
        password: var("AVERO_E2E_ALICE_PASSWORD"),
    }
}

fn bob() -> User {
    User {
        addr: var("AVERO_E2E_BOB"),
        password: var("AVERO_E2E_BOB_PASSWORD"),
    }
}

fn imap(user: &User) -> ServerConfig {
    ServerConfig {
        host: host(),
        port: port("AVERO_E2E_IMAP_PORT", 3993),
        username: user.addr.clone(),
        security: Security::Tls,
    }
}

fn smtp_cfg(user: &User) -> ServerConfig {
    ServerConfig {
        host: host(),
        port: port("AVERO_E2E_SMTP_PORT", 3465),
        username: user.addr.clone(),
        security: Security::Tls,
    }
}

fn password(user: &User) -> Credentials {
    Credentials::Password(user.password.clone())
}

fn wrong_password() -> Credentials {
    Credentials::Password("certainement-pas-le-bon".to_string())
}

async fn connect(user: &User) -> ImapSource {
    ImapSource::connect(&imap(user), &password(user))
        .await
        .expect("IMAP login")
}

/// Creates `path` unless the server already lists it.
async fn ensure_folder(source: &mut ImapSource, path: &str) {
    let folders = source.list_folders().await.unwrap();
    if !folders.iter().any(|f| f.path == path) {
        source.create_folder(path).await.unwrap();
    }
}

/// Deletes every message of `path` (leftovers of an earlier run).
async fn empty_folder(source: &mut ImapSource, path: &str) {
    let status = source.select(path).await.unwrap();
    if status.exists > 0 {
        let uids = source.fetch_uids(1).await.unwrap();
        source.delete_permanently(&uids).await.unwrap();
    }
    assert_eq!(source.select(path).await.unwrap().exists, 0, "{path}");
}

/// A store holding one password account for `user`.
fn store_for(user: &User) -> (Store, AccountId) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Bob".to_string(),
            email: user.addr.clone(),
            imap: imap(user),
            smtp: smtp_cfg(user),
            auth: AuthKind::Password,
        })
        .unwrap();
    (store, account.id)
}

fn inbox(store: &Store, account: AccountId) -> Vec<UnifiedItem> {
    store
        .unified(FolderRole::Inbox, AccountFilter::Only(account), 100, 0)
        .unwrap()
}

fn in_role(store: &Store, account: AccountId, role: FolderRole) -> Vec<UnifiedItem> {
    store
        .unified(role, AccountFilter::Only(account), 100, 0)
        .unwrap()
}

async fn sync(store: &Store, source: &mut ImapSource, account: AccountId) -> usize {
    let report = sync_account(store, source, account, |_| {}).await.unwrap();
    assert_eq!(report.skipped_folders, 0, "{report:?}");
    report.new_messages
}

/// A message from Alice to Bob.
fn outgoing(alice: &User, bob: &User, subject: &str, body: &str) -> Outgoing {
    Outgoing {
        from_name: Some("Alice Martin".to_string()),
        from_addr: alice.addr.clone(),
        to: vec![Recipient::new(Some("Bob Durand"), &bob.addr)],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.to_string(),
        body: body.to_string(),
        in_reply_to: None,
        references: None,
        attachments: Vec::new(),
    }
}

/// Bytes that look like a PDF, with every byte value (so the base64
/// round trip is really tested).
fn pdf_bytes() -> Vec<u8> {
    let mut data = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj << /Type /Catalog >> endobj\n".to_vec();
    data.extend((0..=255u8).cycle().take(5000));
    data.extend_from_slice(b"\n%%EOF\n");
    data
}

/// Waits until `user`'s INBOX holds `count` messages (SMTP delivery is
/// asynchronous in GreenMail).
async fn wait_for_inbox(user: &User, count: u32) {
    let mut source = connect(user).await;
    for _ in 0..50 {
        if source.select("INBOX").await.unwrap().exists >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{count} messages never arrived");
}

#[tokio::test]
#[ignore]
async fn smtp_check_accepts_the_right_password() {
    let alice = alice();
    smtp::check(&smtp_cfg(&alice), &password(&alice))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn smtp_check_refuses_a_wrong_password() {
    let alice = alice();
    let result = smtp::check(&smtp_cfg(&alice), &wrong_password()).await;
    assert!(matches!(result, Err(SendError::Auth(_))), "{result:?}");
}

#[tokio::test]
#[ignore]
async fn smtp_send_refuses_a_wrong_password() {
    let (alice, bob) = (alice(), bob());
    let msg = outgoing(&alice, &bob, "jamais envoyé", "non");
    let result = smtp::send(&smtp_cfg(&alice), &wrong_password(), &msg).await;
    assert!(matches!(result, Err(SendError::Auth(_))), "{result:?}");
}

#[tokio::test]
#[ignore]
async fn imap_refuses_a_wrong_password() {
    let bob = bob();
    let result = ImapSource::connect(&imap(&bob), &wrong_password()).await;
    assert!(
        matches!(result, Err(SyncError::Auth(_))),
        "{:?}",
        result.err()
    );
}

/// "Envoyés" and "Courrier indésirable" as a French server names them
/// on the wire (modified UTF-7, RFC 3501 5.1.3).
const SENT_FR: &str = "Envoy&AOk-s";
const JUNK_FR: &str = "Courrier ind&AOk-sirable";

/// Folders without SPECIAL-USE attributes are recognised by their name,
/// accents included: the sent copies must show up in "Envoyés".
#[tokio::test]
#[ignore]
async fn french_folder_names_get_their_role() {
    let (alice, bob) = (alice(), bob());
    let mut imap = connect(&bob).await;
    for path in [SENT_FR, JUNK_FR] {
        ensure_folder(&mut imap, path).await;
        empty_folder(&mut imap, path).await;
    }
    let raw = smtp::build_message(&outgoing(&bob, &alice, "Copie envoyée", "Bonjour"))
        .unwrap()
        .formatted();
    imap.append(SENT_FR, &raw, true).await.unwrap();

    let (store, account) = store_for(&bob);
    sync(&store, &mut imap, account).await;
    let role_of = |path: &str| {
        store
            .folders(account)
            .unwrap()
            .into_iter()
            .find(|f| f.path == path)
            .map(|f| f.role)
    };
    assert_eq!(role_of(SENT_FR), Some(FolderRole::Sent));
    assert_eq!(role_of(JUNK_FR), Some(FolderRole::Junk));
    let sent = in_role(&store, account, FolderRole::Sent);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].subject, "Copie envoyée");

    // Leave Bob's folders empty for the other tests.
    empty_folder(&mut imap, SENT_FR).await;
}

/// A big attachment (4 MB) survives SMTP, the server and BODY.PEEK[].
#[tokio::test]
#[ignore]
async fn large_attachment_round_trip() {
    let (alice, bob) = (alice(), bob());
    let mut imap = connect(&bob).await;
    empty_folder(&mut imap, "INBOX").await;
    let data: Vec<u8> = (0..4_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let mut msg = outgoing(&alice, &bob, "Gros fichier", "Ci-joint.");
    msg.attachments.push(OutgoingAttachment {
        name: "données.bin".to_string(),
        mime: "application/octet-stream".to_string(),
        data: data.clone(),
    });
    smtp::send(&smtp_cfg(&alice), &password(&alice), &msg)
        .await
        .unwrap();
    wait_for_inbox(&bob, 1).await;

    imap.select("INBOX").await.unwrap();
    let uids = imap.fetch_uids(1).await.unwrap();
    assert_eq!(uids.len(), 1);
    let raw = imap.fetch_body(uids[0]).await.unwrap().expect("body");
    let body = parse_body(&raw);
    assert_eq!(body.attachments.len(), 1);
    assert_eq!(body.attachments[0].name, "données.bin");
    assert_eq!(attachment_data(&raw, 0).unwrap(), data);
    empty_folder(&mut imap, "INBOX").await;
}

/// The whole life of three messages, from Alice's SMTP to Bob's trash.
#[tokio::test]
#[ignore]
async fn mail_goes_round_end_to_end() {
    let (alice, bob) = (alice(), bob());

    // A clean start, even when GreenMail was not restarted.
    let mut alice_imap = connect(&alice).await;
    ensure_folder(&mut alice_imap, "Sent").await;
    empty_folder(&mut alice_imap, "Sent").await;
    let mut bob_imap = connect(&bob).await;
    ensure_folder(&mut bob_imap, "Trash").await;
    ensure_folder(&mut bob_imap, "Archive").await;
    for folder in bob_imap.list_folders().await.unwrap() {
        empty_folder(&mut bob_imap, &folder.path).await;
    }

    // Alice sends three messages; each copy goes to her Sent folder, as
    // the app does after sending.
    let smtp = smtp_cfg(&alice);
    let creds = password(&alice);
    let pdf = pdf_bytes();

    let mut first = outgoing(&alice, &bob, "Facture d'octobre", "Voici la facture.");
    first.attachments.push(OutgoingAttachment {
        name: "facture-octobre.pdf".to_string(),
        mime: "application/pdf".to_string(),
        data: pdf.clone(),
    });
    let first_raw = smtp::send(&smtp, &creds, &first).await.unwrap();
    let first_id = parse_body(&first_raw).message_id.expect("Message-ID");
    alice_imap.append("Sent", &first_raw, true).await.unwrap();
    // Distinct Date headers (one-second resolution) make the order certain.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let second = outgoing(
        &alice,
        &bob,
        "Réunion à 14 h",
        "On se voit à 14 h, salle B.",
    );
    let second_raw = smtp::send(&smtp, &creds, &second).await.unwrap();
    alice_imap.append("Sent", &second_raw, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let mut third = outgoing(&alice, &bob, "Re: Facture d'octobre", "Payée hier.");
    third.in_reply_to = Some(first_id.clone());
    third.references = Some(first_id.clone());
    let third_raw = smtp::send(&smtp, &creds, &third).await.unwrap();
    alice_imap.append("Sent", &third_raw, true).await.unwrap();

    // Alice's Sent folder, synced like any account.
    let (alice_store, alice_account) = store_for(&alice);
    sync(&alice_store, &mut alice_imap, alice_account).await;
    let sent = in_role(&alice_store, alice_account, FolderRole::Sent);
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert!(sent.iter().all(|m| m.seen), "copies are appended as read");

    // Bob syncs: three new messages, newest first, as sent.
    wait_for_inbox(&bob, 3).await;
    let (store, account) = store_for(&bob);
    assert_eq!(sync(&store, &mut bob_imap, account).await, 3);
    let items = inbox(&store, account);
    let subjects: Vec<&str> = items.iter().map(|m| m.subject.as_str()).collect();
    assert_eq!(
        subjects,
        [
            "Re: Facture d'octobre",
            "Réunion à 14 h",
            "Facture d'octobre"
        ]
    );
    for item in &items {
        assert_eq!(item.from_addr, alice.addr);
        assert_eq!(item.from_display, "Alice Martin");
        assert!(!item.seen, "{}", item.subject);
    }
    assert!(items[2].has_attachments, "multipart/mixed seen in headers");
    assert!(!items[1].has_attachments);
    assert!(!items[0].has_attachments);
    assert!(items[0].date > items[1].date && items[1].date > items[2].date);

    // Accents are ignored by the search, case too.
    let found = store
        .search("reunion", None, AccountFilter::Only(account), 10)
        .unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].subject, "Réunion à 14 h");

    // A second sync finds nothing new and duplicates nothing.
    assert_eq!(sync(&store, &mut bob_imap, account).await, 0);
    assert_eq!(inbox(&store, account).len(), 3);

    // Opening the message with the attachment, as the reader does.
    let refs = store.message_refs(&items[2].row_ids).unwrap();
    let invoice = &refs[0];
    bob_imap.select(&invoice.folder_path).await.unwrap();
    let raw = bob_imap
        .fetch_body(invoice.uid)
        .await
        .unwrap()
        .expect("body");
    let body = parse_body(&raw);
    assert_eq!(body.subject, "Facture d'octobre");
    assert_eq!(body.text.trim_end(), "Voici la facture.");
    assert_eq!(body.from.as_ref().unwrap().addr, alice.addr);
    assert_eq!(body.to[0].addr, bob.addr);
    assert_eq!(body.to[0].name.as_deref(), Some("Bob Durand"));
    assert_eq!(body.message_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(body.attachments.len(), 1);
    assert_eq!(body.attachments[0].name, "facture-octobre.pdf");
    assert_eq!(body.attachments[0].mime, "application/pdf");
    assert_eq!(body.attachments[0].size, pdf.len());
    assert_eq!(attachment_data(&raw, 0).unwrap(), pdf);
    store
        .put_body(
            invoice.row_id,
            &raw,
            &body.text,
            !body.attachments.is_empty(),
        )
        .unwrap();
    // BODY.PEEK: reading the body does not mark it read on the server.
    assert_eq!(sync(&store, &mut bob_imap, account).await, 0);
    assert!(!inbox(&store, account)[2].seen);
    // The cached body text is searchable too.
    let found = store
        .search("facture", None, AccountFilter::Only(account), 10)
        .unwrap();
    assert_eq!(found.len(), 2, "{found:?}");

    // The reply's thread headers made it through.
    let reply = &store.message_refs(&items[0].row_ids).unwrap()[0];
    bob_imap.select(&reply.folder_path).await.unwrap();
    let reply_raw = bob_imap.fetch_body(reply.uid).await.unwrap().unwrap();
    let reply_body = parse_body(&reply_raw);
    assert_eq!(reply_body.references.as_deref(), Some(first_id.as_str()));
    let reply_text = String::from_utf8_lossy(&reply_raw);
    assert!(
        reply_text.contains(&format!("In-Reply-To: {first_id}")),
        "{reply_text}"
    );
    assert_eq!(reply_body.text.trim_end(), "Payée hier.");
    // ...and were stored at sync: the reply and the invoice make one
    // conversation.
    let stored = store.headers(reply.row_id).unwrap().unwrap();
    assert_eq!(stored.in_reply_to.as_deref(), Some(first_id.as_str()));
    assert_eq!(stored.references, vec![first_id.clone()]);
    assert_eq!(stored.to, vec![bob.addr.clone()]);
    let threads = store
        .threads(FolderRole::Inbox, AccountFilter::Only(account), 10, 0)
        .unwrap();
    let counts: Vec<usize> = threads.iter().map(|t| t.count).collect();
    assert_eq!(counts, vec![2, 1], "{threads:?}");

    // Read, then unread again: the server's flag comes back on sync.
    let meeting = &store.message_refs(&items[1].row_ids).unwrap()[0];
    bob_imap.select(&meeting.folder_path).await.unwrap();
    bob_imap.set_seen(&[meeting.uid], true).await.unwrap();
    sync(&store, &mut bob_imap, account).await;
    assert!(inbox(&store, account)[1].seen);
    assert_eq!(
        store
            .unread_count(FolderRole::Inbox, AccountFilter::Only(account))
            .unwrap(),
        2
    );
    bob_imap.select(&meeting.folder_path).await.unwrap();
    bob_imap.set_seen(&[meeting.uid], false).await.unwrap();
    sync(&store, &mut bob_imap, account).await;
    assert!(!inbox(&store, account)[1].seen);

    // The meeting goes to the archive: gone from the inbox, found there.
    bob_imap.select(&meeting.folder_path).await.unwrap();
    bob_imap.move_to(&[meeting.uid], "Archive").await.unwrap();
    sync(&store, &mut bob_imap, account).await;
    let subjects: Vec<String> = inbox(&store, account)
        .into_iter()
        .map(|m| m.subject)
        .collect();
    assert_eq!(subjects, ["Re: Facture d'octobre", "Facture d'octobre"]);
    let archived = in_role(&store, account, FolderRole::Archive);
    assert_eq!(archived.len(), 1, "{archived:?}");
    assert_eq!(archived[0].subject, "Réunion à 14 h");

    // The reply goes to the trash.
    bob_imap.select("INBOX").await.unwrap();
    bob_imap.move_to(&[reply.uid], "Trash").await.unwrap();
    sync(&store, &mut bob_imap, account).await;
    assert_eq!(inbox(&store, account).len(), 1);
    let trashed = in_role(&store, account, FolderRole::Trash);
    assert_eq!(trashed.len(), 1, "{trashed:?}");
    assert_eq!(trashed[0].subject, "Re: Facture d'octobre");

    // Deleted for good from the trash, as the app does: server first,
    // then the local row once the server confirmed.
    let in_trash = &store.message_refs(&trashed[0].row_ids).unwrap()[0];
    bob_imap.select(&in_trash.folder_path).await.unwrap();
    bob_imap.delete_permanently(&[in_trash.uid]).await.unwrap();
    store.delete_messages(&[in_trash.row_id]).unwrap();
    assert_eq!(bob_imap.select("Trash").await.unwrap().exists, 0);
    assert_eq!(sync(&store, &mut bob_imap, account).await, 0);
    assert!(in_role(&store, account, FolderRole::Trash).is_empty());

    // Deleted for good straight from the inbox by another client: the
    // next sync notices the expunge (apply_flags) and drops the row.
    bob_imap.select("INBOX").await.unwrap();
    bob_imap.delete_permanently(&[invoice.uid]).await.unwrap();
    sync(&store, &mut bob_imap, account).await;
    assert!(inbox(&store, account).is_empty());
    let found = store
        .search("facture", None, AccountFilter::Only(account), 10)
        .unwrap();
    assert!(found.is_empty(), "{found:?}");

    // New mail after the inbox was emptied: found by the incremental
    // sync (UIDs above the last one seen), once.
    let late = outgoing(&alice, &bob, "Dernier point", "Rien d'autre.");
    smtp::send(&smtp, &creds, &late).await.unwrap();
    wait_for_inbox(&bob, 1).await;
    assert_eq!(sync(&store, &mut bob_imap, account).await, 1);
    assert_eq!(sync(&store, &mut bob_imap, account).await, 0);
    let subjects: Vec<String> = inbox(&store, account)
        .into_iter()
        .map(|m| m.subject)
        .collect();
    assert_eq!(subjects, ["Dernier point"]);

    // A fresh store sees exactly what is left on the server.
    let (fresh, fresh_account) = store_for(&bob);
    assert_eq!(sync(&fresh, &mut bob_imap, fresh_account).await, 2);
    assert_eq!(inbox(&fresh, fresh_account)[0].subject, "Dernier point");
    assert_eq!(
        in_role(&fresh, fresh_account, FolderRole::Archive)[0].subject,
        "Réunion à 14 h"
    );
}

/// IDLE: a mail delivered while the watcher waits wakes it up, and the
/// sync that follows reports it as new (to be notified), unlike the
/// messages of the first fill.
#[tokio::test]
#[ignore]
async fn idle_wakes_up_when_a_mail_arrives() {
    let (alice, bob) = (alice(), bob());
    let (store, account) = store_for(&bob);
    let mut source = connect(&bob).await;
    let report = sync_account(&store, &mut source, account, |_| {})
        .await
        .unwrap();
    assert!(report.arrivals.is_empty(), "first fill: {report:?}");

    let mut watch = ImapWatch::connect(&imap(&bob), &password(&bob))
        .await
        .expect("IMAP login for the watcher");
    assert!(watch.uses_idle(), "GreenMail announces IDLE");
    let subject = format!("Réveil IDLE {}", std::process::id());
    let send = async {
        // Leave the watcher time to enter IDLE.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let msg = outgoing(&alice, &bob, &subject, "Coucou");
        smtp::send(&smtp_cfg(&alice), &password(&alice), &msg)
            .await
            .unwrap();
    };
    let waiting = tokio::time::timeout(Duration::from_secs(60), watch.wait());
    let (woken, ()) = tokio::join!(waiting, send);
    let change = woken.expect("woken up within a minute").unwrap();
    assert_eq!(change, Change::Changed);

    let report = sync_account(&store, &mut source, account, |_| {})
        .await
        .unwrap();
    assert!(
        report
            .arrivals
            .iter()
            .any(|a| a.subject == subject && a.from_name.as_deref() == Some("Alice Martin")),
        "{report:?}"
    );
}
