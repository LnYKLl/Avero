//! Tests of "Plus tard", drafts and "Programmés", on a store in demo
//! mode, with a clock set by hand (no waiting).

use super::later::{INTERRUPTED, LATE_AFTER};
use super::*;
use crate::later::{Clock, When};
use avero_core::{DraftCopy, DraftFields, DraftFile, Listing, NewAccount, NewMessage};

/// Now, for every test: Tuesday 27 January 2027, well after the mail.
const NOW: i64 = 1_800_000_000;

fn account(store: &Store, email: &str) -> AccountId {
    let server = ServerConfig {
        host: "imap.example.com".to_string(),
        port: 993,
        username: email.to_string(),
        security: Security::Tls,
    };
    store
        .add_account(NewAccount {
            name: "Marie".to_string(),
            email: email.to_string(),
            imap: server.clone(),
            smtp: server,
            auth: avero_core::AuthKind::Password,
        })
        .unwrap()
        .id
}

/// An account with an inbox of `count` read messages (uid 1 the
/// oldest), and an empty drafts folder.
fn setup(store: &Store, count: u32) -> AccountId {
    let id = account(store, "marie@ex.fr");
    let inbox = store.upsert_folder(id, "INBOX", FolderRole::Inbox).unwrap();
    store
        .upsert_folder(id, "Drafts", FolderRole::Drafts)
        .unwrap();
    let messages: Vec<NewMessage> = (1..=count)
        .map(|uid| NewMessage {
            uid,
            message_id: Some(format!("<{uid}@ex.fr>")),
            subject: format!("Sujet {uid}"),
            from_name: Some(format!("Paul {uid}")),
            from_addr: "paul@ex.fr".to_string(),
            date: 1_700_000_000 + i64::from(uid),
            seen: true,
            ..Default::default()
        })
        .collect();
    store.insert_messages(inbox.id, &messages).unwrap();
    id
}

fn app_on(store: Store) -> App {
    let (mut app, _task) = App::new(store, Backend::Demo, Settings::default(), None);
    app.clock = Clock::fixed(NOW);
    app
}

fn app_with_mail(count: u32) -> App {
    let store = Store::open_in_memory().unwrap();
    setup(&store, count);
    app_on(store)
}

fn key(app: &mut App, c: &str) {
    let _ = app.update(Message::Key(
        keyboard::Key::Character(c.into()),
        keyboard::Modifiers::default(),
    ));
}

fn later(app: &mut App, message: LaterMsg) {
    let _ = app.update(Message::Later(message));
}

fn compose(app: &mut App, message: ComposeMsg) {
    let _ = app.update(Message::Compose(message));
}

fn subjects(app: &App) -> Vec<String> {
    app.items.iter().map(|i| i.subject.clone()).collect()
}

/// A temporary database file (for what must survive a restart).
fn temp_db(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("avero-later-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

// ------------------------------------------------------------ snooze

#[test]
fn h_snoozes_the_conversation_until_the_time_chosen() {
    let mut app = app_with_mail(3);
    assert_eq!(subjects(&app), vec!["Sujet 3", "Sujet 2", "Sujet 1"]);
    let _ = app.update(Message::MessageSelected(2));
    key(&mut app, "h");
    let Some(Overlay::Later(picker)) = &app.overlay else {
        panic!("the choices are asked");
    };
    assert_eq!(picker.choices(), &When::SNOOZE);
    later(&mut app, LaterMsg::Pick(When::Tomorrow));
    assert!(app.overlay.is_none());
    // Gone from the inbox, and from the reader.
    assert_eq!(subjects(&app), vec!["Sujet 3", "Sujet 2"]);
    assert!(app.reading.is_none());
    assert!(
        app.toast
            .as_ref()
            .unwrap()
            .text
            .starts_with("Rappel demain à 08:00")
    );
    // Listed in "Plus tard" with its time.
    later(&mut app, LaterMsg::ShowSnoozed);
    assert_eq!(subjects(&app), vec!["Sujet 1"]);
    let until = app.items[0].snoozed_until.unwrap();
    let expected = crate::later::resolve(When::Tomorrow, &app.clock.local()).timestamp();
    assert_eq!(until, expected);
    assert!(until > NOW);
}

#[test]
fn woken_conversation_is_back_on_top_unread_with_a_reminder() {
    let mut app = app_with_mail(3);
    let rows = app.items[2].row_ids.clone();
    let _ = app.snooze(&rows, NOW + 3600);
    // Not yet.
    app.clock.set(NOW + 3599);
    let (woken, _) = app.wake_due();
    assert!(woken.is_empty());
    assert_eq!(subjects(&app), vec!["Sujet 3", "Sujet 2"]);
    // Its time: on top (sorted by when it woke up), unread.
    app.clock.set(NOW + 3600);
    let (woken, _) = app.wake_due();
    assert_eq!(woken.len(), 1);
    assert_eq!(subjects(&app), vec!["Sujet 1", "Sujet 3", "Sujet 2"]);
    assert!(!app.items[0].seen);
    assert_eq!(
        app.reminders(&woken),
        vec![crate::notify::Notice {
            title: "Rappel : Paul 1".into(),
            body: "Sujet 1".into(),
        }]
    );
    // Notifications off: none.
    app.settings.notifications = false;
    assert!(app.reminders(&woken).is_empty());
    // The timer does the same (nothing left to wake).
    later(&mut app, LaterMsg::Tick);
    later(&mut app, LaterMsg::ShowSnoozed);
    assert!(app.items.is_empty());
}

#[test]
fn reminder_due_while_closed_wakes_at_start() {
    let path = temp_db("wake");
    {
        let store = Store::open(&path).unwrap();
        setup(&store, 2);
        let rows: Vec<i64> = store
            .list(
                &Listing::Folder(FolderRole::Inbox),
                AccountFilter::All,
                true,
                10,
                0,
            )
            .unwrap()
            .into_iter()
            .flat_map(|i| i.row_ids)
            .collect();
        store.snooze(&rows[1..], NOW - 60).unwrap();
    }
    let mut app = app_on(Store::open(&path).unwrap());
    app.settings.notifications = false;
    assert_eq!(subjects(&app), vec!["Sujet 2"]);
    // The first tick (sent by `App::new`).
    later(&mut app, LaterMsg::Tick);
    assert_eq!(subjects(&app), vec!["Sujet 1", "Sujet 2"]);
    drop(app);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reminder_is_cancelled_from_later() {
    let mut app = app_with_mail(2);
    let _ = app.update(Message::MessageSelected(1));
    key(&mut app, "h");
    later(&mut app, LaterMsg::Pick(When::Monday));
    later(&mut app, LaterMsg::ShowSnoozed);
    assert_eq!(subjects(&app), vec!["Sujet 1"]);
    let _ = app.update(Message::MessageSelected(0));
    assert!(app.selection_snoozed());
    // `h` in "Plus tard" cancels.
    key(&mut app, "h");
    assert!(app.overlay.is_none());
    assert!(app.items.is_empty());
    let _ = app.update(Message::FolderSelected(FolderRole::Inbox));
    assert_eq!(subjects(&app), vec!["Sujet 2", "Sujet 1"]);
    assert!(app.store.next_wake().unwrap().is_none());
}

#[test]
fn typed_date_is_checked() {
    let mut app = app_with_mail(1);
    let _ = app.update(Message::MessageSelected(0));
    later(&mut app, LaterMsg::Snooze);
    later(&mut app, LaterMsg::CustomText("hier".into()));
    later(&mut app, LaterMsg::PickCustom);
    let Some(Overlay::Later(picker)) = &app.overlay else {
        panic!("still asking");
    };
    assert!(picker.error.is_some());
    later(&mut app, LaterMsg::CustomText("15/03/2027 9h30".into()));
    later(&mut app, LaterMsg::PickCustom);
    assert!(app.overlay.is_none());
    later(&mut app, LaterMsg::ShowSnoozed);
    let until = app.items[0].snoozed_until.unwrap();
    let local = chrono::TimeZone::timestamp_opt(&chrono::Local, until, 0).unwrap();
    assert_eq!(
        local.format("%d/%m/%Y %H:%M").to_string(),
        "15/03/2027 09:30"
    );
}

// ------------------------------------------------------------ drafts

/// Opens a new composer and types in it.
fn write(app: &mut App, subject: &str) {
    compose(app, ComposeMsg::New);
    compose(app, ComposeMsg::To("Léa <lea@ex.fr>, zo".into()));
    compose(app, ComposeMsg::Bcc("chef@ex.fr".into()));
    compose(app, ComposeMsg::Subject(subject.into()));
    compose(
        app,
        ComposeMsg::Attached(vec![("plan.pdf".into(), Arc::new(vec![1, 2, 3]))]),
    );
}

#[test]
fn composer_is_saved_after_a_change_and_found_again_after_a_crash() {
    let path = temp_db("crash");
    {
        let store = Store::open(&path).unwrap();
        setup(&store, 1);
        let mut app = app_on(store);
        write(&mut app, "Ordre du jour");
        let composer = app.composer.as_ref().unwrap();
        assert!(composer.dirty && composer.autosave_pending);
        compose(&mut app, ComposeMsg::Autosave);
        let composer = app.composer.as_ref().unwrap();
        assert!(!composer.dirty && !composer.autosave_pending);
        let id = composer.draft_id.unwrap();
        let saved = app.store.draft(id).unwrap().unwrap();
        assert_eq!(saved.fields.subject, "Ordre du jour");
        assert!(saved.open);
        // Avero stops here, the composer still open.
    }
    let app = app_on(Store::open(&path).unwrap());
    let composer = app.composer.as_ref().expect("the draft is back");
    assert_eq!(composer.subject, "Ordre du jour");
    assert_eq!(composer.to, "Léa <lea@ex.fr>, zo");
    assert_eq!(composer.bcc, "chef@ex.fr");
    assert_eq!(composer.attachments.len(), 1);
    assert_eq!(*composer.attachments[0].data, vec![1, 2, 3]);
    assert!(app.toast.as_ref().unwrap().text.contains("récupéré"));
    drop(app);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn blank_composer_leaves_no_draft() {
    let mut app = app_with_mail(1);
    compose(&mut app, ComposeMsg::New);
    compose(&mut app, ComposeMsg::Cancel);
    assert!(app.composer.is_none());
    assert!(app.store.open_drafts().unwrap().is_empty());
    assert!(app.store.unsaved_drafts().unwrap().is_empty());
}

#[test]
fn closing_saves_the_draft_and_its_server_copy_replaces_the_previous() {
    let mut app = app_with_mail(1);
    let account = app.accounts[0].id;
    write(&mut app, "Devis");
    compose(&mut app, ComposeMsg::Cancel);
    assert!(app.composer.is_none());
    let draft = app.store.unsaved_drafts().unwrap().remove(0);
    assert!(!draft.open);
    assert!(draft.fields.message_id.ends_with("@ex.fr>"));
    // The worker saved it in "Drafts" as UID 7.
    let copy = DraftCopy {
        account,
        folder: "Drafts".into(),
        uid: 7,
    };
    later(
        &mut app,
        LaterMsg::DraftSaved(draft.id, Event::DraftSaved(Ok(Some(copy.clone())))),
    );
    let saved = app.store.draft(draft.id).unwrap().unwrap();
    assert_eq!(saved.server, Some(copy.clone()));
    assert!(!saved.server_dirty);
    assert!(app.store.unsaved_drafts().unwrap().is_empty());
    // A refusal keeps it here, for the next start.
    let mut fields = draft.fields.clone();
    fields.subject = "Devis v2".into();
    app.store
        .save_draft(Some(draft.id), &fields, None, NOW)
        .unwrap();
    app.store.set_draft_open(draft.id, false).unwrap();
    later(
        &mut app,
        LaterMsg::DraftSaved(draft.id, Event::DraftSaved(Err("NO quota".into()))),
    );
    assert!(app.toast.as_ref().unwrap().error);
    assert_eq!(app.store.unsaved_drafts().unwrap().len(), 1);
    // Its old copy is still the one to replace.
    assert_eq!(
        app.store.draft(draft.id).unwrap().unwrap().server,
        Some(copy)
    );
}

/// Puts a message in the drafts folder of `account` (with its body), as
/// a sync would.
fn server_draft(app: &App, account: AccountId, uid: u32, raw: &str) -> i64 {
    let folder = app
        .store
        .folder_by_role(account, FolderRole::Drafts)
        .unwrap()
        .unwrap();
    let parsed = avero_sync::parse::parse_headers(uid, true, raw.as_bytes());
    app.store.insert_messages(folder.id, &[parsed]).unwrap();
    let row = app.store.message_row(folder.id, uid).unwrap().unwrap();
    app.store.put_body(row, raw.as_bytes(), "", false).unwrap();
    row
}

#[test]
fn own_draft_reopens_from_the_drafts_folder_as_it_was() {
    let mut app = app_with_mail(1);
    let account = app.accounts[0].id;
    write(&mut app, "Devis");
    compose(&mut app, ComposeMsg::Cancel);
    let draft = app.store.unsaved_drafts().unwrap().remove(0);
    // Its copy on the server (as `draft_message` writes it).
    let raw = format!(
        "From: marie@ex.fr\r\nTo: lea@ex.fr\r\nSubject: Devis\r\nMessage-ID: {}\r\n\r\nx\r\n",
        draft.fields.message_id
    );
    server_draft(&app, account, 7, &raw);
    let _ = app.update(Message::FolderSelected(FolderRole::Drafts));
    let _ = app.update(Message::MessageSelected(0));
    let composer = app.composer.as_ref().expect("opened in the composer");
    assert_eq!(composer.draft_id, Some(draft.id));
    assert_eq!(composer.bcc, "chef@ex.fr", "what only this computer knew");
    assert_eq!(composer.attachments.len(), 1);
    assert_eq!(composer.server_copy.as_ref().unwrap().uid, 7);
    assert!(app.reading.is_none());
    // Sent: gone from here and from the drafts folder.
    compose(&mut app, ComposeMsg::To("lea@ex.fr".into()));
    compose(&mut app, ComposeMsg::Send);
    let _ = app.update(Message::Worker(account, Event::Sent(Ok(None))));
    assert!(app.composer.is_none());
    assert!(app.store.draft(draft.id).unwrap().is_none());
    assert!(app.items.is_empty());
}

#[test]
fn draft_written_elsewhere_opens_from_its_server_copy() {
    let mut app = app_with_mail(1);
    let account = app.accounts[0].id;
    let raw = "From: Marie <marie@ex.fr>\r\nTo: Paul <paul@ex.fr>\r\nBcc: chef@ex.fr\r\n\
               Subject: Vacances\r\nMessage-ID: <web-1@ex.fr>\r\nIn-Reply-To: <1@ex.fr>\r\n\
               MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
               --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nÀ bientôt\r\n\
               --b\r\nContent-Type: text/plain; name=a.txt\r\n\
               Content-Disposition: attachment; filename=a.txt\r\n\r\nfichier\r\n--b--\r\n";
    let row = server_draft(&app, account, 9, raw);
    let _ = app.update(Message::FolderSelected(FolderRole::Drafts));
    let _ = app.update(Message::MessageSelected(0));
    // The body is parsed off the UI thread first.
    assert!(app.composer.is_none());
    assert_eq!(app.pending_draft, Some(row));
    let opened = crate::background::OpenedBody::parse(raw.as_bytes());
    let _ = app.update(Message::BodyParsed(row, Ok(Arc::new(opened))));
    let composer = app.composer.as_ref().expect("opened in the composer");
    assert_eq!(composer.to, "Paul <paul@ex.fr>");
    assert_eq!(composer.bcc, "chef@ex.fr");
    assert_eq!(composer.subject, "Vacances");
    assert_eq!(composer.body.text(), "À bientôt");
    assert_eq!(composer.in_reply_to.as_deref(), Some("<1@ex.fr>"));
    assert_eq!(composer.message_id, "<web-1@ex.fr>");
    assert_eq!(composer.attachments.len(), 1);
    let copy = composer.server_copy.clone().unwrap();
    assert_eq!((copy.folder.as_str(), copy.uid), ("Drafts", 9));
    // Kept here from now on.
    let record = app
        .store
        .draft_by_message_id("<web-1@ex.fr>")
        .unwrap()
        .unwrap();
    assert!(!record.server_dirty);
    // Closed untouched: nothing to save again.
    compose(&mut app, ComposeMsg::Cancel);
    assert!(app.store.unsaved_drafts().unwrap().is_empty());
}

#[test]
fn sending_a_draft_deletes_it_here() {
    let mut app = app_with_mail(1);
    let account = app.accounts[0].id;
    write(&mut app, "Devis");
    compose(&mut app, ComposeMsg::To("lea@ex.fr".into()));
    compose(&mut app, ComposeMsg::Autosave);
    let id = app.composer.as_ref().unwrap().draft_id.unwrap();
    compose(&mut app, ComposeMsg::Send);
    assert!(app.composer.as_ref().unwrap().sending);
    let _ = app.update(Message::Worker(account, Event::Sent(Ok(None))));
    assert!(app.composer.is_none());
    assert!(app.store.draft(id).unwrap().is_none());
}

#[test]
fn discarding_deletes_the_draft() {
    let mut app = app_with_mail(1);
    write(&mut app, "Brouillon jetable");
    compose(&mut app, ComposeMsg::Autosave);
    let id = app.composer.as_ref().unwrap().draft_id.unwrap();
    compose(&mut app, ComposeMsg::Discard);
    assert!(app.composer.is_none());
    assert!(app.store.draft(id).unwrap().is_none());
}

// --------------------------------------------------------- scheduled

/// Writes a message to Léa and schedules it with `when`.
fn schedule(app: &mut App, when: When) -> i64 {
    compose(app, ComposeMsg::New);
    compose(app, ComposeMsg::To("lea@ex.fr".into()));
    compose(app, ComposeMsg::Subject("Plus tard".into()));
    later(app, LaterMsg::SendLater);
    let Some(Overlay::Later(picker)) = &app.overlay else {
        panic!("the time is asked");
    };
    assert_eq!(picker.choices(), &When::SEND);
    later(app, LaterMsg::Pick(when));
    assert!(app.composer.is_none());
    app.outbox[0].id
}

#[test]
fn send_later_checks_the_message_first() {
    let mut app = app_with_mail(1);
    compose(&mut app, ComposeMsg::New);
    compose(&mut app, ComposeMsg::Subject("Sans destinataire".into()));
    later(&mut app, LaterMsg::SendLater);
    assert!(app.overlay.is_none());
    assert!(app.composer.as_ref().unwrap().error.is_some());
}

#[test]
fn scheduled_mail_leaves_at_its_time() {
    let mut app = app_with_mail(1);
    let id = schedule(&mut app, When::Tomorrow);
    let at = app.outbox[0].send_at.unwrap();
    assert_eq!(
        at,
        crate::later::resolve(When::Tomorrow, &app.clock.local()).timestamp()
    );
    later(&mut app, LaterMsg::ShowScheduled);
    assert!(app.scheduled_shown() && app.items.is_empty());
    assert_eq!(app.outbox.len(), 1);
    // Not before its time.
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.is_empty());
    app.clock.set(at);
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.contains(&id));
    // Not sent twice while it goes.
    later(&mut app, LaterMsg::Tick);
    assert_eq!(app.sending.len(), 1);
    later(&mut app, LaterMsg::Sent(id, Event::Sent(Ok(None))));
    assert!(app.sending.is_empty());
    assert!(app.outbox.is_empty(), "sent on time: gone");
}

#[test]
fn mail_due_while_closed_leaves_at_start_and_says_it_was_late() {
    let path = temp_db("late");
    let at = {
        let store = Store::open(&path).unwrap();
        setup(&store, 1);
        let mut app = app_on(store);
        schedule(&mut app, When::Tonight);
        app.outbox[0].send_at.unwrap()
    };
    // Avero opens again a day later.
    let mut app = app_on(Store::open(&path).unwrap());
    app.clock.set(at + 86_400);
    later(&mut app, LaterMsg::Tick);
    let id = *app.sending.iter().next().expect("sent at start");
    later(&mut app, LaterMsg::Sent(id, Event::Sent(Ok(None))));
    let record = &app.outbox[0];
    assert_eq!(record.sent_at, Some(at + 86_400));
    assert!(at + 86_400 - at > LATE_AFTER);
    let status = scheduled_status(record, app.clock.now(), false);
    assert!(status.starts_with("Envoyé en retard"), "{status}");
    assert!(status.contains("prévu"), "{status}");
    later(&mut app, LaterMsg::Dismiss(id));
    assert!(app.outbox.is_empty());
    drop(app);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn failed_mail_stays_with_its_error_until_sent_again() {
    let mut app = app_with_mail(1);
    let id = schedule(&mut app, When::Tonight);
    app.clock.set(app.outbox[0].send_at.unwrap());
    later(&mut app, LaterMsg::Tick);
    later(
        &mut app,
        LaterMsg::Sent(id, Event::Sent(Err("envoi refusé par le serveur".into()))),
    );
    let record = &app.outbox[0];
    assert_eq!(record.error.as_deref(), Some("envoi refusé par le serveur"));
    let status = scheduled_status(record, app.clock.now(), false);
    assert!(status.contains("Échec"), "{status}");
    // Not tried again on its own.
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.is_empty());
    // "Envoyer maintenant".
    later(&mut app, LaterMsg::SendNow(id));
    assert!(app.sending.contains(&id));
    later(&mut app, LaterMsg::Sent(id, Event::Sent(Ok(None))));
    assert!(app.outbox.is_empty());
}

#[test]
fn send_cut_short_is_reported_not_repeated() {
    let path = temp_db("cut");
    {
        let store = Store::open(&path).unwrap();
        setup(&store, 1);
        let mut app = app_on(store);
        let id = schedule(&mut app, When::Tonight);
        app.clock.set(app.outbox[0].send_at.unwrap());
        later(&mut app, LaterMsg::Tick);
        assert!(app.sending.contains(&id));
        // Avero stops before the answer.
    }
    let mut app = app_on(Store::open(&path).unwrap());
    assert_eq!(app.outbox[0].error.as_deref(), Some(INTERRUPTED));
    app.clock.set(NOW + 10 * 86_400);
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.is_empty());
    drop(app);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mail_another_window_is_sending_is_not_sent_again() {
    let mut app = app_with_mail(1);
    let id = schedule(&mut app, When::Tonight);
    // Another Avero on the same database reserved it first.
    assert!(app.store.reserve_send(id, NOW).unwrap());
    later(&mut app, LaterMsg::SendNow(id));
    app.clock.set(NOW + 10 * 86_400);
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.is_empty());
}

#[test]
fn scheduled_mail_is_edited_then_keeps_its_time_or_is_cancelled() {
    let mut app = app_with_mail(1);
    let id = schedule(&mut app, When::Monday);
    let at = app.outbox[0].send_at.unwrap();
    later(&mut app, LaterMsg::Edit(id));
    let composer = app.composer.as_ref().unwrap();
    assert_eq!(composer.send_at, Some(at));
    assert_eq!(composer.subject, "Plus tard");
    // Not sent while being edited.
    app.clock.set(at);
    later(&mut app, LaterMsg::Tick);
    assert!(app.sending.is_empty());
    compose(&mut app, ComposeMsg::Subject("Plus tard (modifié)".into()));
    compose(&mut app, ComposeMsg::Cancel);
    assert!(app.composer.is_none());
    let record = &app.outbox[0];
    assert_eq!(record.send_at, Some(at));
    assert_eq!(record.fields.subject, "Plus tard (modifié)");
    // No copy in the drafts folder for a scheduled mail.
    assert!(app.store.unsaved_drafts().unwrap().is_empty());
    later(&mut app, LaterMsg::Cancel(id));
    assert!(app.outbox.is_empty());
    assert!(app.store.draft(id).unwrap().is_none());
}

#[test]
fn draft_files_are_saved_only_when_they_change() {
    let mut app = app_with_mail(1);
    write(&mut app, "Pièce jointe");
    compose(&mut app, ComposeMsg::Autosave);
    let id = app.composer.as_ref().unwrap().draft_id.unwrap();
    assert!(!app.composer.as_ref().unwrap().files_dirty);
    // Changed behind the composer's back: a save without file changes
    // does not write them again.
    let other = DraftFile {
        name: "autre.pdf".into(),
        mime: "application/pdf".into(),
        data: vec![9],
    };
    let fields: DraftFields = app.store.draft(id).unwrap().unwrap().fields;
    app.store
        .save_draft(Some(id), &fields, Some(std::slice::from_ref(&other)), NOW)
        .unwrap();
    compose(&mut app, ComposeMsg::Subject("Pièce jointe !".into()));
    compose(&mut app, ComposeMsg::Autosave);
    assert_eq!(app.store.draft_files(id).unwrap(), vec![other]);
    compose(&mut app, ComposeMsg::RemoveAttachment(0));
    compose(&mut app, ComposeMsg::Autosave);
    assert!(app.store.draft_files(id).unwrap().is_empty());
}
